//! Module 15: struct syntax and literals, `match`, compile-time imports, and
//! GNU-style inline assembly.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn both_engines(source: &str) -> (String, String) {
    let interp = lusa::run_source(source).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(source).expect("VM failed");
    (
        format!("{}\n", interp.join("\n")),
        format!("{}\n", vm.join("\n")),
    )
}

fn native_exe(label: &str) -> PathBuf {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    out_dir.join(format!(
        "lusa-module15-{label}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ))
}

fn compiler_or_skip(label: &str) -> Option<cbackend::CCompiler> {
    match cbackend::find_compiler(None) {
        Ok(cc) => Some(cc),
        Err(e) => {
            eprintln!("skipping native Module 15 test `{label}`: {e}");
            None
        }
    }
}

fn build_native(label: &str, source: &str) -> Option<PathBuf> {
    let compiler = compiler_or_skip(label)?;
    let exe = native_exe(label);
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    Some(exe)
}

fn build_native_file(label: &str, input: &Path) -> Option<PathBuf> {
    let compiler = compiler_or_skip(label)?;
    let exe = native_exe(label);
    let _ = std::fs::remove_file(&exe);
    cbackend::build_file(input, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build_file with {} failed:\n{e}",
            compiler.display_name()
        )
    });
    Some(exe)
}

fn run_exe(exe: &Path) -> Output {
    let output = Command::new(exe).output().expect("run native program");
    let _ = std::fs::remove_file(exe);
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

const STRUCT_SOURCE: &str = r#"
struct Point {
    x: i32,
    y: i32,
    label: string
}

struct Box
    origin: Point,
    label: string
end

local p = Point{x: 3, label: "point"}
local boxed = Box{origin: clone p, label: "boxed"}
print(p.x, p.y, boxed.origin.x, boxed.origin.y, boxed.label)
boxed.origin.y = 7
local copy = clone boxed
copy.label = "copy"
copy.origin.x = 9
copy.origin.label = "copy-point"
print(boxed.origin.x, boxed.origin.y, boxed.label)
print(copy.origin.x, copy.origin.y, copy.label)
print(boxed.origin.label, copy.origin.label)
local taken = move(boxed)
print(taken.origin.x, taken.origin.y, taken.label)
return 0
"#;

const STRUCT_EXPECTED: &str = "3 0 3 0 boxed\n3 7 boxed\n9 7 copy\npoint copy-point\n3 7 boxed\n";

#[test]
fn struct_syntaxes_literals_fields_and_deep_clone_agree() {
    let (interp, vm) = both_engines(STRUCT_SOURCE);
    assert_eq!(interp, STRUCT_EXPECTED);
    assert_eq!(vm, STRUCT_EXPECTED);

    let Some(exe) = build_native("structs", STRUCT_SOURCE) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), STRUCT_EXPECTED);
}

#[test]
fn scalar_only_structs_keep_copy_semantics_through_nested_fields_and_clone() {
    let source = r#"
struct Inner { x: i32, y: i32 }
struct Outer
    inner: Inner,
    id: i32
end

local o = Outer{inner: Inner{x: 1, y: 2}, id: 7}
local copy = clone o
local inner = copy.inner
local again = copy.inner
print(inner.x, again.x, copy.id)
local direct = o.inner
print(direct.y, o.inner.y)
return 0
"#;
    let expected = "1 1 7\n2 2\n";
    let (interp, vm) = both_engines(source);
    assert_eq!(interp, expected);
    assert_eq!(vm, expected);

    let Some(exe) = build_native("struct-copy", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), expected);
}

#[test]
fn struct_layout_emits_typedefs_and_deep_drop_helpers() {
    let c = emit(STRUCT_SOURCE);

    assert!(c.contains("typedef struct {"));
    assert!(c.contains("} Point;"));
    assert!(c.contains("} Box;"));
    assert!(c.contains("int32_t x;"));
    assert!(c.contains("Point origin;"));
    assert!(c.contains("LusaString * label;"));
    assert!(c.contains("(Point){ .x = "));
    assert!(c.contains(".origin = "));

    assert!(c.contains("static void lusa_drop_Point(Point *value)"));
    assert!(c.contains("static void lusa_drop_Box(Box *value)"));
    assert!(c.contains("lusa_drop_string(&value->label);"));
    assert!(c.contains("lusa_drop_Point(&value->origin);"));
    assert!(c.contains("static Box lusa_clone_Box(Box value)"));
    assert!(c.contains("value.origin = lusa_clone_Point(value.origin);"));
    assert!(c.contains("value.label = lusa_string_clone(value.label);"));

    // Struct field access is a direct member access, not a keyed hash lookup.
    assert!(!c.contains("lusa_table_get_i32_key(v"));
    assert!(!c.contains("lusa_table_set_string(v"));
}

const MATCH_SOURCE: &str = r#"
struct Pair { x: i32 }

function pick(flag: bool, p: Pair): Pair
    match flag with
    | true => return p
    | _ => return clone p
    end
end

function classify(n: i32): i32
    match n with
    | 0 => print("zero")
    | 1 => print("one")
    | value => print("many", value)
    end
    return 0
end

classify(0)
classify(1)
classify(7)

local t = { value = 5 }
match t.value with
| 5 => print("five")
| _ => print("not five")
end

match "hi" with
| "hi" => print("greeting")
| other => print("other", other)
end

local a = Pair{x: 1}
local b = pick(false, a)
print(b.x)
local c = pick(true, a)
print(c.x)
return 0
"#;

const MATCH_EXPECTED: &str = "zero\none\nmany 7\nfive\ngreeting\n1\n1\n";

#[test]
fn match_arms_agree_and_lower_to_plain_conditionals() {
    let (interp, vm) = both_engines(MATCH_SOURCE);
    assert_eq!(interp, MATCH_EXPECTED);
    assert_eq!(vm, MATCH_EXPECTED);

    let c = emit(MATCH_SOURCE);
    assert!(c.contains("if ("));
    // `match` is syntactic sugar: no dispatcher or runtime arm metadata.
    assert!(!c.to_ascii_lowercase().contains("lusa_match"));

    let Some(exe) = build_native("match", MATCH_SOURCE) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), MATCH_EXPECTED);
}

const MATCH_MOVE_SOURCE: &str = r#"
function bad(t: table): i32
    match t with
    | other =>
        local moved = other
        print(other)
    end
    return 0
end
bad({x = 1})
return 0
"#;

#[test]
fn match_ownership_is_checked_per_arm_before_codegen() {
    let err = cbackend::emit_c(MATCH_MOVE_SOURCE).expect_err("emission must fail");
    assert!(err.contains("error[E1001]"), "diagnostic: {err}");
    assert!(
        err.contains("use of moved-out value `other`"),
        "diagnostic: {err}"
    );
    assert!(err.contains("--> lusa:6:"), "diagnostic: {err}");

    let err = lusa::run_source(MATCH_MOVE_SOURCE).expect_err("interpreter must reject the move");
    assert_eq!(err.code, "E1001");
    let err = lusa::vm::run_source_vm(MATCH_MOVE_SOURCE).expect_err("VM must reject the move");
    assert_eq!(err.code, "E1001");
}

fn write_project_files(dir: &Path) {
    std::fs::create_dir_all(dir.join("geo")).expect("geo dir");
    std::fs::create_dir_all(dir.join("counter")).expect("counter dir");
    std::fs::write(
        dir.join("geo").join("point.lusa"),
        r#"
struct Point {
    x: i32,
    y: i32
}

function make_point(x: i32, y: i32): Point
    return Point{x: x, y: y}
end
"#,
    )
    .expect("write point.lusa");
    std::fs::write(
        dir.join("geo").join("shape.lusa"),
        r#"
import "point.lusa"

struct Shape {
    origin: Point,
    tag: string
}
"#,
    )
    .expect("write shape.lusa");
    std::fs::write(
        dir.join("counter").join("mod.lusa"),
        r#"
struct Counter {
    count: i32
}

function bump(c: Counter): Counter
    return Counter{count: c.count + 1}
end
"#,
    )
    .expect("write counter/mod.lusa");
    std::fs::write(dir.join("main.lusa"), IMPORT_SOURCE).expect("write main.lusa");
}

const IMPORT_SOURCE: &str = r#"
import "geo/shape.lusa"
import "geo/point.lusa"
mod counter

local p = make_point(2, 3)
local s = Shape{origin: p, tag: "box"}
print(s.origin.x, s.origin.y, s.tag)

local c = Counter{count: 4}
local d = bump(c)
print(d.count)
return 0
"#;

const IMPORT_EXPECTED: &str = "2 3 box\n5\n";

#[test]
fn imports_and_modules_merge_symbols_relative_to_the_source() {
    let dir = std::env::temp_dir().join(format!("lusa-module15-imports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write_project_files(&dir);

    let interp = lusa::run_source_with_base(IMPORT_SOURCE, Some(&dir)).expect("interpreter import");
    assert_eq!(format!("{}\n", interp.join("\n")), IMPORT_EXPECTED);
    let vm = lusa::vm::run_source_vm_with_base(IMPORT_SOURCE, Some(&dir)).expect("VM import");
    assert_eq!(format!("{}\n", vm.join("\n")), IMPORT_EXPECTED);

    let c = cbackend::emit_c_with_base(IMPORT_SOURCE, Some(&dir)).expect("C import expansion");
    assert!(c.contains("} Shape;"));
    assert!(c.contains("} Counter;"));
    assert!(!c.contains("import \""));
    assert_eq!(c.matches("} Point;").count(), 1, "point.lusa included once");

    let main = dir.join("main.lusa");
    if let Some(exe) = build_native_file("imports", &main) {
        let output = run_exe(&exe);
        assert!(output.status.success(), "native program failed: {output:?}");
        assert_eq!(stdout(&output), IMPORT_EXPECTED);
    }

    let missing = cbackend::emit_c_with_base("import \"nope.lusa\"\nreturn 0\n", Some(&dir))
        .expect_err("missing import must fail");
    assert!(missing.contains("cannot find imported file"), "{missing}");
    assert!(missing.contains("nope.lusa"), "{missing}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unsafe_asm_uses_the_gnu_volatile_form() {
    let c = emit("unsafe do\n    unsafe_asm(\"nop\")\nend\nreturn 0\n");
    assert!(c.contains("__asm__ __volatile__ (text : : );"));
    assert!(c.contains("LUSA_ASM(\"nop\")"));
}
