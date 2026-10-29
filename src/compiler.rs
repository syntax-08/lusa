//! Bytecode compiler: Lusa AST → `Chunk`s of `Instr`.
//!
//! Closures capture **shared slots** (`Rc<RefCell<SlotV>>` at runtime), which
//! gives the same observable alias/mutation behavior as the interpreter's
//! scope chains. Multi-value expansion (calls, `...`, the last position of
//! expression lists) follows the interpreter's rules via a runtime "last
//! expansion count" marker consumed by `CallExp`/`RetExp`/`TruncateToOne`.
//!
//! The compiler is index-based: a stack of per-function frames whose nesting
//! is expressed with parent *indexes* (no recursive borrows), with Lua-style
//! upvalue-chain synthesis when a nested function reads outer locals.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use crate::ast::*;
use crate::error::{LResult, LusaError, SourceMap, Span};
use crate::value::{SlotV, Value};

// ===== compiled units =====

/// A compiled function unit plus the upvalues it captures
/// (the payload of `MakeClosure`).
pub struct Compiled {
    pub proto: Rc<Chunk>,
    pub upvals: Vec<UpDef>,
    /// Struct declarations of the whole program (root unit only).  The VM
    /// registers them with the runtime so struct ownership (heap-bearing and
    /// `#[peripheral]` structs move) matches the interpreter.
    pub structs: Vec<crate::interp::StructDef>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpDef {
    /// captures the parent *frame local slot*
    Local(u16),
    /// captures the parent *closure upvalue index*
    Up(u16),
}

/// A compiled function body.
pub struct Chunk {
    pub name: Option<Arc<str>>,
    pub nparams: usize,
    pub has_vararg: bool,
    /// `async function` bodies produce a task handle instead of running
    /// immediately when called.
    pub is_async: bool,
    pub nlocals: usize,
    pub consts: Vec<Value>,
    pub code: Vec<Instr>,
    pub prototypes: Vec<Rc<Compiled>>,
    /// slot → declared name (labels for ownership errors)
    pub slot_labels: Vec<Arc<str>>,
    /// upvalue → captured name (labels)
    pub upval_labels: Vec<Arc<str>>,
    /// `DeclFromSeg` binding groups, kept out of line so `Instr` stays `Copy`
    pub decl_groups: Vec<Vec<(u16, u16)>>,
    /// Parallel to `code`: 1-based source line that emitted each instruction.
    ///
    /// This is *cold* metadata. The dispatch loop never reads it, so it costs
    /// address space per instruction but no work in the common path; only the
    /// error path (`Chunk::span_at`) touches it.
    pub lines: Vec<u32>,
    /// Parallel to `code`: local binding referenced by the instruction.
    ///
    /// Registers are reused between sibling scopes, so a single per-slot label
    /// is not enough to describe an ownership error. The compiler snapshots the
    /// binding name at emission time and the VM reads it only when raising.
    pub local_names: Vec<Option<Arc<str>>>,
    /// Shared line index for the whole source file (nested chunks point to the
    /// same map, so a closure error still resolves to the original line).
    pub source_map: Option<Arc<SourceMap>>,
}

impl Chunk {
    /// Source line of instruction `pc`, or 0 when the chunk has no debug info.
    #[inline]
    pub fn line_at(&self, pc: usize) -> u32 {
        self.lines.get(pc).copied().unwrap_or(0)
    }

    /// Exact source span of instruction `pc`, reconstructed from its line.
    /// Called only on the error path.
    pub fn span_at(&self, pc: usize) -> Span {
        match &self.source_map {
            Some(map) => map.span_of_line(self.line_at(pc)),
            None => Span::new(0, 0),
        }
    }

    /// Binding name to show in an ownership diagnostic. Falls back to the
    /// per-slot label for instructions that do not name a single local
    /// (parameters, hidden loop slots).
    #[inline]
    pub fn local_name_at(&self, pc: usize, slot: usize) -> Arc<str> {
        self.local_names
            .get(pc)
            .and_then(|name| name.clone())
            .or_else(|| self.slot_labels.get(slot).cloned())
            .unwrap_or_else(|| Arc::from("(local)"))
    }
}

/// Stack-machine instructions. `i32` jump fields hold **absolute instruction
/// indexes** (patched after compilation).
///
/// `repr(u8)` keeps the discriminant one byte wide and lets LLVM lower the
/// dense tag match to a single jump table. All payloads are small integers:
/// the whole value is `Copy`, so the dispatch loop can load it by value
/// without touching the heap or cloning an `Rc`.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum Instr {
    LoadConst(u16),
    LoadNil,
    GetLoc(u16),
    GetUp(u16),
    GetGlob(u16), // const index of the name
    // moving reads (assignment sources, call args, returns, table entries):
    // heap values leave `SlotV::Moved` behind; label = destination string
    TakeLoc(u16, u16),
    TakeUp(u16, u16),
    TakeGlob(u16, u16), // (name const, label const)
    TakeField(u16, u16, u16), // (key const, source-label const, dest-label const)
    TakeIndex(u16, u16),      // (source-label const, dest-label const)
    NewTable,
    /// Tags the table on top of the stack as an instance of the named struct.
    /// The tag drives value-copy semantics for scalar-only structs.
    MarkStruct(u16),
    GetField(u16), // TOS = obj
    GetIndex,      // TOS = key, under = obj
    SetGlob(u16),  // TOS = value
    // single-value assignment fast path: no segment bookkeeping
    SetLoc(u16), // TOS = value
    SetUp(u16),  // TOS = value
    /// Detaches a closure-captured cell before its register is handed to a new
    /// lexical binding. The closure keeps the old `Rc<RefCell<SlotV>>`; the
    /// register gets a fresh direct cell. Never emitted for uncaptured slots.
    ResetSlot(u16),
    SetFieldK(u16), // TOS = obj, below = value
    SetIndex,      // TOS = key, under1 = obj, under2 = value
    SetArr,        // TOS = value, under = table (appends via next_arr)
    SetArrExp,     // like SetArr but appends last_ret values in order
    // binary / unary ops
    Add, Sub, Mul, Div, FloorDiv, Mod, Pow, Concat,
    Eq, Ne, Lt, Le, Gt, Ge,
    Neg, Not, Len,
    /// `&place`: stores the value in the interpreter's simulated memory and
    /// pushes its address (the dynamic-engine analogue of a C `&`).
    AddrOf,
    CloneV,
    /// `expr?`: unwraps `Ok`/`Some` or returns `Err`/`None` from this frame.
    Try,
    /// `await expr`: pops a task handle and drives it to completion, then
    /// pushes the task result.
    Await,
    /// `task.yield_now()`: suspends the current task.  On resume the VM
    /// pushes `nil` as the call result (one-value semantics).
    YieldNow,
    /// Wraps TOS in a dynamic-engine `Box<T>` (idempotent for existing boxes).
    MakeBox,
    BorrowErr, // borrow is rejected in bytecode mode
    // calls — `nt` = number of journaled argument takes finalized by this call
    // (builtins restore them, matching the interpreter's `is_builtin()` rules)
    Call(u16, u16),
    CallExp(u16, u16), // nargs = base + last_ret
    // single-value call variants: the return path truncates instead of the
    // caller executing a separate `TruncateToOne`
    CallOne(u16, u16),
    CallExpOne(u16, u16),
    /// direct `coroutine.yield(...)` suspension point. The VM leaves the
    /// instruction and resumes after it when the coroutine is resumed.
    Yield(u16, u16),
    YieldExp(u16, u16),
    YieldOne(u16, u16),
    YieldExpOne(u16, u16),
    MethodCall(u16, u16, u16),
    MethodCallExp(u16, u16, u16),
    MethodCallOne(u16, u16, u16),
    MethodCallExpOne(u16, u16, u16),
    // deferred call-argument takes (same moving reads; journaled per-frame
    // until the call decides: builtins restore, user functions commit)
    ArgLoc(u16, u16),
    ArgUp(u16, u16),
    ArgGlob(u16, u16),
    ArgField(u16, u16, u16), // (key const, source-label const, dest-label const)
    ArgIndex(u16, u16),      // (source-label const, dest-label const)
    // control flow
    Jump(i32),
    JumpIfFalse(i32),     // pops tested value
    JumpIfTrueKeep(i32),  // peeks: true keeps (or), false pops
    JumpIfFalseKeep(i32), // peeks: false keeps (and), true pops
    // `unsafe do ... end` depth tracking for the pointer sandbox
    UnsafeEnter,
    UnsafeLeave,
    // stack-value fixups
    TruncateToOne, // keep the FIRST of the last_ret values (Lua rule)
    DropValues,    // statement calls: discard last_ret values
    Pop,           // discard one value
    // closures & returns
    MakeClosure(u16),
    RetFixed(u16),
    RetExp(u16), // nvals = base + last_ret
    // varargs
    VarargLoad,    // pushes all `...` (multi)
    VarargLoadOne, // first of `...` (single)
    // loops
    ForNumInit {
        var: u16,
        limit: u16,
        step: u16,
        end: i32,
    },
    ForNumStep {
        var: u16,
        limit: u16,
        step: u16,
        top: i32,
        end: i32,
    },
    ForInSetup {
        iter: u16,
        first: u16,
        nvars: u16,
        end: i32,
    },
    ForInStep {
        iter: u16,
        first: u16,
        nvars: u16,
        top: i32,
        end: i32,
    },
    // list segments (assignment / declaration value groups)
    SegStart,
    SegEnd,
    DeclFromSeg(u16), // index into Chunk::decl_groups
    StoreLocSeg(u16),
    StoreUpSeg(u16),
    StoreGlobSeg(u16),  // name-const
    StoreFieldSeg(u16), // TOS = obj; name-const
    StoreIndexFromSeg,  // TOS = key, under = obj
    BorrowImm,          // TOS = value; wraps it in an immutable alias
    ClearSlot(u16),     // drop a lexical binding as its block exits
}

// ===== compiler state =====

/// One register in a frame. `captured` is the compiler's record that a closure
/// took this slot as an upvalue; it drives the `ResetSlot` emission.
#[derive(Clone)]
struct SlotInfo {
    name: Arc<str>,
    captured: bool,
}

/// One lexical level inside a function.
///
/// `names` is only a compile-time resolver (AST name → register). It never
/// reaches the VM. `slots` records every register opened at this level, so the
/// allocator can return them to the free list when the block ends.
struct Scope {
    names: HashMap<String, u16>,
    slots: Vec<u16>,
}

impl Scope {
    fn new() -> Scope {
        Scope {
            names: HashMap::new(),
            slots: Vec::new(),
        }
    }
}

/// One function-in-progress. Lives in `Compiler.frames`; nesting is expressed
/// with the parent *index* (no recursive borrows).
struct Frame {
    name: String,
    parent: Option<usize>,
    /// lexical levels: name → register
    scopes: Vec<Scope>,
    /// register file layout: slot → binding info (high-water mark = `nlocals`)
    slots: Vec<SlotInfo>,
    /// registers released by finished blocks, ready for reuse
    free_slots: Vec<u16>,
    /// current source line, stamped onto every emitted instruction
    line: u32,
    /// parallel to `code` — see `Chunk::lines`
    lines: Vec<u32>,
    /// parallel to `code` — see `Chunk::local_names`
    local_names: Vec<Option<Arc<str>>>,
    consts: Vec<ConstKey>,
    consts_idx: HashMap<ConstKey, u16>,
    protos: Vec<Rc<Compiled>>,
    upvals: Vec<UpDef>,
    upval_labels: Vec<String>,
    code: Vec<Instr>,
    /// out-of-line `DeclFromSeg` binding groups
    decl_groups: Vec<Vec<(u16, u16)>>,
    /// pending break jumps of the innermost enclosing loops
    breaks: Vec<Vec<usize>>,
    /// lexical scope depth at each enclosing loop entry
    loop_depths: Vec<usize>,
    /// number of `unsafe` blocks currently open in this function
    unsafe_depth: usize,
    /// `unsafe_depth` at each enclosing loop entry, so `break` can leave only
    /// the unsafe blocks it actually escapes
    loop_unsafe_depths: Vec<usize>,
    nparams: usize,
    has_vararg: bool,
    is_async: bool,
}

#[derive(Clone, Debug, PartialEq, Hash, Eq)]
enum ConstKey {
    Nil,
    Bool(bool),
    Num(u64),
    Str(String),
}

impl ConstKey {
    fn of(v: &Value) -> ConstKey {
        match v {
            Value::Nil => ConstKey::Nil,
            Value::Bool(b) => ConstKey::Bool(*b),
            Value::Number(n) => ConstKey::Num(n.to_bits()),
            Value::Str(s) => ConstKey::Str(s.to_string()),
            other => unreachable!("consts hold scalars only; got {}", other.type_name()),
        }
    }

    fn to_value(&self) -> Value {
        match self {
            ConstKey::Nil => Value::Nil,
            ConstKey::Bool(b) => Value::Bool(*b),
            ConstKey::Num(bits) => Value::Number(f64::from_bits(*bits)),
            ConstKey::Str(s) => Value::Str(Arc::from(s.as_str())),
        }
    }
}

struct Compiler {
    frames: Vec<Frame>,
    /// Shared by every chunk produced from this source file.
    source: Arc<SourceMap>,
    /// `struct` declarations seen so far (declaration order matters, like
    /// the interpreter's runtime registration).
    struct_defs: HashMap<String, Vec<(String, String)>>,
    /// Every struct declaration in source order, with its attributes.
    struct_list: Vec<crate::interp::StructDef>,
}

impl Frame {
    fn new(name: impl Into<String>, parent: Option<usize>) -> Frame {
        Frame {
            name: name.into(),
            parent,
            scopes: vec![Scope::new()],
            slots: Vec::new(),
            free_slots: Vec::new(),
            line: 1,
            lines: Vec::new(),
            local_names: Vec::new(),
            consts: Vec::new(),
            consts_idx: HashMap::new(),
            protos: Vec::new(),
            upvals: Vec::new(),
            upval_labels: Vec::new(),
            code: Vec::new(),
            decl_groups: Vec::new(),
            breaks: Vec::new(),
            loop_depths: Vec::new(),
            unsafe_depth: 0,
            loop_unsafe_depths: Vec::new(),
            nparams: 0,
            has_vararg: false,
            is_async: false,
        }
    }

    fn find_local(&self, name: &str) -> Option<u16> {
        for scope in self.scopes.iter().rev() {
            if let Some(&slot) = scope.names.get(name) {
                return Some(slot);
            }
        }
        None
    }
}

impl Compiler {
    fn cur(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("a frame is always compiling")
    }

    fn top(&self) -> usize {
        self.frames.len() - 1
    }

    /// Emits one instruction, stamping the current line and binding name into
    /// the cold debug side-tables in the same step. Keeping all three vectors
    /// in lockstep here is what makes the parallel mapping reliable.
    fn em(&mut self, i: Instr) -> usize {
        let f = self.cur();
        let line = f.line;
        let local = match i {
            Instr::GetLoc(s)
            | Instr::SetLoc(s)
            | Instr::TakeLoc(s, _)
            | Instr::ArgLoc(s, _)
            | Instr::StoreLocSeg(s) => f.slots.get(s as usize).map(|info| info.name.clone()),
            Instr::ForNumInit { var, .. } | Instr::ForNumStep { var, .. } => {
                f.slots.get(var as usize).map(|info| info.name.clone())
            }
            _ => None,
        };
        f.code.push(i);
        f.lines.push(line);
        f.local_names.push(local);
        f.code.len() - 1
    }

    fn pos(&self) -> usize {
        self.frames.last().map(|f| f.code.len()).unwrap_or(0)
    }

    /// Sets the line stamped on following instructions. `Span` is char-offset
    /// based, so this is one binary search over the shared `SourceMap`.
    fn set_line(&mut self, span: Span) {
        let line = self.source.line_of(span.start);
        self.cur().line = line;
    }

    fn konst(&mut self, k: &ConstKey) -> u16 {
        let f = self.cur();
        if let Some(&i) = f.consts_idx.get(k) {
            return i;
        }
        let i = f.consts.len() as u16;
        f.consts.push(k.clone());
        f.consts_idx.insert(k.clone(), i);
        i
    }

    fn konst_str(&mut self, s: &str) -> u16 {
        self.konst(&ConstKey::Str(s.to_string()))
    }

    /// Allocates a register for a new lexical binding.
    ///
    /// Registers are handed out from the lowest free index, so a straight-line
    /// function still gets R0, R1, R2, …; when a block ends its registers go
    /// back to `free_slots` and the next sibling block reuses them. The chunk's
    /// `nlocals` is therefore a high-water mark, not a declaration count.
    fn declare(&mut self, name: &str) -> u16 {
        let (slot, needs_reset) = {
            let f = self.cur();
            let slot = match f.free_slots.pop() {
                Some(slot) => slot,
                None => {
                    let slot = f.slots.len() as u16;
                    f.slots.push(SlotInfo {
                        name: Arc::from(""),
                        captured: false,
                    });
                    slot
                }
            };
            let info = &mut f.slots[slot as usize];
            // A captured slot still points at the old closure's shared cell.
            // Detach it before the new binding writes through it.
            let needs_reset = info.captured;
            info.name = Arc::from(name);
            info.captured = false;
            let scope = f.scopes.last_mut().expect("scope levels are never empty");
            scope.names.insert(name.to_string(), slot);
            scope.slots.push(slot);
            (slot, needs_reset)
        };
        if needs_reset {
            self.em(Instr::ResetSlot(slot));
        }
        slot
    }

    fn push_shadow(&mut self) {
        self.cur().scopes.push(Scope::new());
    }

    /// Closes the current lexical level.
    ///
    /// Captured registers get a `ResetSlot` at the end of the block. Besides
    /// making the register safe to reuse, this gives each loop iteration a
    /// fresh cell for captured locals — closures from different iterations must
    /// not alias one another. Uncaptured registers need no instruction at all:
    /// the next declaration simply overwrites the direct cell.
    fn close_scope(&mut self) {
        let (slots, captured): (Vec<u16>, Vec<u16>) = {
            let f = self.cur();
            let scope = f.scopes.last().expect("scope levels are never empty");
            let slots: Vec<u16> = scope.slots.clone();
            let captured = slots
                .iter()
                .copied()
                .filter(|&slot| f.slots[slot as usize].captured)
                .collect();
            (slots, captured)
        };
        for slot in captured {
            self.em(Instr::ResetSlot(slot));
        }
        // Release aliases and other heap values at lexical scope exit.  The
        // register may be reused immediately afterwards for a sibling block.
        for slot in slots {
            self.em(Instr::ClearSlot(slot));
        }
    }

    /// Emits the cleanup instructions for scopes at `depth` and deeper without
    /// popping them. Used by `break`, which jumps past the normal block-exit
    /// sequence but must still release aliases and captured cells.
    fn close_scopes_from(&mut self, depth: usize) {
        let (slots, captured): (Vec<u16>, Vec<u16>) = {
            let f = self.cur();
            let mut slots = Vec::new();
            for scope in f.scopes.iter().skip(depth) {
                slots.extend(scope.slots.iter().copied());
            }
            let captured = slots
                .iter()
                .copied()
                .filter(|&slot| f.slots[slot as usize].captured)
                .collect();
            (slots, captured)
        };
        for slot in captured {
            self.em(Instr::ResetSlot(slot));
        }
        for slot in slots {
            self.em(Instr::ClearSlot(slot));
        }
    }

    fn pop_shadow(&mut self) {
        self.close_scope();
        let scope = self
            .cur()
            .scopes
            .pop()
            .expect("scope levels are never empty");
        // Reuse the most recently released registers first. This keeps the
        // allocation pattern stable when blocks are nested.
        self.cur().free_slots.extend(scope.slots.into_iter().rev());
    }

    /// Patches an already-emitted jump to an absolute instruction index.
    fn patch(&mut self, at: usize, target: usize) {
        let code = &mut self.cur().code;
        match &mut code[at] {
            Instr::Jump(t)
            | Instr::JumpIfFalse(t)
            | Instr::JumpIfTrueKeep(t)
            | Instr::JumpIfFalseKeep(t)
            | Instr::ForNumInit { end: t, .. }
            | Instr::ForNumStep { end: t, .. }
            | Instr::ForInSetup { end: t, .. }
            | Instr::ForInStep { end: t, .. } => *t = target as i32,
            _ => unreachable!("patching a non-jump instruction"),
        }
    }
}

// ===== name resolution =====

enum Resolved {
    Local(u16),
    Upval(u16),
    Global,
}

/// Resolves `name` within the frame stack: current frame locals first, then a
/// synthesized upvalue chain through ancestor frames, then a global.
fn resolve(c: &mut Compiler, name: &str) -> Resolved {
    let top = c.top();
    if let Some(slot) = c.frames[top].find_local(name) {
        return Resolved::Local(slot);
    }
    // find the ancestor level holding the name
    let mut level = top;
    while let Some(p) = c.frames[level].parent {
        if c.frames[p].find_local(name).is_some() {
            level = p;
            // synthesize upvalue entries from level+1 up to top; each frame
            // points at the entry registered in its inner neighbor
            let mut cur_idx = 0u16;
            let mut j = level + 1;
            while j <= top {
                let def = if j == level + 1 {
                    let slot = c.frames[level].find_local(name).expect("checked above");
                    // Record that this register's cell escapes into a closure.
                    // `close_scope`/`declare` will emit a detachment before the
                    // register is reused.
                    c.frames[level].slots[slot as usize].captured = true;
                    UpDef::Local(slot)
                } else {
                    UpDef::Up(cur_idx)
                };
                let already = c.frames[j].upvals.iter().position(|u| *u == def);
                cur_idx = match already {
                    Some(i) => i as u16,
                    None => {
                        c.frames[j].upvals.push(def);
                        c.frames[j].upval_labels.push(name.to_string());
                        (c.frames[j].upvals.len() - 1) as u16
                    }
                };
                j += 1;
            }
            return Resolved::Upval(cur_idx);
        }
        level = p;
    }
    Resolved::Global
}

/// Emits the load for a variable read.
fn emit_var_load(c: &mut Compiler, name: &str) {
    match resolve(c, name) {
        Resolved::Local(slot) => {
            c.em(Instr::GetLoc(slot));
        }
        Resolved::Upval(i) => {
            c.em(Instr::GetUp(i));
        }
        Resolved::Global => {
            let k = c.konst_str(name);
            c.em(Instr::GetGlob(k));
        }
    }
}

// ===== moving reads =====

/// Compiles a value in a *moving* position (assignment sources, call args,
/// returns, table entries, for-in groups). Var/Field/Index sources consume the
/// heap value out of its slot (parity with the interpreter's `eval_take_single`);
/// anything else compiles as a plain read.
///
/// When `arg` is true the take is DEFERRED: the `Arg*` ops push the value and
/// journal the source in the current frame; the call instruction finalizes them
/// (builtins restore — they never take ownership; user functions commit).
/// Returns true when a take op was emitted (call sites count them).
fn compile_take_value(c: &mut Compiler, e: &Expr, dest: &str, arg: bool) -> LResult<bool> {
    // Moving reads belong to the source expression, which may be on a
    // different line from the statement that owns it.
    c.set_line(e.span);
    match &e.kind {
        ExprKind::Var(name) => {
            let lk = c.konst_str(dest);
            match resolve(c, name) {
                Resolved::Local(slot) => {
                    if arg {
                        c.em(Instr::ArgLoc(slot, lk));
                    } else {
                        c.em(Instr::TakeLoc(slot, lk));
                    }
                    return Ok(true);
                }
                Resolved::Upval(i) => {
                    if arg {
                        c.em(Instr::ArgUp(i, lk));
                    } else {
                        c.em(Instr::TakeUp(i, lk));
                    }
                    return Ok(true);
                }
                Resolved::Global => {
                    let k = c.konst_str(name);
                    if arg {
                        c.em(Instr::ArgGlob(k, lk));
                    } else {
                        c.em(Instr::TakeGlob(k, lk));
                    }
                    return Ok(true);
                }
            }
        }
        ExprKind::Field { obj, name } => {
            compile_expr(c, obj)?;
            truncate_if_expansion(c, obj);
            let k = c.konst_str(name);
            let src = match &obj.kind {
                ExprKind::Var(vn) => format!("{}.{}", vn, name),
                _ => format!("<value>.{}", name),
            };
            let sk = c.konst_str(&src);
            let lk = c.konst_str(dest);
            c.set_line(e.span);
            if arg {
                c.em(Instr::ArgField(k, sk, lk));
            } else {
                c.em(Instr::TakeField(k, sk, lk));
            }
            return Ok(true);
        }
        ExprKind::Index { obj, key } => {
            compile_expr(c, obj)?;
            truncate_if_expansion(c, obj);
            compile_expr(c, key)?;
            truncate_if_expansion(c, key);
            let src = match &obj.kind {
                ExprKind::Var(vn) => format!("{}[]", vn),
                _ => format!("<value>[]"),
            };
            let sk = c.konst_str(&src);
            let lk = c.konst_str(dest);
            c.set_line(e.span);
            if arg {
                c.em(Instr::ArgIndex(sk, lk));
            } else {
                c.em(Instr::TakeIndex(sk, lk));
            }
            return Ok(true);
        }
        ExprKind::Move { e: inner } => {
            return compile_take_value(c, inner, dest, arg);
        }
        _ => {}
    }
    // expansion nodes keep their multi-value shape; the caller truncates
    // mid-list positions (`i + 1 < n` rule mirrors the interpreter)
    if is_expansion(e) {
        compile_expand_value(c, e)?;
    } else {
        compile_expr(c, e)?;
    }
    Ok(false)
}

/// Compiles a value whose results may be multi (calls already push all of
/// theirs through `CallExp`; `...` must push the whole rest here).
fn compile_expand_value(c: &mut Compiler, e: &Expr) -> LResult<()> {
    c.set_line(e.span);
    match &e.kind {
        ExprKind::Vararg => {
            c.em(Instr::VarargLoad);
        }
        _ => compile_expr(c, e)?,
    }
    Ok(())
}

// ===== expressions =====

/// scalar constant of simple literal nodes
fn scalar_of(e: &Expr) -> Value {
    match &e.kind {
        ExprKind::Nil => Value::Nil,
        ExprKind::True => Value::Bool(true),
        ExprKind::False => Value::Bool(false),
        ExprKind::Num(n) => Value::Number(*n),
        ExprKind::Int(text) => Value::Number(crate::value::int_literal_to_f64(text)),
        _ => unreachable!("scalar_of on non-literal"),
    }
}

/// Compiles an expression in *single-value* position: pushes exactly one value.
fn compile_expr(c: &mut Compiler, e: &Expr) -> LResult<()> {
    // Every instruction emitted below inherits this line unless a nested
    // expression updates it. Ownership faults point at the variable read/write,
    // which is exactly the node that sets the line on entry.
    c.set_line(e.span);
    match &e.kind {
        ExprKind::Nil | ExprKind::True | ExprKind::False | ExprKind::Num(_) | ExprKind::Int(_) => {
            let k = c.konst(&ConstKey::of(&scalar_of(e)));
            c.em(Instr::LoadConst(k));
        }
        ExprKind::Str(segs) => compile_interp_str(c, segs)?,
        ExprKind::Var(name) => emit_var_load(c, name),
        ExprKind::Field { obj, name } => {
            compile_expr(c, obj)?;
            let k = c.konst_str(name);
            c.set_line(e.span);
            c.em(Instr::GetField(k));
        }
        ExprKind::Index { obj, key } => {
            compile_expr(c, obj)?;
            compile_expr(c, key)?;
            c.set_line(e.span);
            c.em(Instr::GetIndex);
        }
        ExprKind::Paren(inner) => {
            compile_expr(c, inner)?;
            c.set_line(e.span);
            emit_truncate_one(c);
        }
        ExprKind::Vararg => {
            c.em(Instr::VarargLoadOne);
        }
        ExprKind::Call { callee, args, span_args_end } => {
            compile_call(c, callee, args, e.span, *span_args_end)?;
        }
        ExprKind::Method { obj, name, args, span_args_end } => {
            compile_method(c, obj, name, args, e.span, *span_args_end)?;
        }
        ExprKind::Table(entries) => compile_table(c, entries)?,
        ExprKind::ArrayLit(items) => compile_array(c, items)?,
        ExprKind::AddrOf(inner) => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(Instr::AddrOf);
        }
        ExprKind::StructLit { name, fields } => compile_struct_lit(c, name, fields, e.span)?,
        ExprKind::Func(lit) => {
            compile_nested_fn(c, lit, "anonymous")?;
        }
        ExprKind::Bin { op, l, r } => {
            compile_expr(c, l)?;
            truncate_if_expansion(c, l);
            compile_expr(c, r)?;
            truncate_if_expansion(c, r);
            c.set_line(e.span);
            c.em(match op {
                BinOp::Add => Instr::Add,
                BinOp::Sub => Instr::Sub,
                BinOp::Mul => Instr::Mul,
                BinOp::Div => Instr::Div,
                BinOp::FloorDiv => Instr::FloorDiv,
                BinOp::Mod => Instr::Mod,
                BinOp::Pow => Instr::Pow,
                BinOp::Concat => Instr::Concat,
                BinOp::Eq => Instr::Eq,
                BinOp::Ne => Instr::Ne,
                BinOp::Lt => Instr::Lt,
                BinOp::Gt => Instr::Gt,
                BinOp::Le => Instr::Le,
                BinOp::Ge => Instr::Ge,
            });
        }
        ExprKind::Log { op, l, r } => {
            compile_expr(c, l)?;
            truncate_if_expansion(c, l);
            c.set_line(e.span);
            let skip = match op {
                LogOp::And => c.em(Instr::JumpIfFalseKeep(0)),
                LogOp::Or => c.em(Instr::JumpIfTrueKeep(0)),
            };
            // The keep-jumps already pop the value when evaluation must
            // continue (false for `or`, true for `and`) and keep it when the
            // expression short-circuits.  An extra Pop here would remove the
            // surrounding value (for example the callee) on the continue path.
            compile_expr(c, r)?;
            truncate_if_expansion(c, r);
            c.set_line(e.span);
            let end = c.pos();
            c.patch(skip, end);
        }
        ExprKind::Un { op, e: inner } => {
            if matches!(op, UnOp::Deref) {
                // `*ptr` reads a simulated memory cell through the builtin,
                // so the VM enforces the same `unsafe` sandbox as the
                // interpreter (checked inside the builtin).
                let callee = Expr {
                    kind: ExprKind::Var(Box::new("box_read".to_string())),
                    span: e.span,
                };
                compile_expr(c, &callee)?;
                compile_expr(c, inner)?;
                truncate_if_expansion(c, inner);
                c.set_line(e.span);
                c.em(Instr::CallOne(1, 0));
                return Ok(());
            }
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(match op {
                UnOp::Neg => Instr::Neg,
                UnOp::Not => Instr::Not,
                UnOp::Len => Instr::Len,
                UnOp::Deref => unreachable!("handled above"),
            });
        }
        ExprKind::Borrow { e: inner, .. } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(Instr::BorrowImm);
        }
        ExprKind::Clone { e: inner } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(Instr::CloneV);
        }
        ExprKind::Try { value: inner } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(Instr::Try);
        }
        ExprKind::Await { value: inner } => {
            compile_expr(c, inner)?;
            truncate_if_expansion(c, inner);
            c.set_line(e.span);
            c.em(Instr::Await);
        }
        ExprKind::Move { e: inner } => {
            let dest = crate::eval::label_of(inner);
            let took = compile_take_value(c, inner, &dest, false)?;
            if !took {
                truncate_if_expansion(c, inner);
            }
        }
    }
    Ok(())
}

/// Non-last positions truncate expansions to their first value.
fn truncate_if_expansion(c: &mut Compiler, e: &Expr) {
    if is_expansion(e) {
        emit_truncate_one(c);
    }
}

/// Emits a one-value truncation, fusing it into the call instruction when the
/// last emitted instruction is that call. The VM then truncates on return,
/// saving a dispatch on the extremely common `f(...)` single-value path.
fn emit_truncate_one(c: &mut Compiler) {
    if let Some(last) = c.cur().code.last_mut() {
        let fused = match *last {
            Instr::Call(a, b) => Some(Instr::CallOne(a, b)),
            Instr::CallExp(a, b) => Some(Instr::CallExpOne(a, b)),
            Instr::Yield(a, b) => Some(Instr::YieldOne(a, b)),
            Instr::YieldExp(a, b) => Some(Instr::YieldExpOne(a, b)),
            Instr::MethodCall(a, b, d) => Some(Instr::MethodCallOne(a, b, d)),
            Instr::MethodCallExp(a, b, d) => Some(Instr::MethodCallExpOne(a, b, d)),
            _ => None,
        };
        if let Some(ins) = fused {
            *last = ins;
            return;
        }
    }
    c.em(Instr::TruncateToOne);
}

/// Is this expression a known multi-return producer?
fn is_expansion(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Call { .. } | ExprKind::Method { .. } | ExprKind::Vararg
    )
}

/// Compiles an interpolated string: literal + `\{expr}` segments.
/// Pieces are pushed in order and joined with a `Concat` step each
/// (left-associative, mirroring the interpreter).
fn compile_interp_str(c: &mut Compiler, segs: &[StrAstSeg]) -> LResult<()> {
    if segs.len() == 1 {
        if let StrAstSeg::Lit(text) = &segs[0] {
            let k = c.konst_str(text);
            c.em(Instr::LoadConst(k));
            return Ok(());
        }
    }
    for (i, seg) in segs.iter().enumerate() {
        match seg {
            StrAstSeg::Lit(text) => {
                let k = c.konst_str(text);
                c.em(Instr::LoadConst(k));
            }
            StrAstSeg::Interp(ex) => {
                compile_expr(c, ex)?;
                truncate_if_expansion(c, ex);
            }
        }
        if i > 0 {
            c.em(Instr::Concat);
        }
    }
    Ok(())
}

// ===== calls =====

/// Compiles a call: callee first, then args; only the LAST arg keeps expansion.
fn compile_call(
    c: &mut Compiler,
    callee: &Expr,
    args: &[Expr],
    span: Span,
    _end: Span,
) -> LResult<()> {
    if is_direct_task_yield(c, callee) {
        if !args.is_empty() {
            return Err(LusaError::new(
                crate::error::E_CALL,
                Some(span),
                "`task.yield_now` does not take arguments".to_string(),
            ));
        }
        c.set_line(span);
        c.em(Instr::YieldNow);
        return Ok(());
    }
    if is_direct_coroutine_yield(c, callee) {
        let n = args.len();
        let mut nt = 0usize;
        for (i, arg) in args.iter().enumerate() {
            let dest = format!("(yield argument {})", i + 1);
            if compile_take_value(c, arg, &dest, true)? {
                nt += 1;
            }
            if i + 1 < n {
                truncate_if_expansion(c, arg);
            }
        }
        c.set_line(span);
        if n > 0 && is_expansion(&args[n - 1]) {
            c.em(Instr::YieldExp((n - 1) as u16, nt as u16));
        } else {
            c.em(Instr::Yield(n as u16, nt as u16));
        }
        return Ok(());
    }
    compile_expr(c, callee)?;
    truncate_if_expansion(c, callee);
    let n = args.len();
    let mut nt = 0usize;
    for (i, arg) in args.iter().enumerate() {
        let dest = format!("(argument {})", i + 1);
        if compile_take_value(c, arg, &dest, true)? {
            nt += 1;
        }
        if i + 1 < n {
            truncate_if_expansion(c, arg);
        }
    }
    c.set_line(span);
    if n > 0 && is_expansion(&args[n - 1]) {
        c.em(Instr::CallExp((n - 1) as u16, nt as u16));
    } else {
        c.em(Instr::Call(n as u16, nt as u16));
    }
    Ok(())
}

/// `true` for a direct `coroutine.yield(...)` call whose `coroutine` name
/// resolves to the standard-library global. A user local named `coroutine`
/// keeps normal call semantics.
fn is_direct_coroutine_yield(c: &mut Compiler, callee: &Expr) -> bool {
    let ExprKind::Field { obj, name } = &callee.kind else {
        return false;
    };
    if name.as_str() != "yield" {
        return false;
    }
    let ExprKind::Var(root) = &obj.kind else {
        return false;
    };
    root.as_str() == "coroutine" && matches!(resolve(c, "coroutine"), Resolved::Global)
}

/// `true` for a direct `task.yield_now()` call inside an async function whose
/// `task` root resolves to the standard-library global.  Outside async code
/// the call remains an ordinary builtin call and reports a runtime error.
fn is_direct_task_yield(c: &mut Compiler, callee: &Expr) -> bool {
    if !c.cur().is_async {
        return false;
    }
    let ExprKind::Field { obj, name } = &callee.kind else {
        return false;
    };
    if name.as_str() != "yield_now" {
        return false;
    }
    let ExprKind::Var(root) = &obj.kind else {
        return false;
    };
    root.as_str() == "task" && matches!(resolve(c, "task"), Resolved::Global)
}

/// Compiles a function literal that must be callable without a surrounding
/// bytecode frame (a coroutine entry or a function called from one). Visible
/// interpreter locals are exposed as synthetic upvalues so captured reads
/// keep working; the returned `captures` list lets the coroutine library
/// synchronize those cells with their originating scopes.
pub struct RuntimeFunction {
    pub compiled: Rc<Compiled>,
    pub upvals: Rc<[Rc<RefCell<SlotV>>]>,
    pub captures: Vec<(usize, crate::scope::ScopeRef, Arc<str>)>,
}

pub fn compile_runtime_function(
    lit: &FuncLit,
    env: &[crate::scope::ScopeRef],
    structs: &HashMap<String, Vec<(String, String)>>,
    seed: Option<&crate::value::CaptureMap>,
) -> LResult<RuntimeFunction> {
    let source = Arc::new(SourceMap::new(""));
    let mut c = Compiler {
        frames: vec![Frame::new("runtime", None)],
        source,
        struct_defs: structs.clone(),
        struct_list: Vec::new(),
    };

    // Expose every name visible through the closure environment. The compiler
    // only creates upvalues for the names the body actually reads or writes.
    let mut seen: HashMap<String, ()> = HashMap::new();
    for sc in env {
        for name in sc.borrow().vars.keys() {
            if seen.insert(name.clone(), ()).is_none() {
                c.declare(name);
            }
        }
    }

    compile_nested_fn(&mut c, lit, "coroutine")?;
    let parent = c.frames.pop().expect("runtime parent frame");
    let compiled = parent
        .protos
        .into_iter()
        .next()
        .expect("coroutine prototype");

    let mut upvals: Vec<Rc<RefCell<SlotV>>> =
        Vec::with_capacity(compiled.proto.upval_labels.len());
    let mut captures: Vec<(usize, crate::scope::ScopeRef, Arc<str>)> = Vec::new();
    for (i, label) in compiled.proto.upval_labels.iter().enumerate() {
        // A helper called from a running coroutine must share the coroutine's
        // existing cells, not take a fresh snapshot of the outer scopes.
        if let Some(existing) = seed.and_then(|map| map.get(&**label)) {
            upvals.push(existing.1.clone());
            captures.push((i, existing.0.clone(), label.clone()));
            continue;
        }
        if let Some(sc) = crate::scope::find_in(env, &**label) {
            let value = {
                let b = sc.borrow();
                match b.vars.get(&**label) {
                    Some(SlotV::Set(v)) => v.clone(),
                    _ => Value::Nil,
                }
            };
            upvals.push(Rc::new(RefCell::new(SlotV::Set(value))));
            captures.push((i, sc, label.clone()));
        } else {
            upvals.push(Rc::new(RefCell::new(SlotV::Set(Value::Nil))));
        }
    }

    Ok(RuntimeFunction {
        compiled,
        upvals: Rc::from(upvals),
        captures,
    })
}

/// Compiles `obj:name(args...)`.
fn compile_method(
    c: &mut Compiler,
    obj: &Expr,
    name: &str,
    args: &[Expr],
    span: Span,
    _end: Span,
) -> LResult<()> {
    compile_expr(c, obj)?;
    truncate_if_expansion(c, obj);
    let k = c.konst_str(name);
    let n = args.len();
    let mut nt = 0usize;
    for (i, arg) in args.iter().enumerate() {
        let dest = format!("(argument {} of :{})", i + 1, name);
        if compile_take_value(c, arg, &dest, true)? {
            nt += 1;
        }
        if i + 1 < n {
            truncate_if_expansion(c, arg);
        }
    }
    c.set_line(span);
    if n > 0 && is_expansion(&args[n - 1]) {
        c.em(Instr::MethodCallExp(k, (n - 1) as u16, nt as u16));
    } else {
        c.em(Instr::MethodCall(k, n as u16, nt as u16));
    }
    Ok(())
}

/// Compiles a table constructor; the fresh table stays under the entries.
fn compile_table(c: &mut Compiler, entries: &[TableEntry]) -> LResult<()> {
    c.em(Instr::NewTable);
    let n = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let is_last = i + 1 == n;
        match &entry.key {
            Some(key) => {
                compile_expr(c, key)?;
                truncate_if_expansion(c, key);
                let _ = compile_take_value(c, &entry.value, "(into table)", false)?;
                truncate_if_expansion(c, &entry.value);
                c.set_line(entry.span);
                c.em(Instr::SetIndex);
            }
            None => {
                if is_last && is_expansion(&entry.value) {
                    compile_expand_value(c, &entry.value)?;
                    c.set_line(entry.span);
                    c.em(Instr::SetArrExp);
                } else {
                    let dest = format!("item {}", c.pos());
                    let _ = compile_take_value(c, &entry.value, &dest, false)?;
                    truncate_if_expansion(c, &entry.value);
                    c.set_line(entry.span);
                    c.em(Instr::SetArr);
                }
            }
        }
    }
    Ok(())
}

/// Compiles a fixed-size array literal into a table with 0-based numeric keys.
fn compile_array(c: &mut Compiler, items: &[Expr]) -> LResult<()> {
    c.em(Instr::NewTable);
    for (i, item) in items.iter().enumerate() {
        let key = ConstKey::Num((i as f64).to_bits());
        let k = c.konst(&key);
        c.em(Instr::LoadConst(k));
        let dest = format!("item {i}");
        let _ = compile_take_value(c, item, &dest, false)?;
        truncate_if_expansion(c, item);
        c.set_line(item.span);
        c.em(Instr::SetIndex);
    }
    Ok(())
}

/// Compiles a static struct literal into a table with the struct's declared
/// fields initialized in declaration order, then the supplied fields in source
/// order.  The dynamic engines use the same shape, so programs produce the
/// same field layout in both backends.
fn compile_struct_lit(
    c: &mut Compiler,
    name: &str,
    fields: &[StructLitField],
    span: Span,
) -> LResult<()> {
    let def = match c.struct_defs.get(name) {
        Some(def) => def.clone(),
        None => {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(span),
                format!("unknown struct `{name}`"),
            )
            .help("declare it with `struct Name ... end` before using a struct literal"));
        }
    };
    let mut seen: Vec<&str> = Vec::new();
    for field in fields {
        if !def.iter().any(|(fname, _)| fname == &field.name) {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(field.span),
                format!("struct `{name}` has no field `{}`", field.name),
            ));
        }
        if seen.contains(&field.name.as_str()) {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(field.span),
                format!("duplicate field `{}` in struct literal", field.name),
            ));
        }
        seen.push(field.name.as_str());
    }

    emit_struct_defaults(c, &def, 0, span, name)?;
    for field in fields {
        c.set_line(field.span);
        let key = c.konst_str(&field.name);
        c.em(Instr::LoadConst(key));
        let dest = format!("field `{}` of `{name}`", field.name);
        let _ = compile_take_value(c, &field.value, &dest, false)?;
        if !matches!(field.value.kind, ExprKind::Nil) {
            let declared = def
                .iter()
                .find(|(fname, _)| fname == &field.name)
                .map(|(_, ty)| ty.as_str());
            if declared.and_then(box_annotation).is_some() {
                c.set_line(field.span);
                c.em(Instr::MakeBox);
            }
        }
        truncate_if_expansion(c, &field.value);
        c.set_line(field.span);
        c.em(Instr::SetIndex);
    }
    Ok(())
}

/// Emits `NewTable` followed by keyed defaults for every declared field.
fn emit_struct_defaults(
    c: &mut Compiler,
    fields: &[(String, String)],
    depth: usize,
    span: Span,
    name: &str,
) -> LResult<()> {
    c.set_line(span);
    c.em(Instr::NewTable);
    for (fname, ty) in fields {
        c.set_line(span);
        let key = c.konst_str(fname);
        c.em(Instr::LoadConst(key));
        emit_struct_default_value(c, ty, depth, span)?;
        c.set_line(span);
        c.em(Instr::SetIndex);
    }
    let tag = c.konst_str(name);
    c.em(Instr::MarkStruct(tag));
    Ok(())
}

fn emit_struct_default_value(
    c: &mut Compiler,
    ty: &str,
    depth: usize,
    span: Span,
) -> LResult<()> {
    c.set_line(span);
    if ty == "bool" {
        let k = c.konst(&ConstKey::of(&Value::Bool(false)));
        c.em(Instr::LoadConst(k));
        return Ok(());
    }
    if vec_annotation(ty).is_some() {
        c.em(Instr::NewTable);
        return Ok(());
    }
    if box_annotation(ty).is_some() {
        c.em(Instr::LoadNil);
        return Ok(());
    }
    if let Some((element, length)) = array_annotation(ty) {
        c.em(Instr::NewTable);
        for index in 0..length {
            let key = c.konst(&ConstKey::Num((index as f64).to_bits()));
            c.em(Instr::LoadConst(key));
            emit_struct_default_value(c, element, depth + 1, span)?;
            c.set_line(span);
            c.em(Instr::SetIndex);
        }
        return Ok(());
    }
    if ty == "string"
        || ty == "table"
        || ty.starts_with("table<")
        || ty.starts_with('*')
        || ty == "nil"
    {
        c.em(Instr::LoadNil);
        return Ok(());
    }
    if depth < 8 {
        if let Some(fields) = c.struct_defs.get(ty).cloned() {
            return emit_struct_defaults(c, &fields, depth + 1, span, ty);
        }
    }
    let k = c.konst(&ConstKey::of(&Value::Number(0.0)));
    c.em(Instr::LoadConst(k));
    Ok(())
}

/// Compiles a nested function literal as a prototype + MakeClosure.
fn compile_nested_fn(c: &mut Compiler, lit: &FuncLit, name: &str) -> LResult<()> {
    let parent = c.top();
    let has_vararg = lit.has_vararg;
    let is_async = lit.is_async;
    c.frames.push(Frame::new(format!("{}::{}", c.frames[parent].name, name), Some(parent)));
    c.set_line(lit.span_body);
    {
        let f = c.cur();
        f.has_vararg = has_vararg;
        f.nparams = lit.params.len();
        f.is_async = is_async;
    }
    for p in &lit.params {
        c.declare(p);
    }
    compile_block(c, &lit.body)?;
    // implicit return nil
    c.em(Instr::LoadNil);
    c.em(Instr::RetFixed(1));
    let frame = c.frames.pop().expect("popped the child frame");
    let compiled = Rc::new(Compiled {
        proto: Rc::new(Chunk {
            name: Some(Arc::from(frame.name.as_str())),
            nparams: frame.nparams,
            has_vararg: frame.has_vararg,
            is_async: frame.is_async,
            nlocals: frame.slots.len(),
            consts: frame.consts.iter().map(|k| k.to_value()).collect(),
            code: frame.code,
            prototypes: frame.protos,
            slot_labels: frame.slots.iter().map(|s| s.name.clone()).collect(),
            upval_labels: frame.upval_labels.iter().map(|s| Arc::from(s.as_str())).collect(),
            decl_groups: frame.decl_groups,
            lines: frame.lines,
            local_names: frame.local_names,
            source_map: Some(c.source.clone()),
        }),
        upvals: frame.upvals,
        structs: Vec::new(),
    });
    let idx = c.frames.last().expect("parent frame").protos.len() as u16;
    c.frames.last_mut().expect("parent frame").protos.push(compiled);
    c.em(Instr::MakeClosure(idx));
    Ok(())
}
// ===== statements =====

fn compile_block(c: &mut Compiler, block: &Block) -> LResult<()> {
    c.push_shadow();
    for st in block {
        compile_stmt(c, st)?;
    }
    c.pop_shadow();
    Ok(())
}

fn compile_stmt(c: &mut Compiler, st: &St) -> LResult<()> {
    c.set_line(st.span);
    match &st.kind {
        StKind::Local { targets, values, annotations, .. } => {
            compile_local(c, targets, values, annotations, st.span)?
        }
        StKind::Struct { name, fields, peripheral, .. } => {
            let list = fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone()))
                .collect::<Vec<_>>();
            c.struct_list.push(crate::interp::StructDef {
                name: name.clone(),
                fields: list.clone(),
                peripheral: *peripheral,
            });
            c.struct_defs.insert(name.clone(), list);
        }
        StKind::Unsafe { body } => {
            c.set_line(st.span);
            c.em(Instr::UnsafeEnter);
            c.cur().unsafe_depth += 1;
            let result = compile_block(c, body);
            c.cur().unsafe_depth = c.cur().unsafe_depth.saturating_sub(1);
            result?;
            c.set_line(st.span);
            c.em(Instr::UnsafeLeave);
        }
        StKind::Assign { targets, values, .. } => compile_assign(c, targets, values, st.span)?,
        StKind::ExprSt(e) => {
            compile_expr(c, e)?;
            c.set_line(st.span);
            if is_expansion(e) {
                c.em(Instr::DropValues);
            } else {
                c.em(Instr::Pop);
            }
        }
        StKind::If { branches, else_block } => {
            let eb = else_block.as_ref().map(|b| b);
            compile_if(c, branches, eb)?
        }
        StKind::While { cond, body } => compile_while(c, cond, body)?,
        StKind::Repeat { body, cond } => compile_repeat(c, body, cond)?,
        StKind::ForNum { var, start, limit, step, body } => {
            let sp = step.as_ref();
            compile_for_num(c, var, start, limit, sp, body, st.span)?
        }
        StKind::ForIter { vars, values, body } => {
            compile_for_iter(c, vars, values, body, st.span)?
        }
        StKind::FuncSt { target, func, .. } => compile_func_st(c, target, func)?,
        // `#[no_std]` only changes the C backend's runtime selection; the
        // bytecode VM has no hosted runtime to disable.
        StKind::NoStd => {}
        StKind::Do { body } => compile_block(c, body)?,
        StKind::Return { values, span } => compile_return(c, values, *span)?,
        StKind::Break => {
            c.set_line(st.span);
            let loop_depth = c.cur().loop_depths.last().copied();
            if let Some(depth) = loop_depth {
                // A `break` can escape one or more `unsafe` blocks that were
                // opened inside the loop body.  Leave exactly those blocks
                // before jumping out, so the sandbox capability is not leaked.
                let loop_unsafe = c
                    .cur()
                    .loop_unsafe_depths
                    .last()
                    .copied()
                    .unwrap_or(0);
                let current_unsafe = c.cur().unsafe_depth;
                for _ in loop_unsafe..current_unsafe {
                    c.em(Instr::UnsafeLeave);
                }
                c.close_scopes_from(depth);
                c.set_line(st.span);
            }
            let at = c.em(Instr::Jump(0));
            // record under the innermost loop
            if let Some(loops) = c.cur().breaks.last_mut() {
                loops.push(at);
            } else {
                return Err(LusaError::new(
                    crate::error::E_BREAK,
                    Some(st.span),
                    "`break` outside of a loop".to_string(),
                ));
            }
        }
    }
    Ok(())
}

/// `local a, b = v1, v2`: values first, then fresh slots (Lua rule: the value
/// expressions see enclosing variables of the same name).
fn compile_local(
    c: &mut Compiler,
    targets: &[String],
    values: &[Expr],
    annotations: &[Option<String>],
    span: Span,
) -> LResult<()> {
    c.set_line(span);
    c.em(Instr::SegStart);
    // slots pre-declared before their value (`local function f()` recursion)
    let mut pre_slots: Vec<Option<u16>> = vec![None; targets.len()];
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = targets.get(i).map(|s| s.as_str()).unwrap_or("(value)");
        // `local function f() ... end` recursion: pre-bind the target so the
        // body captures its own slot (matches the interpreter's name lookup)
        if let ExprKind::Func(_) = &v.kind {
            if let Some(name) = targets.get(i) {
                let slot = c.declare(name);
                pre_slots[i] = Some(slot);
            }
        }
        let _ = compile_take_value(c, v, dest, false)?;
        if !matches!(v.kind, ExprKind::Nil) {
            if let Some(ann) = annotations.get(i).and_then(|a| a.as_deref()) {
                if box_annotation(ann).is_some() {
                    c.set_line(v.span);
                    c.em(Instr::MakeBox);
                }
            }
        }
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    // Locals without an initializer default to nil; struct and raw-pointer
    // annotations get their zero values (matching the interpreter's sandbox).
    for i in values.len()..targets.len() {
        match annotations.get(i).and_then(|a| a.as_deref()) {
            Some(ann) if ann.starts_with('*') => {
                let k = c.konst(&ConstKey::Num(0.0f64.to_bits()));
                let _ = c.em(Instr::LoadConst(k));
            }
            Some(ann) if vec_annotation(ann).is_some() => {
                c.em(Instr::NewTable);
            }
            Some(ann) if array_annotation(ann).is_some() => {
                let (element, length) = array_annotation(ann).unwrap();
                let zero = if element == "bool" {
                    ConstKey::Bool(false)
                } else if element == "string"
                    || element == "table"
                    || element.starts_with("table<")
                    || c.struct_defs.contains_key(element)
                {
                    ConstKey::Nil
                } else {
                    ConstKey::Num(0.0f64.to_bits())
                };
                c.em(Instr::NewTable);
                for index in 0..length {
                    let key = c.konst(&ConstKey::Num((index as f64).to_bits()));
                    let value = c.konst(&zero);
                    c.em(Instr::LoadConst(key));
                    c.em(Instr::LoadConst(value));
                    c.em(Instr::SetIndex);
                }
            }
            Some(ann) if c.struct_defs.contains_key(ann) => {
                let fields = c.struct_defs.get(ann).cloned().unwrap_or_default();
                emit_struct_defaults(c, &fields, 0, span, ann)?;
            }
            _ => {
                let _ = c.em(Instr::LoadNil);
            }
        }
    }
    // bind targets to slots — fresh for plain locals, reusing the pre-declared
    // slot when the value was a function (recursion capture) — map names to consts
    c.set_line(span);
    let mut slot_name: Vec<(u16, u16)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        let slot = match pre_slots[i] {
            Some(s) => s,
            None => c.declare(t),
        };
        let k = c.konst_str(t);
        slot_name.push((slot, k));
    }
    let group_idx = c.cur().decl_groups.len() as u16;
    c.cur().decl_groups.push(slot_name);
    c.set_line(span);
    c.em(Instr::DeclFromSeg(group_idx));
    c.em(Instr::SegEnd);
    Ok(())
}

fn compile_assign(c: &mut Compiler, targets: &[Expr], values: &[Expr], span: Span) -> LResult<()> {
    // Single target, single non-expanding value: the segment machinery exists
    // to account for multi-value groups, but here it only costs three extra
    // instructions. Emit a direct store instead (the hot loop assignment path).
    if targets.len() == 1 && values.len() == 1 && !is_expansion(&values[0]) {
        if let ExprKind::Un { op: UnOp::Deref, e } = &targets[0].kind {
            // `*ptr = value` - a simulated-memory write through the builtin.
            let callee = Expr {
                kind: ExprKind::Var(Box::new("box_write".to_string())),
                span: targets[0].span,
            };
            compile_expr(c, &callee)?;
            compile_expr(c, e)?;
            truncate_if_expansion(c, e);
            let dest = format!("`*ptr` (deref)");
            let _ = compile_take_value(c, &values[0], &dest, false)?;
            c.set_line(targets[0].span);
            c.em(Instr::CallOne(2, 0));
            c.em(Instr::DropValues);
            return Ok(());
        }
        if let ExprKind::Var(name) = &targets[0].kind {
            let dest = format!("`{}`", name);
            let _ = compile_take_value(c, &values[0], &dest, false)?;
            c.set_line(targets[0].span);
            match resolve(c, name) {
                Resolved::Local(slot) => {
                    c.em(Instr::SetLoc(slot));
                }
                Resolved::Upval(i) => {
                    c.em(Instr::SetUp(i));
                }
                Resolved::Global => {
                    let k = c.konst_str(name);
                    c.em(Instr::SetGlob(k));
                }
            }
            return Ok(());
        }
    }
    c.em(Instr::SegStart);
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = match targets.get(i).map(|t| &t.kind) {
            Some(ExprKind::Var(name)) => format!("`{}`", name),
            Some(ExprKind::Field { name, .. }) => format!("field `{}`", name),
            Some(ExprKind::Index { .. }) => "(indexed)".to_string(),
            _ => "(value)".to_string(),
        };
        let _ = compile_take_value(c, v, &dest, false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    for t in targets {
        c.set_line(t.span);
        match &t.kind {
            ExprKind::Var(name) => match resolve(c, name) {
                Resolved::Local(slot) => {
                    c.em(Instr::StoreLocSeg(slot));
                }
                Resolved::Upval(i) => {
                    c.em(Instr::StoreUpSeg(i));
                }
                Resolved::Global => {
                    let k = c.konst_str(name);
                    c.em(Instr::StoreGlobSeg(k));
                }
            },
            ExprKind::Field { obj, name } => {
                compile_expr(c, obj)?;
                let k = c.konst_str(name);
                c.em(Instr::StoreFieldSeg(k));
            }
            ExprKind::Index { obj, key } => {
                compile_expr(c, obj)?;
                compile_expr(c, key)?;
                c.em(Instr::StoreIndexFromSeg);
            }
            _ => {
                return Err(LusaError::new(
                    crate::error::E_PARSE,
                    Some(t.span),
                    "cannot assign to this expression",
                ))
            }
        }
    }
    c.set_line(span);
    c.em(Instr::SegEnd);
    Ok(())
}

fn compile_if(c: &mut Compiler, branches: &[(Expr, Block)], else_block: Option<&Block>) -> LResult<()> {
    let mut end_jumps: Vec<usize> = Vec::new();
    let n = branches.len();
    for (i, (cond, body)) in branches.iter().enumerate() {
        compile_expr(c, cond)?;
        truncate_if_expansion(c, cond);
        c.set_line(cond.span);
        let jump = c.em(Instr::JumpIfFalse(0));
        compile_block(c, body)?;
        let span = cond.span;
        if i + 1 < n || else_block.is_some() {
            c.set_line(cond.span);
            end_jumps.push(c.em(Instr::Jump(0)));
        }
        let _ = span;
        let next = c.pos();
        c.patch(jump, next);
    }
    if let Some(els) = else_block {
        compile_block(c, els)?;
    }
    let end = c.pos();
    for j in end_jumps {
        c.patch(j, end);
    }
    Ok(())
}

fn compile_while(c: &mut Compiler, cond: &Expr, body: &Block) -> LResult<()> {
    let start = c.pos();
    compile_expr(c, cond)?;
    truncate_if_expansion(c, cond);
    c.set_line(cond.span);
    let exit = c.em(Instr::JumpIfFalse(0));
    c.cur().breaks.push(Vec::new());
    let loop_depth = c.cur().scopes.len();
    let loop_unsafe = c.cur().unsafe_depth;
    c.cur().loop_depths.push(loop_depth);
    c.cur().loop_unsafe_depths.push(loop_unsafe);
    compile_block(c, body)?;
    c.cur().loop_depths.pop();
    c.cur().loop_unsafe_depths.pop();
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let back = start;
    c.set_line(cond.span);
    c.em(Instr::Jump(back as i32));
    let end = c.pos();
    for b in breaks {
        c.patch(b, end);
    }
    c.patch(exit, end);
    Ok(())
}

fn compile_repeat(c: &mut Compiler, body: &Block, cond: &Expr) -> LResult<()> {
    let start = c.pos();
    c.cur().breaks.push(Vec::new());
    let loop_depth = c.cur().scopes.len();
    let loop_unsafe = c.cur().unsafe_depth;
    c.cur().loop_depths.push(loop_depth);
    c.cur().loop_unsafe_depths.push(loop_unsafe);
    // repeat condition sees body locals
    c.push_shadow();
    for s in body {
        compile_stmt(c, s)?;
    }
    compile_expr(c, cond)?;
    c.pop_shadow();
    c.cur().loop_depths.pop();
    c.cur().loop_unsafe_depths.pop();
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    c.set_line(cond.span);
    let _ = c.em(Instr::JumpIfFalse(start as i32));
    let end = c.pos();
    for b in breaks {
        c.patch(b, end);
    }
    Ok(())
}

fn compile_for_num(
    c: &mut Compiler,
    var: &str,
    start: &Expr,
    limit: &Expr,
    step: Option<&Expr>,
    body: &Block,
    span: Span,
) -> LResult<()> {
    // start, limit, step are evaluated BEFORE the loop variable is declared
    compile_expr(c, start)?;
    truncate_if_expansion(c, start);
    compile_expr(c, limit)?;
    truncate_if_expansion(c, limit);
    match step {
        Some(s) => {
            compile_expr(c, s)?;
            truncate_if_expansion(c, s);
        }
        None => {
            let k = c.konst(&ConstKey::Num(1.0f64.to_bits()));
            c.em(Instr::LoadConst(k));
        }
    }
    // hidden slot layout: [var, limit, step]
    c.push_shadow();
    c.set_line(span);
    let var_slot = c.declare(var);
    let limit_slot = c.declare("(for limit)");
    let step_slot = c.declare("(for step)");
    c.cur().breaks.push(Vec::new());
    let loop_depth = c.cur().scopes.len();
    let loop_unsafe = c.cur().unsafe_depth;
    c.cur().loop_depths.push(loop_depth);
    c.cur().loop_unsafe_depths.push(loop_unsafe);
    c.set_line(span);
    let init = c.em(Instr::ForNumInit { var: var_slot, limit: limit_slot, step: step_slot, end: 0 });
    compile_block(c, body)?;
    c.cur().loop_depths.pop();
    c.cur().loop_unsafe_depths.pop();
    c.set_line(span);
    let step_at = c.em(Instr::ForNumStep { var: var_slot, limit: limit_slot, step: step_slot, top: (init + 1) as i32, end: 0 });
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let end = c.pos();
    c.patch(step_at, end);
    for b in breaks {
        c.patch(b, end);
    }
    c.pop_shadow();
    Ok(())
}

fn compile_for_iter(
    c: &mut Compiler,
    vars: &[String],
    values: &[Expr],
    body: &Block,
    span: Span,
) -> LResult<()> {
    c.em(Instr::SegStart);
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let dest = format!("(for control {})", i + 1);
        let _ = compile_take_value(c, v, &dest, false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    c.push_shadow();
    c.set_line(span);
    // hidden iterator slot first, then the loop variables
    let iter_slot = c.declare("(iter)");
    let mut first = iter_slot + 1;
    for v in vars {
        let s = c.declare(v);
        first = first.min(s);
    }
    let nvars = vars.len() as u16;
    c.cur().breaks.push(Vec::new());
    let loop_depth = c.cur().scopes.len();
    let loop_unsafe = c.cur().unsafe_depth;
    c.cur().loop_depths.push(loop_depth);
    c.cur().loop_unsafe_depths.push(loop_unsafe);
    c.set_line(span);
    let setup = c.em(Instr::ForInSetup { iter: iter_slot, first, nvars, end: 0 });
    compile_block(c, body)?;
    c.cur().loop_depths.pop();
    c.cur().loop_unsafe_depths.pop();
    c.set_line(span);
    let step_at = c.em(Instr::ForInStep { iter: iter_slot, first, nvars, top: (setup + 1) as i32, end: 0 });
    let breaks = c.cur().breaks.pop().expect("loop breaks pushed");
    let end = c.pos();
    c.patch(step_at, end);
    for b in breaks {
        c.patch(b, end);
    }
    c.pop_shadow();
    c.em(Instr::SegEnd);
    Ok(())
}

/// `function name(...) ... end` sugar — binds before the body so that plain
/// recursion works (`local function f` sees `f` inside its own body).
fn compile_func_st(c: &mut Compiler, target: &Expr, func: &FuncLit) -> LResult<()> {
    match &target.kind {
        ExprKind::Var(name) => {
            match resolve(c, name) {
                Resolved::Local(slot) => {
                    // slot exists already (reuse) or declare fresh then store
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    c.set_line(target.span);
                    let fall = c.em(Instr::StoreLocSeg(slot));
                    let _ = fall;
                }
                Resolved::Upval(i) => {
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    c.set_line(target.span);
                    c.em(Instr::StoreUpSeg(i));
                }
                Resolved::Global => {
                    compile_expr(c, &Expr { kind: ExprKind::Func(func.clone()), span: target.span })?;
                    c.set_line(target.span);
                    let k = c.konst_str(name);
                    c.em(Instr::StoreGlobSeg(k));
                }
            };
        }
        ExprKind::Field { .. } | ExprKind::Index { .. } => {
            compile_assign(
                c,
                &[target.clone()],
                &[Expr { kind: ExprKind::Func(func.clone()), span: target.span }],
                target.span,
            )?;
        }
        _ => {
            return Err(LusaError::new(
                crate::error::E_PARSE,
                Some(target.span),
                "cannot define a function here",
            ))
        }
    }
    Ok(())
}

fn compile_return(c: &mut Compiler, values: &[Expr], span: Span) -> LResult<()> {
    let n = values.len();
    for (i, v) in values.iter().enumerate() {
        let _ = compile_take_value(c, v, "(return)", false)?;
        if i + 1 < n {
            truncate_if_expansion(c, v);
        }
    }
    c.set_line(span);
    if n > 0 && is_expansion(&values[n - 1]) {
        c.em(Instr::RetExp((n - 1) as u16));
    } else {
        c.em(Instr::RetFixed(n as u16));
    }
    Ok(())
}
// ===== top-level entry =====

/// Compiles a whole source program into a runnable main chunk.
/// Top-level statements compile as the body of a chunk named "main".
pub fn compile_program(src: &str) -> LResult<Rc<Compiled>> {
    compile_program_with_base(src, None)
}

/// Compiles a source program, resolving imports relative to `base`.
pub fn compile_program_with_base(src: &str, base: Option<&Path>) -> LResult<Rc<Compiled>> {
    let source = Arc::new(SourceMap::new(src));
    let ast = crate::parser::parse_with_base(src, base)?;
    let c = &mut Compiler {
        frames: vec![Frame::new("main", None)],
        source,
        struct_defs: HashMap::new(),
        struct_list: Vec::new(),
    };
    {
        let f = c.cur();
        f.has_vararg = true; // top level may use `...` like the REPL needs
    }
    let r = compile_block(c, &ast);
    match r {
        Ok(()) => {}
        Err(e) => return Err(e),
    }
    // top level: implicit return with no values
    c.em(Instr::RetFixed(0));
    let f = c.frames.pop().expect("main frame");
    Ok(Rc::new(Compiled {
        proto: Rc::new(Chunk {
            name: Some(Arc::from(f.name.as_str())),
            nparams: f.nparams,
            has_vararg: f.has_vararg,
            is_async: false,
            nlocals: f.slots.len(),
            consts: f.consts.iter().map(|k| k.to_value()).collect(),
            code: f.code,
            prototypes: f.protos,
            slot_labels: f.slots.iter().map(|s| s.name.clone()).collect(),
            upval_labels: f.upval_labels.iter().map(|s| Arc::from(s.as_str())).collect(),
            decl_groups: f.decl_groups,
            lines: f.lines,
            local_names: f.local_names,
            source_map: Some(c.source.clone()),
        }),
        upvals: f.upvals,
        structs: std::mem::take(&mut c.struct_list),
    }))
}

// ===== tests =====

#[cfg(test)]
mod tests {
    use super::*;

    /// Registers bound by each `local` statement, in declaration order.
    fn declared_slots(compiled: &Compiled) -> Vec<u16> {
        compiled
            .proto
            .decl_groups
            .iter()
            .map(|group| group[0].0)
            .collect()
    }

    #[test]
    fn straight_line_locals_get_incremental_registers() {
        let compiled = compile_program("local a = 1\nlocal b = 2\nlocal c = 3\n").unwrap();
        assert_eq!(declared_slots(&compiled), vec![0, 1, 2]);
        assert_eq!(compiled.proto.nlocals, 3);
    }

    #[test]
    fn sibling_blocks_reuse_the_same_registers() {
        let compiled =
            compile_program("do local a = 1 local b = 2 end\ndo local c = 3 local d = 4 end\n")
                .unwrap();
        // Both blocks reuse R0/R1: the high-water mark is 2, not 4.
        assert_eq!(declared_slots(&compiled), vec![0, 1, 0, 1]);
        assert_eq!(compiled.proto.nlocals, 2);
    }

    #[test]
    fn nested_blocks_release_registers_lifo() {
        let compiled = compile_program(
            "do local a = 1 do local b = 2 end local c = 3 end\nlocal d = 4\n",
        )
        .unwrap();
        // a -> R0, b -> R1, c reuses R1 after the inner block, d -> R0 again.
        assert_eq!(declared_slots(&compiled), vec![0, 1, 1, 0]);
        assert_eq!(compiled.proto.nlocals, 2);
    }

    #[test]
    fn line_table_follows_the_expression_that_owns_the_instruction() {
        // The source spans a line break: the diagnostics must follow `t`, not
        // the `local` statement that owns it.
        let src = "local t = {1}\nlocal u =\n    t\nprint(t)\n";
        let compiled = compile_program(src).unwrap();
        let take = compiled
            .proto
            .code
            .iter()
            .position(|i| matches!(i, Instr::TakeLoc(..)))
            .expect("move of `t`");
        assert_eq!(compiled.proto.lines[take], 3);
        // `print(t)` compiles the argument as a deferred moving read.
        let read = compiled
            .proto
            .code
            .iter()
            .position(|i| matches!(i, Instr::ArgLoc(..)))
            .expect("read of `t`");
        assert_eq!(compiled.proto.lines[read], 4);
    }

    #[test]
    fn reused_register_keeps_the_binding_name_of_each_instruction() {
        let src = "do\n    local x = {1}\n    local y = x\n    print(x)\nend\n\
                   do\n    local z = 2\n    print(z)\nend\n";
        let compiled = compile_program(src).unwrap();
        let args: Vec<usize> = compiled
            .proto
            .code
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i, Instr::ArgLoc(..)))
            .map(|(pc, _)| pc)
            .collect();
        assert_eq!(args.len(), 2, "one moving read per print");
        assert_eq!(
            compiled.proto.local_names[args[0]].as_deref(),
            Some("x"),
            "first block must still name R0 as `x`"
        );
        assert_eq!(
            compiled.proto.local_names[args[1]].as_deref(),
            Some("z"),
            "second block names the reused R0 as `z`"
        );
        let slot = |pc: usize| match compiled.proto.code[pc] {
            Instr::ArgLoc(slot, _) => slot,
            _ => unreachable!(),
        };
        assert_eq!(slot(args[0]), slot(args[1]), "the register was reused");
    }

    #[test]
    fn reset_slot_is_emitted_only_for_captured_registers() {
        let plain = compile_program("do local a = 1 end do local b = 2 end\n").unwrap();
        assert!(!plain
            .proto
            .code
            .iter()
            .any(|i| matches!(i, Instr::ResetSlot(_))));

        let captured = compile_program(
            "local f\ndo local x = 1 f = function() return x end end\ndo local y = 2 print(y) end\n",
        )
        .unwrap();
        assert!(captured
            .proto
            .code
            .iter()
            .any(|i| matches!(i, Instr::ResetSlot(_))));
    }
}