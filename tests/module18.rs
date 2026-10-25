//! Module 18: inline assembly (`asm!`) and direct hardware controls.

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

/// Shared native program: a volatile register write followed by an assembly
/// fence, then a volatile read.  `nop` is safe in user mode on x86 and ARM;
/// privileged instructions such as `cli`/`sei` are covered by emission-only
/// tests and comments in the example.
const ASM_PROGRAM: &str = r#"
unsafe do
    local regs: *mut u32 = unsafe_alloc(16)
    regs.write_volatile(1)
    asm!("nop")
    local value: u32 = regs.read_volatile()
    print("asm", value)
end
return 0
"#;

#[test]
fn asm_emits_gnu_volatile_inline_assembly() {
    let c = emit("unsafe do\n    asm!(\"nop\")\nend\nreturn 0\n");
    assert!(
        c.contains("#define LUSA_ASM(text) ({ __asm__ __volatile__ (text : : ); })"),
        "GNU volatile macro:\n{c}"
    );
    assert!(c.contains("LUSA_ASM(\"nop\")"), "asm call lowering:\n{c}");
    assert!(
        !c.contains("asm!(\"nop\")"),
        "macro syntax must not leak into C:\n{c}"
    );
}

#[test]
fn asm_preserves_multiple_instruction_strings() {
    let c = emit(
        "unsafe do\n\
             asm!(\"cli\")\n\
             asm!(\"sei\")\n\
             asm!(\"nop; nop\")\n\
         end\n\
         return 0\n",
    );
    for text in ["cli", "sei", "nop; nop"] {
        let expected = format!("LUSA_ASM(\"{text}\")");
        assert!(c.contains(&expected), "missing {expected:?}:\n{c}");
    }
}

#[test]
fn asm_requires_unsafe_in_every_engine() {
    let src = "asm!(\"nop\")\nreturn 0\n";
    let err = lusa::run_source(src).expect_err("interpreter must gate asm");
    assert_eq!(err.code, "E0321");
    assert!(err.msg.contains("unsafe"), "interpreter message: {}", err.msg);

    let err = lusa::vm::run_source_vm(src).expect_err("VM must gate asm");
    assert_eq!(err.code, "E0321");
    assert!(err.msg.contains("unsafe"), "VM message: {}", err.msg);

    let err = emit_err(src);
    assert!(err.contains("E0316"), "native unsafe gate:\n{err}");
    assert!(err.contains("inline assembly"), "native wording:\n{err}");
}

#[test]
fn asm_rejects_malformed_strings_and_argument_counts() {
    let cases = [
        (
            "unsafe do\n    asm!()\nend\nreturn 0\n",
            "requires an assembly string",
        ),
        (
            "unsafe do\n    asm!(5)\nend\nreturn 0\n",
            "expects a string literal",
        ),
        (
            "unsafe do\n    asm!(\"nop\", \"nop\")\nend\nreturn 0\n",
            "`)`",
        ),
        (
            "unsafe do\n    asm!(\"\")\nend\nreturn 0\n",
            "non-empty assembly string",
        ),
        (
            "unsafe do\n    asm!(\"bad\\n\")\nend\nreturn 0\n",
            "must not contain",
        ),
    ];
    for (source, needle) in cases {
        let err = lusa::run_source(source).expect_err("dynamic parser must reject asm");
        assert_eq!(err.code, "E0101", "dynamic error for {source:?}: {err}");
        let err = emit_err(source);
        assert!(err.contains("E0101"), "native error for {source:?}:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

#[test]
fn asm_and_unsafe_asm_share_one_lowering() {
    let c = emit(
        "unsafe do\n\
             asm!(\"nop\")\n\
             unsafe_asm(\"nop\")\n\
         end\n\
         return 0\n",
    );
    assert_eq!(
        c.matches("LUSA_ASM(\"nop\")").count(),
        2,
        "both spellings must lower identically:\n{c}"
    );
}

#[test]
fn dynamic_engines_run_asm_in_control_flow() {
    let src = "local i: u32 = 0\n\
               unsafe do\n\
                   while i ~= 3 do\n\
                       asm!(\"nop\")\n\
                       i = i + 1\n\
                   end\n\
               end\n\
               print(i)\n\
               return 0\n";
    let (interp, vm) = both_engines(src);
    assert_eq!(interp, "3");
    assert_eq!(vm, interp);
}

#[test]
fn native_asm_program_runs() {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 18 test: {e}");
            return;
        }
    };
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module18-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(ASM_PROGRAM, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "asm 1\n");
}
