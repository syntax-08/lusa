//! Module 21: safe error handling with `Result<T, E>`, `Option<T>`, the `?`
//! operator, and freestanding sum types.

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
            eprintln!("skipping native Module 21 test `{tag}`: {e}");
            return None;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module21-{tag}-{}.exe", std::process::id()));
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

const ERROR_HANDLING: &str = r#"
function parse(n: i32): Result<i32,string>
    if n < 0 then
        return Err("negative")
    end
    return Ok(n * 2)
end

function double(n: i32): Result<i32,string>
    local value: i32 = parse(n)?
    return Ok(value + 1)
end

function maybe(n: i32): Option<i32>
    if n == 0 then
        return None
    end
    return Some(n)
end

function plus_ten(n: i32): Option<i32>
    local value: i32 = maybe(n)?
    return Some(value + 10)
end

function direct(): Result<i32,string>
    local value: i32 = Ok(41)?
    return Ok(value + 1)
end

function reuse(): Result<i32,string>
    local result: Result<i32,string> = parse(4)
    local value: i32 = result?
    if result:is_ok() then
        return Ok(value + 1)
    end
    return Err("moved")
end

print(double(3):unwrap())
print(double(-1):unwrap_err())
print(double(3):is_ok(), double(-1):is_err())
print(plus_ten(5):unwrap(), plus_ten(0):is_none())
print(Ok(5):unwrap_or(9), Err("bad"):unwrap_or(9))
print(Some(2):unwrap_or(0), None:unwrap_or(0))
print(Ok(7):expect("ok"), Some(8):expect("some"))
print(Ok(5))
print(Err("boom"))
print(Some("hi"))
print(None)
print(direct():unwrap())
print(reuse():unwrap())

match double(3) with
| Ok(value) => print("ok", value)
| Err(error) => print("err", error)
end

match double(-1) with
| Ok(value) => print("ok", value)
| Err(error) => print("err", error)
end

match maybe(0) with
| Some(value) => print("some", value)
| None => print("none")
end

match maybe(4) with
| Some(value) => print("some", value)
| None => print("none")
end
"#;

const ERROR_HANDLING_OUTPUT: &str = "7
negative
true true
15 true
5 9
2 0
7 8
Ok(5)
Err(boom)
Some(hi)
None
42
9
ok 7
err negative
none
some 4";

#[test]
fn result_and_option_run_in_every_engine() {
    assert_native(ERROR_HANDLING, "combinators", ERROR_HANDLING_OUTPUT);

    let source = emit(ERROR_HANDLING);
    assert!(source.contains("lusa_print_result_"), "{source}");
    assert!(source.contains("lusa_print_option_"), "{source}");

    let (interp, vm) = both_engines(ERROR_HANDLING);
    assert_eq!(interp, ERROR_HANDLING_OUTPUT);
    assert_eq!(vm, interp);
}

const PAYLOADS: &str = r#"
struct Point
    x: i32
    y: i32
end

function point(ok: bool): Result<Point,string>
    if ok then
        return Ok(Point { x = 3, y = 4 })
    end
    return Err("no point")
end

function use_point(ok: bool): Result<i32,string>
    local p: Point = point(ok)?
    return Ok(p.x + p.y)
end

print(use_point(true):unwrap())
print(use_point(false):unwrap_err())

function numbers(ok: bool): Result<Vec<i32>,string>
    if ok then
        local v: Vec<i32> = [7, 8, 9]
        return Ok(v)
    end
    return Err("empty")
end

function sum_numbers(ok: bool): Result<i32,string>
    local v: Vec<i32> = numbers(ok)?
    local total = 0
    local i = 0
    while i < v:len() do
        total = total + v[i]
        i = i + 1
    end
    return Ok(total)
end

print(sum_numbers(true):unwrap())
print(sum_numbers(false):unwrap_err())

function boxed(ok: bool): Result<Box<i32>,string>
    if ok then
        return Ok(Box(41))
    end
    return Err("no box")
end

function read_box(ok: bool): Result<i32,string>
    local b: Box<i32> = boxed(ok)?
    return Ok(*b + 1)
end

print(read_box(true):unwrap())
print(read_box(false):unwrap_err())

function nested(ok: bool): Result<Result<i32,string>,string>
    if ok then
        return Ok(Ok(7))
    end
    return Err("outer")
end

function use_nested(ok: bool): Result<i32,string>
    local inner: Result<i32,string> = nested(ok)?
    return inner
end

print(use_nested(true):unwrap())
print(use_nested(false):unwrap_err())
"#;

const PAYLOADS_OUTPUT: &str = "7
no point
24
empty
42
no box
7
outer";

#[test]
fn sum_payloads_cover_structs_vectors_boxes_and_nested_sums() {
    assert_native(PAYLOADS, "payloads", PAYLOADS_OUTPUT);

    let source = emit(PAYLOADS);
    assert!(source.contains("typedef struct LusaResult {"), "{source}");
    assert!(source.contains("typedef struct LusaOption {"), "{source}");
    assert!(source.contains("lusa_result_new_ok_result_"), "{source}");
    assert!(source.contains("lusa_clone_result_"), "{source}");
    assert!(source.contains("lusa_drop_result_"), "{source}");

    let (interp, vm) = both_engines(PAYLOADS);
    assert_eq!(interp, PAYLOADS_OUTPUT);
    assert_eq!(vm, interp);
}

const CONTROL_FLOW: &str = r#"
function parse(n: i32): Result<i32,string>
    if n < 0 then
        return Err("negative")
    end
    return Ok(n * 2)
end

function branch(n: i32): Result<i32,string>
    if n > 0 then
        local value = parse(n)?
        return Ok(value + 1)
    end
    return Err("n")
end

function parse_pos(n: i32): Result<i32,string>
    if n == 2 then
        return Err("two")
    end
    return Ok(n)
end

function loop_try(n: i32): Result<i32,string>
    local total = 0
    local i = 0
    while i < n do
        total = total + parse_pos(i)?
        i = i + 1
    end
    return Ok(total)
end

print(branch(4):unwrap())
print(branch(-1):unwrap_err())
print(loop_try(2):unwrap())
print(loop_try(5):unwrap_err())
"#;

const CONTROL_FLOW_OUTPUT: &str = "9
n
1
two";

#[test]
fn try_short_circuits_through_branches_and_loops() {
    assert_native(CONTROL_FLOW, "control-flow", CONTROL_FLOW_OUTPUT);

    let (interp, vm) = both_engines(CONTROL_FLOW);
    assert_eq!(interp, CONTROL_FLOW_OUTPUT);
    assert_eq!(vm, interp);
}

const OWNERSHIP: &str = r#"
struct Holder
    result: Result<i32,string>,
    option: Option<i32>
end

function passthrough(r: Result<i32,string>): Result<i32,string>
    local value: i32 = r?
    return Ok(value)
end

print(passthrough(Ok(6)):unwrap())
print(passthrough(Err("e")):unwrap_err())

local holder: Holder
holder.result = Ok(11)
holder.option = Some(4)
print(holder.result:unwrap(), holder.option:unwrap())
holder.result = Err("holder")
print(holder.result:unwrap_err())
holder.option = None
print(holder.option:is_none())

local sums: Vec<Result<i32,string>> = []
sums:push(Ok(1))
sums:push(Err("vec"))
print(sums:len(), sums[0]:unwrap(), sums[1]:unwrap_err())
"#;

const OWNERSHIP_OUTPUT: &str = "6
e
11 4
holder
true
2 1 vec";

#[test]
fn sum_parameters_struct_fields_and_vectors_preserve_ownership() {
    assert_native(OWNERSHIP, "ownership", OWNERSHIP_OUTPUT);

    let (interp, vm) = both_engines(OWNERSHIP);
    assert_eq!(interp, OWNERSHIP_OUTPUT);
    assert_eq!(vm, interp);
}

const MATCH_RETURN: &str = r#"
function parse(n: i32): Result<i32,string>
    if n < 0 then
        return Err("negative")
    end
    return Ok(n * 2)
end

function classify(r: Result<i32,string>): i32
    match r with
    | Ok(value) => return value
    | Err(_) => return -1
    end
end

function maybe(n: i32): Option<i32>
    if n < 0 then
        return None
    end
    return Some(n)
end

function classify_opt(o: Option<i32>): i32
    match o with
    | Some(value) => return value + 100
    | None => return 0
    end
end

print(classify(Ok(4)), classify(Err("x")))
print(classify_opt(Some(5)), classify_opt(None))
print(classify(parse(6)), classify(parse(-1)))
"#;

const MATCH_RETURN_OUTPUT: &str = "4 -1
105 0
12 -1";

#[test]
fn match_arms_bind_payloads_and_return_from_functions() {
    assert_native(MATCH_RETURN, "match-return", MATCH_RETURN_OUTPUT);

    let (interp, vm) = both_engines(MATCH_RETURN);
    assert_eq!(interp, MATCH_RETURN_OUTPUT);
    assert_eq!(vm, interp);
}

const NO_STD_SUMS: &str = r#"
#[no_std]

function compute(n: i32): Result<i32,i32>
    if n < 0 then
        return Err(n)
    end
    return Ok(n * 2)
end

function maybe(n: i32): Option<i32>
    if n < 0 then
        return None
    end
    return Some(n + 1)
end

function run_result(): Result<i32,i32>
    local value: i32 = compute(4)?
    return Ok(value)
end

function run_option(): Option<i32>
    local picked: i32 = maybe(3)?
    return Some(picked)
end

#[entry]
function main(): void
    local outcome: Result<i32,i32> = run_result()
    local choice: Option<i32> = run_option()
end
"#;

#[test]
fn freestanding_sums_use_the_arena_without_print_helpers() {
    let source = emit(NO_STD_SUMS);
    assert!(source.starts_with("/* lusa:no_std */"), "{source}");
    assert!(source.contains("typedef union LusaHeapArena {"), "{source}");
    assert!(source.contains("lusa_result_new_ok_result_i32_i32_"), "{source}");
    assert!(source.contains("lusa_result_take_err_result_i32_i32_"), "{source}");
    assert!(source.contains("lusa_option_none_option_i32_"), "{source}");
    assert!(source.contains("int main(void) {"), "{source}");
    for banned in [
        "#include <stdlib.h>",
        "malloc(",
        "free(",
        "lusa_print_result_",
        "lusa_print_option_",
    ] {
        assert!(
            !source.contains(banned),
            "freestanding sum output must not contain {banned:?}:\n{source}"
        );
    }

    assert_native(NO_STD_SUMS, "no-std", "");
}

#[test]
fn invalid_sum_programs_produce_typed_diagnostics() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "function f(): Result<i32,string>\n    return Ok()\nend\n",
            "E0311",
            "`Ok` takes exactly one value",
        ),
        (
            "function f(): Option<i32>\n    return None(1)\nend\n",
            "E0311",
            "`None` takes no arguments",
        ),
        (
            "function f(): Result<i32,string>\n    local r: Result<i32,string> = Ok(1)\n    local v = r:nope()\n    return Ok(v)\nend\n",
            "E0318",
            "unknown Result method",
        ),
        (
            "local value: i32 = Ok(1)?\n",
            "E0312",
            "`?` can only be used inside a function",
        ),
        (
            "function f(): void\n    local r: Result<i32,string> = Ok(1)\n    local v: i32 = r?\nend\n",
            "E0312",
            "requires a compatible Result or Option operand",
        ),
        (
            "function f(): Result<i32,string>\n    local v: i32 = 1?\n    return Ok(v)\nend\n",
            "E0312",
            "`?` requires a Result or Option value",
        ),
        (
            "function f(): Result<i32,string>\n    local o: Option<i32> = Some(1)\n    local v: i32 = o?\n    return Ok(v)\nend\n",
            "E0312",
            "requires a compatible Result or Option operand",
        ),
        (
            "function f(): Result<i32,string>\n    local r: Result<i32,i32> = Ok(1)\n    local v: i32 = r?\n    return Ok(v)\nend\n",
            "E0312",
            "propagates error type `i32` but the function returns `string`",
        ),
        (
            "function f(): Result<[i32; 3],string>\n    return Err(\"x\")\nend\n",
            "E0301",
            "fixed arrays cannot be Result success payloads",
        ),
        (
            "function f(): i32\n    return Ok(1):unwrap_or(\"no\")\nend\n",
            "E0308",
            "mismatched types in Result::unwrap_or fallback",
        ),
        (
            "function f(): Option<i32>\n    return None:unwrap()\nend\n",
            "E0308",
            "cannot infer the payload type",
        ),
    ];

    for (source, code, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains(&format!("error[{code}]")), "diagnostic:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

#[test]
fn wrong_variant_combinators_fail_at_runtime() {
    let cases = [
        (
            "print(Err(\"boom\"):unwrap())\n",
            "called `Result::unwrap()` on an `Err` value: boom",
        ),
        (
            "print(None:unwrap())\n",
            "called `Option::unwrap()` on a `None` value",
        ),
        (
            "print(Ok(1):unwrap_err())\n",
            "called `Result::unwrap_err()` on an `Ok` value: 1",
        ),
    ];

    for (source, needle) in cases {
        for err in [
            lusa::run_source(source).expect_err("interpreter must reject the wrong variant"),
            lusa::vm::run_source_vm(source).expect_err("VM must reject the wrong variant"),
        ] {
            assert_eq!(err.code, "E9999");
            assert!(err.msg.contains(needle), "unexpected message: {}", err.msg);
        }
    }
}
