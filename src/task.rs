//! Cooperative async tasks (module 22).
//!
//! An `async function` call produces a suspended task.  The task owns a
//! private coroutine VM: `await` resumes it until it completes, and
//! `task.yield_now()` suspends it at the next instruction boundary.  There are
//! no OS threads; `task.run()` is the tiny freestanding executor used by the
//! dynamic engines and by programs that prefer an explicit queue.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_CALL, E_RUNTIME};
use crate::interp::Interp;
use crate::value::{CoroState, CoroutineObj, TaskObj, TaskState, Value};

type BF = Box<dyn Fn(&mut Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

fn one(v: Value) -> LResult<Vec<Value>> {
    Ok(vec![v])
}

fn task_from(value: &Value, span: Span) -> LResult<Rc<RefCell<TaskObj>>> {
    match value.base_value() {
        Value::Task(task) => Ok(Rc::clone(task)),
        other => Err(LusaError::new(
            E_CALL,
            Some(span),
            format!(
                "expected a task, got {} ({})",
                other.type_name(),
                other.fmt_value()
            ),
        )
        .help("an async call returns a task; pass that handle to `await`, `task.spawn`, or `task.is_done`")),
    }
}

/// Wraps an already-created coroutine object in a lazy task handle.
pub fn from_coroutine(
    coroutine: Rc<RefCell<CoroutineObj>>,
) -> Rc<RefCell<TaskObj>> {
    Rc::new(RefCell::new(TaskObj {
        id: NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed),
        state: TaskState::Pending,
        coroutine: Some(coroutine),
        result: Vec::new(),
        error: None,
        started: false,
        queued: false,
        args: Vec::new(),
    }))
}

/// Creates a task for a compiled function without running it.
pub fn spawn_compiled(
    compiled: Rc<crate::compiler::Compiled>,
    upvals: Rc<[Rc<RefCell<crate::value::SlotV>>]>,
    args: Vec<Value>,
    id: u64,
) -> Rc<RefCell<TaskObj>> {
    let coroutine = Rc::new(RefCell::new(CoroutineObj::new(
        id,
        Some((compiled, upvals.clone())),
        Some(upvals),
        Vec::new(),
    )));
    let task = from_coroutine(coroutine);
    task.borrow_mut().args = args;
    task
}

/// Creates a task from a function value.  Lusa interpreter functions are
/// compiled on demand, exactly like coroutine entries.
pub fn spawn_value(
    interp: &mut Interp,
    callee: &Value,
    args: Vec<Value>,
    span: Span,
) -> LResult<Rc<RefCell<TaskObj>>> {
    let coroutine = crate::coroutine::object_from_function(interp, callee, span)?;
    let task = from_coroutine(coroutine);
    task.borrow_mut().args = args;
    Ok(task)
}

/// True for functions declared with `async`.
pub fn is_async_value(value: &Value) -> bool {
    match value.base_value() {
        Value::Func(f) => match &f.body {
            crate::value::FuncBody::Lusa(lit) => lit.is_async,
            crate::value::FuncBody::Vm(compiled, _) => compiled.proto.is_async,
            crate::value::FuncBody::Builtin(_) => false,
        },
        _ => false,
    }
}

fn task_error(task: &TaskObj, span: Span) -> LusaError {
    LusaError::new(
        E_RUNTIME,
        Some(span),
        task.error
            .clone()
            .unwrap_or_else(|| "async task failed".to_string()),
    )
}

/// Drives a task until it yields or settles.
pub fn resume_task(
    interp: &mut Interp,
    task: &Rc<RefCell<TaskObj>>,
    span: Span,
) -> LResult<()> {
    let (state, coroutine) = {
        let task = task.borrow();
        (task.state, task.coroutine.clone())
    };
    match state {
        TaskState::Done | TaskState::Failed => return Ok(()),
        TaskState::Running => {
            return Err(LusaError::new(
                E_RUNTIME,
                Some(span),
                "cannot await a task that is already running".to_string(),
            )
            .help("a task cannot await itself; spawn a child task instead"));
        }
        TaskState::Pending => {}
    }
    let Some(coroutine) = coroutine else {
        let mut task = task.borrow_mut();
        task.state = TaskState::Failed;
        task.error = Some("task has no coroutine entry".to_string());
        return Ok(());
    };

    let args = {
        let mut task = task.borrow_mut();
        let first = !task.started;
        task.state = TaskState::Running;
        task.started = true;
        if first {
            std::mem::take(&mut task.args)
        } else {
            Vec::new()
        }
    };

    let results = crate::coroutine::resume(interp, &coroutine, args, span);
    let dead = coroutine.borrow().state == CoroState::Dead;
    let success = matches!(results.first(), Some(Value::Bool(true)));

    let mut task = task.borrow_mut();
    if dead {
        task.coroutine = None;
        if success {
            task.state = TaskState::Done;
            task.result = results.into_iter().skip(1).collect();
        } else {
            task.state = TaskState::Failed;
            task.error = Some(
                results
                    .get(1)
                    .map(|v| v.fmt_value())
                    .unwrap_or_else(|| "async task failed".to_string()),
            );
        }
    } else {
        task.state = TaskState::Pending;
    }
    Ok(())
}

/// Drives `task` to completion and returns every result value.
pub fn await_task(
    interp: &mut Interp,
    task: &Rc<RefCell<TaskObj>>,
    span: Span,
) -> LResult<Vec<Value>> {
    loop {
        let state = task.borrow().state;
        match state {
            TaskState::Done => {
                let result = task.borrow().result.clone();
                return Ok(result);
            }
            TaskState::Failed => {
                let task = task.borrow();
                return Err(task_error(&task, span));
            }
            TaskState::Running => {
                return Err(LusaError::new(
                    E_RUNTIME,
                    Some(span),
                    "cannot await a task that is already running".to_string(),
                ));
            }
            TaskState::Pending => resume_task(interp, task, span)?,
        }
    }
}

/// Awaits a task value and returns its first result (`nil` when the task has
/// no results).  Used by the compiler's `Await` instruction and by
/// `task.await`.
pub fn await_value(interp: &mut Interp, value: &Value, span: Span) -> LResult<Value> {
    let task = task_from(value, span)?;
    let mut results = await_task(interp, &task, span)?;
    if results.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(results.remove(0))
    }
}

fn arg<'a>(args: &'a [Value], index: usize, what: &str, span: Span) -> LResult<&'a Value> {
    args.get(index).ok_or_else(|| {
        LusaError::new(
            E_CALL,
            Some(span),
            format!("missing argument `{what}` for task.{what}"),
        )
    })
}

fn b_spawn(interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let value = arg(&args, 0, "spawn", span)?.clone();
    let task = match value.base_value() {
        Value::Task(task) => Rc::clone(task),
        Value::Func(_) => spawn_value(interp, &value, Vec::new(), span)?,
        other => {
            return Err(LusaError::new(
                E_CALL,
                Some(span),
                format!(
                    "task.spawn expects a task or function, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            )
            .help("write `task.spawn(worker())` for an async call, or `task.spawn(worker)` for a plain function"));
        }
    };
    {
        let mut slot = task.borrow_mut();
        if matches!(slot.state, TaskState::Pending) && !slot.queued {
            slot.queued = true;
            drop(slot);
            interp.task_queue.push(Rc::clone(&task));
        }
    }
    one(Value::Task(task))
}

fn b_run(interp: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let mut ran = 0usize;
    loop {
        let batch: Vec<Rc<RefCell<TaskObj>>> = std::mem::take(&mut interp.task_queue);
        if batch.is_empty() {
            break;
        }
        for task in batch {
            {
                let mut task = task.borrow_mut();
                task.queued = false;
            }
            let state = task.borrow().state;
            if matches!(state, TaskState::Done | TaskState::Failed) {
                continue;
            }
            await_task(interp, &task, span)?;
            ran += 1;
        }
    }
    one(Value::Number(ran as f64))
}

fn b_yield_now(interp: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if interp.active_coroutines.is_empty() {
        return Err(LusaError::new(
            E_RUNTIME,
            Some(span),
            "`task.yield_now` can only be used inside an async function or task".to_string(),
        )
        .help("declare the function with `async function`"));
    }
    // Direct calls are lowered to a suspension instruction by the compiler.
    // Reaching the builtin means the call could not be lowered (for example
    // through a dynamic alias); keep it cooperative by returning control.
    one(Value::Nil)
}

fn b_is_done(_interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let task = task_from(arg(&args, 0, "is_done", span)?, span)?;
    let task = task.borrow();
    one(Value::Bool(matches!(
        task.state,
        TaskState::Done | TaskState::Failed
    )))
}

fn b_await(interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let task = task_from(arg(&args, 0, "await", span)?, span)?;
    await_task(interp, &task, span)
}

fn b_status(_interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let task = task_from(arg(&args, 0, "status", span)?, span)?;
    let text = match task.borrow().state {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Done => "done",
        TaskState::Failed => "failed",
    };
    one(Value::Str(Arc::from(text)))
}

/// Public `task` module installed next to `coroutine` and friends.
pub fn module() -> Vec<(String, BF)> {
    vec![
        ("spawn".to_string(), Box::new(b_spawn) as BF),
        ("run".to_string(), Box::new(b_run) as BF),
        ("yield_now".to_string(), Box::new(b_yield_now) as BF),
        ("is_done".to_string(), Box::new(b_is_done) as BF),
        ("await".to_string(), Box::new(b_await) as BF),
        ("status".to_string(), Box::new(b_status) as BF),
    ]
}
