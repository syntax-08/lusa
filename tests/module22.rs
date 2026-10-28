//! Module 22: asynchronous concurrency with `async`, `await`, and cooperative
//! tasks.  The native backend lowers async functions to stackless state
//! machines; the interpreter and VM drive the same source with a lazy task
//! runtime.

use std::process::Command;

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn emit_err(source: &str) -> String {
    cbackend::emit_c(source).expect_err("C backend must reject the program")
}

fn both_engines(src: &str) -> (String, String) {
    let interp = lusa::run_source(src).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(src).expect("VM failed");
    (interp.join("\n"), vm.join("\n"))
}

/// Builds and runs a native program.  Returns `None` when the host has no
/// detectable C compiler so local developer runs still exercise emission and
/// the dynamic engines; CI always has one.
fn native_output(source: &str, tag: &str) -> Option<String> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 22 test `{tag}`: {e}");
            return None;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module22-{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });

    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    Some(stdout.trim_end_matches('\n').to_string())
}

fn assert_native(source: &str, tag: &str, expected: &str) {
    if let Some(stdout) = native_output(source, tag) {
        assert_eq!(stdout, expected, "native output for `{tag}`");
    }
}

const ASYNC_BASIC: &str = r#"
async function add(a: i32, b: i32): i32
    task.yield_now()
    return a + b
end

async fn alias(a: i32): i32
    return a + 1
end

async function nested(): i32
    local value: i32 = await add(20, 22)
    return value
end

local lazy = add(2, 3)
print("status:", task.status(lazy))
print("done before:", task.is_done(lazy))
print("awaited:", await lazy)
print("done after:", task.is_done(lazy))
print("nested:", await nested())

local spawned = task.spawn(add(5, 6))
print("run:", task.run())
print("spawned:", await spawned)
print("postfix:", add(7, 8).await)
print("alias:", alias(4)::await)
"#;

const ASYNC_BASIC_OUTPUT: &str = "status: pending
done before: false
awaited: 5
done after: true
nested: 42
run: 1
spawned: 11
postfix: 15
alias: 5";

const ASYNC_EXECUTOR: &str = r#"
async function job(n: i32): i32
    local i: i32 = 0
    while i < n do
        task.yield_now()
        i = i + 1
    end
    return n
end

local a = job(2)
local b = job(1)
print("lazy", task.is_done(a), task.is_done(b))
task.spawn(a)
task.spawn(b)
print("run", task.run())
print("done", task.is_done(a), task.is_done(b))
print("results", await a, await b)
print("again", task.run())
"#;

const ASYNC_EXECUTOR_OUTPUT: &str = "lazy false false
run 2
done true true
results 2 1
again 0";

const ASYNC_CHAIN: &str = r#"
async function countdown(n: i32): i32
    local i: i32 = 0
    while i < n do
        task.yield_now()
        i = i + 1
    end
    return n * 10
end

async function parent(): i32
    local first: i32 = await countdown(3)
    local second: i32 = await countdown(1)
    return first + second
end

local p = parent()
print("status", task.status(p))
print("done", task.is_done(p))
print("value", await p)
print("done again", task.is_done(p))
print("value again", await p)
print("method", task.await(countdown(2)))
print("postfix", countdown(4).await)
"#;

const ASYNC_CHAIN_OUTPUT: &str = "status pending
done false
value 40
done again true
value again 40
method 20
postfix 40";

#[test]
fn async_functions_lower_to_stackless_state_machines() {
    let c = emit(ASYNC_BASIC);

    for marker in [
        "typedef struct LusaTaskState_",
        "static int lusa_task_step_",
        "switch (base->state)",
        "base->awaiting",
        "LUSA_TASK_YIELD",
        "LUSA_TASK_WAIT",
        "lusa_task_new(",
        "lusa_task_spawn(",
        "lusa_task_drive(",
        "lusa_task_run(",
        "lusa_task_result_i32(",
    ] {
        assert!(c.contains(marker), "missing {marker:?}:\n{c}");
    }
    assert!(
        c.matches("switch (base->state)").count() >= 2,
        "each async function needs its own dispatch switch:\n{c}"
    );

    // The executor is cooperative C; no platform threads or synchronization
    // primitives may sneak into a native build.
    for banned in [
        "pthread_create",
        "CreateThread",
        "std::thread",
        "std::async",
        "<thread>",
        "<mutex>",
    ] {
        assert!(
            !c.contains(banned),
            "async lowering must stay cooperative, found {banned:?}:\n{c}"
        );
    }
}

#[test]
fn tasks_are_lazy_and_run_in_every_engine() {
    assert_native(ASYNC_BASIC, "basic", ASYNC_BASIC_OUTPUT);

    let (interp, vm) = both_engines(ASYNC_BASIC);
    assert_eq!(interp, ASYNC_BASIC_OUTPUT);
    assert_eq!(vm, interp);
}

#[test]
fn executor_polls_spawned_tasks_until_their_state_transitions_finish() {
    assert_native(ASYNC_EXECUTOR, "executor", ASYNC_EXECUTOR_OUTPUT);

    let (interp, vm) = both_engines(ASYNC_EXECUTOR);
    assert_eq!(interp, ASYNC_EXECUTOR_OUTPUT);
    assert_eq!(vm, interp);
}

#[test]
fn await_waits_for_child_tasks_and_repeated_awaits_reuse_the_result() {
    assert_native(ASYNC_CHAIN, "chain", ASYNC_CHAIN_OUTPUT);

    let (interp, vm) = both_engines(ASYNC_CHAIN);
    assert_eq!(interp, ASYNC_CHAIN_OUTPUT);
    assert_eq!(vm, interp);

    // A yielding parent must stay pending until its child completes.  If
    // `await` short-circuited it would report a partial or empty value here.
    assert!(ASYNC_CHAIN_OUTPUT.contains("value 40"));
    assert!(ASYNC_CHAIN_OUTPUT.contains("done again true"));
}

const NO_STD_ASYNC: &str = r#"
#[no_std]

local sink: i32 = 0

async function add(a: i32, b: i32): i32
    task.yield_now()
    return a + b
end

#[entry]
function _start(): void
    local t: task<i32> = add(20, 22)
    task.spawn(t)
    sink = task.run()
    local x: i32 = await t
    if x ~= 42 then
        sink = 99
    end
end
"#;

#[test]
fn freestanding_async_tasks_use_the_static_arena() {
    let c = emit(NO_STD_ASYNC);

    assert!(c.starts_with("/* lusa:no_std */"), "{c}");
    for marker in [
        "LUSA_TASK_ARENA_BYTES",
        "static unsigned char lusa_task_arena[LUSA_TASK_ARENA_BYTES];",
        "lusa_task_alloc(",
        "typedef struct LusaTaskState_",
        "static int lusa_task_step_",
        "switch (base->state)",
        "LUSA_TASK_YIELD",
        "LUSA_TASK_WAIT",
        "lusa_task_spawn(",
        "lusa_task_run(",
        "void _start(void) {",
    ] {
        assert!(c.contains(marker), "missing {marker:?}:\n{c}");
    }

    // Freestanding async must not pull in the hosted heap, the string
    // runtime, printing, status strings, or a second C entry point.
    for banned in [
        "int main(void)",
        "#include <stdlib.h>",
        "malloc(",
        "free(",
        "lusa_string_new",
        "lusa_str_",
        "lusa_print_",
        "lusa_task_status",
        "pthread_create",
    ] {
        assert!(
            !c.contains(banned),
            "freestanding async output must not contain {banned:?}:\n{c}"
        );
    }
}

#[test]
fn invalid_async_programs_report_typed_diagnostics() {
    let cases: &[(&str, &str, &str)] = &[
        (
            r#"
struct Point
    x: i32
    y: i32
end

async function make(): Point
    return Point { x = 1, y = 2 }
end
"#,
            "E0312",
            "native async functions cannot return a struct by value yet",
        ),
        (
            r#"
async function broken(): i32
    local values: [i32; 3] = [1, 2, 3]
    return values[0]
end
"#,
            "E0312",
            "fixed arrays cannot be locals in an `async function`",
        ),
        (
            r#"
function sync(): void
    task.yield_now()
end
"#,
            "E0312",
            "`task.yield_now` can only be used inside an `async function`",
        ),
        (
            r#"
#[no_std]

local sink: i32 = 0

async function f(): i32
    task.yield_now()
    return 1
end

#[entry]
function _start(): void
    local t: task<i32> = f()
    local text: string = task.status(t)
    sink = 1
end
"#,
            "E0312",
            "`task.status` is unavailable in `#[no_std]` programs",
        ),
        (
            r#"
local pending: task<[i32; 3]>
"#,
            "E0301",
            "native async task results must be scalar, string, table, or a pointer",
        ),
    ];

    for (source, code, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains(&format!("error[{code}]")), "diagnostic:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}
