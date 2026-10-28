//! Bytecode VM: executes compiled `Chunk`s with a stack machine.
//!
//! Frames share one value stack; segments (`SegStart`/`SegEnd`) carve list
//! regions for multi-value bookkeeping; `last_ret` carries the expansion count
//! of the last value-producing operation (Lua's "first value" and trailing
//! expansion rules). Locals live directly in per-frame slot vecs; a slot is
//! promoted to a shared `Rc<RefCell<SlotV>>` cell only when a closure captures
//! it. Closures therefore keep the interpreter's aliasing and moved-value
//! (E1001) semantics, while the common non-captured local path has no `Rc`
//! allocation and no `RefCell` borrow check.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::compiler::{Compiled, Instr};
use crate::error::{LResult, LusaError, Span, E_CALL, E_LEN, E_MOVED, E_RUNTIME};
use crate::interp::{Interp, MAX_CALL_DEPTH};
use crate::scope;
use crate::value::{format_number, span0, AliasKind, SlotV, TableObj, Value};

// ===== state =====

pub struct Vm {
    pub frames: Vec<VmFrame>,
    pub stack: Vec<Value>,
    pub segs: Vec<Seg>,
    /// values handed out by the last `coroutine.yield`
    pub yielded: Vec<Value>,
    /// number of values left by the last expansion (call / `...`)
    pub last_ret: usize,
    /// `LUSA_TRACE_VM` is read once per VM, never per instruction
    pub trace: bool,
    /// Recycled `Vec<SlotCell>` buffers (frame local tables). Captured cells
    /// are dropped normally; the vector capacity is what is being reused.
    slot_vec_pool: Vec<Vec<SlotCell>>,
    /// One-entry read cache for `GetGlob`. The scope version makes stale hits
    /// impossible even when a builtin or another frame mutates globals.
    global_cache: Option<GlobalCache>,
}

struct GlobalCache {
    name: Arc<str>,
    version: u64,
    value: Value,
}

pub struct Seg {
    start: usize,
    cursor: usize,
}

/// A local slot. Most locals stay `Direct`; `Shared` is created lazily the
/// first time a nested closure captures the slot. The enum is the only branch
/// on the hot local path, and it is perfectly predicted for non-capturing
/// functions (the overwhelmingly common case).
pub enum SlotCell {
    Direct(SlotV),
    Shared(Rc<RefCell<SlotV>>),
}

/// Drops the frame's handle to a captured cell so the register can be rebound.
/// The closure keeps its own `Rc`; the register becomes a fresh direct cell.
#[inline]
fn fresh_if_shared(cell: &mut SlotCell) {
    if matches!(cell, SlotCell::Shared(_)) {
        *cell = SlotCell::new();
    }
}

impl SlotCell {
    #[inline]
    fn new() -> Self {
        SlotCell::Direct(SlotV::Set(Value::Nil))
    }

    /// Replaces the slot contents. A captured slot keeps its identity.
    #[inline]
    fn set(&mut self, v: SlotV) {
        match self {
            SlotCell::Direct(s) => *s = v,
            SlotCell::Shared(h) => *h.borrow_mut() = v,
        }
    }

    /// Reads a copy of the value, or `None` when the slot was moved out.
    #[inline]
    fn read_clone(&self) -> Option<Value> {
        match self {
            SlotCell::Direct(SlotV::Set(v)) => Some(v.clone()),
            SlotCell::Direct(SlotV::Moved(_)) => None,
            SlotCell::Shared(h) => match &*h.borrow() {
                SlotV::Set(v) => Some(v.clone()),
                SlotV::Moved(_) => None,
            },
        }
    }

    /// Fast scalar read. `None` means "not a scalar"; callers then use
    /// `read_clone`/`inspect` so moved-value errors remain distinct.
    #[inline]
    fn scalar(&self) -> Option<Value> {
        match self {
            SlotCell::Direct(SlotV::Set(Value::Number(n))) => Some(Value::Number(*n)),
            SlotCell::Direct(SlotV::Set(Value::Bool(b))) => Some(Value::Bool(*b)),
            SlotCell::Direct(SlotV::Set(Value::Nil)) => Some(Value::Nil),
            SlotCell::Shared(h) => {
                let b = h.borrow();
                match &*b {
                    SlotV::Set(Value::Number(n)) => Some(Value::Number(*n)),
                    SlotV::Set(Value::Bool(v)) => Some(Value::Bool(*v)),
                    SlotV::Set(Value::Nil) => Some(Value::Nil),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Inspects the slot without cloning. Used by the deferred argument
    /// journal and by numeric loop checks.
    #[inline]
    fn inspect<R>(&self, f: impl FnOnce(&SlotV) -> R) -> R {
        match self {
            SlotCell::Direct(s) => f(s),
            SlotCell::Shared(h) => {
                let b = h.borrow();
                f(&b)
            }
        }
    }

    /// Promotes this slot to a shared cell (or returns the existing one).
    fn make_shared(&mut self) -> Rc<RefCell<SlotV>> {
        if let SlotCell::Shared(h) = self {
            return h.clone();
        }
        let old = std::mem::replace(self, SlotCell::Direct(SlotV::Set(Value::Nil)));
        let value = match old {
            SlotCell::Direct(s) => s,
            SlotCell::Shared(_) => unreachable!("checked above"),
        };
        let handle = Rc::new(RefCell::new(value));
        *self = SlotCell::Shared(handle.clone());
        handle
    }
}

pub struct VmFrame {
    pub proto: Rc<Compiled>,
    pub slots: Vec<SlotCell>,
    /// instance-captured upvalues, in `proto.upvals` order
    pub upvals: Rc<[Rc<RefCell<SlotV>>]>,
    pub varargs: Vec<Value>,
    pub pc: usize,
    /// set by the caller's `CallOne`; the return path keeps only the first
    /// result (and materializes `nil` for a zero-result call)
    pub(crate) truncate_return: bool,
    /// deferred call-argument takes pending finalization by the next call op
    pub(crate) undo: Vec<UndoRec>,
    /// length of the shared value stack at frame entry; `?` truncates here
    pub(crate) stack_base: usize,
    /// length of the segment stack at frame entry; `?` truncates here
    pub(crate) seg_base: usize,
    /// `unsafe` depth of the caller at entry. Restored when this frame returns
    /// or unwinds, so an early `return` cannot leak sandbox capability.
    entry_unsafe_depth: usize,
}

impl Vm {
    pub fn new() -> Vm {
        Vm {
            // The frame stack is bounded by `MAX_CALL_DEPTH`; reserving it once
            // removes `Vec` growth from the call path.
            frames: Vec::with_capacity(MAX_CALL_DEPTH),
            // Warm the shared value stack so ordinary expression evaluation
            // never reallocates. Individual frames reserve a small window too.
            stack: Vec::with_capacity(1024),
            segs: Vec::with_capacity(16),
            yielded: Vec::new(),
            last_ret: 1,
            trace: std::env::var_os("LUSA_TRACE_VM").is_some(),
            slot_vec_pool: Vec::new(),
            global_cache: None,
        }
    }
}

enum StepOutcome {
    Continue,
    /// Return values are left on the value stack; the count says how many.
    Returned(usize),
    /// A `coroutine.yield` suspended the VM. `one` says the yield expression
    /// expects a single resume value; the yielded values live in `Vm::yielded`.
    Yielded(usize, bool),
}

// ===== driving =====

/// Calls a compiled function with the given captured upvalues and arguments,
/// running until it returns. Used both by direct VM calls (`Call` dispatch)
/// and by the interpreter's `call_value` (`FuncBody::Vm`).
pub fn invoke(
    interp: &mut Interp,
    compiled: &Rc<Compiled>,
    upvals: Rc<[Rc<RefCell<SlotV>>]>,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    let stack_base = interp.vm.stack.len();
    let seg_base = interp.vm.segs.len();
    let frames_base = interp.vm.frames.len();
    match invoke_inner(interp, compiled, upvals, args, span) {
        Ok(vals) => Ok(vals),
        Err(e) => {
            // unwind the frames that were entered here, recycling their slots
            while interp.vm.frames.len() > frames_base {
                pop_frame_recycling(interp);
            }
            interp.vm.stack.truncate(stack_base);
            interp.vm.segs.truncate(seg_base);
            interp.vm.last_ret = 1;
            let _ = span;
            Err(e)
        }
    }
}

fn invoke_inner(
    interp: &mut Interp,
    compiled: &Rc<Compiled>,
    upvals: Rc<[Rc<RefCell<SlotV>>]>,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    if interp.vm.frames.len() >= MAX_CALL_DEPTH {
        return Err(
            LusaError::new(
                E_RUNTIME,
                Some(span),
                "call stack overflow (too many nested calls)",
            )
            .help(format!("Lusa allows {} nested calls", MAX_CALL_DEPTH)),
        );
    }
    push_frame(interp, compiled, upvals, args, span)?;
    let n = run_till_return(interp, interp.vm.frames.len() - 1)?;
    let sp = interp.vm.stack.len();
    let take = n.min(sp);
    Ok(interp.vm.stack.split_off(sp - take))
}

/// Upper bound for the free list. It is deliberately modest: the point is to
/// recycle the slots of the current call depth, not to retain unbounded memory
/// after a program creates a huge number of closures.
const SLOT_VEC_POOL_MAX: usize = 256;

#[inline]
fn take_slot_vec(interp: &mut Interp, nlocals: usize) -> Vec<SlotCell> {
    let mut slots = interp.vm.slot_vec_pool.pop().unwrap_or_default();
    slots.clear();
    if slots.capacity() < nlocals {
        slots.reserve(nlocals - slots.capacity());
    }
    slots
}

/// Pops a frame and recycles its slot vector. Captured cells are dropped (their
/// closure keeps the `Rc` alive); direct slots are reset to `Nil` so the next
/// frame starts clean without allocating.
fn pop_frame_recycling(interp: &mut Interp) {
    let mut frame = interp.vm.frames.pop().expect("frame");
    // Capability belongs to the frame that opened it. Restore the caller's
    // depth on every exit path, including early returns and error unwinds.
    interp.unsafe_depth = frame.entry_unsafe_depth;
    // Drop values first: a closure stored in this frame may be the only extra
    // owner of another slot in the same frame.
    frame.varargs.clear();
    frame.undo.clear();
    let mut slots = std::mem::take(&mut frame.slots);
    for cell in &mut slots {
        *cell = SlotCell::new();
    }
    if interp.vm.slot_vec_pool.len() < SLOT_VEC_POOL_MAX {
        interp.vm.slot_vec_pool.push(slots);
    }
}

/// Builds the callee frame: nil-filled slots, renamed params, varargs rest.
fn push_frame(
    interp: &mut Interp,
    compiled: &Rc<Compiled>,
    upvals: Rc<[Rc<RefCell<SlotV>>]>,
    args: Vec<Value>,
    _span: Span,
) -> LResult<()> {
    if interp.vm.frames.len() >= crate::interp::MAX_CALL_DEPTH {
        return Err(LusaError::new(
            crate::error::E_RUNTIME,
            Some(span0()),
            "call stack overflow (too many nested calls)",
        )
        .help(format!(
            "Lusa allows {} nested calls",
            crate::interp::MAX_CALL_DEPTH
        )));
    }
    let proto = &compiled.proto;
    let nparams = proto.nparams;
    // Reserve the operand window for this frame up front. The shared stack
    // keeps its capacity across calls, so after warm-up pushes are sequential
    // writes into already-committed memory.
    interp.vm.stack.reserve(proto.nlocals + 32);
    let mut slots = take_slot_vec(interp, proto.nlocals);
    for _ in 0..proto.nlocals {
        slots.push(SlotCell::new());
    }
    for (i, slot) in slots.iter_mut().enumerate() {
        if i >= nparams {
            break;
        }
        let v = args.get(i).cloned().unwrap_or(Value::Nil);
        if let Some(vid) = v.vid() {
            let name = proto
                .slot_labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| Arc::from("(parameter)"));
            crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
        }
        slot.set(SlotV::Set(v));
    }
    let varargs: Vec<Value> = if proto.has_vararg {
        args.into_iter().skip(nparams).collect()
    } else {
        Vec::new()
    };
    let stack_base = interp.vm.stack.len();
    let seg_base = interp.vm.segs.len();
    interp.vm.frames.push(VmFrame {
        proto: compiled.clone(),
        slots,
        upvals,
        varargs,
        pc: 0,
        truncate_return: false,
        undo: Vec::new(),
        stack_base,
        seg_base,
        entry_unsafe_depth: interp.unsafe_depth,
    });
    Ok(())
}

/// Builds the callee frame directly from a region of the shared value stack.
///
/// `arg_start..arg_start+nargs` are the explicit arguments; when `recv_pos` is
/// set it points at the method receiver (param 0). The source region is
/// truncated to `base` only after all parameters/varargs have been copied.
fn push_frame_at(
    interp: &mut Interp,
    compiled: Rc<Compiled>,
    upvals: Rc<[Rc<RefCell<SlotV>>]>,
    arg_start: usize,
    nargs: usize,
    recv_pos: Option<usize>,
    base: usize,
    _span: Span,
) -> LResult<()> {
    if interp.vm.frames.len() >= MAX_CALL_DEPTH {
        return Err(LusaError::new(
            crate::error::E_RUNTIME,
            Some(span0()),
            "call stack overflow (too many nested calls)",
        )
        .help(format!("Lusa allows {} nested calls", MAX_CALL_DEPTH)));
    }
    let proto = &compiled.proto;
    let nparams = proto.nparams;
    let recv_off = usize::from(recv_pos.is_some());
    let total = nargs + recv_off;
    interp.vm.stack.reserve(proto.nlocals + 32);
    let mut slots = take_slot_vec(interp, proto.nlocals);
    for _ in 0..proto.nlocals {
        slots.push(SlotCell::new());
    }
    let value_at = |stack: &[Value], i: usize| -> Value {
        let idx = if i == 0 && recv_pos.is_some() {
            recv_pos.unwrap_or(arg_start)
        } else {
            arg_start + i - recv_off
        };
        stack.get(idx).cloned().unwrap_or(Value::Nil)
    };
    for (i, slot) in slots.iter_mut().enumerate().take(nparams.min(total)) {
        let v = value_at(&interp.vm.stack, i);
        if let Some(vid) = v.vid() {
            let name = proto
                .slot_labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| Arc::from("(parameter)"));
            crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
        }
        slot.set(SlotV::Set(v));
    }
    let varargs: Vec<Value> = if proto.has_vararg && total > nparams {
        let mut out = Vec::with_capacity(total - nparams);
        for i in nparams..total {
            out.push(value_at(&interp.vm.stack, i));
        }
        out
    } else {
        Vec::new()
    };
    interp.vm.stack.truncate(base.min(interp.vm.stack.len()));
    let stack_base = interp.vm.stack.len();
    let seg_base = interp.vm.segs.len();
    interp.vm.frames.push(VmFrame {
        proto: compiled,
        slots,
        upvals,
        varargs,
        pc: 0,
        truncate_return: false,
        undo: Vec::new(),
        stack_base,
        seg_base,
        entry_unsafe_depth: interp.unsafe_depth,
    });
    Ok(())
}
/// Keeps the first of `n` values on the stack, materializing `nil` for a
/// zero-result call. Shared by `TruncateToOne` and fused single-value calls.
#[inline]
fn truncate_results_to_one(interp: &mut Interp, n: usize) {
    if n == 0 {
        interp.vm.stack.push(Value::Nil);
    } else if n > 1 {
        let sp = interp.vm.stack.len();
        interp.vm.stack.truncate(sp.saturating_sub(n - 1));
    }
    interp.vm.last_ret = 1;
}

/// Runs instructions until the frame at `base` returns, unwinding nested
/// frames along the way. Return values are already at the top of the shared
/// value stack; this returns how many there are (no `Vec` is built here).
fn run_till_return(interp: &mut Interp, base: usize) -> LResult<usize> {
    loop {
        if interp.vm.frames.len() <= base {
            return Ok(0);
        }
        match step(interp)? {
            StepOutcome::Continue => {}
            StepOutcome::Yielded(_, _) => {
                return Err(LusaError::new(
                    E_RUNTIME,
                    Some(span0()),
                    "attempt to yield from outside a coroutine",
                ));
            }
            StepOutcome::Returned(n) => {
                let truncate = interp
                    .vm
                    .frames
                    .last()
                    .map(|f| f.truncate_return)
                    .unwrap_or(false);
                pop_frame_recycling(interp);
                if truncate {
                    truncate_results_to_one(interp, n);
                } else {
                    interp.vm.last_ret = n;
                }
                if interp.vm.frames.len() == base {
                    return Ok(interp.vm.last_ret);
                }
                // Nested return: the values stay exactly where the callee
                // pushed them, ready to feed the caller's stack expression.
            }
        }
    }
}

/// Suspends the current VM at a `coroutine.yield` site. The yielded values are
/// moved into `Vm::yielded`; the coroutine driver consumes them after the VM
/// state has been swapped back out.
fn yield_values(
    interp: &mut Interp,
    nargs: usize,
    n_takes: usize,
    one: bool,
) -> LResult<StepOutcome> {
    if n_takes != 0 {
        finalize_args(interp, false, n_takes)?;
    }
    let len = interp.vm.stack.len();
    let start = len.saturating_sub(nargs);
    let values: Vec<Value> = interp.vm.stack.drain(start..).collect();
    interp.vm.yielded = values;
    interp.vm.last_ret = 1;
    Ok(StepOutcome::Yielded(interp.vm.yielded.len(), one))
}

/// What a coroutine VM did in one `drive_coroutine` slice.
pub enum CoroDrive {
    /// suspended at a yield; `usize` = yielded value count, `bool` = the
    /// resume result is truncated to one value
    Yielded(usize, bool),
    /// the coroutine's outermost frame returned; `usize` = result count
    Returned(usize),
}

/// Starts a new coroutine frame in the currently installed VM.
pub fn coroutine_begin(
    interp: &mut Interp,
    compiled: Rc<Compiled>,
    upvals: Rc<[Rc<RefCell<SlotV>>]>,
    args: Vec<Value>,
    span: Span,
) -> LResult<()> {
    push_frame(interp, &compiled, upvals, args, span)
}

/// Runs the installed coroutine VM until it yields or its outermost frame
/// returns. Frames are not unwound on error here; `coroutine_reset` does that.
pub fn drive_coroutine(interp: &mut Interp) -> LResult<CoroDrive> {
    loop {
        match step(interp)? {
            StepOutcome::Continue => {}
            StepOutcome::Yielded(n, one) => return Ok(CoroDrive::Yielded(n, one)),
            StepOutcome::Returned(n) => {
                let truncate = interp
                    .vm
                    .frames
                    .last()
                    .map(|f| f.truncate_return)
                    .unwrap_or(false);
                pop_frame_recycling(interp);
                if truncate {
                    truncate_results_to_one(interp, n);
                } else {
                    interp.vm.last_ret = n;
                }
                if interp.vm.frames.is_empty() {
                    return Ok(CoroDrive::Returned(interp.vm.last_ret));
                }
            }
        }
    }
}

/// Supplies resume arguments as the result of the suspended yield.
pub fn coroutine_supply(interp: &mut Interp, mut args: Vec<Value>, one: bool) {
    if one {
        let value = if args.is_empty() {
            Value::Nil
        } else {
            args.swap_remove(0)
        };
        interp.vm.stack.push(value);
        interp.vm.last_ret = 1;
    } else {
        let n = args.len();
        interp.vm.stack.append(&mut args);
        interp.vm.last_ret = n;
    }
}

/// Takes the yielded values out of the installed coroutine VM.
pub fn coroutine_take_yielded(interp: &mut Interp) -> Vec<Value> {
    std::mem::take(&mut interp.vm.yielded)
}

/// Takes the top `n` result values out of the installed coroutine VM.
pub fn coroutine_take_results(interp: &mut Interp, n: usize) -> Vec<Value> {
    let take = n.min(interp.vm.stack.len());
    interp.vm.stack.split_off(interp.vm.stack.len() - take)
}

/// Drops every value and frame in the installed coroutine VM. Runs after a
/// coroutine error or `coroutine.close`.
pub fn coroutine_reset(interp: &mut Interp) {
    while !interp.vm.frames.is_empty() {
        pop_frame_recycling(interp);
    }
    interp.vm.stack.clear();
    interp.vm.segs.clear();
    interp.vm.yielded.clear();
    interp.vm.last_ret = 1;
}

/// Runs a compiled main program chunk to completion.
pub fn run_main(interp: &mut Interp, compiled: Rc<Compiled>) -> LResult<Value> {
    interp.vm.frames.clear();
    interp.vm.stack.clear();
    interp.vm.segs.clear();
    interp.vm.yielded.clear();
    interp.vm.last_ret = 1;
    let args: Vec<Value> = Vec::new();
    invoke(interp, &compiled, Rc::from([]), args, span0())?;
    Ok(Value::Nil)
}
/// Prints the compiled bytecode when `LUSA_DEBUG_VM=1` (debug aid).
pub fn debug_dump(compiled: &Compiled) {
    if std::env::var("LUSA_DEBUG_VM").is_err() {
        return;
    }
    fn go(c: &Compiled, indent: &str) {
        eprintln!(
            "{}chunk nparams={} has_vararg={} nlocals={}",
            indent, c.proto.nparams, c.proto.has_vararg, c.proto.nlocals
        );
        for (i, ins) in c.proto.code.iter().enumerate() {
            let line = c.proto.line_at(i);
            let local = match c.proto.local_names.get(i).and_then(|name| name.clone()) {
                Some(name) => format!("   ; {}", name),
                None => String::new(),
            };
            eprintln!("{}  {:04} line {:>3}  {:?}{}", indent, i, line, ins, local);
        }
        for p in &c.proto.prototypes {
            go(p, &format!("{}  ", indent));
        }
    }
    go(compiled, "");
}

/// Compiles and runs a program in bytecode mode. Mirrors `run_source`.
pub fn run_source_vm(src: &str) -> LResult<Vec<String>> {
    run_source_vm_with_base(src, None)
}

/// Compiles and runs a program in bytecode mode, resolving imports relative
/// to `base`.
pub fn run_source_vm_with_base(src: &str, base: Option<&std::path::Path>) -> LResult<Vec<String>> {
    let joined = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(s, move || {
                let compiled = crate::compiler::compile_program_with_base(src, base)?;
                debug_dump(&compiled);
                let mut interp = Interp::new();
                interp.install_stdlib();
                let result = run_main(&mut interp, compiled);
                let out = std::mem::take(&mut interp.out);
                result.map(|_| out)
            })
            .expect("failed to spawn VM thread")
            .join()
    });
    joined.expect("VM thread panicked")
}

// ===== execution =====

fn err(code: &'static str, msg: impl Into<String>) -> LusaError {
    LusaError::new(code, Some(span0()), msg.into())
}

/// Error at a known source span (ownership/borrow faults).
fn err_at(span: Span, code: &'static str, msg: impl Into<String>) -> LusaError {
    LusaError::new(code, Some(span), msg.into())
}

/// Source span of the instruction currently executing in `frame`.
///
/// `step` advanced `pc` before dispatch, so the instruction being executed is
/// `pc - 1`. This is only called on error paths; the hot loop never consults
/// the line table.
#[inline]
fn frame_span(frame: &VmFrame) -> Span {
    frame.proto.proto.span_at(frame.pc.saturating_sub(1))
}

#[inline]
fn current_span(interp: &Interp) -> Span {
    interp
        .vm
        .frames
        .last()
        .map(frame_span)
        .unwrap_or_else(span0)
}

/// Global read whose source span is resolved only if the read fails. Global
/// argument/call-finalization paths are warm enough that an eager line lookup
/// would show up; the ownership error is the rare branch.
fn read_global_deferred_span(interp: &mut Interp, name: &str) -> LResult<Value> {
    match scope::read_slot(&[interp.globals.clone()], name, span0()) {
        Ok(v) => Ok(v),
        Err(mut e) => {
            e.span = Some(current_span(interp));
            Err(e)
        }
    }
}

#[inline(always)]
fn step(interp: &mut Interp) -> LResult<StepOutcome> {
    // The top frame is the only one we can execute. Use `last()` so the hot
    // path does not recompute an index and bounds-check the frame vector twice.
    let frame = interp.vm.frames.last().expect("a frame is stepping");
    let pc = frame.pc;
    // `Instr` is Copy and compact: this is a small register/memory copy, not a
    // heap clone. No `Rc` is touched while decoding.
    let ins = match frame.proto.proto.code.get(pc).copied() {
        Some(ins) => ins,
        // fell off the end of a chunk without an explicit return
        None => return Ok(StepOutcome::Returned(0)),
    };
    interp.vm.frames.last_mut().unwrap().pc = pc + 1;

    // Trace flag is cached in `Vm`; checking an env var here would allocate a
    // `String` and take the environment lock on every single instruction.
    if interp.vm.trace {
        let fidx = interp.vm.frames.len() - 1;
        eprintln!(
            "[frame {} pc {:03}] {:?} stack={} segs={:?} last_ret={}",
            fidx,
            pc,
            &ins,
            interp.vm.stack.len(),
            interp
                .vm
                .segs
                .iter()
                .map(|s| (s.start, s.cursor))
                .collect::<Vec<_>>(),
            interp.vm.last_ret,
        );
    }

    exec_instr(&ins, interp)
}

/// The interpreter-loop body: one dense `match` over the opcode tag.
///
/// All instruction payloads are integers, so decoding never allocates. LLVM
/// lowers this match to a single jump table; the previous version dispatched
/// twice (common opcodes here, the rest through `instr_ops`).
#[inline(always)]
fn exec_instr(ins: &Instr, interp: &mut Interp) -> LResult<StepOutcome> {
    Ok(match ins {
        Instr::LoadConst(k) => {
            push_const(interp, *k)?;
            StepOutcome::Continue
        }
        Instr::LoadNil => {
            interp.vm.stack.push(Value::Nil);
            StepOutcome::Continue
        }
        Instr::GetLoc(slot) => {
            push_local(interp, *slot as usize)?;
            StepOutcome::Continue
        }
        Instr::GetUp(idx) => {
            push_upval(interp, *idx as usize)?;
            StepOutcome::Continue
        }
        Instr::TakeLoc(slot, labelk) => {
            let src_label = slot_label(interp, *slot as usize);
            let dest = const_str(interp, *labelk)?;
            take_local_storage(interp, *slot as usize, &src_label, &dest)?;
            StepOutcome::Continue
        }
        Instr::TakeUp(idx, labelk) => {
            let handle = upval_handle(interp, *idx as usize)?;
            let src_label = upval_label(interp, *idx as usize);
            let dest = const_str(interp, *labelk)?;
            take_slot_storage(interp, &handle, &src_label, &dest)?;
            StepOutcome::Continue
        }
        Instr::TakeGlob(k, labelk) => {
            let name = const_str(interp, *k)?;
            let dest = const_str(interp, *labelk)?;
            let v = match read_global_fast(interp, &name) {
                Some(v) => v,
                None => scope::read_slot(
                    std::slice::from_ref(&interp.globals),
                    &name,
                    current_span(interp),
                )?,
            };
            if matches!(v, Value::Table(_) | Value::Func(_)) {
                if let Some(copy) = interp.copy_struct_value(&v) {
                    interp.vm.stack.push(copy);
                    interp.vm.last_ret = 1;
                    return Ok(StepOutcome::Continue);
                }
                let vid = v.vid().expect("heap value has vid");
                if !interp.borrowed_by(vid).is_empty() {
                    return Err(err_at(
                        current_span(interp),
                        crate::error::E_BORROW_CONFLICT,
                        format!("cannot move out of `{}` because it is borrowed", name),
                    ));
                }
                let span = current_span(interp);
                let note = crate::value::MoveNote { to: dest.to_string(), span };
                scope::set_slot(&[], &interp.globals, &name, SlotV::Moved(vec![note]));
                crate::value::with_reg(|r| r.rename(vid, dest.to_string(), span));
            }
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::TakeField(k_key, k_src, k_label) => {
            let obj = pop1(interp);
            let key = const_value(interp, *k_key)?;
            let src = const_str(interp, *k_src)?;
            let dest = const_str(interp, *k_label)?;
            let t = expect_table(&obj)?;
            take_table_storage(interp, &t, &key, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::TakeIndex(sk, lk) => {
            let key = pop1(interp);
            let obj = pop1(interp);
            let src = const_str(interp, *sk)?;
            let dest = const_str(interp, *lk)?;
            let t = expect_table(&obj)?;
            take_table_storage(interp, &t, &key, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::GetGlob(k) => {
            let name = const_str(interp, *k)?;
            let version = interp.globals.borrow().version;
            let cached = match &interp.vm.global_cache {
                Some(c) if c.version == version && Arc::ptr_eq(&c.name, &name) => {
                    Some(c.value.clone())
                }
                _ => None,
            };
            let v = match cached {
                Some(v) => v,
                None => {
                    let v = match read_global_fast(interp, &name) {
                        Some(v) => v,
                        None => scope::read_slot(
                            std::slice::from_ref(&interp.globals),
                            &name,
                            current_span(interp),
                        )?,
                    };
                    interp.vm.global_cache = Some(GlobalCache {
                        name,
                        version,
                        value: v.clone(),
                    });
                    v
                }
            };
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::NewTable => {
            interp
                .vm
                .stack
                .push(Value::Table(Rc::new(TableObj::new())));
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::MarkStruct(k) => {
            let name = const_str(interp, *k)?;
            if let Some(Value::Table(t)) = interp.vm.stack.last() {
                interp
                    .struct_tables
                    .borrow_mut()
                    .insert(t.vid, name.to_string());
            }
            StepOutcome::Continue
        }
        Instr::GetField(k) => {
            let obj = pop1(interp);
            let key = const_value(interp, *k)?;
            let v = crate::eval::read_field(&obj, &key, span0())?;
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::GetIndex => {
            let key = pop1(interp);
            let obj = pop1(interp);
            let v = crate::eval::read_field(&obj, &key, span0())?;
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SetGlob(k) => {
            let name = const_str(interp, *k)?;
            let v = pop1(interp);
            if let Some(vid) = v.vid() {
                crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
            }
            scope::set_slot(&[], &interp.globals, &name, SlotV::Set(v));
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SetLoc(slot) => {
            let v = pop1(interp);
            let frame = interp.vm.frames.last_mut().expect("frame");
            if let Some(vid) = v.vid() {
                let pc = frame.pc.saturating_sub(1);
                let label = frame.proto.proto.local_name_at(pc, *slot as usize);
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            let cell = frame.slots.get_mut(*slot as usize).ok_or_else(|| {
                err(
                    crate::error::E_RUNTIME,
                    format!("internal: slot {} out of range", slot),
                )
            })?;
            cell.set(SlotV::Set(v));
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::ResetSlot(slot) => {
            // End of a captured binding's lexical life. The closure (if any)
            // holds its own `Rc`; detaching the register lets the allocator
            // reuse the index without aliasing the captured variable.
            let frame = interp.vm.frames.last_mut().expect("frame");
            if let Some(cell) = frame.slots.get_mut(*slot as usize) {
                fresh_if_shared(cell);
            }
            StepOutcome::Continue
        }
        Instr::ClearSlot(slot) => {
            // Lexical scope exit: dropping the value releases borrow aliases
            // before the register is reused by a sibling block.
            let frame = interp.vm.frames.last_mut().expect("frame");
            if let Some(cell) = frame.slots.get_mut(*slot as usize) {
                cell.set(SlotV::Set(Value::Nil));
            }
            StepOutcome::Continue
        }
        Instr::SetUp(idx) => {
            let v = pop1(interp);
            let frame = interp.vm.frames.last().expect("frame");
            let cell = frame.upvals.get(*idx as usize).ok_or_else(|| {
                err(
                    crate::error::E_RUNTIME,
                    format!("internal: upvalue {} out of range", idx),
                )
            })?;
            if let Some(vid) = v.vid() {
                let label = frame
                    .proto
                    .proto
                    .upval_labels
                    .get(*idx as usize)
                    .cloned()
                    .unwrap_or_else(|| Arc::from("(captured)"));
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            *cell.borrow_mut() = SlotV::Set(v);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SetFieldK(k) => {
            let v = pop1(interp);
            let obj = pop1(interp);
            let key = const_value(interp, *k)?;
            interp.write_table_slot(
                obj.clone(),
                key,
                v,
                "field",
                current_span(interp),
            )?;
            interp.vm.stack.push(obj);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SetIndex => {
            // layout from compile_table: [table, key, value]
            let v = pop1(interp);
            let key = pop1(interp);
            let obj = pop1(interp);
            interp.write_table_slot(
                obj.clone(),
                key,
                v,
                "index",
                current_span(interp),
            )?;
            interp.vm.stack.push(obj);
            StepOutcome::Continue
        }
        Instr::SetArr => {
            let v = pop1(interp);
            let obj = pop1(interp);
            let t = expect_table(&obj)?;
            let idx = t.next_arr.get();
            t.next_arr.set(idx + 1);
            let _ = t.set(Value::Number(idx as f64), v);
            interp.vm.stack.push(obj);
            StepOutcome::Continue
        }
        Instr::SetArrExp => {
            let n = interp.vm.last_ret;
            let sp = interp.vm.stack.len();
            // layout: [table, v1..vn] — the table sits UNDER the expansion
            let obj = interp
                .vm
                .stack
                .get(sp.saturating_sub(n + 1))
                .cloned()
                .unwrap_or(Value::Nil);
            let t = expect_table(&obj)?;
            // Drain in place: no temporary Vec for the expanded values.
            for v in interp.vm.stack.drain(sp.saturating_sub(n)..) {
                let idx = t.next_arr.get();
                t.next_arr.set(idx + 1);
                let _ = t.set(Value::Number(idx as f64), v);
            }
            // the table already occupies the right spot; nothing to push
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::Add => bin_arith("+", |a, b| a + b, interp)?,
        Instr::Sub => bin_arith("-", |a, b| a - b, interp)?,
        Instr::Mul => bin_arith("*", |a, b| a * b, interp)?,
        Instr::Div => bin_arith("/", |a, b| a / b, interp)?,
        Instr::FloorDiv => bin_arith("//", |a, b| (a / b).floor(), interp)?,
        Instr::Mod => bin_arith("%", |a, b| a - (a / b).floor() * b, interp)?,
        Instr::Pow => bin_arith("^", |a, b| a.powf(b), interp)?,
        Instr::Concat => bin_concat(interp)?,
        Instr::Eq => bin_cmp(CmpKind::Eq, interp)?,
        Instr::Ne => bin_cmp(CmpKind::Ne, interp)?,
        Instr::Lt => bin_cmp(CmpKind::Lt, interp)?,
        Instr::Le => bin_cmp(CmpKind::Le, interp)?,
        Instr::Gt => bin_cmp(CmpKind::Gt, interp)?,
        Instr::Ge => bin_cmp(CmpKind::Ge, interp)?,
        Instr::Neg => {
            let v = pop1(interp);
            let n = to_num(&v, "attempt to negate a non-number")?;
            interp.vm.stack.push(Value::Number(-n));
            StepOutcome::Continue
        }
        Instr::AddrOf => {
            let v = pop1(interp);
            let value = match v.base_value() {
                Value::Number(n) => *n,
                other => {
                    return Err(err(
                        E_RUNTIME,
                        format!(
                            "the dynamic engines can take the address of numeric values only; found {}",
                            other.type_name()
                        ),
                    ));
                }
            };
            let addr = interp.addr_next;
            interp.addr_next = interp.addr_next.saturating_add(8);
            interp.unsafe_mem.insert(addr, value);
            interp.vm.stack.push(Value::Number(addr as f64));
            StepOutcome::Continue
        }
        Instr::Not => {
            let v = pop1(interp);
            interp.vm.stack.push(Value::Bool(!v.truthy()));
            StepOutcome::Continue
        }
        Instr::Len => {
            let v = pop1(interp);
            let n = match v.base_value() {
                Value::Str(s) => s.chars().count(),
                Value::Table(t) => t.len(),
                other => {
                    return Err(err(
                        E_LEN,
                        format!(
                            "attempt to get the length of a {} value ({})",
                            other.type_name(),
                            other.fmt_value()
                        ),
                    ))
                }
            };
            interp.vm.stack.push(Value::Number(n as f64));
            StepOutcome::Continue
        }
        Instr::CloneV => {
            let v = pop1(interp);
            let c = interp.deep_clone_value(&v.base_value());
            interp.vm.stack.push(c);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::Await => {
            let value = pop1(interp);
            let result = crate::task::await_value(interp, &value, current_span(interp))?;
            interp.vm.stack.push(result);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::YieldNow => yield_values(interp, 0, 0, true)?,
        Instr::Try => {
            let value = pop1(interp);
            let Some((kind, tag, payload)) = value.base_value().sum_parts() else {
                return Err(err(
                    E_CALL,
                    format!(
                        "the `?` operator needs a Result or Option value, found {} ({})",
                        value.type_name(),
                        value.fmt_value()
                    ),
                ));
            };
            let success =
                (kind == "result" && tag == "ok") || (kind == "option" && tag == "some");
            if success {
                let cloned = interp.deep_clone_value(&payload);
                interp.vm.stack.push(cloned);
                interp.vm.last_ret = 1;
                StepOutcome::Continue
            } else {
                let propagated = interp.deep_clone_value(&value);
                let (stack_base, seg_base) = interp
                    .vm
                    .frames
                    .last()
                    .map(|f| (f.stack_base, f.seg_base))
                    .unwrap_or((0, 0));
                interp
                    .vm
                    .stack
                    .truncate(stack_base.min(interp.vm.stack.len()));
                interp.vm.segs.truncate(seg_base.min(interp.vm.segs.len()));
                interp.vm.stack.push(propagated);
                interp.vm.last_ret = 1;
                StepOutcome::Returned(1)
            }
        }
        Instr::MakeBox => {
            let v = pop1(interp);
            interp.vm.stack.push(crate::builtins::box_value(v));
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::BorrowErr => {
            return Err(err(
                E_RUNTIME,
                "borrow is not supported in bytecode mode yet; run this program with the interpreter",
            ))
        }
        Instr::BorrowImm => {
            let value = pop1(interp);
            let alias = interp.make_alias(
                value,
                "<borrow>",
                AliasKind::Imm,
                current_span(interp),
            )?;
            interp.vm.stack.push(alias);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::UnsafeEnter => {
            interp.unsafe_depth += 1;
            StepOutcome::Continue
        }
        Instr::UnsafeLeave => {
            interp.unsafe_depth = interp.unsafe_depth.saturating_sub(1);
            StepOutcome::Continue
        }
        Instr::Jump(t) => {
            interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            StepOutcome::Continue
        }
        Instr::JumpIfFalse(t) => {
            let v = pop1(interp);
            if !v.truthy() {
                interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            }
            StepOutcome::Continue
        }
        Instr::JumpIfTrueKeep(t) => {
            // Peek without cloning the heap value: only its truthiness matters.
            let cond = interp.vm.stack.last().map(|v| v.truthy()).unwrap_or(false);
            if cond {
                interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            } else {
                interp.vm.stack.pop();
            }
            StepOutcome::Continue
        }
        Instr::JumpIfFalseKeep(t) => {
            let cond = interp.vm.stack.last().map(|v| v.truthy()).unwrap_or(false);
            if !cond {
                interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            } else {
                interp.vm.stack.pop();
            }
            StepOutcome::Continue
        }
        Instr::TruncateToOne => {
            let lr = interp.vm.last_ret;
            truncate_results_to_one(interp, lr);
            StepOutcome::Continue
        }
        Instr::DropValues => {
            let lr = interp.vm.last_ret;
            let sp = interp.vm.stack.len();
            interp.vm.stack.truncate(sp.saturating_sub(lr));
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::Pop => {
            interp.vm.stack.pop();
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        // ===== deferred call-argument takes =====
        Instr::ArgLoc(slot, labelk) => {
            let src = slot_label(interp, *slot as usize);
            let dest = const_str(interp, *labelk)?;
            arg_push_local(interp, *slot as usize, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::ArgUp(idx, labelk) => {
            let handle = upval_handle(interp, *idx as usize)?;
            let src = upval_label(interp, *idx as usize);
            let dest = const_str(interp, *labelk)?;
            arg_push_slot(interp, &handle, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::ArgGlob(k, labelk) => {
            let name = const_str(interp, *k)?;
            let dest = const_str(interp, *labelk)?;
            arg_push_glob(interp, &name, &dest)?;
            StepOutcome::Continue
        }
        Instr::ArgField(k_key, k_src, k_label) => {
            let obj = pop1(interp);
            let key = const_value(interp, *k_key)?;
            let src = const_str(interp, *k_src)?;
            let dest = const_str(interp, *k_label)?;
            let t = expect_table(&obj)?;
            arg_push_entry(interp, &t, &key, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::ArgIndex(sk, lk) => {
            let key = pop1(interp);
            let obj = pop1(interp);
            let src = const_str(interp, *sk)?;
            let dest = const_str(interp, *lk)?;
            let t = expect_table(&obj)?;
            arg_push_entry(interp, &t, &key, &src, &dest)?;
            StepOutcome::Continue
        }
        Instr::Call(base, nt) => {
            call_plain(interp, *base as usize, false, *nt as usize, false, span0())?
        }
        Instr::CallExp(base, nt) => {
            call_plain(interp, *base as usize, true, *nt as usize, false, span0())?
        }
        Instr::CallOne(base, nt) => {
            call_plain(interp, *base as usize, false, *nt as usize, true, span0())?
        }
        Instr::CallExpOne(base, nt) => {
            call_plain(interp, *base as usize, true, *nt as usize, true, span0())?
        }
        Instr::Yield(base, nt) => {
            yield_values(interp, *base as usize, *nt as usize, false)?
        }
        Instr::YieldExp(base, nt) => {
            let n = *base as usize + interp.vm.last_ret;
            yield_values(interp, n, *nt as usize, false)?
        }
        Instr::YieldOne(base, nt) => {
            yield_values(interp, *base as usize, *nt as usize, true)?
        }
        Instr::YieldExpOne(base, nt) => {
            let n = *base as usize + interp.vm.last_ret;
            yield_values(interp, n, *nt as usize, true)?
        }
        Instr::MethodCall(k, base, nt) => {
            call_method(interp, *k, *base as usize, false, *nt as usize, false, span0())?
        }
        Instr::MethodCallExp(k, base, nt) => {
            call_method(interp, *k, *base as usize, true, *nt as usize, false, span0())?
        }
        Instr::MethodCallOne(k, base, nt) => {
            call_method(interp, *k, *base as usize, false, *nt as usize, true, span0())?
        }
        Instr::MethodCallExpOne(k, base, nt) => {
            call_method(interp, *k, *base as usize, true, *nt as usize, true, span0())?
        }
        Instr::RetFixed(n) => {
            let n = (*n as usize).min(interp.vm.stack.len());
            StepOutcome::Returned(n)
        }
        Instr::RetExp(base) => {
            let count = *base as usize + interp.vm.last_ret;
            let n = count.min(interp.vm.stack.len());
            StepOutcome::Returned(n)
        }
        Instr::VarargLoad => {
            // Push all varargs without cloning the frame's `Vec<Value>` first.
            let n = {
                let Vm { frames, stack, .. } = &mut interp.vm;
                let varargs = &frames.last().expect("frame").varargs;
                stack.reserve(varargs.len());
                for v in varargs {
                    stack.push(v.clone());
                }
                varargs.len()
            };
            interp.vm.last_ret = n;
            StepOutcome::Continue
        }
        Instr::VarargLoadOne => {
            let first = interp
                .vm
                .frames
                .last()
                .and_then(|f| f.varargs.first().cloned())
                .unwrap_or(Value::Nil);
            interp.vm.stack.push(first);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SegStart => {
            let sp = interp.vm.stack.len();
            interp.vm.segs.push(Seg { start: sp, cursor: sp });
            StepOutcome::Continue
        }
        Instr::SegEnd => {
            let seg = interp.vm.segs.pop().expect("SegEnd without SegStart");
            interp.vm.stack.truncate(seg.start);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::DeclFromSeg(group) => {
            // Borrow the chunk through one `Rc` clone, then bind targets one by
            // one. The old version built a `Vec<(handle, label)>` per execution
            // of every `local` statement.
            let proto = interp.vm.frames.last().expect("frame").proto.clone();
            let bindings = proto
                .proto
                .decl_groups
                .get(*group as usize)
                .expect("decl group index");
            for &(slot, namek) in bindings {
                let name = const_str_from_frame(&proto, namek);
                let v = seg_front_or_nil(interp)?;
                if let Some(vid) = v.vid() {
                    crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
                }
                let cell = interp
                    .vm
                    .frames
                    .last_mut()
                    .expect("frame")
                    .slots
                    .get_mut(slot as usize)
                    .expect("slot ok");
                cell.set(SlotV::Set(v));
            }
            StepOutcome::Continue
        }
        Instr::StoreLocSeg(slot) => {
            // Read the segment value first, then touch the slot in place. For
            // numbers (the hot loop case) there is no label clone at all.
            let v = seg_front_or_nil(interp)?;
            let frame = interp.vm.frames.last_mut().expect("frame");
            if let Some(vid) = v.vid() {
                let pc = frame.pc.saturating_sub(1);
                let label = frame.proto.proto.local_name_at(pc, *slot as usize);
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            let cell = frame.slots.get_mut(*slot as usize).ok_or_else(|| {
                err(
                    crate::error::E_RUNTIME,
                    format!("internal: slot {} out of range", slot),
                )
            })?;
            cell.set(SlotV::Set(v));
            StepOutcome::Continue
        }
        Instr::StoreUpSeg(idx) => {
            let v = seg_front_or_nil(interp)?;
            let frame = interp.vm.frames.last().expect("frame");
            let cell = frame.upvals.get(*idx as usize).ok_or_else(|| {
                err(
                    crate::error::E_RUNTIME,
                    format!("internal: upvalue {} out of range", idx),
                )
            })?;
            if let Some(vid) = v.vid() {
                let label = frame
                    .proto
                    .proto
                    .upval_labels
                    .get(*idx as usize)
                    .cloned()
                    .unwrap_or_else(|| Arc::from("(captured)"));
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            *cell.borrow_mut() = SlotV::Set(v);
            StepOutcome::Continue
        }
        Instr::StoreGlobSeg(k) => {
            let name = const_str(interp, *k)?;
            let v = seg_front_or_nil(interp)?;
            if let Some(vid) = v.vid() {
                crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
            }
            scope::set_slot(&[], &interp.globals, &name, SlotV::Set(v));
            StepOutcome::Continue
        }
        Instr::StoreFieldSeg(k) => {
            let obj = pop1(interp);
            let v = seg_front_or_nil(interp)?;
            let key = const_value(interp, *k)?;
            interp.write_table_slot(
                obj,
                key,
                v,
                "field",
                current_span(interp),
            )?;
            StepOutcome::Continue
        }
        Instr::StoreIndexFromSeg => {
            let key = pop1(interp);
            let obj = pop1(interp);
            let v = seg_front_or_nil(interp)?;
            interp.write_table_slot(
                obj,
                key,
                v,
                "index",
                current_span(interp),
            )?;
            StepOutcome::Continue
        }
        Instr::MakeClosure(k) => {
            let frame = interp.vm.frames.last_mut().expect("frame");
            let compiled = frame.proto.proto.prototypes.get(*k as usize).cloned().expect("proto index");
            let mut instance: Vec<Rc<RefCell<SlotV>>> = Vec::with_capacity(compiled.upvals.len());
            for def in &compiled.upvals {
                let handle = match def {
                    crate::compiler::UpDef::Local(slot) => frame
                        .slots
                        .get_mut(*slot as usize)
                        .expect("captured slot")
                        .make_shared(),
                    crate::compiler::UpDef::Up(idx) => {
                        frame.upvals.get(*idx as usize).cloned().expect("captured upval")
                    }
                };
                instance.push(handle);
            }
            let vid = crate::value::with_reg(|r| r.alloc_vid());
            let instance: Rc<[_]> = instance.into();
            let f = Value::Func(Rc::new(crate::value::FuncObj {
                vid,
                name: compiled.proto.name.clone(),
                env: Vec::new(),
                body: crate::value::FuncBody::Vm(compiled, instance),
            }));
            interp.vm.stack.push(f);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::ForNumInit { var, limit, step, end } => {
            let s = pop1(interp);
            let l = pop1(interp);
            let start = pop1(interp);
            let sv = to_num(&s, "for loop step must be a number")?;
            let lv = to_num(&l, "for loop limit must be a number")?;
            let iv = to_num(&start, "for loop start must be a number")?;
            let frame = interp.vm.frames.last_mut().expect("frame");
            // Each execution of the loop opens a fresh binding. If a closure
            // captured this register on a previous execution, keep the old
            // cell for that closure and give the loop a direct one.
            fresh_if_shared(&mut frame.slots[*var as usize]);
            frame.slots[*var as usize].set(SlotV::Set(Value::Number(iv)));
            frame.slots[*limit as usize].set(SlotV::Set(Value::Number(lv)));
            frame.slots[*step as usize].set(SlotV::Set(Value::Number(sv)));
            let in_range = if sv >= 0.0 { iv <= lv } else { iv >= lv };
            if !in_range {
                frame.pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForNumStep { var, limit, step, top, end } => {
            // One mutable frame borrow for all three reads, the write and the
            // pc update. Control slots are normally direct, so the fast helper
            // avoids three independent cell dispatches.
            let frame = interp.vm.frames.last_mut().expect("frame");
            let (cur, lim, st) = frame_nums3(
                frame,
                *var as usize,
                *limit as usize,
                *step as usize,
            )?;
            let next = cur + st;
            let go = if st >= 0.0 { next <= lim } else { next >= lim };
            if go {
                // A closure in the body captured the previous iteration's
                // binding. Detach before writing the next value so each
                // iteration gets its own cell.
                let cell = &mut frame.slots[*var as usize];
                fresh_if_shared(cell);
                cell.set(SlotV::Set(Value::Number(next)));
                frame.pc = *top as usize;
            } else {
                frame.pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForInSetup { iter, first, nvars, end } => {
            let it = seg_front_or_nil(interp)?;
            let cell = interp
                .vm
                .frames
                .last_mut()
                .expect("frame")
                .slots
                .get_mut(*iter as usize)
                .ok_or_else(|| err(E_RUNTIME, "internal: iterator slot out of range"))?;
            cell.set(SlotV::Set(it.clone()));
            let row = advance_iter(interp, &it, *nvars as usize)?;
            store_row(interp, *first as usize, *nvars as usize, &row)?;
            if row.is_empty() {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForInStep { iter, first, nvars, top, end } => {
            let it = interp
                .vm
                .frames
                .last()
                .expect("frame")
                .slots
                .get(*iter as usize)
                .and_then(|cell| cell.read_clone())
                .ok_or_else(|| {
                    err_at(
                        current_span(interp),
                        E_MOVED,
                        "use of moved-out value `(iter)`",
                    )
                })?;
            let row = advance_iter(interp, &it, *nvars as usize)?;
            store_row(interp, *first as usize, *nvars as usize, &row)?;
            if row.is_empty() {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            } else {
                interp.vm.frames.last_mut().unwrap().pc = *top as usize;
            }
            StepOutcome::Continue
        }
    })
}

fn pop1(interp: &mut Interp) -> Value {
    interp.vm.stack.pop().unwrap_or(Value::Nil)
}

/// Classified callee for the allocation-free VM call path. `Bad` keeps the
/// old behavior of finalizing the argument journal before reporting the error.
enum Callee {
    Vm(Rc<Compiled>, Rc<[Rc<RefCell<SlotV>>]>),
    Builtin,
    Lusa,
    Bad,
}

impl Callee {
    fn is_builtin(&self) -> bool {
        matches!(self, Callee::Builtin)
    }
}

#[inline]
fn classify(v: &Value) -> Callee {
    match v {
        Value::Func(f) => match &f.body {
            crate::value::FuncBody::Builtin(_) => Callee::Builtin,
            crate::value::FuncBody::Lusa(_) => Callee::Lusa,
            crate::value::FuncBody::Vm(c, u) => Callee::Vm(c.clone(), u.clone()),
        },
        _ => Callee::Bad,
    }
}

/// Compiles an interpreter function for execution inside a coroutine. The
/// snapshot makes captured locals readable; direct `coroutine.yield` sites in
/// the compiled body become suspension points.
fn compile_lusa_runtime(
    interp: &mut Interp,
    func: &Value,
) -> Option<crate::compiler::RuntimeFunction> {
    let Value::Func(f) = func.base_value() else {
        return None;
    };
    let crate::value::FuncBody::Lusa(lit) = &f.body else {
        return None;
    };
    let structs: std::collections::HashMap<String, Vec<(String, String)>> = interp
        .struct_defs
        .iter()
        .map(|(name, def)| (name.clone(), def.fields.clone()))
        .collect();
    let seed = interp.coroutine_captures.last().cloned();
    let runtime =
        crate::compiler::compile_runtime_function(lit, &f.env, &structs, seed.as_ref()).ok()?;
    if let Some(map) = interp.coroutine_captures.last_mut() {
        for (index, scope, name) in &runtime.captures {
            if let Some(cell) = runtime.upvals.get(*index) {
                map.insert(name.to_string(), (scope.clone(), cell.clone()));
            }
        }
    }
    Some(runtime)
}

/// Plain call. Arguments stay on the shared stack until the frame is built;
/// VM→VM calls never allocate a `Vec<Value>` for them.
fn call_plain(
    interp: &mut Interp,
    base: usize,
    exp: bool,
    n_takes: usize,
    want_one: bool,
    span: Span,
) -> LResult<StepOutcome> {
    let nargs = if exp { base + interp.vm.last_ret } else { base };
    let sp = interp.vm.stack.len();
    let func_pos = sp.saturating_sub(nargs + 1);
    let callee = match interp.vm.stack.get(func_pos) {
        Some(v) => classify(v),
        None => Callee::Bad,
    };
    if n_takes != 0 {
        finalize_args(interp, callee.is_builtin(), n_takes)?;
    }
    // Async functions never run inline: the call returns a lazy task handle.
    let callee_value = interp.vm.stack.get(func_pos).cloned().unwrap_or(Value::Nil);
    if crate::task::is_async_value(&callee_value) {
        let start = (func_pos + 1).min(interp.vm.stack.len());
        let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
        interp.vm.stack.truncate(func_pos.min(interp.vm.stack.len()));
        let task = crate::task::spawn_value(interp, &callee_value, args, span)?;
        interp.vm.stack.push(Value::Task(task));
        interp.vm.last_ret = 1;
        return Ok(StepOutcome::Continue);
    }
    match callee {
        Callee::Vm(compiled, upvals) => {
            // Push the callee frame and let the interpreter loop step it. The
            // caller's `Call` already left the arguments on the shared stack;
            // no Rust recursion (and no second giant prologue) per VM call.
            push_frame_at(
                interp,
                compiled,
                upvals,
                func_pos + 1,
                nargs,
                None,
                func_pos,
                span,
            )?;
            if want_one {
                interp
                    .vm
                    .frames
                    .last_mut()
                    .expect("callee frame")
                    .truncate_return = true;
            }
            Ok(StepOutcome::Continue)
        }
        Callee::Lusa if !interp.active_coroutines.is_empty() => {
            let func = interp.vm.stack.get(func_pos).cloned().unwrap_or(Value::Nil);
            let start = (func_pos + 1).min(interp.vm.stack.len());
            let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
            interp.vm.stack.truncate(func_pos.min(interp.vm.stack.len()));
            interp.vm.last_ret = 1;
            if let Some(runtime) = compile_lusa_runtime(interp, &func) {
                push_frame(interp, &runtime.compiled, runtime.upvals, args, span)?;
                if want_one {
                    interp
                        .vm
                        .frames
                        .last_mut()
                        .expect("callee frame")
                        .truncate_return = true;
                }
                return Ok(StepOutcome::Continue);
            }
            invoke_non_vm(interp, func, args, span)?;
            if want_one {
                let n = interp.vm.last_ret;
                truncate_results_to_one(interp, n);
            }
            Ok(StepOutcome::Continue)
        }
        _ => {
            let func = interp.vm.stack.get(func_pos).cloned().unwrap_or(Value::Nil);
            let start = (func_pos + 1).min(interp.vm.stack.len());
            let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
            interp.vm.stack.truncate(func_pos.min(interp.vm.stack.len()));
            interp.vm.last_ret = 1;
            invoke_non_vm(interp, func, args, span)?;
            if want_one {
                let n = interp.vm.last_ret;
                truncate_results_to_one(interp, n);
            }
            Ok(StepOutcome::Continue)
        }
    }
}

/// Method call (`obj:name(...)`). The receiver is read straight from the
/// stack as parameter 0; the non-VM path drains `[receiver, args...]` in one
/// operation.
fn call_method(
    interp: &mut Interp,
    key_k: u16,
    base: usize,
    exp: bool,
    n_takes: usize,
    want_one: bool,
    span: Span,
) -> LResult<StepOutcome> {
    let nargs = if exp { base + interp.vm.last_ret } else { base };
    let sp = interp.vm.stack.len();
    let obj_pos = sp.saturating_sub(nargs + 1);
    let key = const_value(interp, key_k)?;
    let obj_value = interp
        .vm
        .stack
        .get(obj_pos)
        .cloned()
        .unwrap_or(Value::Nil);
    // Dynamic `Vec<T>` and `Result`/`Option` fallbacks: a table with no real
    // field of that name behaves like the corresponding built-in surface.
    if let (Value::Table(table), Value::Str(name)) = (obj_value.base_value(), &key) {
        let is_sum = Interp::is_sum_method(name)
            && Value::Table(Rc::clone(table)).sum_parts().is_some();
        if table.get(&key).is_none() && (is_sum || Interp::is_vec_method(name)) {
            if n_takes != 0 {
                finalize_args(interp, false, n_takes)?;
            }
            let start = obj_pos.min(interp.vm.stack.len());
            let mut values: Vec<Value> = interp.vm.stack.drain(start..).collect();
            if !values.is_empty() {
                values.remove(0); // receiver
            }
            let results = if is_sum {
                interp.sum_method_call(&Rc::clone(table), name, values, span)?
            } else {
                interp.vec_method_call(&Rc::clone(table), name, values, span)?
            };
            let n = results.len();
            interp.vm.stack.reserve(n);
            for value in results {
                interp.vm.stack.push(value);
            }
            interp.vm.last_ret = n;
            if want_one && n > 1 {
                truncate_results_to_one(interp, n);
            }
            return Ok(StepOutcome::Continue);
        }
    }
    // Strings expose the `string` library as pseudo-methods, matching the
    // tree-walking interpreter (`s:match(pat)` == `string.match(s, pat)`).
    let f = match obj_value.base_value() {
        Value::Str(_) => {
            let module = read_global_deferred_span(interp, "string")?;
            let table = match module.base_value() {
                Value::Table(table) => table,
                _ => {
                    return Err(LusaError::new(
                        E_CALL,
                        Some(span),
                        "`string` is not a table",
                    ))
                }
            };
            table.get(&key).ok_or_else(|| {
                LusaError::new(
                    E_CALL,
                    Some(span),
                    format!("string has no method `:{}`", key.fmt_value()),
                )
            })?
        }
        _ => crate::eval::read_field(&obj_value, &key, span)?,
    };
    let callee = classify(&f);
    if n_takes != 0 {
        finalize_args(interp, callee.is_builtin(), n_takes)?;
    }
    if crate::task::is_async_value(&f) {
        let start = obj_pos.min(interp.vm.stack.len());
        let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
        interp.vm.stack.truncate(obj_pos.min(interp.vm.stack.len()));
        let task = crate::task::spawn_value(interp, &f, args, span)?;
        interp.vm.stack.push(Value::Task(task));
        interp.vm.last_ret = 1;
        return Ok(StepOutcome::Continue);
    }
    match callee {
        Callee::Vm(compiled, upvals) => {
            push_frame_at(
                interp,
                compiled,
                upvals,
                obj_pos + 1,
                nargs,
                Some(obj_pos),
                obj_pos,
                span,
            )?;
            if want_one {
                interp
                    .vm
                    .frames
                    .last_mut()
                    .expect("callee frame")
                    .truncate_return = true;
            }
            Ok(StepOutcome::Continue)
        }
        Callee::Lusa if !interp.active_coroutines.is_empty() => {
            let start = obj_pos.min(interp.vm.stack.len());
            let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
            interp.vm.last_ret = 1;
            if let Some(runtime) = compile_lusa_runtime(interp, &f) {
                push_frame(interp, &runtime.compiled, runtime.upvals, args, span)?;
                if want_one {
                    interp
                        .vm
                        .frames
                        .last_mut()
                        .expect("callee frame")
                        .truncate_return = true;
                }
                return Ok(StepOutcome::Continue);
            }
            invoke_non_vm(interp, f, args, span)?;
            if want_one {
                let n = interp.vm.last_ret;
                truncate_results_to_one(interp, n);
            }
            Ok(StepOutcome::Continue)
        }
        _ => {
            let start = obj_pos.min(interp.vm.stack.len());
            let args: Vec<Value> = interp.vm.stack.drain(start..).collect();
            interp.vm.last_ret = 1;
            invoke_non_vm(interp, f, args, span)?;
            if want_one {
                let n = interp.vm.last_ret;
                truncate_results_to_one(interp, n);
            }
            Ok(StepOutcome::Continue)
        }
    }
}

/// Calls builtin/interpreted functions and pushes their results by value
/// (no defensive clone of every result element).
fn invoke_non_vm(
    interp: &mut Interp,
    func: Value,
    args: Vec<Value>,
    span: Span,
) -> LResult<StepOutcome> {
    let results = match &func {
        Value::Func(f) => match &f.body {
            crate::value::FuncBody::Builtin(b) => b(interp, args, span)?,
            crate::value::FuncBody::Lusa(_) => interp.call_value(&func, args, span)?,
            crate::value::FuncBody::Vm(compiled, upvals) => {
                crate::vm::invoke(interp, compiled, upvals.clone(), args, span)?
            }
        },
        Value::Nil => {
            return Err(err(
                E_CALL,
                format!("attempt to call a nil value (global `{}`?)", func.fmt_value()),
            ))
        }
        other => {
            return Err(err(
                E_CALL,
                format!("attempt to call a {} value ({})", other.type_name(), other.fmt_value()),
            ))
        }
    };
    let n = results.len();
    interp.vm.stack.reserve(n);
    for v in results {
        interp.vm.stack.push(v);
    }
    interp.vm.last_ret = n;
    Ok(StepOutcome::Continue)
}

// ===== moving reads =====

/// Mirrors the interpreter's `move_err` presentation.
fn moved_out_err(label: &str, span: Span) -> LusaError {
    LusaError::new(
        crate::error::E_MOVED,
        Some(span),
        format!("use of moved-out value `{}`", label),
    )
    .help(format!(
        "`{}` no longer holds the value; use `clone {}` (copy) or `borrow {}` (keep the original) when you still need it",
        label, label, label
    ))
}

/// Moving read of a slot (local or captured): heap values leave `SlotV::Moved`
/// behind; scalars are copied and the slot stays intact.
fn take_slot_storage(
    interp: &mut Interp,
    handle: &Rc<RefCell<SlotV>>,
    src_label: &Arc<str>,
    dest: &str,
) -> LResult<()> {
    // Scalars are copied and the source slot stays intact; no ownership
    // journal, label or heap check is needed for them.
    let scalar = {
        let b = handle.borrow();
        match &*b {
            SlotV::Set(Value::Number(n)) => Some(Value::Number(*n)),
            SlotV::Set(Value::Bool(v)) => Some(Value::Bool(*v)),
            SlotV::Set(Value::Nil) => Some(Value::Nil),
            _ => None,
        }
    };
    if let Some(v) = scalar {
        interp.vm.stack.push(v);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    let current = match &*handle.borrow() {
        SlotV::Set(v) => Some(v.clone()),
        SlotV::Moved(_) => None,
    };
    let v = match current {
        Some(v) => v,
        None => return Err(moved_out_err(&src_label.to_string(), current_span(interp))),
    };
    if let Some(copy) = interp.copy_struct_value(&v) {
        interp.vm.stack.push(copy);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    if matches!(v, Value::Table(_) | Value::Func(_)) {
        let vid = v.vid().expect("heap value has vid");
        if !interp.borrowed_by(vid).is_empty() {
            return Err(err_at(
                current_span(interp),
                crate::error::E_BORROW_CONFLICT,
                format!("cannot move out of `{}` because it is borrowed", src_label),
            ));
        }
        let note = crate::value::MoveNote { to: dest.to_string(), span: span0() };
        *handle.borrow_mut() = SlotV::Moved(vec![note]);
        crate::value::with_reg(|r| r.rename(vid, dest.to_string(), span0()));
    }
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

/// Local-slot variant of `take_slot_storage`. The scalar path avoids both the
/// `SlotCell` promotion check and the `Rc`/`RefCell` machinery entirely.
fn take_local_storage(
    interp: &mut Interp,
    slot: usize,
    src_label: &Arc<str>,
    dest: &str,
) -> LResult<()> {
    let scalar = {
        let frame = interp.vm.frames.last().expect("frame");
        let cell = frame.slots.get(slot).ok_or_else(|| {
            err(
                crate::error::E_RUNTIME,
                format!("internal: slot {} out of range", slot),
            )
        })?;
        cell.scalar()
    };
    if let Some(v) = scalar {
        interp.vm.stack.push(v);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    let v = {
        let frame = interp.vm.frames.last().expect("frame");
        let cell = frame.slots.get(slot).ok_or_else(|| {
            err(
                crate::error::E_RUNTIME,
                format!("internal: slot {} out of range", slot),
            )
        })?;
        cell.read_clone()
            .ok_or_else(|| moved_out_err(&src_label.to_string(), frame_span(frame)))?
    };
    if let Some(copy) = interp.copy_struct_value(&v) {
        interp.vm.stack.push(copy);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    if matches!(v, Value::Table(_) | Value::Func(_)) {
        let vid = v.vid().expect("heap value has vid");
        if !interp.borrowed_by(vid).is_empty() {
            return Err(err_at(
                current_span(interp),
                crate::error::E_BORROW_CONFLICT,
                format!("cannot move out of `{}` because it is borrowed", src_label),
            ));
        }
        let note = crate::value::MoveNote { to: dest.to_string(), span: span0() };
        if let Some(cell) = interp
            .vm
            .frames
            .last_mut()
            .and_then(|f| f.slots.get_mut(slot))
        {
            cell.set(SlotV::Moved(vec![note]));
        }
        crate::value::with_reg(|r| r.rename(vid, dest.to_string(), span0()));
    }
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

/// Moving read of a table field (parity with `take_field_from_table`):
/// missing keys yield nil and move nothing.
fn take_table_storage(
    interp: &mut Interp,
    t: &Rc<TableObj>,
    key: &Value,
    src: &str,
    dest: &str,
) -> LResult<()> {
    let v = match t.get_slot(key) {
        Some(SlotV::Set(v)) => v,
        Some(SlotV::Moved(_)) => return Err(moved_out_err(src, current_span(interp))),
        None => {
            interp.vm.stack.push(Value::Nil);
            interp.vm.last_ret = 1;
            return Ok(());
        }
    };
    if let Some(copy) = interp.copy_struct_value(&v) {
        interp.vm.stack.push(copy);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    if matches!(v, Value::Table(_) | Value::Func(_)) {
        let vid = v.vid().expect("heap value has vid");
        if !interp.borrowed_by(vid).is_empty() {
            return Err(err(
                crate::error::E_BORROW_CONFLICT,
                format!("cannot move out of `{}` because it is borrowed", src),
            ));
        }
        let note = crate::value::MoveNote { to: dest.to_string(), span: span0() };
        t.move_out(key, note);
        crate::value::with_reg(|r| r.rename(vid, dest.to_string(), span0()));
    }
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

// ===== call-argument move journal =====

/// One deferred call-argument take. Builtins restore these at the call
/// boundary (parity: builtins never take ownership — `eval_args(no_moves)`);
/// user functions commit the move at the call boundary instead of eagerly.
pub(crate) enum UndoRec {
    /// scalars, aliases, missing/absent sources — nothing to restore or move
    Done,
    /// direct or closure-captured local, addressed by its frame slot index
    Local {
        slot: usize,
        vid: Option<u64>,
        dest: String,
        value: Value,
    },
    /// captured upvalue; the handle keeps identity even across promotions
    Slot {
        handle: Rc<RefCell<SlotV>>,
        vid: Option<u64>,
        dest: String,
        value: Value,
    },
    Glob {
        name: String,
        vid: Option<u64>,
        dest: String,
        value: Value,
    },
    Field {
        table: Rc<TableObj>,
        key: Value,
        vid: Option<u64>,
        dest: String,
        value: Value,
    },
}

fn same_heap(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Table(x), Value::Table(y)) => Rc::ptr_eq(x, y),
        (Value::Func(x), Value::Func(y)) => Rc::ptr_eq(x, y),
        (Value::Iter(x), Value::Iter(y)) => Rc::ptr_eq(x, y),
        (Value::Coroutine(x), Value::Coroutine(y)) => Rc::ptr_eq(x, y),
        _ => false,
    }
}

/// Pushes a local slot value and journals the source by slot index, so a
/// closure created while later arguments are evaluated can promote the slot
/// without invalidating the pending move.
fn arg_push_local(
    interp: &mut Interp,
    slot: usize,
    src: &Arc<str>,
    dest: &str,
) -> LResult<()> {
    let mut v = {
        let frame = interp.vm.frames.last().expect("frame");
        let cell = frame.slots.get(slot).ok_or_else(|| {
            err(
                crate::error::E_RUNTIME,
                format!("internal: slot {} out of range", slot),
            )
        })?;
        cell.read_clone()
            .ok_or_else(|| moved_out_err(&src.to_string(), frame_span(frame)))?
    };
    let rec = if let Some(copy) = interp.copy_struct_value(&v) {
        v = copy;
        UndoRec::Done
    } else if matches!(v, Value::Table(_) | Value::Func(_) | Value::Iter(_) | Value::Coroutine(_)) {
        UndoRec::Local {
            slot,
            vid: v.vid(),
            dest: dest.to_string(),
            value: v.clone(),
        }
    } else {
        UndoRec::Done
    };
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    interp.vm.frames.last_mut().expect("frame").undo.push(rec);
    Ok(())
}

/// Pushes the value onto the stack and records its source for finalization.
/// The slot is NOT modified here — the move happens at the call boundary.
fn arg_push_slot(
    interp: &mut Interp,
    handle: &Rc<RefCell<SlotV>>,
    src: &Arc<str>,
    dest: &str,
) -> LResult<()> {
    let mut v = match &*handle.borrow() {
        SlotV::Set(v) => v.clone(),
        SlotV::Moved(_) => return Err(moved_out_err(&src.to_string(), current_span(interp))),
    };
    let rec = if let Some(copy) = interp.copy_struct_value(&v) {
        v = copy;
        UndoRec::Done
    } else if matches!(v, Value::Table(_) | Value::Func(_) | Value::Iter(_) | Value::Coroutine(_)) {
        UndoRec::Slot {
            handle: handle.clone(),
            vid: v.vid(),
            dest: dest.to_string(),
            value: v.clone(),
        }
    } else {
        UndoRec::Done
    };
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    interp.vm.frames.last_mut().expect("frame").undo.push(rec);
    Ok(())
}

/// Same as `arg_push_slot` but for a global (restore goes by name).
fn arg_push_glob(interp: &mut Interp, name: &str, dest: &str) -> LResult<()> {
    let mut v = read_global_deferred_span(interp, name)?;
    let rec = if let Some(copy) = interp.copy_struct_value(&v) {
        v = copy;
        UndoRec::Done
    } else if matches!(v, Value::Table(_) | Value::Func(_) | Value::Iter(_) | Value::Coroutine(_)) {
        UndoRec::Glob {
            name: name.to_string(),
            vid: v.vid(),
            dest: dest.to_string(),
            value: v.clone(),
        }
    } else {
        UndoRec::Done
    };
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    interp.vm.frames.last_mut().expect("frame").undo.push(rec);
    Ok(())
}

/// Same for a table entry (field or indexed access). Missing keys yield nil
/// and move nothing (parity with `take_field_from_table`).
fn arg_push_entry(
    interp: &mut Interp,
    t: &Rc<TableObj>,
    key: &Value,
    src: &str,
    dest: &str,
) -> LResult<()> {
    let mut v = match t.get_slot(key) {
        Some(SlotV::Set(v)) => Some(v),
        Some(SlotV::Moved(_)) => return Err(moved_out_err(src, current_span(interp))),
        None => None,
    };
    let rec = if let Some(copy) = v.as_ref().and_then(|value| interp.copy_struct_value(value)) {
        v = Some(copy);
        UndoRec::Done
    } else if let Some(vv) = &v {
        if matches!(vv, Value::Table(_) | Value::Func(_) | Value::Iter(_) | Value::Coroutine(_)) {
            UndoRec::Field {
                table: t.clone(),
                key: key.clone(),
                vid: vv.vid(),
                dest: dest.to_string(),
                value: vv.clone(),
            }
        } else {
            UndoRec::Done
        }
    } else {
        UndoRec::Done
    };
    interp.vm.stack.push(v.unwrap_or(Value::Nil));
    interp.vm.last_ret = 1;
    interp.vm.frames.last_mut().expect("frame").undo.push(rec);
    Ok(())
}

/// Finalizes the journaled argument takes for a call: builtins rollback
/// (their arguments are non-owning plain reads in the interpreter), user
/// functions commit the moves at the boundary.
fn finalize_args(interp: &mut Interp, is_builtin: bool, n_takes: usize) -> LResult<()> {
    if n_takes == 0 {
        return Ok(());
    }
    // Move the journal buffer out (O(1), no allocation), process the pending
    // suffix, then put the empty buffer back for reuse by the next call.
    let mut undo = {
        let frame = interp.vm.frames.last_mut().expect("frame");
        std::mem::take(&mut frame.undo)
    };
    let start = undo.len().saturating_sub(n_takes);
    if is_builtin {
        for rec in undo.drain(start..).rev() {
            restore_arg(interp, rec)?;
        }
    } else {
        for rec in undo.drain(start..) {
            commit_arg(interp, rec)?;
        }
    }
    interp.vm.frames.last_mut().expect("frame").undo = undo;
    Ok(())
}

/// Applies a deferred move into the recorded source slot. If the slot was
/// reassigned or moved since (only possible through captured upvals), the
/// newer state wins and the pending move degrades to a rename.
fn commit_arg(interp: &mut Interp, rec: UndoRec) -> LResult<()> {
    match rec {
        UndoRec::Done => {}
        UndoRec::Local { slot, vid, dest, value } => {
            let mark = interp
                .vm
                .frames
                .last()
                .and_then(|f| f.slots.get(slot))
                .map(|cell| {
                    cell.inspect(|s| matches!(s, SlotV::Set(cur) if same_heap(cur, &value)))
                })
                .unwrap_or(false);
            if mark {
                let note = crate::value::MoveNote { to: dest.clone(), span: span0() };
                if let Some(cell) = interp
                    .vm
                    .frames
                    .last_mut()
                    .and_then(|f| f.slots.get_mut(slot))
                {
                    cell.set(SlotV::Moved(vec![note]));
                }
            }
            if let Some(vid) = vid {
                crate::value::with_reg(|r| r.rename(vid, dest.clone(), span0()));
            }
        }
        UndoRec::Slot { handle, vid, dest, value, .. } => {
            let mark = {
                let b = handle.borrow();
                matches!(&*b, SlotV::Set(cur) if same_heap(cur, &value))
            };
            if mark {
                let note = crate::value::MoveNote { to: dest.clone(), span: span0() };
                *handle.borrow_mut() = SlotV::Moved(vec![note]);
            }
            if let Some(vid) = vid {
                crate::value::with_reg(|r| r.rename(vid, dest.clone(), span0()));
            }
        }
        UndoRec::Glob { name, vid, dest, value } => {
            let cur = read_global_deferred_span(interp, &name)?;
            if same_heap(&cur, &value) {
                let note = crate::value::MoveNote { to: dest.clone(), span: span0() };
                scope::set_slot(&[], &interp.globals, &name, SlotV::Moved(vec![note]));
            }
            if let Some(vid) = vid {
                crate::value::with_reg(|r| r.rename(vid, dest.clone(), span0()));
            }
        }
        UndoRec::Field { table, key, vid, dest, value, .. } => {
            let mark = match table.get_slot(&key) {
                Some(SlotV::Set(cur)) => same_heap(&cur, &value),
                _ => false,
            };
            if mark {
                let note = crate::value::MoveNote { to: dest.clone(), span: span0() };
                table.move_out(&key, note);
            }
            if let Some(vid) = vid {
                crate::value::with_reg(|r| r.rename(vid, dest.clone(), span0()));
            }
        }
    }
    Ok(())
}

/// Rolls a journaled source back for builtin calls: the slot keeps ownership
/// exactly like the interpreter's plain (non-moving) builtin arguments.
fn restore_arg(interp: &mut Interp, rec: UndoRec) -> LResult<()> {
    match rec {
        UndoRec::Done => {}
        UndoRec::Local { slot, value, .. } => {
            if let Some(cell) = interp
                .vm
                .frames
                .last_mut()
                .and_then(|f| f.slots.get_mut(slot))
            {
                if cell.inspect(|s| matches!(s, SlotV::Moved(_))) {
                    cell.set(SlotV::Set(value));
                }
            }
        }
        UndoRec::Slot { handle, value, .. } => match &*handle.borrow() {
            // the deferred take never moved the slot; only a nested take or
            // reassignment through upvals could have altered it since
            SlotV::Set(cur) => {
                if same_heap(cur, &value) {
                    // still ours — nothing to do
                }
            }
            SlotV::Moved(_) => {
                *handle.borrow_mut() = SlotV::Set(value);
            }
        },
        UndoRec::Glob { name, value, .. } => {
            let cur = read_global_deferred_span(interp, &name)?;
            let keep = match &cur {
                Value::Table(t) => matches!(&value, Value::Table(v) if Rc::ptr_eq(t, v)),
                _ => false,
            };
            if keep {
                scope::set_slot(&[], &interp.globals, &name, SlotV::Set(value));
            }
        }
        UndoRec::Field { table, key, value, .. } => match table.get_slot(&key) {
            Some(SlotV::Set(cur)) => {
                if same_heap(&cur, &value) {
                    // still ours and untouched — nothing to restore
                }
            }
            _ => {
                table.set_slot(&key, SlotV::Set(value));
            }
        },
    }
    Ok(())
}

// ===== value helpers =====

fn expect_table(v: &Value) -> LResult<Rc<TableObj>> {
    match v.base_value() {
        Value::Table(t) => Ok(t.clone()),
        other => Err(err(
            crate::error::E_INDEX,
            format!(
                "attempt to index a {} value ({})",
                other.type_name(),
                other.fmt_value()
            ),
        )),
    }
}

fn to_num(v: &Value, msg: &str) -> LResult<f64> {
    match v.base_value() {
        Value::Number(n) => Ok(*n),
        other => Err(err(
            crate::error::E_ARITH,
            format!("{} (got a {} value)", msg, other.type_name()),
        )),
    }
}

// ===== arithmetic / comparisons =====

fn num_binop(op: &str, l: &Value, r: &Value, f: fn(f64, f64) -> f64) -> LResult<Value> {
    let ln = match l.base_value() {
        Value::Number(a) => Some(*a),
        _ => None,
    };
    let rn = match r.base_value() {
        Value::Number(b) => Some(*b),
        _ => None,
    };
    match (ln, rn) {
        (Some(a), Some(b)) => Ok(Value::Number(f(a, b))),
        _ => {
            let bad = if ln.is_none() { l } else { r };
            Err(err(
                crate::error::E_ARITH,
                format!(
                    "attempt to perform arithmetic `{}` on a {} value ({}) — numbers only",
                    op,
                    bad.type_name(),
                    bad.fmt_value()
                ),
            ))
        }
    }
}

#[inline(always)]
fn bin_arith(op: &str, f: fn(f64, f64) -> f64, interp: &mut Interp) -> LResult<StepOutcome> {
    let r = pop1(interp);
    let l = pop1(interp);
    // Numeric fast path: no `Value::base_value` / type-name / formatting
    // machinery on the common case. Aliases deliberately take the slow path.
    if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
        interp.vm.stack.push(Value::Number(f(*a, *b)));
        interp.vm.last_ret = 1;
        return Ok(StepOutcome::Continue);
    }
    bin_arith_slow(op, f, l, r, interp)
}

#[inline(never)]
fn bin_arith_slow(
    op: &str,
    f: fn(f64, f64) -> f64,
    l: Value,
    r: Value,
    interp: &mut Interp,
) -> LResult<StepOutcome> {
    let v = num_binop(op, &l, &r, f)?;
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(StepOutcome::Continue)
}

fn bin_concat(interp: &mut Interp) -> LResult<StepOutcome> {
    let r = pop1(interp);
    let l = pop1(interp);
    let ls = match l.base_value() {
        Value::Str(s) => s.to_string(),
        Value::Number(n) => format_number(*n),
        other => {
            return Err(err(
                crate::error::E_CONCAT,
                format!(
                    "attempt to concatenate a {} value ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
    };
    let rs = match r.base_value() {
        Value::Str(s) => s.to_string(),
        Value::Number(n) => format_number(*n),
        other => {
            return Err(err(
                crate::error::E_CONCAT,
                format!(
                    "attempt to concatenate a {} value ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
    };
    interp
        .vm
        .stack
        .push(Value::Str(Arc::from(format!("{}{}", ls, rs).as_str())));
    interp.vm.last_ret = 1;
    Ok(StepOutcome::Continue)
}

pub enum CmpKind {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

fn values_eq(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Table(a), Value::Table(b)) => Rc::ptr_eq(a, b),
        (Value::Func(a), Value::Func(b)) => Rc::ptr_eq(a, b),
        (Value::Iter(a), Value::Iter(b)) => Rc::ptr_eq(a, b),
        (Value::Coroutine(a), Value::Coroutine(b)) => Rc::ptr_eq(a, b),
        _ => false,
    }
}

#[inline(always)]
fn bin_cmp(kind: CmpKind, interp: &mut Interp) -> LResult<StepOutcome> {
    let r = pop1(interp);
    let l = pop1(interp);
    if let (Value::Number(a), Value::Number(b)) = (&l, &r) {
        let result = match kind {
            CmpKind::Eq => a == b,
            CmpKind::Ne => a != b,
            CmpKind::Lt => a < b,
            CmpKind::Le => a <= b,
            CmpKind::Gt => a > b,
            CmpKind::Ge => a >= b,
        };
        interp.vm.stack.push(Value::Bool(result));
        interp.vm.last_ret = 1;
        return Ok(StepOutcome::Continue);
    }
    bin_cmp_slow(kind, l, r, interp)
}

#[inline(never)]
fn bin_cmp_slow(
    kind: CmpKind,
    l: Value,
    r: Value,
    interp: &mut Interp,
) -> LResult<StepOutcome> {
    let lb = l.base_value();
    let rb = r.base_value();
    let result = match kind {
        CmpKind::Eq => values_eq(lb, rb),
        CmpKind::Ne => !values_eq(lb, rb),
        CmpKind::Lt | CmpKind::Le | CmpKind::Gt | CmpKind::Ge => match (lb, rb) {
            (Value::Number(a), Value::Number(b)) => match kind {
                CmpKind::Lt => a < b,
                CmpKind::Le => a <= b,
                CmpKind::Gt => a > b,
                CmpKind::Ge => a >= b,
                _ => unreachable!(),
            },
            (Value::Str(a), Value::Str(b)) => {
                let (x, y) = (a.to_string(), b.to_string());
                match kind {
                    CmpKind::Lt => x < y,
                    CmpKind::Le => x <= y,
                    CmpKind::Gt => x > y,
                    CmpKind::Ge => x >= y,
                    _ => unreachable!(),
                }
            }
            _ => {
                return Err(err(
                    crate::error::E_CMP,
                    format!(
                        "attempt to compare {} with {} ({} with {})",
                        l.type_name(),
                        r.type_name(),
                        l.fmt_value(),
                        r.fmt_value()
                    ),
                ))
            }
        },
    };
    interp.vm.stack.push(Value::Bool(result));
    interp.vm.last_ret = 1;
    Ok(StepOutcome::Continue)
}

// ===== constants =====

fn const_value(interp: &Interp, k: u16) -> LResult<Value> {
    let frame = interp.vm.frames.last().expect("frame");
    frame
        .proto
        .proto
        .consts
        .get(k as usize)
        .cloned()
        .ok_or_else(|| err(crate::error::E_RUNTIME, format!("internal: constant {} out of range", k)))
}

/// Fast path for a global read: one hash lookup and one `Value` clone, no
/// `Rc<ScopeRef>` clone and no second/third lookup. Missing or moved slots fall
/// back to `scope::read_slot` so error history and nil semantics stay identical.
#[inline]
fn read_global_fast(interp: &Interp, name: &str) -> Option<Value> {
    let b = interp.globals.borrow();
    match b.vars.get(name) {
        Some(SlotV::Set(Value::Number(n))) => Some(Value::Number(*n)),
        Some(SlotV::Set(Value::Bool(v))) => Some(Value::Bool(*v)),
        Some(SlotV::Set(Value::Nil)) => Some(Value::Nil),
        Some(SlotV::Set(v)) => Some(v.clone()),
        _ => None,
    }
}

fn push_const(interp: &mut Interp, k: u16) -> LResult<()> {
    let v = const_value(interp, k)?;
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

/// String constant as an interned `Arc<str>`: a refcount bump, not a copy.
fn const_str(interp: &Interp, k: u16) -> LResult<Arc<str>> {
    let frame = interp.vm.frames.last().expect("frame");
    match frame.proto.proto.consts.get(k as usize) {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => Err(err(
            crate::error::E_RUNTIME,
            format!("internal: constant {} is not a string ({})", k, other.type_name()),
        )),
        None => Err(err(
            crate::error::E_RUNTIME,
            format!("internal: constant {} out of range", k),
        )),
    }
}

fn const_str_from_frame(proto: &Rc<Compiled>, k: u16) -> Arc<str> {
    match proto.proto.consts.get(k as usize) {
        Some(Value::Str(s)) => s.clone(),
        _ => Arc::from(""),
    }
}

// ===== slots =====

/// Fast local read for `GetLoc`: direct cells avoid all `Rc`/`RefCell` work;
/// captured cells fall back to the shared cell path.
#[inline]
fn push_local(interp: &mut Interp, slot: usize) -> LResult<()> {
    let frame = interp.vm.frames.last().expect("frame");
    let cell = frame
        .slots
        .get(slot)
        .ok_or_else(|| err(crate::error::E_RUNTIME, format!("internal: slot {} out of range", slot)))?;
    // Numeric locals dominate loops and recursive calls.
    if let Some(v) = cell.scalar() {
        interp.vm.stack.push(v);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    match cell.read_clone() {
        Some(v) => {
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            Ok(())
        }
        None => {
            let pc = frame.pc.saturating_sub(1);
            let label = frame.proto.proto.local_name_at(pc, slot);
            Err(err_at(
                frame.proto.proto.span_at(pc),
                crate::error::E_MOVED,
                format!("use of moved-out value `{}`", label),
            ))
        }
    }
}

/// Fast upvalue read for `GetUp` (same strategy as `push_local`).
#[inline]
fn push_upval(interp: &mut Interp, idx: usize) -> LResult<()> {
    let frame = interp.vm.frames.last().expect("frame");
    let handle = frame.upvals.get(idx).ok_or_else(|| {
        err(
            crate::error::E_RUNTIME,
            format!("internal: upvalue {} out of range", idx),
        )
    })?;
    let scalar = {
        let b = handle.borrow();
        match &*b {
            SlotV::Set(Value::Number(n)) => Some(Value::Number(*n)),
            SlotV::Set(Value::Bool(v)) => Some(Value::Bool(*v)),
            SlotV::Set(Value::Nil) => Some(Value::Nil),
            _ => None,
        }
    };
    if let Some(v) = scalar {
        interp.vm.stack.push(v);
        interp.vm.last_ret = 1;
        return Ok(());
    }
    let value = match &*handle.borrow() {
        SlotV::Set(v) => Some(v.clone()),
        SlotV::Moved(_) => None,
    };
    match value {
        Some(v) => {
            interp.vm.stack.push(v);
            interp.vm.last_ret = 1;
            Ok(())
        }
        None => {
            let label = frame
                .proto
                .proto
                .upval_labels
                .get(idx)
                .cloned()
                .unwrap_or_else(|| Arc::from("(captured)"));
            Err(err_at(
                frame_span(frame),
                crate::error::E_MOVED,
                format!("use of moved-out value `{}`", label),
            ))
        }
    }
}

/// Fast path for the three numeric loop-control slots. They are almost always
/// direct (not closure-captured), so this avoids three separate `frame_num`
/// calls and three independent `SlotCell` dispatches.
#[inline]
fn frame_nums3(frame: &VmFrame, var: usize, limit: usize, step: usize) -> LResult<(f64, f64, f64)> {
    if let (
        Some(SlotCell::Direct(SlotV::Set(Value::Number(i)))),
        Some(SlotCell::Direct(SlotV::Set(Value::Number(l)))),
        Some(SlotCell::Direct(SlotV::Set(Value::Number(s)))),
    ) = (
        frame.slots.get(var),
        frame.slots.get(limit),
        frame.slots.get(step),
    ) {
        return Ok((*i, *l, *s));
    }
    Ok((
        frame_num(frame, var)?,
        frame_num(frame, limit)?,
        frame_num(frame, step)?,
    ))
}

/// Reads a numeric loop-control slot without cloning `Value` or its handle.
#[inline]
fn frame_num(frame: &VmFrame, slot: usize) -> LResult<f64> {
    let cell = frame.slots.get(slot).ok_or_else(|| {
        err(
            crate::error::E_RUNTIME,
            format!("internal: slot {} out of range", slot),
        )
    })?;
    let state = cell.inspect(|s| match s {
        SlotV::Set(Value::Number(n)) => Ok(*n),
        SlotV::Set(other) => Err(Some(other.type_name())),
        SlotV::Moved(_) => Err(None),
    });
    match state {
        Ok(n) => Ok(n),
        // Diagnostics are built only on the error path; the loop hot path
        // never resolves a line.
        Err(Some(name)) => Err(err_at(
            frame_span(frame),
            crate::error::E_ARITH,
            format!("for control variables must be numbers (got {})", name),
        )),
        Err(None) => {
            let pc = frame.pc.saturating_sub(1);
            let label = frame.proto.proto.local_name_at(pc, slot);
            Err(err_at(
                frame_span(frame),
                crate::error::E_MOVED,
                format!("use of moved-out value `{}`", label),
            ))
        }
    }
}

fn upval_handle(interp: &mut Interp, idx: usize) -> LResult<Rc<RefCell<SlotV>>> {
    interp
        .vm
        .frames
        .last()
        .expect("frame")
        .upvals
        .get(idx)
        .cloned()
        .ok_or_else(|| err(crate::error::E_RUNTIME, format!("internal: upvalue {} out of range", idx)))
}

fn slot_label(interp: &mut Interp, slot: usize) -> Arc<str> {
    let frame = interp.vm.frames.last().expect("frame");
    let pc = frame.pc.saturating_sub(1);
    frame.proto.proto.local_name_at(pc, slot)
}

fn upval_label(interp: &mut Interp, idx: usize) -> Arc<str> {
    let proto = interp.vm.frames.last().expect("frame").proto.clone();
    proto
        .proto
        .upval_labels
        .get(idx)
        .cloned()
        .unwrap_or_else(|| Arc::from("(captured)"))
}

// ===== segments & iteration =====

/// Reads the next value of the active segment (or nil when the group ended).
/// When NO segment is active the value was pushed directly (FuncSt sugar:
/// `function f() end` = MakeClosure + Store*Seg), so take it off the stack.
fn seg_front_or_nil(interp: &mut Interp) -> LResult<Value> {
    if let Some(seg) = interp.vm.segs.last() {
        let idx = seg.cursor;
        interp.vm.segs.last_mut().expect("checked").cursor += 1;
        Ok(interp.vm.stack.get(idx).cloned().unwrap_or(Value::Nil))
    } else {
        match interp.vm.stack.pop() {
            Some(v) => {
                interp.vm.last_ret = 1;
                Ok(v)
            }
            None => Ok(Value::Nil),
        }
    }
}

/// Latest port of the interpreter's `next_iter`: table iterators skip moved
/// or deleted entries; function iterators call with no extra args.
fn advance_iter(interp: &mut Interp, it: &Value, nvars: usize) -> LResult<Vec<Value>> {
    match it.base_value() {
        Value::Iter(iter) => {
            let (start_idx, tbl) = {
                let b = iter.borrow();
                (b.idx, b.table.clone())
            };
            let mut idx = start_idx;
            loop {
                if idx >= tbl.entry_count() {
                    iter.borrow_mut().idx = idx;
                    return Ok(Vec::new());
                }
                let (k, v) = tbl.entry_at(idx).expect("idx < count");
                idx += 1;
                if v.is_none() {
                    continue;
                }
                iter.borrow_mut().idx = idx;
                let pair = vec![k, v.expect("checked")];
                return Ok(pair.into_iter().take(nvars).collect());
            }
        }
        Value::Func(_) => {
            let results = interp.call_value(it, Vec::new(), span0())?;
            if results.is_empty() {
                return Ok(Vec::new());
            }
            let n = nvars.max(1);
            let mut out: Vec<Value> = results.into_iter().take(n).collect();
            while out.len() < n {
                out.push(Value::Nil);
            }
            Ok(out)
        }
        other => Err(err(
            crate::error::E_INDEX,
            format!(
                "generic `for` needs an iterator (got from pairs/ipairs or a function); found {}",
                other.type_name()
            ),
        )),
    }
}

/// Writes row values into the consecutive loop variable slots. Each iteration
/// opens fresh bindings (missing values become `nil`, like the interpreter),
/// while closures from the previous iteration keep their detached cells.
fn store_row(interp: &mut Interp, first: usize, nvars: usize, row: &[Value]) -> LResult<()> {
    let frame = interp.vm.frames.last_mut().expect("frame");
    for i in 0..nvars {
        let cell = &mut frame.slots[first + i];
        fresh_if_shared(cell);
        cell.set(SlotV::Set(row.get(i).cloned().unwrap_or(Value::Nil)));
    }
    Ok(())
}
