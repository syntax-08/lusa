//! Bytecode VM: executes compiled `Chunk`s with a stack machine.
//!
//! Frames share one value stack; segments (`SegStart`/`SegEnd`) carve list
//! regions for multi-value bookkeeping; `last_ret` carries the expansion count
//! of the last value-producing operation (Lua's "first value" and trailing
//! expansion rules). Locals live in per-frame slot vecs of shared
//! `Rc<RefCell<SlotV>>` cells, so closures capture slots the same way the
//! interpreter captures scopes — including moved-value (E1001) checks, which
//! come free through the `SlotV` protocol and the ownership registry.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use crate::compiler::{Compiled, Instr};
use crate::error::{LResult, LusaError, Span, E_CALL, E_LEN, E_MOVED, E_RUNTIME};
use crate::interp::{Interp, MAX_CALL_DEPTH};
use crate::scope;
use crate::value::{format_number, span0, SlotV, TableObj, Value};

// ===== state =====

pub struct Vm {
    pub frames: Vec<VmFrame>,
    pub stack: Vec<Value>,
    pub segs: Vec<Seg>,
    /// number of values left by the last expansion (call / `...`)
    pub last_ret: usize,
}

pub struct Seg {
    start: usize,
    cursor: usize,
}

pub struct VmFrame {
    pub proto: Rc<Compiled>,
    pub slots: Vec<Rc<RefCell<SlotV>>>,
    /// instance-captured upvalues, in `proto.upvals` order
    pub upvals: Vec<Rc<RefCell<SlotV>>>,
    pub varargs: Vec<Value>,
    pub pc: usize,
    /// deferred call-argument takes pending finalization by the next call op
    pub(crate) undo: Vec<UndoRec>,
}

impl Vm {
    pub fn new() -> Vm {
        Vm {
            frames: Vec::new(),
            stack: Vec::new(),
            segs: Vec::new(),
            last_ret: 1,
        }
    }
}

enum StepOutcome {
    Continue,
    Returned(Vec<Value>),
}

// ===== driving =====

/// Calls a compiled function with the given captured upvalues and arguments,
/// running until it returns. Used both by direct VM calls (`Call` dispatch)
/// and by the interpreter's `call_value` (`FuncBody::Vm`).
pub fn invoke(
    interp: &mut Interp,
    compiled: &Rc<Compiled>,
    upvals: Vec<Rc<RefCell<SlotV>>>,
    args: Vec<Value>,
    span: Span,
) -> LResult<Vec<Value>> {
    let stack_base = interp.vm.stack.len();
    let seg_base = interp.vm.segs.len();
    let frames_base = interp.vm.frames.len();
    match invoke_inner(interp, compiled, upvals, args, span) {
        Ok(vals) => Ok(vals),
        Err(e) => {
            // unwind the frames that were entered here; intact cleanup
            interp.vm.frames.truncate(frames_base);
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
    upvals: Vec<Rc<RefCell<SlotV>>>,
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
    run_till_return(interp, interp.vm.frames.len() - 1)
}

/// Builds the callee frame: nil-filled slots, renamed params, varargs rest.
fn push_frame(
    interp: &mut Interp,
    compiled: &Rc<Compiled>,
    upvals: Vec<Rc<RefCell<SlotV>>>,
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
    let proto = compiled.proto.clone();
    let nparams = proto.nparams;
    let mut slots: Vec<Rc<RefCell<SlotV>>> = Vec::with_capacity(proto.nlocals);
    for _ in 0..proto.nlocals {
        slots.push(Rc::new(RefCell::new(SlotV::Set(Value::Nil))));
    }
    for (i, slot) in slots.iter().enumerate() {
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
        *slot.borrow_mut() = SlotV::Set(v);
    }
    let varargs: Vec<Value> = if proto.has_vararg {
        args.into_iter().skip(nparams).collect()
    } else {
        Vec::new()
    };
    interp.vm.frames.push(VmFrame {
        proto: compiled.clone(),
        slots,
        upvals,
        varargs,
        pc: 0,
        undo: Vec::new(),
    });
    Ok(())
}

/// Runs instructions until the frame at `base` returns, unwinding nested
/// frames along the way; returns the return values.
fn run_till_return(interp: &mut Interp, base: usize) -> LResult<Vec<Value>> {
    loop {
        if interp.vm.frames.len() <= base {
            let vals = Vec::new();
            return Ok(vals);
        }
        match step(interp)? {
            StepOutcome::Continue => {}
            StepOutcome::Returned(vals) => {
                interp.vm.frames.pop();
                if interp.vm.frames.len() == base {
                    interp.vm.last_ret = vals.len().max(1);
                    return Ok(vals);
                } else {
                    // a nested call's result feeds the caller on the stack
                    for v in vals.iter() {
                        interp.vm.stack.push(v.clone());
                    }
                    interp.vm.last_ret = vals.len();
                }
            }
        }
    }
}

/// Runs a compiled main program chunk to completion.
pub fn run_main(interp: &mut Interp, compiled: Rc<Compiled>) -> LResult<Value> {
    interp.vm.frames.clear();
    interp.vm.stack.clear();
    interp.vm.segs.clear();
    interp.vm.last_ret = 1;
    let args: Vec<Value> = Vec::new();
    invoke(interp, &compiled, Vec::new(), args, span0())?;
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
            eprintln!("{}  {:04} {:?}", indent, i, ins);
        }
        for p in &c.proto.prototypes {
            go(p, &format!("{}  ", indent));
        }
    }
    go(compiled, "");
}

/// Compiles and runs a program in bytecode mode. Mirrors `run_source`.
pub fn run_source_vm(src: &str) -> LResult<Vec<String>> {
    let joined = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(s, move || {
                let compiled = crate::compiler::compile_program(src)?;
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

fn step(interp: &mut Interp) -> LResult<StepOutcome> {
    // copy the cell handles we will touch, then run against `interp`
    let pc = interp.vm.frames.last().expect("a frame is stepping").pc;
    let ins: Instr = {
        let proto = interp.vm.frames.last().unwrap().proto.clone();
        let max = proto.proto.code.len();
        if pc >= max {
            // fell off the end of a chunk without an explicit return
            return Ok(StepOutcome::Returned(Vec::new()));
        }
        clone_instr(&proto.proto.code[pc])
    };
    interp.vm.frames.last_mut().unwrap().pc += 1;

    if std::env::var("LUSA_TRACE_VM").is_ok() {
        eprintln!(
            "[frame {} pc {:03}] {:?} stack={} segs={:?} last_ret={}",
            interp.vm.frames.len() - 1,
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

    instr_kind(&ins, interp)
}

/// derives the dispatch `Instr` (the enum is big; working with a clone in one shot)
fn clone_instr(ins: &Instr) -> Instr {
    // Instr derives Clone in this module (v1 simplicity)
    ins.clone()
}

/// the interpreter-loop body: executes one instruction with full access
fn instr_kind(ins: &Instr, interp: &mut Interp) -> LResult<StepOutcome> {
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
            let handle = slot_handle(interp, *slot as usize)?;
            let label = slot_label(interp, *slot as usize);
            slot_push(interp, &handle, label)?;
            StepOutcome::Continue
        }
        Instr::GetUp(idx) => {
            let handle = upval_handle(interp, *idx as usize)?;
            let label = upval_label(interp, *idx as usize);
            slot_push(interp, &handle, label)?;
            StepOutcome::Continue
        }
        Instr::TakeLoc(slot, labelk) => {
            let handle = slot_handle(interp, *slot as usize)?;
            let src_label = slot_label(interp, *slot as usize);
            let dest = const_str(interp, *labelk)?;
            take_slot_storage(interp, &handle, &src_label, &dest)?;
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
            let v = scope::read_slot(&[interp.globals.clone()], &name, span0())?;
            if matches!(v, Value::Table(_) | Value::Func(_)) {
                let vid = v.vid().expect("heap value has vid");
                if !interp.borrowed_by(vid).is_empty() {
                    return Err(err(
                        crate::error::E_BORROW_CONFLICT,
                        format!("cannot move out of `{}` because it is borrowed", name),
                    ));
                }
                let note = crate::value::MoveNote { to: dest.clone(), span: span0() };
                scope::set_slot(&[], &interp.globals, &name, SlotV::Moved(vec![note]));
                crate::value::with_reg(|r| r.rename(vid, dest, span0()));
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
            let v = scope::read_slot(&[interp.globals.clone()], &name, span0())?;
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
                crate::value::with_reg(|r| r.rename(vid, name.clone(), span0()));
            }
            scope::set_slot(&[], &interp.globals, &name, SlotV::Set(v));
            StepOutcome::Continue
        }
        Instr::SetFieldK(k) => {
            let v = pop1(interp);
            let obj = pop1(interp);
            let key = const_value(interp, *k)?;
            let t = expect_table(&obj)?;
            let _ = t.set(key, v);
            interp.vm.stack.push(obj);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::SetIndex => {
            // layout from compile_table: [table, key, value]
            let v = pop1(interp);
            let key = pop1(interp);
            let obj = pop1(interp);
            let t = expect_table(&obj)?;
            let _ = t.set(key, v);
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
            let vals: Vec<Value> = interp
                .vm
                .stack
                .drain(sp.saturating_sub(n)..)
                .collect();
            for v in vals {
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
            let c = v.base_value().deep_clone();
            interp.vm.stack.push(c);
            interp.vm.last_ret = 1;
            StepOutcome::Continue
        }
        Instr::BorrowErr => {
            return Err(err(
                E_RUNTIME,
                "borrow is not supported in bytecode mode yet; run this program with the interpreter",
            ))
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
            let cond = interp.vm.stack.last().cloned().unwrap_or(Value::Nil);
            if cond.truthy() {
                interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            } else {
                interp.vm.stack.pop();
            }
            StepOutcome::Continue
        }
        Instr::JumpIfFalseKeep(t) => {
            let cond = interp.vm.stack.last().cloned().unwrap_or(Value::Nil);
            if !cond.truthy() {
                interp.vm.frames.last_mut().unwrap().pc = *t as usize;
            } else {
                interp.vm.stack.pop();
            }
            StepOutcome::Continue
        }
        Instr::TruncateToOne => {
            let lr = interp.vm.last_ret;
            if lr == 0 {
                interp.vm.stack.push(Value::Nil);
                interp.vm.last_ret = 1;
            } else if lr > 1 {
                let sp = interp.vm.stack.len();
                interp.vm.stack.truncate(sp - (lr - 1));
                interp.vm.last_ret = 1;
            }
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
        _ => instr_ops(ins, interp)?,
    })
}

// ===== complex operations =====

fn instr_ops(ins: &Instr, interp: &mut Interp) -> LResult<StepOutcome> {
    Ok(match ins {
        // ===== deferred call-argument takes =====
        Instr::ArgLoc(slot, labelk) => {
            let handle = slot_handle(interp, *slot as usize)?;
            let src = slot_label(interp, *slot as usize);
            let dest = const_str(interp, *labelk)?;
            arg_push_slot(interp, &handle, &src, &dest)?;
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
            let (func, args) = call_pop(interp, *base, false);
            finalize_args(interp, &func, *nt)?;
            do_call_into(interp, func, args, span0())?
        }
        Instr::CallExp(base, nt) => {
            let (func, args) = call_pop(interp, *base, true);
            finalize_args(interp, &func, *nt)?;
            do_call_into(interp, func, args, span0())?
        }
        Instr::MethodCall(k, base, nt) => {
            // layout: [obj, a1..an] — the receiver sits UNDER the args
            let args = drain_args(interp, *base, false);
            let obj = pop1(interp);
            let name = const_str(interp, *k)?;
            let key = Value::Str(Arc::from(name.as_str()));
            let f = crate::eval::read_field(&obj, &key, span0())?;
            finalize_args(interp, &f, *nt)?;
            let mut full = vec![obj.clone()];
            full.extend(args);
            do_call_into(interp, f, full, span0())?
        }
        Instr::MethodCallExp(k, base, nt) => {
            let args = drain_args(interp, *base, true);
            let obj = pop1(interp);
            let name = const_str(interp, *k)?;
            let key = Value::Str(Arc::from(name.as_str()));
            let f = crate::eval::read_field(&obj, &key, span0())?;
            finalize_args(interp, &f, *nt)?;
            let mut full = vec![obj.clone()];
            full.extend(args);
            do_call_into(interp, f, full, span0())?
        }
        Instr::RetFixed(n) => {
            let vals = drain_top(interp, *n);
            interp.vm.last_ret = vals.len();
            StepOutcome::Returned(vals)
        }
        Instr::RetExp(base) => {
            let count = *base + interp.vm.last_ret;
            let vals = drain_top(interp, count);
            interp.vm.last_ret = vals.len();
            StepOutcome::Returned(vals)
        }
        Instr::VarargLoad => {
            let varargs = interp.vm.frames.last().map(|f| f.varargs.clone()).unwrap_or_default();
            for v in &varargs {
                interp.vm.stack.push(v.clone());
            }
            interp.vm.last_ret = varargs.len();
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
        Instr::DeclFromSeg(bindings) => {
            let frame = interp.vm.frames.last().expect("frame");
            let mut targets: Vec<(Rc<RefCell<SlotV>>, Arc<str>)> = Vec::new();
            for (slot, namek) in bindings {
                let handle = frame.slots.get(*slot as usize).cloned().expect("slot ok");
                let name = const_str_from_frame(&frame.proto, *namek);
                targets.push((handle, name));
            }
            let _ = frame;
            for (handle, name) in targets {
                let v = seg_front_or_nil(interp)?;
                if let Some(vid) = v.vid() {
                    crate::value::with_reg(|r| r.rename(vid, name.to_string(), span0()));
                }
                *handle.borrow_mut() = SlotV::Set(v);
            }
            StepOutcome::Continue
        }
        Instr::StoreLocSeg(slot) => {
            let handle = slot_handle(interp, *slot as usize)?;
            let v = seg_front_or_nil(interp)?;
            if let Some(vid) = v.vid() {
                let label = slot_label(interp, *slot as usize);
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            *handle.borrow_mut() = SlotV::Set(v);
            StepOutcome::Continue
        }
        Instr::StoreUpSeg(idx) => {
            let handle = upval_handle(interp, *idx as usize)?;
            let v = seg_front_or_nil(interp)?;
            if let Some(vid) = v.vid() {
                let label = upval_label(interp, *idx as usize);
                crate::value::with_reg(|r| r.rename(vid, label.to_string(), span0()));
            }
            *handle.borrow_mut() = SlotV::Set(v);
            StepOutcome::Continue
        }
        Instr::StoreGlobSeg(k) => {
            let name = const_str(interp, *k)?;
            let v = seg_front_or_nil(interp)?;
            if let Some(vid) = v.vid() {
                crate::value::with_reg(|r| r.rename(vid, name.clone(), span0()));
            }
            scope::set_slot(&[], &interp.globals, &name, SlotV::Set(v));
            StepOutcome::Continue
        }
        Instr::StoreFieldSeg(k) => {
            let obj = pop1(interp);
            let v = seg_front_or_nil(interp)?;
            let key = const_value(interp, *k)?;
            let t = expect_table(&obj)?;
            let _ = t.set(key, v);
            StepOutcome::Continue
        }
        Instr::StoreIndexFromSeg => {
            let key = pop1(interp);
            let obj = pop1(interp);
            let v = seg_front_or_nil(interp)?;
            let t = expect_table(&obj)?;
            let _ = t.set(key, v);
            StepOutcome::Continue
        }
        Instr::MakeClosure(k) => {
            let frame = interp.vm.frames.last().expect("frame");
            let compiled = frame.proto.proto.prototypes.get(*k as usize).cloned().expect("proto index");
            let mut instance: Vec<Rc<RefCell<SlotV>>> = Vec::new();
            for def in &compiled.upvals {
                let handle = match def {
                    crate::compiler::UpDef::Local(slot) => {
                        frame.slots.get(*slot as usize).cloned().expect("captured slot")
                    }
                    crate::compiler::UpDef::Up(idx) => {
                        frame.upvals.get(*idx as usize).cloned().expect("captured upval")
                    }
                };
                instance.push(handle);
            }
            let _ = frame;
            let vid = crate::value::with_reg(|r| r.alloc_vid());
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
            set_num_slot(interp, *var as usize, iv)?;
            set_num_slot(interp, *limit as usize, lv)?;
            set_num_slot(interp, *step as usize, sv)?;
            let in_range = if sv >= 0.0 { iv <= lv } else { iv >= lv };
            if !in_range {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForNumStep { var, limit, step, top, end } => {
            let cur = num_slot_call(interp, *var as usize)?;
            let lim = num_slot_call(interp, *limit as usize)?;
            let st = num_slot_call(interp, *step as usize)?;
            let next = cur + st;
            let go = if st >= 0.0 { next <= lim } else { next >= lim };
            if go {
                set_num_slot(interp, *var as usize, next)?;
                interp.vm.frames.last_mut().unwrap().pc = *top as usize;
            } else {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForInSetup { iter, first, nvars, end } => {
            let it = seg_front_or_nil(interp)?;
            let it_handle = slot_handle(interp, *iter as usize)?;
            *it_handle.borrow_mut() = SlotV::Set(it.clone());
            let row = advance_iter(interp, &it, *nvars)?;
            store_row(interp, *first as usize, *nvars, &row)?;
            if row.is_empty() {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            }
            StepOutcome::Continue
        }
        Instr::ForInStep { iter, first, nvars, top, end } => {
            let handle = slot_handle(interp, *iter as usize)?;
            let it: Value = match handle.borrow().as_value() {
                Some(v) => v.clone(),
                None => return Err(err(E_MOVED, "use of moved value `(iter)`")),
            };
            let row = advance_iter(interp, &it, *nvars)?;
            store_row(interp, *first as usize, *nvars, &row)?;
            if row.is_empty() {
                interp.vm.frames.last_mut().unwrap().pc = *end as usize;
            } else {
                interp.vm.frames.last_mut().unwrap().pc = *top as usize;
            }
            StepOutcome::Continue
        }
        other => {
            return Err(err(
                E_RUNTIME,
                format!("incomplete VM opcode path — please report: {:?}", other_type_name(other)),
            ))
        }
    })
}

fn other_type_name(_i: &Instr) -> &'static str {
    "unhandled"
}
// ===== runtime helpers =====

fn pop1(interp: &mut Interp) -> Value {
    interp.vm.stack.pop().unwrap_or(Value::Nil)
}

/// pops `n` values keeping their push order
fn drain_top(interp: &mut Interp, n: usize) -> Vec<Value> {
    let sp = interp.vm.stack.len();
    let take = n.min(sp);
    interp.vm.stack.drain(sp - take..).collect()
}

/// pops a call: `func` under `base + expansion` args; returns (func, args)
fn call_pop(interp: &mut Interp, base: usize, exp: bool) -> (Value, Vec<Value>) {
    let nargs = if exp { base + interp.vm.last_ret } else { base };
    let sp = interp.vm.stack.len();
    let take = nargs.min(sp.saturating_sub(1).max(0));
    let args: Vec<Value> = interp.vm.stack.drain(sp - take..).collect();
    let func = interp.vm.stack.pop().unwrap_or(Value::Nil);
    interp.vm.last_ret = 1;
    (func, args)
}

fn do_call_into(interp: &mut Interp, func: Value, mut args: Vec<Value>, span: Span) -> LResult<StepOutcome> {
    let results = match &func {
        Value::Func(f) => match &f.body {
            crate::value::FuncBody::Builtin(b) => b(interp, std::mem::take(&mut args), span)?,
            crate::value::FuncBody::Vm(compiled, upvals) => {
                crate::vm::invoke(interp, compiled, upvals.clone(), args, span)?
            }
            crate::value::FuncBody::Lusa(_) => interp.call_value(&func, args, span)?,
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
    for v in results.iter() {
        interp.vm.stack.push(v.clone());
    }
    interp.vm.last_ret = results.len();
    Ok(StepOutcome::Continue)
}

// ===== moving reads =====

/// Mirrors the interpreter's `move_err` presentation.
fn moved_out_err(label: &str) -> LusaError {
    LusaError::new(
        crate::error::E_MOVED,
        Some(span0()),
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
    let current = match &*handle.borrow() {
        SlotV::Set(v) => Some(v.clone()),
        SlotV::Moved(_) => None,
    };
    let v = match current {
        Some(v) => v,
        None => return Err(moved_out_err(&src_label.to_string())),
    };
    if matches!(v, Value::Table(_) | Value::Func(_)) {
        let vid = v.vid().expect("heap value has vid");
        if !interp.borrowed_by(vid).is_empty() {
            return Err(err(
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
        Some(SlotV::Moved(_)) => return Err(moved_out_err(src)),
        None => {
            interp.vm.stack.push(Value::Nil);
            interp.vm.last_ret = 1;
            return Ok(());
        }
    };
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
        _ => false,
    }
}

/// Pushes the value onto the stack and records its source for finalization.
/// The slot is NOT modified here — the move happens at the call boundary.
fn arg_push_slot(
    interp: &mut Interp,
    handle: &Rc<RefCell<SlotV>>,
    src: &Arc<str>,
    dest: &str,
) -> LResult<()> {
    let v = match &*handle.borrow() {
        SlotV::Set(v) => v.clone(),
        SlotV::Moved(_) => return Err(moved_out_err(&src.to_string())),
    };
    let rec = if matches!(v, Value::Table(_) | Value::Func(_) | Value::Iter(_)) {
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
    let v = scope::read_slot(&[interp.globals.clone()], name, span0())?;
    let rec = if matches!(v, Value::Table(_) | Value::Func(_) | Value::Iter(_)) {
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
    let v = match t.get_slot(key) {
        Some(SlotV::Set(v)) => Some(v),
        Some(SlotV::Moved(_)) => return Err(moved_out_err(src)),
        None => None,
    };
    let rec = match &v {
        Some(vv) if matches!(vv, Value::Table(_) | Value::Func(_) | Value::Iter(_)) => {
            UndoRec::Field {
                table: t.clone(),
                key: key.clone(),
                vid: vv.vid(),
                dest: dest.to_string(),
                value: vv.clone(),
            }
        }
        _ => UndoRec::Done,
    };
    interp.vm.stack.push(v.unwrap_or(Value::Nil));
    interp.vm.last_ret = 1;
    interp.vm.frames.last_mut().expect("frame").undo.push(rec);
    Ok(())
}

/// Finalizes the journaled argument takes for a call: builtins rollback
/// (their arguments are non-owning plain reads in the interpreter), user
/// functions commit the moves at the boundary.
fn finalize_args(interp: &mut Interp, func: &Value, n_takes: usize) -> LResult<()> {
    if n_takes == 0 {
        return Ok(());
    }
    let is_builtin = matches!(
        func,
        Value::Func(f) if matches!(f.body, crate::value::FuncBody::Builtin(_))
    );
    let undo_len = interp.vm.frames.last().expect("frame").undo.len();
    let start = undo_len.saturating_sub(n_takes);
    let rows: Vec<UndoRec> = interp
        .vm
        .frames
        .last_mut()
        .expect("frame")
        .undo
        .drain(start..)
        .collect();
    if is_builtin {
        for rec in rows.into_iter().rev() {
            restore_arg(interp, rec)?;
        }
    } else {
        for rec in rows {
            commit_arg(interp, rec)?;
        }
    }
    Ok(())
}

/// Applies a deferred move into the recorded source slot. If the slot was
/// reassigned or moved since (only possible through captured upvals), the
/// newer state wins and the pending move degrades to a rename.
fn commit_arg(interp: &mut Interp, rec: UndoRec) -> LResult<()> {
    match rec {
        UndoRec::Done => {}
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
            let cur = scope::read_slot(&[interp.globals.clone()], &name, span0())?;
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
            let cur = scope::read_slot(&[interp.globals.clone()], &name, span0())?;
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

fn drain_args(interp: &mut Interp, base: usize, exp: bool) -> Vec<Value> {
    let nargs = if exp { base + interp.vm.last_ret } else { base };
    let sp = interp.vm.stack.len();
    let take = nargs.min(sp);
    let args: Vec<Value> = interp.vm.stack.drain(sp - take..).collect();
    interp.vm.last_ret = 1;
    args
}

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

fn bin_arith(op: &str, f: fn(f64, f64) -> f64, interp: &mut Interp) -> LResult<StepOutcome> {
    let r = pop1(interp);
    let l = pop1(interp);
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
        _ => false,
    }
}

fn bin_cmp(kind: CmpKind, interp: &mut Interp) -> LResult<StepOutcome> {
    let r = pop1(interp);
    let l = pop1(interp);
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

fn const_value(interp: &mut Interp, k: u16) -> LResult<Value> {
    let proto = interp.vm.frames.last().expect("frame").proto.clone();
    proto
        .proto
        .consts
        .get(k as usize)
        .cloned()
        .ok_or_else(|| err(crate::error::E_RUNTIME, format!("internal: constant {} out of range", k)))
}

fn push_const(interp: &mut Interp, k: u16) -> LResult<()> {
    let v = const_value(interp, k)?;
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

fn const_str(interp: &mut Interp, k: u16) -> LResult<String> {
    match const_value(interp, k)? {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(err(
            crate::error::E_RUNTIME,
            format!("internal: constant {} is not a string ({})", k, other.type_name()),
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

fn slot_handle(interp: &mut Interp, slot: usize) -> LResult<Rc<RefCell<SlotV>>> {
    interp
        .vm
        .frames
        .last()
        .expect("frame")
        .slots
        .get(slot)
        .cloned()
        .ok_or_else(|| err(crate::error::E_RUNTIME, format!("internal: slot {} out of range", slot)))
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

fn slot_push(interp: &mut Interp, handle: &Rc<RefCell<SlotV>>, label: Arc<str>) -> LResult<()> {
    let v = match handle.borrow().as_value() {
        Some(v) => v.clone(),
        None => return Err(err(crate::error::E_MOVED, format!("use of moved value `{}`", label))),
    };
    interp.vm.stack.push(v);
    interp.vm.last_ret = 1;
    Ok(())
}

fn slot_label(interp: &mut Interp, slot: usize) -> Arc<str> {
    let proto = interp.vm.frames.last().expect("frame").proto.clone();
    proto
        .proto
        .slot_labels
        .get(slot)
        .cloned()
        .unwrap_or_else(|| Arc::from("(local)"))
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

fn set_num_slot(interp: &mut Interp, slot: usize, n: f64) -> LResult<()> {
    let handle = slot_handle(interp, slot)?;
    *handle.borrow_mut() = SlotV::Set(Value::Number(n));
    Ok(())
}

fn num_slot_call(interp: &mut Interp, slot: usize) -> LResult<f64> {
    let handle = slot_handle(interp, slot)?;
    let label = slot_label(interp, slot);
    let v = match handle.borrow().as_value() {
        Some(v) => v.clone(),
        None => {
            return Err(err(
                crate::error::E_MOVED,
                format!("use of moved value `{}`", label),
            ))
        }
    };
    match v.base_value() {
        Value::Number(n) => Ok(*n),
        other => Err(err(
            crate::error::E_ARITH,
            format!("for control variables must be numbers (got {})", other.type_name()),
        )),
    }
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

/// Writes row values into the consecutive loop variable slots; leftover
/// variables stay unassigned for parity with the interpreter.
fn store_row(interp: &mut Interp, first: usize, nvars: usize, row: &[Value]) -> LResult<()> {
    for (i, v) in row.iter().enumerate() {
        if i >= nvars {
            break;
        }
        let handle = slot_handle(interp, first + i)?;
        *handle.borrow_mut() = SlotV::Set(v.clone());
    }
    Ok(())
}