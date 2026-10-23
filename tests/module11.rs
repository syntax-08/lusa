//! Module 11: static structs, raw pointers, and kernel-level intrinsics.

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
fn structs_emit_typedefs_and_direct_member_access() {
    let c = emit(
        "struct RegisterBank { control: u32, data: i32, status: u32 }\n\
         local bank: RegisterBank\n\
         bank.control = 7\n\
         print(bank.control, bank.data)\n\
         return 0\n",
    );
    assert!(c.contains("typedef struct {"));
    assert!(c.contains("} RegisterBank;"));
    assert!(c.contains("RegisterBank init"));
    assert!(c.contains(".control = "));
    // Struct field access is a direct member access, not a keyed hash lookup.
    assert!(!c.contains("lusa_table_get_u32_key(v"));
    assert!(!c.contains("lusa_table_set_string(v"));
}

#[test]
fn raw_pointers_emit_volatile_arithmetic_and_dereferences() {
    let c = emit(
        "local p: *mut u32 = 0x1000\n\
         unsafe do\n\
             local q: *mut u32 = p + 2\n\
             *q = 255\n\
             local v: u32 = *q\n\
             print(v)\n\
         end\n\
         return 0\n",
    );
    assert!(c.contains("volatile uint32_t *"));
    assert!(c.contains("(*("));
    assert!(c.contains("= (uint32_t)("));
}

#[test]
fn intrinsics_emit_asm_hooks_and_allocator() {
    let c = emit(
        "unsafe do\n\
             unsafe_nop()\n\
             unsafe_asm(\"cli\")\n\
             local buf: *mut u32 = unsafe_alloc(64)\n\
             print(buf)\n\
         end\n\
         return 0\n",
    );
    assert!(c.contains("__asm__ __volatile__(text)"));
    assert!(c.contains("__asm__ __volatile__(\"nop\")"));
    assert!(c.contains("lusa_unsafe_alloc("));
    assert!(c.contains("lusa_free_allocs();"));
}

#[test]
fn pointer_rules_report_cargo_style_diagnostics() {
    let err = cbackend::emit_c("local p: *mut u32 = 0x1000\nlocal q: *mut u32 = p + 1\nreturn 0\n")
        .expect_err("arithmetic outside unsafe must fail");
    assert!(err.contains("error[E0320]"), "diagnostic: {err}");

    let err = cbackend::emit_c("local p: *mut u32 = 0x1000\nlocal v: u32 = *p\nreturn 0\n")
        .expect_err("dereference outside unsafe must fail");
    assert!(err.contains("error[E0321]"), "diagnostic: {err}");

    let err = cbackend::emit_c("local p: *const u32 = 0x1000\nunsafe do\n*p = 5\nend\nreturn 0\n")
        .expect_err("writes through *const must fail");
    assert!(err.contains("error[E0322]"), "diagnostic: {err}");
}

#[test]
fn dynamic_engines_simulate_structs_pointers_and_intrinsics() {
    let src = r#"
struct RegisterBank { control: u32, data: i32, status: u32 }

local bank: RegisterBank
bank.control = 7
bank.data = -3

unsafe do
    local regs: *mut u32 = unsafe_alloc(64)
    unsafe_write_mem(regs, 11)
    local v: u32 = unsafe_read_mem(regs)
    *(regs + 1) = 9
    local w: u32 = *(regs + 1)
    unsafe_nop()
    unsafe_asm("cli")
    print("inside:", bank.control, bank.data, v, w)
end

print("outside:", bank.control, bank.status)
"#;
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "inside: 7 -3 11 9\noutside: 7 0");
    assert_eq!(vm, interp);
}

#[test]
fn dynamic_engines_reject_deref_outside_unsafe() {
    let src = "local p: *mut u32 = 0x1000\nlocal v: u32 = *p\nprint(v)\n";
    let err = lusa::run_source(src).expect_err("interpreter must reject the deref");
    assert_eq!(err.code, "E0321");
    let err = lusa::vm::run_source_vm(src).expect_err("VM must reject the deref");
    assert_eq!(err.code, "E0321");
}

#[test]
fn native_struct_pointer_and_intrinsic_program_runs() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 11 test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module11-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    let source = r#"
struct RegisterBank {
    control: u32,
    data: i32,
    status: u32,
}

local bank: RegisterBank
bank.control = 7
bank.data = -3

unsafe do
    local regs: *mut u32 = unsafe_alloc(64)
    unsafe_write_mem(regs, 11)
    local v: u32 = unsafe_read_mem(regs)
    *(regs + 1) = 9
    local w: u32 = *(regs + 1)
    *(regs + 2) = *(regs) + *(regs + 1)
    unsafe_nop()
    print("inside:", bank.control, bank.data, v, w, *(regs + 2))
end

print("outside:", bank.control, bank.status)
return 0
"#;
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "inside: 7 -3 11 9 20\noutside: 7 0\n");
}
