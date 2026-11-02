//! Cooperative async tasks and the event-driven executor (modules 22 and 28).
//!
//! An `async function` call produces a suspended task.  The task owns a
//! private coroutine VM.  Polling a task resumes that VM until it completes,
//! calls `task.yield_now()`, or parks on `task.wait(event)` /
//! `task.sleep(ticks)`.  There are no OS threads.
//!
//! `task.run()` is the executor.  It polls ready tasks in FIFO order, moves
//! parked tasks back to the ready queue once their event was woken or their
//! deadline passed, and idles when nothing is ready.  On the host there are no
//! interrupts, so idling advances the tick clock to the earliest deadline;
//! when only event waiters remain, `task.run()` returns and a later call
//! resumes them.  Native firmware sleeps with `wfi` instead (see the C runtime
//! in `compiler_bootstrap.lusa`).
//!
//! Awaiting inside an async function polls the child inline: when the child
//! yields or parks, the parent does too, so only top-level tasks ever sit in
//! the executor's queues.  A blocking `await` outside async code drives its
//! task inline and runs the executor while that task is parked.
//!
//! Wakes are stamped rather than consumed: `task.wake(event)` records the
//! next wake sequence number for `event`, and a task parked on `event` is
//! runnable when that stamp is newer than the one taken when its poll began.
//! A wake that lands between a task's readiness check and its park is never
//! lost, and every waiter of one event wakes.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::error::{LResult, LusaError, Span, E_CALL, E_DEADLOCK, E_RUNTIME};
use crate::interp::Interp;
use crate::value::{CoroState, CoroutineObj, TaskObj, TaskState, TaskWait, Value};

type BF = Box<dyn Fn(&mut Interp, Vec<Value>, Span) -> LResult<Vec<Value>>>;

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Number of wake events (`task.wake(0)` through `task.wake(31)`).
pub const EVENTS: u32 = 32;

/// Executor state shared by every task of one interpreter.
#[derive(Default)]
pub struct Executor {
    /// parked top-level tasks, in park order
    pub parked: Vec<Rc<RefCell<TaskObj>>>,
    /// tick clock advanced by `task.tick()` and by the host idle step
    pub ticks: u32,
    /// wake sequence counter
    pub wakes: u32,
    /// sequence number of the latest wake of each event
    pub signals: [u32; EVENTS as usize],
    /// `wakes` when the current top-level poll began
    pub poll_stamp: u32,
    /// park request left by the last `task.wait`/`task.sleep`
    pub park: Option<TaskWait>,
}

fn one(v: Value) -> LResult<Vec<Value>> {
    Ok(vec![v])
}

pub fn task_from(value: &Value, span: Span) -> LResult<Rc<RefCell<TaskObj>>> {
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
        parked: false,
        wait: None,
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

/// Resumes a task once: until it yields, parks, or settles.  A park leaves
/// the request in `task.wait`.
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
        task.wait = None;
        if first {
            std::mem::take(&mut task.args)
        } else {
            Vec::new()
        }
    };

    interp.executor.park = None;
    let results = crate::coroutine::resume(interp, &coroutine, args, span);
    let park = interp.executor.park.take();
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
        task.wait = park;
    }
    Ok(())
}

/// Polls a top-level task once with a fresh wake stamp.  A failed task
/// raises its error, as it always has under `task.run()` and `await`.
fn poll_top(interp: &mut Interp, task: &Rc<RefCell<TaskObj>>, span: Span) -> LResult<()> {
    let saved = interp.executor.poll_stamp;
    interp.executor.poll_stamp = interp.executor.wakes;
    let result = resume_task(interp, task, span);
    interp.executor.poll_stamp = saved;
    result?;
    let task = task.borrow();
    if task.state == TaskState::Failed {
        return Err(task_error(&task, span));
    }
    Ok(())
}

/// Polls an awaited child once on behalf of a parent task.  Returns the
/// child's first result when it has settled, or `None` while it is pending;
/// the parent then suspends with the child's park request, if any.
pub fn poll_child(
    interp: &mut Interp,
    task: &Rc<RefCell<TaskObj>>,
    span: Span,
) -> LResult<Option<Value>> {
    if task.borrow().state == TaskState::Pending {
        resume_task(interp, task, span)?;
    }
    let task = task.borrow();
    match task.state {
        TaskState::Done => Ok(Some(task.result.first().cloned().unwrap_or(Value::Nil))),
        TaskState::Failed => Err(task_error(&task, span)),
        TaskState::Running => Err(LusaError::new(
            E_RUNTIME,
            Some(span),
            "cannot await a task that is already running".to_string(),
        )
        .help("a task cannot await itself; spawn a child task instead")),
        TaskState::Pending => {
            interp.executor.park = task.wait;
            Ok(None)
        }
    }
}

fn wait_ready(executor: &Executor, wait: &TaskWait) -> bool {
    match *wait {
        TaskWait::Event { event, stamp } => {
            (executor.signals[event as usize].wrapping_sub(stamp) as i32) > 0
        }
        TaskWait::Until { deadline } => (executor.ticks.wrapping_sub(deadline) as i32) >= 0,
    }
}

/// Appends a pending task to the ready queue unless it is already queued or
/// parked.
fn push_ready(interp: &mut Interp, task: &Rc<RefCell<TaskObj>>) {
    let mut slot = task.borrow_mut();
    if slot.state == TaskState::Pending && !slot.queued && !slot.parked {
        slot.queued = true;
        drop(slot);
        interp.task_queue.push_back(Rc::clone(task));
    }
}

fn park(interp: &mut Interp, task: &Rc<RefCell<TaskObj>>) {
    let queued = {
        let mut slot = task.borrow_mut();
        if slot.parked {
            return;
        }
        slot.parked = true;
        std::mem::replace(&mut slot.queued, false)
    };
    if queued {
        interp.task_queue.retain(|other| !Rc::ptr_eq(other, task));
    }
    interp.executor.parked.push(Rc::clone(task));
}

/// Puts a task that was just polled back where it belongs: parked when it
/// waits, at the back of the ready queue when it yielded.
fn settle(interp: &mut Interp, task: &Rc<RefCell<TaskObj>>) {
    let (pending, waiting) = {
        let slot = task.borrow();
        (slot.state == TaskState::Pending, slot.wait.is_some())
    };
    if !pending {
        return;
    }
    if waiting {
        park(interp, task);
    } else {
        push_ready(interp, task);
    }
}

/// Moves every parked task whose condition holds to the ready queue, in park
/// order.  Event waiters that stay parked take the current wake sequence as
/// their new stamp, so stamps never fall far enough behind to wrap.
fn collect(interp: &mut Interp) {
    if interp.executor.parked.is_empty() {
        return;
    }
    let seen = interp.executor.wakes;
    let parked = std::mem::take(&mut interp.executor.parked);
    for task in parked {
        let ready = {
            let mut slot = task.borrow_mut();
            if slot.state != TaskState::Pending {
                slot.parked = false;
                slot.wait = None;
                continue;
            }
            let ready = match &slot.wait {
                Some(wait) => wait_ready(&interp.executor, wait),
                None => true,
            };
            if ready {
                slot.parked = false;
                slot.wait = None;
            } else if let Some(TaskWait::Event { stamp, .. }) = &mut slot.wait {
                *stamp = seen;
            }
            ready
        };
        if ready {
            push_ready(interp, &task);
        } else {
            interp.executor.parked.push(task);
        }
    }
}

/// Nothing is ready.  The host has no interrupts, so the only progress is
/// time: jump the clock to the earliest deadline.  Returns false when only
/// event waiters remain.
fn idle(interp: &mut Interp) -> bool {
    let now = interp.executor.ticks;
    let mut earliest: Option<u32> = None;
    for task in &interp.executor.parked {
        if let Some(TaskWait::Until { deadline }) = task.borrow().wait {
            let ahead = deadline.wrapping_sub(now);
            if earliest.map_or(true, |best| ahead < best) {
                earliest = Some(ahead);
            }
        }
    }
    match earliest {
        Some(ahead) => {
            if (ahead as i32) > 0 {
                interp.executor.ticks = now.wrapping_add(ahead);
            }
            true
        }
        None => false,
    }
}

fn pop_ready(interp: &mut Interp) -> Option<Rc<RefCell<TaskObj>>> {
    let task = interp.task_queue.pop_front()?;
    task.borrow_mut().queued = false;
    Some(task)
}

/// One executor step for a blocking `await`: collect, then poll one ready
/// task or idle.  Returns false when nothing can make progress.
fn executor_turn(interp: &mut Interp, span: Span) -> LResult<bool> {
    collect(interp);
    if let Some(task) = pop_ready(interp) {
        if task.borrow().state == TaskState::Pending {
            poll_top(interp, &task, span)?;
            settle(interp, &task);
        }
        return Ok(true);
    }
    if interp.executor.parked.is_empty() {
        return Ok(false);
    }
    Ok(idle(interp))
}

fn deadlock_error(task: &Rc<RefCell<TaskObj>>, span: Span) -> LusaError {
    let what = match task.borrow().wait {
        Some(TaskWait::Event { event, .. }) => format!("event {event}"),
        _ => "an event".to_string(),
    };
    LusaError::new(
        E_DEADLOCK,
        Some(span),
        format!("the awaited task is parked on {what} and nothing is left to wake it"),
    )
    .help("call `task.wake` for that event from another task, a simulated peripheral, or an interrupt handler")
}

/// Drives `task` to completion and returns every result value.  This is the
/// blocking `await` used outside async functions: yields are absorbed, and
/// while the task is parked the executor runs the other tasks and idles.
pub fn await_task(
    interp: &mut Interp,
    task: &Rc<RefCell<TaskObj>>,
    span: Span,
) -> LResult<Vec<Value>> {
    loop {
        let (state, parked) = {
            let slot = task.borrow();
            (slot.state, slot.parked)
        };
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
            TaskState::Pending if parked => {
                if !executor_turn(interp, span)? {
                    // Fail the task, as the native runtime does, so the next
                    // collect unlinks it from the parked list.
                    let error = deadlock_error(task, span);
                    {
                        let mut slot = task.borrow_mut();
                        slot.state = TaskState::Failed;
                        slot.error = Some(format!("{}: {}", error.code, error.msg));
                        slot.coroutine = None;
                    }
                    collect(interp);
                    return Err(error);
                }
            }
            TaskState::Pending => {
                poll_top(interp, task, span)?;
                if task.borrow().wait.is_some() {
                    park(interp, task);
                }
            }
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

/// Validates a `task.*` integer operand.
fn operand(value: &Value, what: &str, limit: u64, span: Span) -> LResult<u32> {
    let number = match value.base_value() {
        Value::Number(n) => *n,
        other => {
            return Err(LusaError::new(
                E_CALL,
                Some(span),
                format!(
                    "`task.{what}` expects an integer, got {} ({})",
                    other.type_name(),
                    other.fmt_value()
                ),
            ))
        }
    };
    if number.fract() != 0.0 || number < 0.0 || number >= limit as f64 {
        return Err(LusaError::new(
            E_CALL,
            Some(span),
            format!("`task.{what}` operand {number} is out of range (0 to {})", limit - 1),
        ));
    }
    Ok(number as u32)
}

fn event_operand(value: &Value, what: &str, span: Span) -> LResult<u32> {
    operand(value, what, EVENTS as u64, span)
        .map_err(|e| e.help("wake events are numbered 0 to 31; `lusa::hal` drivers use 24 to 31"))
}

/// Builds the park request for a direct `task.wait(event)` (`kind` 0) or
/// `task.sleep(ticks)` (`kind` 1) inside an async function.
pub fn park_request(interp: &Interp, kind: u8, value: &Value, span: Span) -> LResult<TaskWait> {
    if kind == 0 {
        let event = event_operand(value, "wait", span)?;
        Ok(TaskWait::Event {
            event,
            stamp: interp.executor.poll_stamp,
        })
    } else {
        let ticks = operand(value, "sleep", 1u64 << 31, span)?;
        Ok(TaskWait::Until {
            deadline: interp.executor.ticks.wrapping_add(ticks),
        })
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
    push_ready(interp, &task);
    one(Value::Task(task))
}

/// The executor: polls ready tasks until every task has settled or only
/// event waiters remain.  Returns the number of distinct tasks it polled for
/// the first time.
fn b_run(interp: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let mut ran = 0usize;
    loop {
        collect(interp);
        let Some(task) = pop_ready(interp) else {
            if interp.executor.parked.is_empty() || !idle(interp) {
                break;
            }
            continue;
        };
        let started = {
            let slot = task.borrow();
            if slot.state != TaskState::Pending {
                continue;
            }
            slot.started
        };
        if !started {
            ran += 1;
        }
        poll_top(interp, &task, span)?;
        settle(interp, &task);
    }
    one(Value::Number(ran as f64))
}

fn outside_async(what: &str, span: Span) -> LusaError {
    LusaError::new(
        E_RUNTIME,
        Some(span),
        format!("`task.{what}` can only be used inside an async function or task"),
    )
    .help("declare the function with `async function`")
}

fn b_yield_now(interp: &mut Interp, _args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if interp.active_coroutines.is_empty() {
        return Err(outside_async("yield_now", span));
    }
    // Direct calls are lowered to a suspension instruction by the compiler.
    // Reaching the builtin means the call could not be lowered (for example
    // through a dynamic alias); keep it cooperative by returning control.
    one(Value::Nil)
}

/// `task.wait` and `task.sleep` suspend the caller, so only a direct call
/// inside an async function (lowered to a park instruction) can run them.
fn suspend_builtin(what: &'static str) -> BF {
    Box::new(move |interp: &mut Interp, _args: Vec<Value>, span: Span| {
        if interp.active_coroutines.is_empty() {
            return Err(outside_async(what, span));
        }
        Err(LusaError::new(
            E_RUNTIME,
            Some(span),
            format!("`task.{what}` must be called directly as `task.{what}(...)`"),
        ))
    })
}

/// `task.wake(event)`: interrupt-safe; every task parked on `event` becomes
/// runnable at the executor's next pass.
fn b_wake(interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    let event = event_operand(arg(&args, 0, "wake", span)?, "wake", span)?;
    let executor = &mut interp.executor;
    executor.wakes = executor.wakes.wrapping_add(1);
    executor.signals[event as usize] = executor.wakes;
    Ok(Vec::new())
}

/// `task.tick()`: advances the executor clock by one tick (called from the
/// timer interrupt handler on hardware).
fn b_tick(interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if !args.is_empty() {
        return Err(LusaError::new(E_CALL, Some(span), "`task.tick` takes no arguments"));
    }
    interp.executor.ticks = interp.executor.ticks.wrapping_add(1);
    Ok(Vec::new())
}

fn b_now(interp: &mut Interp, args: Vec<Value>, span: Span) -> LResult<Vec<Value>> {
    if !args.is_empty() {
        return Err(LusaError::new(E_CALL, Some(span), "`task.now` takes no arguments"));
    }
    one(Value::Number(interp.executor.ticks as f64))
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
    let task = task.borrow();
    let text = match task.state {
        TaskState::Pending if task.wait.is_some() => "parked",
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
        ("wait".to_string(), suspend_builtin("wait")),
        ("sleep".to_string(), suspend_builtin("sleep")),
        ("wake".to_string(), Box::new(b_wake) as BF),
        ("tick".to_string(), Box::new(b_tick) as BF),
        ("now".to_string(), Box::new(b_now) as BF),
        ("is_done".to_string(), Box::new(b_is_done) as BF),
        ("await".to_string(), Box::new(b_await) as BF),
        ("status".to_string(), Box::new(b_status) as BF),
    ]
}
