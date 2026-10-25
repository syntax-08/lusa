//! Module 19: custom entry points, `#[no_std]` bare-metal mode, and
//! `#[link_section = "..."]` placement.

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

fn parse_err(source: &str) -> lusa::error::LusaError {
    lusa::run_source(source).expect_err("dynamic parser must reject the program")
}

/// A complete freestanding startup program: no hosted runtime, a sectioned
/// global promoted from a top-level `local`, and `_start` as the entry symbol.
const NO_STD_PROGRAM: &str = r#"
#[no_std]

#[link_section = ".data.boot"]
local counter: u32 = 7

#[link_section = ".text.boot"]
#[entry]
function _start() : void
    counter = counter + 1
    counter = counter + 1
end
"#;

#[test]
fn no_std_emits_a_freestanding_translation_unit() {
    let c = emit(NO_STD_PROGRAM);

    assert!(
        c.starts_with("/* lusa:no_std */"),
        "the no_std marker must be the first line:\n{c}"
    );
    assert!(c.contains("#include <stdint.h>"), "{c}");
    assert!(c.contains("typedef struct LusaCoro LusaCoro;"), "{c}");
    assert!(c.contains("static int lusa_program_body(void)"), "{c}");
    assert!(c.contains("void _start(void) {"), "{c}");

    // The hosted C runtime, coroutines, pattern matcher, and allocator must be
    // absent from a freestanding translation unit.
    for banned in [
        "#include <stdio.h>",
        "#include <stdlib.h>",
        "#include <string.h>",
        "#include <windows.h>",
        "LusaCoroResult",
        "lusa_coro_new",
        "lusa_print_i32",
        "lusa_free_allocs",
        "Module 13 string pattern runtime",
        "int main(void)",
    ] {
        assert!(!c.contains(banned), "no_std output must not contain {banned:?}:\n{c}");
    }

    // Sections reach both the promoted global and the generated function.
    let section_comment =
        r#"/* #[link_section = ".data.boot"] -> __attribute__((section(".data.boot"))) */"#;
    assert!(c.contains(section_comment), "{c}");
    assert!(c.contains(r#"LUSA_LINK_SECTION(".data.boot")"#), "{c}");
    assert!(c.contains(r#"LUSA_LINK_SECTION(".text.boot")"#), "{c}");
}

#[test]
fn entry_wrapper_runs_program_body_before_the_entry_function() {
    let source = r#"
#[entry]
function start() : void
    counter = counter + 1
end

counter = 1
"#;
    let c = emit(source);

    assert!(!c.contains("int main(void)"), "{c}");
    assert!(c.contains("#include <stdio.h>"), "hosted entry keeps the runtime:\n{c}");
    assert!(
        c.contains("Module 13 string pattern runtime"),
        "hosted entry keeps the pattern runtime:\n{c}"
    );

    let body = c
        .find("static int lusa_program_body(void)")
        .expect("program body must be emitted");
    let wrapper = c.find("void start(void)").expect("entry wrapper must be emitted");
    assert!(body < wrapper, "program body must precede the wrapper:\n{c}");

    let wrapper_text = &c[wrapper..];
    let call = wrapper_text
        .find("(NULL);")
        .expect("wrapper must call the internal function");
    let free = wrapper_text
        .find("lusa_free_allocs();")
        .expect("hosted wrapper must free runtime allocations");
    assert!(call < free, "cleanup must follow the entry call:\n{c}");
    assert!(wrapper_text.contains("if (lusa_program_body() != 0)"), "{c}");
}

#[test]
fn entry_wrapper_is_void_even_when_the_function_returns_a_value() {
    let source = r#"
#[entry]
function answer() : i32
    return 3
end
"#;
    let c = emit(source);

    assert!(c.contains("void answer(void) {"), "{c}");
    assert!(
        !c.contains("int32_t answer(void)"),
        "the exported entry symbol is always void:\n{c}"
    );
    assert!(!c.contains("int main(void)"), "{c}");
}

#[test]
fn link_section_lowers_for_functions_globals_and_promoted_locals() {
    let source = r#"
#[link_section = ".text.boot"]
function boot() : void
end

#[link_section = ".data.boot"]
local counter: u32 = 1

#[link_section = ".bss.state"]
ready = false

boot()
"#;
    let c = emit(source);

    for (section, declaration) in [
        (".text.boot", "static void f"),
        (".data.boot", "static uint32_t "),
        (".bss.state", "static bool "),
    ] {
        let comment = format!(
            "/* #[link_section = \"{section}\"] -> __attribute__((section(\"{section}\"))) */"
        );
        let macro_line = format!("LUSA_LINK_SECTION(\"{section}\")");
        assert_eq!(c.matches(&macro_line).count(), 1, "{section}:\n{c}");
        let placed = format!("{macro_line}\n{declaration}");
        assert!(
            c.contains(&comment) && c.contains(&placed),
            "{section} must annotate {declaration:?}:\n{c}"
        );
    }

    // Hosted programs still get the standard C entry point.
    assert!(c.contains("int main(void)"), "{c}");
}

#[test]
fn dynamic_engines_accept_entry_sections_and_promoted_locals() {
    let source = r#"
#[no_std]

#[link_section = ".data.state"]
local counter: u32 = 7

#[link_section = ".text.boot"]
#[entry]
function _start() : void
    counter = counter + 1
end

#[link_section = ".data.extra"]
next = counter + 1

_start()
print(counter, next)
"#;
    let (interp, vm) = both_engines(source);
    assert_eq!(interp, "8 8");
    assert_eq!(vm, interp);
}

#[test]
fn baremetal_example_emits_without_hosted_references() {
    let c = emit(include_str!("../examples/baremetal_boot.lusa"));

    assert!(c.starts_with("/* lusa:no_std */"), "{c}");
    assert!(!c.contains("int main(void)"), "{c}");
    for banned in ["lusa_str_", "LusaString", "lusa_current_coro", "lusa_free_allocs"] {
        assert!(
            !c.contains(banned),
            "the bare-metal example must not reference {banned:?}:\n{c}"
        );
    }
    // File-scope aggregate initializers stay constant expressions for MSVC.
    assert!(c.contains("static BootState g1 = {0};"), "{c}");
    assert!(c.contains(r#"LUSA_LINK_SECTION(".text.boot")"#), "{c}");
    assert!(c.contains(r#"LUSA_LINK_SECTION(".data.boot")"#), "{c}");
    assert!(c.contains("(g1).ticks"), "{c}");
    assert!(c.contains("f") && c.contains("(NULL);"), "calls must use the no_std coroutine stub:\n{c}");
}

#[test]
fn no_std_must_be_the_first_and_only_file_level_marker() {
    let cases = [
        (
            "local x: u32 = 1\n#[no_std]\nfunction _start() : void\nend\n",
            "`#[no_std]` must be the first statement in the file",
        ),
        (
            "#[no_std]\n#[no_std]\nfunction _start() : void\nend\n",
            "duplicate `#[no_std]` attribute",
        ),
        (
            "#[no_std]\ndo\n    #[no_std]\nend\nfunction _start() : void\nend\n",
            "`#[no_std]` is only allowed at the top level",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains("E0325"), "expected E0325 in:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }

    // Writing another attribute before `#[no_std]` is a parser error.
    let err = emit_err(
        "#[link_section = \".text.boot\"]\n#[no_std]\n\
         function _start() : void\nend\n",
    );
    assert!(err.contains("E0101"), "expected E0101 in:\n{err}");
    assert!(err.contains("must be written on its own"), "{err}");
}

#[test]
fn entry_conflicts_are_rejected() {
    let cases = [
        (
            "#[entry]\nfunction a() : void\nend\n#[entry]\nfunction b() : void\nend\n",
            "duplicate `#[entry]` function (already defined by `a`)",
        ),
        (
            "#[entry]\nfunction a(x: u32) : void\nend\n",
            "`#[entry]` functions cannot take parameters",
        ),
        (
            "account = {}\n#[entry]\nfunction account:start() : void\nend\n",
            "`#[entry]` cannot annotate a method",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains("E0325"), "expected E0325 in:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

#[test]
fn link_section_misplacement_is_rejected() {
    let cases = [
        (
            "x = 1\n#[link_section = \".data.boot\"]\nx = 2\n",
            "`#[link_section]` can only annotate a new global `x`",
        ),
        (
            "function f() : void\n    #[link_section = \".data.boot\"]\n    local y: u32 = 1\nend\nf()\n",
            "`#[link_section]` is only supported on top-level locals and globals",
        ),
        (
            "function f() : void\n    #[link_section = \".data.boot\"]\n    y = 1\nend\nf()\n",
            "`#[link_section]` is only supported on top-level globals",
        ),
        (
            "#[link_section = \".data.boot\"]\nstruct Bad { a: u32 }\n",
            "`#[entry]` and `#[link_section]` are only supported on functions or globals",
        ),
        (
            "#[entry]\nlocal x: u32 = 1\n",
            "`#[entry]` cannot annotate a local",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(
            err.contains("E0325") || err.contains("E0101"),
            "expected a placement diagnostic in:\n{err}"
        );
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

#[test]
fn promoted_locals_must_initialize_every_name() {
    let err = emit_err(
        "#[no_std]\nlocal a: u32, b: u32 = 1\n\
         #[entry]\nfunction _start() : void\nend\n",
    );
    assert!(err.contains("E0325"), "{err}");
    assert!(
        err.contains("a promoted top-level local must initialize every name explicitly"),
        "{err}"
    );
}

#[test]
fn both_parsers_validate_section_names() {
    let long_name = "a".repeat(256);
    let cases = [
        ("", "`link_section` cannot be empty"),
        (".text boot", "invalid character in `link_section` name"),
        (&long_name, "section name is too long (maximum 255 bytes)"),
    ];
    for (name, needle) in cases {
        let source = format!(
            "#[link_section = \"{name}\"]\nlocal x: u32 = 1\nreturn 0\n"
        );
        let err = parse_err(&source);
        assert_eq!(err.code, "E0101", "dynamic parser: {err}");
        assert!(err.to_string().contains(needle), "dynamic parser: {err}");

        let backend = emit_err(&source);
        assert!(backend.contains("E0101"), "C backend:\n{backend}");
        assert!(backend.contains(needle), "C backend:\n{backend}");
    }

    // The same character set accepts digits, underscores, dots, dashes, and `$`.
    let c = emit(
        "#[link_section = \"sec_1.2-3$x\"]\nlocal x: u32 = 1\nreturn 0\n",
    );
    assert!(c.contains(r#"LUSA_LINK_SECTION("sec_1.2-3$x")"#), "{c}");
}

#[test]
fn duplicate_attributes_are_parser_errors() {
    let cases = [
        (
            "#[entry]\n#[entry]\nfunction a() : void\nend\n",
            "duplicate `#[entry]` attribute",
        ),
        (
            "#[link_section = \".a\"]\n#[link_section = \".b\"]\nlocal x: u32 = 1\n",
            "duplicate `#[link_section = \"...\"]` attribute",
        ),
    ];
    for (source, needle) in cases {
        let err = emit_err(source);
        assert!(err.contains("E0101"), "expected E0101 in:\n{err}");
        assert!(err.contains(needle), "expected {needle:?} in:\n{err}");
    }
}

fn native_compiler() -> Option<cbackend::CCompiler> {
    match cbackend::find_compiler(None) {
        Ok(cc) => Some(cc),
        Err(e) => {
            eprintln!("skipping native Module 19 test: {e}");
            None
        }
    }
}

fn build_and_run(compiler: &cbackend::CCompiler, source: &str, tag: &str) -> std::process::Output {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module19-{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None).unwrap_or_else(|e| {
        panic!(
            "native build with {} failed:\n{e}",
            compiler.display_name()
        )
    });
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    run
}

#[test]
fn native_hosted_entry_program_runs() {
    let Some(compiler) = native_compiler() else {
        return;
    };
    let source = r#"
#[entry]
function main() : void
    print("booted")
end
"#;
    let run = build_and_run(&compiler, source, "entry");
    assert!(run.status.success(), "native entry program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(stdout, "booted\n");
}

#[test]
fn native_no_std_program_builds_without_the_hosted_runtime() {
    let Some(compiler) = native_compiler() else {
        return;
    };
    let source = r#"
#[no_std]

local counter: u32 = 7

#[entry]
function main() : void
    counter = counter + 1
    counter = counter + 1
end
"#;
    let run = build_and_run(&compiler, source, "nostd");
    assert!(run.status.success(), "native no_std program failed: {run:?}");
    assert!(run.stdout.is_empty(), "no_std program has no print: {run:?}");
}
