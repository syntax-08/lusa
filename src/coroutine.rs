//! Cooperative coroutines for the interpreter and bytecode VM.
//!
//! A coroutine owns a private bytecode VM. It is swapped into `Interp::vm`
//! only while running, so suspended registers, upvalues and pending
//! expression values survive between resumes. Functions written for the
//! tree-walking interpreter are compiled on demand when a coroutine calls
//! them, which keeps `yield` working through ordinary helper functions.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_CALL, E_RUNTIME};
use crate::interp::Interp;
use crate::value::{CoroState, CoroutineObj, FuncBody, SlotV, Value};
use crate::vm::{CoroDrive, Vm};

type BF = Box<dyn Fn(&mut Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

static NEXT_COROUTINE_ID: AtomicU64 = AtomicU64::new(1);

fn one(v: Value) -> LResult<Vec<Value>> {
    Ok(vec![v])
}

fn arg<'a>(args: &'a [Value], index: usize, what: &str, span: Span) -> LResult<&'a Value> {
    args.get(index).ok_or_else(|| {
        LusaError::new(
            E_CALL,
            Some(span),
            format!("missing argument `{what}` for coroutine.{what}"),
        )
    })
}

fn coroutine_from(value: &Value, span: Span) -> LResult<Rc<RefCell<CoroutineObj>>> {
    match value.base_value() {
        Value::Coroutine(co) => Ok(co.clone()),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!(
                "expected a coroutine, got {} ({})",
                other.type_name(),
                other.fmt_value()
            ),
        )),
    }
}

fn new_object(
    entry: Option<(Rc<crate::compiler::Compiled>, Rc<[Rc<RefCell<SlotV>>]>)>,
    upvals: Option<Rc<[Rc<RefCell<SlotV>>]>>,
    captures: Vec<(usize, crate::scope::ScopeRef, Arc<str>)>,
) -> Value {
    let id = NEXT_COROUTINE_ID.fetch_add(1, Ordering::Relaxed);
    Value::Coroutine(Rc::new(RefCell::new(CoroutineObj::new(
        id, entry, upvals, captures,
    ))))
}

fn sync_from_scopes(map: &crate::value::CaptureMap) {
    for (name, (scope, cell)) in map {
        let value = {
            let b = scope.borrow();
            match b.vars.get(name.as_str()) {
                Some(SlotV::Set(v)) => Some(v.clone()),
                _ => None,
            }
        };
        if let Some(value) = value {
            *cell.borrow_mut() = SlotV::Set(value);
        }
    }
}

fn sync_to_scopes(map: &crate::value::CaptureMap) {
    for (name, (scope, cell)) in map {
        let value = match &*cell.borrow() {
            SlotV::Set(v) => Some(v.clone()),
            SlotV::Moved(_) => None,
        };
        if let Some(value) = value {
            scope
                .borrow_mut()
                .vars
                .insert(name.clone(), SlotV::Set(value));
        }
    }
}

fn b_create(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let callee = arg(&args, 0, "create", span)?.clone();
    let func = match callee.base_value() {
        Value::Func(f) => f.clone(),
        other => {
            return Err(LusaError::new(
                E_CALL,
                Some(span),
                format!(
                    "coroutine.create needs a function, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
    };

    match &func.body {
        FuncBody::Vm(compiled, upvals) => Ok(vec![new_object(
            Some((compiled.clone(), upvals.clone())),
            Some(upvals.clone()),
            Vec::new(),
        )]),
        FuncBody::Lusa(lit) => {
            let structs: std::collections::HashMap<String, Vec<(String, String)>> = i
                .struct_defs
                .iter()
                .map(|(name, def)| (name.clone(), def.fields.clone()))
                .collect();
            let runtime =
                crate::compiler::compile_runtime_function(lit, &func.env, &structs, None)?;
            let upvals = runtime.upvals.clone();
            let captures = runtime.captures.clone();
            Ok(vec![new_object(
                Some((runtime.compiled, upvals.clone())),
                Some(upvals),
                captures,
            )])
        }
        FuncBody::Builtin(_) => Err(LusaError::new(
            E_CALL,
            Some(span),
            "builtin functions cannot be used as coroutine entries".to_string(),
        )),
    }
}

fn error_result(error: &LusaError) -> Vec<Value> {
    vec![
        Value::Bool(false),
        Value::Str(Arc::from(format!("{}: {}", error.code, error.msg).as_str())),
    ]
}

fn b_resume(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let target = coroutine_from(arg(&args, 0, "resume", span)?, span)?;
    let rest: Vec<Value> = args.into_iter().skip(1).collect();
    Ok(resume(i, &target, rest, span))
}

/// Resumes `co`, returning the Lua-style `(true, ...)` / `(false, error)`
/// result list. Runtime failures inside the coroutine become a false result,
/// never an error that escapes `resume`.
pub fn resume(
    i: &mut Interp,
    target: &Rc<RefCell<CoroutineObj>>,
    args: Vec<Value>,
    span: Span,
) -> Vec<Value> {
    {
        let co = target.borrow();
        match co.state {
            CoroState::Dead => {
                return vec![
                    Value::Bool(false),
                    Value::Str(Arc::from("E9999: cannot resume dead coroutine")),
                ]
            }
            CoroState::Running => {
                return vec![
                    Value::Bool(false),
                    Value::Str(Arc::from("E9999: cannot resume a running coroutine")),
                ]
            }
            CoroState::Suspended => {}
        }
    }

    sync_from_scopes(&target.borrow().capture_env);

    let (id, entry, resume_one) = {
        let mut co = target.borrow_mut();
        co.state = CoroState::Running;
        (co.id, co.entry.take(), co.yield_one)
    };

    let coro_vm = {
        let mut co = target.borrow_mut();
        std::mem::replace(&mut co.vm, Vm::new())
    };
    let host_vm = std::mem::replace(&mut i.vm, coro_vm);
    // Sandbox capability is part of the suspended execution state. A
    // coroutine that yielded inside `unsafe` resumes there, while the host
    // continues with its own (usually zero) depth.
    let host_unsafe = i.unsafe_depth;
    i.unsafe_depth = target.borrow().unsafe_depth;

    i.active_coroutines.push(id);
    i.coroutine_captures
        .push(target.borrow().capture_env.clone());
    let outcome: LResult<CoroDrive> = match entry {
        Some((compiled, upvals)) => crate::vm::coroutine_begin(i, compiled, upvals, args, span)
            .and_then(|()| crate::vm::drive_coroutine(i)),
        None => {
            crate::vm::coroutine_supply(i, args, resume_one);
            crate::vm::drive_coroutine(i)
        }
    };
    i.active_coroutines.pop();
    let capture_env = i.coroutine_captures.pop().unwrap_or_default();
    sync_to_scopes(&capture_env);

    enum Done {
        Yield(Vec<Value>, bool),
        Return(Vec<Value>),
        Fail(Vec<Value>),
    }

    // Collect results (and unwind on failure) while the coroutine VM is still
    // installed; only then put the host VM back.
    let done = match outcome {
        Ok(CoroDrive::Yielded(_, one)) => Done::Yield(crate::vm::coroutine_take_yielded(i), one),
        Ok(CoroDrive::Returned(n)) => Done::Return(crate::vm::coroutine_take_results(i, n)),
        Err(error) => {
            let message = error_result(&error);
            crate::vm::coroutine_reset(i);
            Done::Fail(message)
        }
    };

    let coro_unsafe = i.unsafe_depth;
    let coro_vm = std::mem::replace(&mut i.vm, host_vm);
    i.unsafe_depth = host_unsafe;
    {
        let mut co = target.borrow_mut();
        co.vm = coro_vm;
        co.unsafe_depth = coro_unsafe;
        match &done {
            Done::Yield(_, one) => {
                co.state = CoroState::Suspended;
                co.yield_one = *one;
                co.capture_env = capture_env;
            }
            Done::Return(_) | Done::Fail(_) => {
                co.state = CoroState::Dead;
                co.upvals = None;
                co.capture_env.clear();
            }
        }
    }

    match done {
        Done::Yield(mut values, _) => {
            let mut out = Vec::with_capacity(values.len() + 1);
            out.push(Value::Bool(true));
            out.append(&mut values);
            out
        }
        Done::Return(values) => {
            let mut out = Vec::with_capacity(values.len() + 1);
            out.push(Value::Bool(true));
            out.extend(values);
            out
        }
        Done::Fail(message) => message,
    }
}

fn b_yield(i: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let msg = if i.active_coroutines.is_empty() {
        "attempt to yield from outside a coroutine"
    } else {
        "cannot yield across this call boundary; call `coroutine.yield` directly"
    };
    Err(LusaError::new(E_RUNTIME, Some(span), msg.to_string()))
}

fn b_status(i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let target = coroutine_from(arg(&args, 0, "status", span)?, span)?;
    let (state, id) = {
        let co = target.borrow();
        (co.state, co.id)
    };
    let text = match state {
        CoroState::Dead => "dead",
        CoroState::Suspended => "suspended",
        CoroState::Running => {
            if i.active_coroutines.last().copied() == Some(id) {
                "running"
            } else {
                "normal"
            }
        }
    };
    one(Value::Str(Arc::from(text)))
}

fn b_close(_i: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let target = coroutine_from(arg(&args, 0, "close", span)?, span)?;
    let mut co = target.borrow_mut();
    if co.state == CoroState::Running {
        return Err(LusaError::new(
            E_RUNTIME,
            Some(span),
            "cannot close a running coroutine".to_string(),
        ));
    }
    sync_to_scopes(&co.capture_env);
    co.entry = None;
    co.upvals = None;
    co.capture_env.clear();
    co.state = CoroState::Dead;
    co.unsafe_depth = 0;
    let vm = std::mem::replace(&mut co.vm, Vm::new());
    drop(vm);
    drop(co);
    one(Value::Bool(true))
}

fn b_isyieldable(i: &mut Interp, _args: Vec<Value>, _span: Span) -> LResult<Vec<Value>> {
    one(Value::Bool(!i.active_coroutines.is_empty()))
}

/// Public coroutine module installed next to `math`, `string`, and friends.
pub fn module() -> Vec<(String, BF)> {
    vec![
        ("create".to_string(), Box::new(b_create) as BF),
        ("resume".to_string(), Box::new(b_resume) as BF),
        ("yield".to_string(), Box::new(b_yield) as BF),
        ("status".to_string(), Box::new(b_status) as BF),
        ("close".to_string(), Box::new(b_close) as BF),
        ("isyieldable".to_string(), Box::new(b_isyieldable) as BF),
    ]
}
