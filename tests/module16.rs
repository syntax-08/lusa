//! Module 16: fixed-size arrays, address-of, raw pointers, and pointer math.

use std::process::Command;

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn both_engines(src: &str) -> (String, String) {
    let interp = lusa::run_source(src).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(src).expect("VM failed");
    (interp.join("\n"), vm.join("\n"))
}

#[test]
fn arrays_emit_c_buffers_and_zero_based_access() {
    let c = emit(
        "local a: [i32; 4] = [10, 20, 30, 40]\n\
         a[1] = a[0] + a[2]\n\
         print(a[0], a[1], a[3], #a)\n\
         return 0\n",
    );
    assert!(c.contains("[4] = {"), "C11 array declarator:\n{c}");
    assert!(c.contains(")[((int32_t)1)] = "), "zero-based store:\n{c}");
    assert!(c.contains("lusa_print_i32"), "indexed read:\n{c}");
    // `#a` folds to the declared bound.
    assert!(c.contains("((int32_t)4)"), "array length:\n{c}");
}

#[test]
fn arrays_run_in_both_dynamic_engines() {
    let src = r#"
local a: [i32; 3]
a[0] = 4
a[1] = 5
a[2] = a[0] + a[1]
print(a[0], a[1], a[2], #a)

local names: [string; 2] = ["alpha", "beta"]
local i: i32 = 0
while i < 2 do
    print(names[i])
    i = i + 1
end
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "4 5 9 3\nalpha\nbeta");
    assert_eq!(vm, interp);
}

#[test]
fn arrays_of_heap_values_emit_drop_loops() {
    let c = emit(
        "local names: [string; 2] = [\"alpha\", \"beta\"]\n\
         print(names[0])\n\
         return 0\n",
    );
    assert!(c.contains("LusaString * "), "array of strings:\n{c}");
    assert!(c.contains("for (int32_t lusa_i = 0; lusa_i < 2; lusa_i++)"), "drop loop:\n{c}");
    assert!(c.contains("lusa_drop_string(&"), "element drop:\n{c}");
}

#[test]
fn struct_arrays_emit_c_fields_and_raii_helpers() {
    let c = emit(
        "struct Bag { items: [string; 2], count: i32 }\n\
         local b: Bag\n\
         b.items[0] = \"alpha\"\n\
         b.count = 1\n\
         print(b.items[0], b.count)\n\
         return 0\n",
    );
    assert!(c.contains("LusaString * items[2];"), "struct array field:\n{c}");
    assert!(c.contains("static void lusa_drop_Bag(Bag *value)"), "drop helper:\n{c}");
    assert!(c.contains("for (int32_t lusa_i = 0; lusa_i < 2; lusa_i++)"), "field drop loop:\n{c}");
}

#[test]
fn address_of_and_pointer_arithmetic_lower_to_c() {
    let c = emit(
        "local x: u32 = 5\n\
         local p: *mut u32 = &x\n\
         unsafe do\n\
             *p = 9\n\
             local q: *mut u32 = p + 2\n\
             local r: *mut u32 = q - 1\n\
             print(*p, *q, *r)\n\
         end\n\
         return 0\n",
    );
    assert!(c.contains("&("), "address-of:\n{c}");
    assert!(c.contains("volatile uint32_t *"), "pointer type:\n{c}");
    assert!(c.contains(") + ("), "pointer offset:\n{c}");
    assert!(c.contains(") - ("), "pointer offset:\n{c}");
    assert!(c.contains("(*("), "dereference:\n{c}");
}

#[test]
fn dereference_still_requires_unsafe_but_address_of_does_not() {
    let src = "local x: u32 = 5\nlocal p: *mut u32 = &x\nlocal v: u32 = *p\nprint(v)\n";
    let err = lusa::run_source(src).expect_err("interpreter must gate the read");
    assert_eq!(err.code, "E0321");
    let err = lusa::vm::run_source_vm(src).expect_err("VM must gate the read");
    assert_eq!(err.code, "E0321");
}

#[test]
fn dynamic_engines_simulate_address_of_and_pointer_arithmetic() {
    let src = r#"
local x: u32 = 7
local p: *mut u32 = &x
unsafe do
    local q: *mut u32 = p + 1
    print(*p, *q)
end
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "7 0");
    assert_eq!(vm, interp);
}

#[test]
fn native_array_program_runs() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 16 test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module16-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    let source = r#"
local a: [i32; 4] = [10, 20, 30, 40]
a[1] = a[0] + a[2]
local names: [string; 2] = ["alpha", "beta"]
local i: i32 = 0
while i < 2 do
    print(names[i])
    i = i + 1
end
local x: u32 = 5
local p: *mut u32 = &x
unsafe do
    *p = 9
    print(a[0], a[1], a[3], #a, x)
end
return 0
"#;
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
    assert_eq!(stdout, "alpha\nbeta\n10 40 40 4 9\n");
}
