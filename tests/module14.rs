//! Module 14: compile-time borrow checking, explicit moves, and v1.0.0 stabilization.

use std::path::PathBuf;
use std::process::{Command, Output};

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn emit_error(source: &str) -> String {
    cbackend::emit_c(source).expect_err("emission must fail")
}

fn assert_error(source: &str, code: &str, needle: &str) -> String {
    let err = emit_error(source);
    assert!(
        err.contains(&format!("error[{code}]")),
        "expected {code}, got: {err}"
    );
    assert!(err.contains(needle), "expected `{needle}`, got: {err}");
    assert!(err.contains("--> lusa:"), "missing source span: {err}");
    err
}

fn native_exe(label: &str) -> PathBuf {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    out_dir.join(format!(
        "lusa-module14-{label}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ))
}

fn build_native(label: &str, source: &str) -> Option<PathBuf> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 14 test: {e}");
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

const VALID_SOURCE: &str = r#"
local t = { x = 1 }
do
    local a = borrow t
    local b = borrow t
    print("views", a.x, b.x)
end
local copy = clone t
copy.x = 9
print("copy", t.x, copy.x)
local moved = move t
print("moved", moved.x)
return 0
"#;

const VALID_EXPECTED: &str = "views 1 1\ncopy 1 9\nmoved 1\n";

#[test]
fn moves_borrows_and_clones_agree_in_all_engines() {
    let interp = lusa::run_source(VALID_SOURCE).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(VALID_SOURCE).expect("VM failed");
    assert_eq!(format!("{}\n", interp.join("\n")), VALID_EXPECTED);
    assert_eq!(format!("{}\n", vm.join("\n")), VALID_EXPECTED);

    let Some(exe) = build_native("valid", VALID_SOURCE) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), VALID_EXPECTED);
}

const RUNTIME_ERRORS: &str = r#"
local ok1, err1 = pcall(function()
    local t = { x = 1 }
    local a = borrow t
    local b = borrow t
    print("views", a.x, b.x)
end)
print("multi", ok1, err1)
local ok2, err2 = pcall(function()
    local t = { x = 1 }
    local a = borrow t
    local b = t
end)
print("move", ok2, string.sub(err2, 1, 5))
local ok3, err3 = pcall(function()
    local t = { x = 1 }
    local a = borrow t
    a.x = 2
end)
print("immutable", ok3, string.sub(err3, 1, 5))
local ok4, err4 = pcall(function()
    local t = { x = 1 }
    local u = move t
    print(t.x)
end)
print("after", ok4, string.sub(err4, 1, 5))
"#;

#[test]
fn runtime_ownership_failures_are_catchable_and_identical() {
    let expected = "views 1 1\n\
multi true nil\n\
move false E1002\n\
immutable false E1004\n\
after false E1001\n";
    let interp = lusa::run_source(RUNTIME_ERRORS).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(RUNTIME_ERRORS).expect("VM failed");
    assert_eq!(format!("{}\n", interp.join("\n")), expected);
    assert_eq!(format!("{}\n", vm.join("\n")), expected);
}

#[test]
fn static_checker_reports_use_after_move_and_borrow_conflicts() {
    let err = assert_error(
        r#"
local t = { 1 }
local u = t
print(t[1])
return 0
"#,
        "E1001",
        "use of moved-out value `t`",
    );
    assert!(err.contains("--> lusa:4:"), "span should point at the read: {err}");

    let err = assert_error(
        r#"
local t = { 1 }
local view = borrow t
local u = t
return 0
"#,
        "E1002",
        "cannot move out of `t` because it is borrowed",
    );
    assert!(err.contains("borrowed by `view`"), "missing borrow note: {err}");

    let err = assert_error(
        r#"
local t = { 1 }
local u = move t
print(t[1])
return 0
"#,
        "E1001",
        "use of moved-out value `t`",
    );
    assert!(err.contains("--> lusa:4:"), "span should point at the read: {err}");
}

#[test]
fn static_checker_rejects_shared_mutation_and_alias_writes() {
    let err = assert_error(
        r#"
local t = { x = 1 }
local view = borrow t
t.x = 2
return 0
"#,
        "E1002",
        "cannot write `t` while it is borrowed",
    );
    assert!(err.contains("borrowed by `view`"), "missing borrow note: {err}");

    let err = assert_error(
        r#"
local t = { x = 1 }
local view = borrow t
view.x = 2
return 0
"#,
        "E1004",
        "cannot write through immutable borrow `view`",
    );
    assert!(err.contains("--> lusa:4:"), "span should point at the write: {err}");
}

#[test]
fn mutable_method_borrows_exclude_reads_and_moves() {
    let borrowed = r#"
local user = { x = 1 }
function user:set(n: i32): i32
    self.x = n
    return n
end
local view = borrow user
user:set(2)
return 0
"#;
    assert_error(
        borrowed,
        "E1002",
        "cannot mutably borrow `user` while another borrow is active",
    );

    let read_arg = r#"
local user = { x = 1 }
function user:set(n: i32): i32
    self.x = n
    return n
end
user:set(user.x)
return 0
"#;
    assert_error(
        read_arg,
        "E1002",
        "cannot read `user` while it is mutably borrowed by a method call",
    );

}

#[test]
fn static_checker_accepts_multiple_immutable_borrows_and_clone() {
    let c = emit(VALID_SOURCE);
    // Borrows are a front-end analysis only; generated C has no borrow runtime.
    assert!(
        !c.contains("lusa_borrow") && !c.to_lowercase().contains("borrow"),
        "static borrow tracking must be zero-cost in generated C"
    );
    assert!(c.contains("lusa_table_clone("), "clone must lower to a deep copy");
}

#[test]
fn borrows_end_at_loop_break_in_the_vm() {
    let source = r#"
local t = { x = 1 }
while true do
    local view = borrow t
    break
end
local moved = move t
print(moved.x)
"#;
    let interp = lusa::run_source(source).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(source).expect("VM failed");
    assert_eq!(format!("{}\n", interp.join("\n")), "1\n");
    assert_eq!(format!("{}\n", vm.join("\n")), "1\n");
}

#[test]
fn unsafe_blocks_bypass_borrow_checks_in_every_engine() {
    let source = r#"
local t = { x = 1 }
local view = borrow t
unsafe do
    t.x = 2
end
print(view.x, t.x)
return 0
"#;
    let expected = "2 2\n";
    let interp = lusa::run_source(source).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(source).expect("VM failed");
    assert_eq!(format!("{}\n", interp.join("\n")), expected);
    assert_eq!(format!("{}\n", vm.join("\n")), expected);

    let Some(exe) = build_native("unsafe-borrow", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), expected);
}
