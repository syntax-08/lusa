//! Module 12: protected calls, cooperative coroutines, and panic recovery.

use std::path::PathBuf;
use std::process::{Command, Output};

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn both_engines(src: &str) -> (String, String) {
    let interp = lusa::run_source(src).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(src).expect("VM failed");
    (interp.join("\n"), vm.join("\n"))
}

fn native_exe(label: &str) -> PathBuf {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    out_dir.join(format!(
        "lusa-module12-{label}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ))
}

fn build_native(label: &str, source: &str) -> Option<PathBuf> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 12 test: {e}");
            return None;
        }
    };
    let exe = native_exe(label);
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    Some(exe)
}

fn run_exe(exe: &PathBuf) -> Output {
    let output = Command::new(exe).output().expect("run native program");
    let _ = std::fs::remove_file(exe);
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

fn emit_error(source: &str, code: &str) {
    let err = cbackend::emit_c(source).expect_err("emission must fail");
    assert!(
        err.contains(&format!("error[{code}]")),
        "expected {code}, got: {err}"
    );
}

#[test]
fn native_pcall_reports_success_failure_and_assert_errors() {
    let source = r#"
function divide(a: i32, b: i32): i32
    return a / b
end

function boom(): i32
    local owned: string = "owned" .. "-value"
    error("boom")
    return 0
end

function require_positive(n: i32): i32
    assert(n > 0, "positive required")
    return n
end

local ok1, value1, err1 = pcall(divide, 10, 0)
print("div", ok1, value1, err1)

local ok2, err2 = pcall(boom)
print("boom", ok2, err2)

local ok3, err3 = pcall(require_positive, -1)
print("assert", ok3, err3)

local ok4, value4, err4 = pcall(divide, 10, 2)
print("ok", ok4, value4)

local ok5, value5, err5 = pcall(divide, 9, 3)
print("ok2", ok5, value5)
return 0
"#;
    let Some(exe) = build_native("pcall-values", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(
        stdout(&output),
        "div false 0 E2001: division by zero\n\
         boom false E9000: boom\n\
         assert false E9100: positive required\n\
         ok true 5\n\
         ok2 true 3\n"
    );
}

#[test]
fn native_pcall_catches_bounds_and_survives_repeated_failures() {
    let source = r#"
function out(): i32
    local values = { 1, 2, 3 }
    return values[8]
end

local i: i32 = 0
while i < 3 do
    local ok, value, err = pcall(out)
    print("bounds", i, ok, value, err)
    i = i + 1
end

local ok, value, err = pcall(out)
print("after", ok)
return 0
"#;
    let Some(exe) = build_native("pcall-bounds", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(
        stdout(&output),
        "bounds 0 false 0 E2006: table index out of bounds\n\
         bounds 1 false 0 E2006: table index out of bounds\n\
         bounds 2 false 0 E2006: table index out of bounds\n\
         after false\n"
    );
}

#[test]
fn native_pcall_emits_allocation_marks_and_raii_drops() {
    let source = r#"
function leaky(): i32
    unsafe do
        local buffer: *mut u32 = unsafe_alloc(128)
        error("after alloc")
        print(buffer)
    end
    return 0
end

local ok, value, err = pcall(leaky)
print("alloc", ok, value, err)
return 0
"#;
    let c = emit(source);
    assert!(c.contains("lusa_alloc_mark()"));
    assert!(c.contains("lusa_release_allocs_since("));
    assert!(c.contains("lusa_error_clear();"));
    assert!(c.contains("lusa_error_string()"));
    assert!(c.contains("lusa_unsafe_alloc("));

    let Some(exe) = build_native("pcall-alloc", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), "alloc false 0 E9000: after alloc\n");
}

#[test]
fn uncaught_native_errors_report_to_stderr_and_exit_one() {
    let source = r#"
function boom(): void
    error("uncaught")
end

print("before")
boom()
print("after")
return 0
"#;
    let Some(exe) = build_native("uncaught", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(
        !output.status.success(),
        "native program unexpectedly passed"
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout(&output), "before\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lusa runtime: E9000: uncaught"),
        "stderr: {stderr}"
    );
}

#[test]
fn native_coroutines_yield_resume_close_and_report_errors() {
    let source = r#"
function helper(): i32
    print("helper-yieldable", coroutine.isyieldable())
    return coroutine.yield(11)
end

function worker(): i32
    local label: string = "owned" .. "-value"
    print("worker-label", label)
    local value: i32 = helper()
    print("helper-return", value)
    return value + 1
end

local co = coroutine.create(worker)
print("main-yieldable", coroutine.isyieldable())
local s0: string = coroutine.status(co)
print("status0", s0)

local ok1, value1 = coroutine.resume(co, 5)
print("resume1", ok1, value1)
local ok2, value2 = coroutine.resume(co, 9)
print("resume2", ok2, value2)

function keeper(): i32
    local payload: string = "x" .. "y"
    print("keeper-start", payload)
    coroutine.yield(21)
    print("keeper-should-not-run", payload)
    return 0
end

local k = coroutine.create(keeper)
local kok1, kv1 = coroutine.resume(k, 0)
print("keeper-resume", kok1, kv1)
local ks1: string = coroutine.status(k)
print("keeper-status1", ks1)
print("keeper-close", coroutine.close(k))
local ks2: string = coroutine.status(k)
print("keeper-status2", ks2)
local kok2, kv2 = coroutine.resume(k, 0)
print("keeper-resume2", kok2)
print("worker-close-dead", coroutine.close(co))

function idle(): i32
    return 0
end

local idle_co = coroutine.create(idle)
print("idle-close", coroutine.close(idle_co))
local ids: string = coroutine.status(idle_co)
print("idle-status", ids)

function failer(): i32
    local owned: string = "will" .. "-drop"
    print("failer-start", owned)
    error("coroutine boom")
    return 1
end

local f = coroutine.create(failer)
local fok, fvalue, ferr = coroutine.resume(f, 0)
print("failer-result", fok, ferr)
local fs: string = coroutine.status(f)
print("failer-status", fs)
return 0
"#;
    let Some(exe) = build_native("coroutine-basic", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(
        stdout(&output),
        "main-yieldable false\n\
         status0 suspended\n\
         worker-label owned-value\n\
         helper-yieldable true\n\
         resume1 true 11\n\
         helper-return 9\n\
         resume2 true 10\n\
         keeper-start xy\n\
         keeper-resume true 21\n\
         keeper-status1 suspended\n\
         keeper-close true\n\
         keeper-status2 dead\n\
         keeper-resume2 false\n\
         worker-close-dead true\n\
         idle-close true\n\
         idle-status dead\n\
         failer-start will-drop\n\
         failer-result false E9000: coroutine boom\n\
         failer-status dead\n"
    );
}

#[test]
fn native_coroutines_support_nested_resumes_and_pcall_inside() {
    let source = r#"
function inner(): i32
    print("inner-start")
    coroutine.yield(1)
    return 2
end

function outer(): i32
    local ico = coroutine.create(inner)
    local ok1, value1 = coroutine.resume(ico, 0)
    print("outer-inner1", ok1, value1)
    local s: string = coroutine.status(ico)
    print("outer-inner-status", s)
    local ok2, value2 = coroutine.resume(ico, 0)
    print("outer-inner2", ok2, value2)
    return value2 + 10
end

local co = coroutine.create(outer)
local ok, value = coroutine.resume(co, 0)
print("outer-result", ok, value)
local s: string = coroutine.status(co)
print("outer-status", s)

function protected_fail(): i32
    error("inside coroutine")
    return 0
end

function protected_worker(): i32
    local pok, perr = pcall(protected_fail)
    print("inside-pcall", pok, perr)
    return 0
end

local p = coroutine.create(protected_worker)
local pok, pvalue = coroutine.resume(p, 0)
print("protected-resume", pok, pvalue)
return 0
"#;
    let Some(exe) = build_native("coroutine-nested", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(
        stdout(&output),
        "inner-start\n\
         outer-inner1 true 1\n\
         outer-inner-status suspended\n\
         outer-inner2 true 2\n\
         outer-result true 12\n\
         outer-status dead\n\
         inside-pcall false E9000: inside coroutine\n\
         protected-resume true 0\n"
    );
}

#[test]
fn native_coroutine_runtime_and_hidden_context_are_emitted() {
    let c = emit(
        "function worker(): i32\n\
             coroutine.yield(1)\n\
             return 2\n\
         end\n\
         local co = coroutine.create(worker)\n\
         local ok, value = coroutine.resume(co, 0)\n\
         print(ok, value)\n\
         return 0\n",
    );
    assert!(c.contains("typedef struct LusaCoro"));
    assert!(c.contains("static LusaCoro *lusa_current_coro"));
    assert!(c.contains("LusaCoro *lusa_co"));
    assert!(c.contains("lusa_coro_new((LusaCoroStep)"));
    assert!(c.contains("lusa_coro_resume("));
    assert!(c.contains("lusa_coro_yield(lusa_co,"));
    assert!(c.contains("lusa_coro_status(") || c.contains("lusa_coro_close("));
    assert!(c.contains("lusa_drop_coroutine(&"));
    assert!(c.contains("lusa_current_coro"));
}

#[test]
fn dynamic_engines_agree_on_coroutine_lifecycle() {
    let src = r#"
function worker(): i32
    local x: i32 = coroutine.yield(1)
    return x + 1
end

local co = coroutine.create(worker)
local s0: string = coroutine.status(co)
print("status0", s0)

local ok1, value1 = coroutine.resume(co, 10)
print("resume1", ok1, value1)
local s1: string = coroutine.status(co)
print("status1", s1)

local ok2, value2 = coroutine.resume(co, 41)
print("resume2", ok2, value2)
local s2: string = coroutine.status(co)
print("status2", s2)

local ok3, value3 = coroutine.resume(co, 0)
print("dead", ok3)

function cancellable(): i32
    coroutine.yield(7)
    return 0
end

local c = coroutine.create(cancellable)
local cok, cvalue = coroutine.resume(c, 0)
print("cancel-resume", cok, cvalue)
local cs1: string = coroutine.status(c)
print("cancel-status1", cs1)
print("cancel-close", coroutine.close(c))
local cs2: string = coroutine.status(c)
print("cancel-status2", cs2)
local cok2, cvalue2 = coroutine.resume(c, 0)
print("cancel-dead", cok2)
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, vm);
    assert_eq!(
        interp,
        "status0 suspended\n\
         resume1 true 1\n\
         status1 suspended\n\
         resume2 true 42\n\
         status2 dead\n\
         dead false\n\
         cancel-resume true 7\n\
         cancel-status1 suspended\n\
         cancel-close true\n\
         cancel-status2 dead\n\
         cancel-dead false"
    );
}

#[test]
fn coroutine_and_pcall_diagnostics_are_checked() {
    emit_error(
        "function worker(n: i32): i32\n\
             coroutine.yield(1)\n\
             return n\n\
         end\n\
         local co = coroutine.create(worker)\n\
         return 0\n",
        "E0319",
    );
    emit_error(
        "function worker(): string\n\
             coroutine.yield(1)\n\
             return \"no\"\n\
         end\n\
         local co = coroutine.create(worker)\n\
         return 0\n",
        "E0319",
    );
    emit_error(
        "function worker(): i32\n\
             coroutine.yield(1)\n\
             return 0\n\
         end\n\
         local co = coroutine.create(worker)\n\
         local ok = coroutine.resume(co, 0)\n\
         return 0\n",
        "E0317",
    );
    emit_error(
        "function worker(): i32\n\
             return 0\n\
         end\n\
         local ok, value = coroutine.resume(worker, 0)\n\
         return 0\n",
        "E0309",
    );
    emit_error(
        "function worker(): i32\n\
             return 0\n\
         end\n\
         coroutine.yield(1)\n\
         return 0\n",
        "E0319",
    );
    emit_error("local co = coroutine.create(worker)\nreturn 0\n", "E0310");
    emit_error(
        "function worker(): i32\n\
             return 0\n\
         end\n\
         local ok = pcall(worker)\n\
         return 0\n",
        "E0317",
    );
    emit_error(
        "function worker(a: i32): i32\n\
             return a\n\
         end\n\
         local ok, value, err = pcall(worker)\n\
         return 0\n",
        "E0311",
    );
}
