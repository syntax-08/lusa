//! Module 8.5: typed Lusa -> C11 -> native executable.
//!
//! Emission tests always run.  Native compile/run tests skip cleanly when the
//! host machine has no detectable C compiler; CI installs GCC/Clang, while a
//! developer can point `LUSA_CC` at TCC, Clang, GCC, or `cl.exe`.

use std::path::PathBuf;
use std::process::Command;

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn assert_has(c: &str, needle: &str) {
    assert!(
        c.contains(needle),
        "generated C is missing `{needle}`:\n{c}"
    );
}

#[test]
fn typed_locals_and_default_inference() {
    let c = emit(
        "local n = 42\n\
         local amount = 2.5\n\
         local id: u32 = 1\n\
         local signed: i32 = -5\n\
         local big: i128 = 1234567890123\n\
         print(n, amount, id, signed, big)\n\
         return 0\n",
    );
    assert_has(&c, "int32_t init");
    assert_has(&c, "float init");
    assert_has(&c, "uint32_t init");
    assert_has(&c, "lusa_i128 init");
    assert_has(&c, "lusa_print_i128");
}

#[test]
fn functions_tables_and_raii_are_emitted() {
    let c = emit(
        "function add(a: i32, b: i32): i32\n\
             return a + b\n\
         end\n\
         function make(n: u32): [string]\n\
             local out: [string] = {\"a\", \"b\"}\n\
             return out\n\
         end\n\
         local text = \"hi\"\n\
         local values: [i32] = {1, 2, 3}\n\
         local total = add(2, 3)\n\
         local names = make(2)\n\
         local copy = clone text\n\
         local moved = values\n\
         print(text, copy, total, #moved, names[1])\n\
         return 0\n",
    );
    assert_has(&c, "static int32_t f");
    assert_has(&c, "static LusaTable * f");
    assert_has(&c, "LusaTable *table");
    assert_has(&c, "lusa_table_new(");
    assert_has(&c, "lusa_table_get_i32_at(");
    assert_has(&c, "lusa_string_clone(");
    assert_has(&c, "lusa_drop_string(&");
    assert_has(&c, "lusa_drop_table(&");
    // A move transfers ownership and clears the source slot.
    assert_has(&c, "= NULL;");
    // Drops run before the return path.
    let drop_pos = c.find("lusa_drop_table(&").expect("table drop");
    let ret_pos = c.rfind("return (int)(").expect("main return");
    assert!(drop_pos < ret_pos, "drops must precede the final return");
}

#[test]
fn strings_with_quotes_and_newlines_are_encoded_safely() {
    let c = emit(
        "local apostrophe = \"it's\"\n\
         local quote = 'a \"quote\"'\n\
         print(apostrophe, quote)\n\
         return 0\n",
    );
    assert_has(&c, "static unsigned char lusa_bytes_");
    // `it's` is stored as a byte table, so the apostrophe survives the
    // compiler-embedding layer without a quoting bug.
    assert_has(&c, "105, 116, 39, 115");
}

#[test]
fn exact_i128_literals_use_the_digit_string_parser() {
    let decimal = "123456789012345678901234567890";
    let hex = "0x1FFFFFFFFFFFFFFFFFFFFFFFF";
    let c = emit(&format!(
        "local a: i128 = {decimal}\n\
         local b: i128 = {hex}\n\
         local c: i128 = -{decimal}\n\
         print(a, b, c)\n\
         return 0\n"
    ));
    assert_has(&c, "lusa_i128_parse(");
    assert_has(&c, decimal);
    assert_has(&c, hex);
    assert!(!c.contains(&format!("(lusa_i128){decimal}")));
}

#[test]
fn unsafe_block_emits_volatile_pointer_access() {
    let c = emit(
        "unsafe do\n\
             local reg: i128 = unsafe_ptr_cast(0x40000000)\n\
             unsafe_write_mem(reg, 1)\n\
             local value: u32 = unsafe_read_mem(reg)\n\
             print(value, \"ok\")\n\
         end\n\
         return 0\n",
    );
    assert_has(&c, "unsafe: raw memory access");
    assert_has(&c, "*(volatile uint32_t *)(uintptr_t)");
    assert_has(&c, "= (uint32_t)(");
    assert_has(&c, "(uintptr_t)(");
}

#[test]
fn volatile_mmio_lowering_is_exact_c11() {
    let c = emit(
        "unsafe do\n\
             local reg: i128 = unsafe_ptr_cast(0x40000000)\n\
             unsafe_write_mem(reg, 7)\n\
             local value: u32 = unsafe_read_mem(reg)\n\
             print(value)\n\
         end\n\
         return 0\n",
    );
    // MMIO reads and writes both go through the same volatile 32-bit lvalue.
    assert_has(&c, "(*(volatile uint32_t *)(uintptr_t)(");
    assert_has(&c, ") = (uint32_t)(");
    assert_has(&c, "uint32_t init");
    // Nothing may lower to a plain, non-volatile dereference.
    assert!(
        !c.contains("(*(uint32_t *)(uintptr_t)("),
        "MMIO access must stay volatile:\n{c}"
    );
}

#[test]
fn full_native_type_matrix_includes_bool_and_string() {
    let c = emit(
        "local flag: bool = true\n\
         local label: string = \"lusa\"\n\
         local count: u32 = 1\n\
         local signed: i32 = -1\n\
         local wide: i128 = 1\n\
         local ratio: f32 = 0.5\n\
         print(flag, label, count, signed, wide, ratio)\n\
         return 0\n",
    );
    assert_has(&c, "bool init");
    assert_has(&c, "LusaString * init");
    assert_has(&c, "uint32_t init");
    assert_has(&c, "int32_t init");
    assert_has(&c, "lusa_i128 init");
    assert_has(&c, "float init");
    assert_has(&c, "lusa_print_bool");
    assert_has(&c, "lusa_print_string");
    assert_has(&c, "typedef struct LusaString");
}

#[test]
fn normal_scope_exits_emit_raii_drops() {
    let c = emit(
        "do\n\
             local text = clone \"owned\"\n\
         end\n\
         do\n\
             local values: [i32] = {1, 2}\n\
         end\n\
         return 0\n",
    );
    // Each scope has one normal-exit drop plus the error-path drop emitted
    // after the local.  A bug that consumes drop metadata would leave only
    // the error-path drop, so require two of each.  Restrict the count to
    // `main` so the runtime's own drop helpers cannot satisfy the assertion.
    let main_start = c.find("int main(void) {").expect("generated main");
    let rest = &c[main_start..];
    let main_end = rest.find("\n}").expect("end of main") + 2;
    let main_body = &rest[..main_end];
    assert!(
        main_body.matches("lusa_drop_string(&").count() >= 2,
        "lexical exits must drop owned strings, not only error paths:\n{main_body}"
    );
    assert!(
        main_body.matches("lusa_drop_table(&").count() >= 2,
        "lexical exits must drop owned tables, not only error paths:\n{main_body}"
    );
}

#[test]
fn global_cleanup_survives_declaration_and_error_paths() {
    let c = emit("text = clone \"global\"\nreturn 0\n");
    assert_has(&c, "static LusaString * g");
    let main_start = c.find("int main(void) {").expect("generated main");
    let rest = &c[main_start..];
    let main_end = rest.find("\n}").expect("end of main") + 2;
    let main_body = &rest[..main_end];
    // One drop runs on each checked error path and one on the normal return.
    // Declaring the global must not consume its drop metadata.
    assert!(
        main_body.matches("lusa_drop_string(&g").count() >= 2,
        "globals must be dropped on normal and error exits:\n{main_body}"
    );
}

#[test]
fn unsafe_intrinsics_require_an_unsafe_block() {
    let err = cbackend::emit_c(
        "local reg: i128 = unsafe_ptr_cast(0x40000000)\n\
         unsafe_write_mem(reg, 1)\n\
         return 0\n",
    )
    .expect_err("raw pointer access outside unsafe must fail");
    assert!(err.contains("E0316"), "diagnostic: {err}");
    assert!(err.contains("unsafe"), "diagnostic: {err}");
    assert!(err.contains("--> lusa:"), "diagnostic: {err}");
}

#[test]
fn type_errors_use_cargo_style_diagnostics() {
    let err = cbackend::emit_c("local x: u32 = \"bad\"\nreturn 0\n")
        .expect_err("mismatched literal must fail");
    assert!(err.contains("error[E0308]"), "diagnostic: {err}");
    assert!(err.contains("--> lusa:1:"), "diagnostic: {err}");
    assert!(err.contains("expected `u32`"), "diagnostic: {err}");
    assert!(err.contains("found    `string`"), "diagnostic: {err}");
}

#[test]
fn native_i128_literal_is_bit_exact_when_the_compiler_has_int128() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native i128 test: {e}");
            return;
        }
    };
    if compiler.kind != cbackend::CompilerKind::Gnu
        || compiler.display_name().to_ascii_lowercase().contains("tcc")
    {
        eprintln!(
            "skipping native i128 test: {} does not provide __int128",
            compiler.display_name()
        );
        return;
    }

    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-i128-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    let decimal = "123456789012345678901234567890";
    let source = format!(
        "local big: i128 = {decimal}\nprint(big)\nreturn 0\n"
    );
    cbackend::build(&source, &exe, None)
        .unwrap_or_else(|e| panic!("native i128 build failed:\n{e}"));
    let run = Command::new(&exe).output().expect("run i128 executable");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "i128 executable failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, format!("{decimal}\n"));
}

#[test]
fn native_build_and_run_when_a_compiler_is_available() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native C backend test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe: PathBuf = out_dir.join(format!("lusa-cbackend-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);

    let source = r#"
function add(a: i32, b: i32): i32
    return a + b
end

local total: i32 = 0
local i: i32 = 0
while i < 3 do
    total = total + i
    i = i + 1
end

local values: [i32] = {10, 20, 30}
local names: [string] = {"a", "b"}
local moved = values
local text = "hi"
local copy = clone text
print(add(total, 4), text, copy, #moved, moved[2], names[1])
if total > 2 then
    print("big")
end
return 0
"#;
    let built = cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    assert_eq!(built.path, compiler.path);
    let run = Command::new(&exe)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", exe.display()));
    let _ = std::fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "native program failed: {run:?}\ncompiler: {}",
        built.display_name()
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "7 hi hi 3 20 a\nbig\n");
}
