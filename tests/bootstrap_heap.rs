//! Module 8: native heap objects, dynamic strings, and scope drops.
//!
//! The self-hosted parser now accepts array tables and index expressions.
//! The native backend allocates strings/tables through VirtualAlloc on
//! Windows (mmap on Linux), deep-drops owned tables recursively, and keeps
//! moved/borrowed slots out of the drop list.

const LEXER: &str = include_str!("../lexer.lusa");
const PARSER: &str = include_str!("../parser.lusa");
const BOOTSTRAP: &str = include_str!("../compiler_bootstrap.lusa");

fn strip_module<'a>(source: &'a str, suffix: &str) -> &'a str {
    source
        .trim_end()
        .strip_suffix(suffix)
        .unwrap_or_else(|| panic!("module must end with `{suffix}`"))
}

fn program(exe: &str, elf: &str) -> String {
    let lexer = strip_module(LEXER, "return move(Lexer)");
    let parser = strip_module(PARSER, "return move(Parser)");
    let bootstrap = strip_module(BOOTSTRAP, "return move(Compiler)");

    let mut src = String::from("local M8Lexer, M8Parser, M8Compiler\n");
    src.push_str("do\n");
    src.push_str(lexer);
    src.push_str("\nM8Lexer = move(Lexer)\nend\n");
    src.push_str("do\n");
    src.push_str(parser);
    src.push_str("\nM8Parser = move(Parser)\nend\n");
    src.push_str("do\n");
    src.push_str(bootstrap);
    src.push_str("\nM8Compiler = move(Compiler)\nend\n");

    src.push_str(
        r#"
local nl = string.char(10)
local q = string.char(34)
local seed =
    "local a = " .. q .. "he" .. q .. nl ..
    "local b = " .. q .. "llo" .. q .. nl ..
    "local s = a .. b" .. nl ..
    "print(s, #s)" .. nl ..
    "do" .. nl ..
    "    local t = {" .. q .. "a" .. q .. ", " .. q .. "b" .. q .. "}" .. nl ..
    "    print(#t, t[1], t[2])" .. nl ..
    "end" .. nl ..
    "local i = 0" .. nl ..
    "while i < 2 do" .. nl ..
    "    local part = a .. " .. q .. "!" .. q .. nl ..
    "    print(part)" .. nl ..
    "    i = i + 1" .. nl ..
    "end" .. nl ..
    "local nums = {10, 20, 30}" .. nl ..
    "local moved = nums" .. nl ..
    "print(#moved, moved[2])" .. nl ..
    "local alias = borrow moved" .. nl ..
    "print(alias[1])" .. nl ..
    "local free_string = " .. q .. "n" .. q .. " .. b" .. nl ..
    "local strings = {free_string}" .. nl ..
    "print(strings[1])" .. nl ..
    "local inner = {" .. q .. "z" .. q .. " .. " .. q .. "!" .. q .. "}" .. nl ..
    "local outer = {inner}" .. nl ..
    "print(outer[1][1])" .. nl ..
    "local qv = " .. q .. "a" .. q .. " .. b" .. nl ..
    "qv = " .. q .. "c" .. q .. " .. b" .. nl ..
    "print(qv)" .. nl ..
    "g = " .. q .. "g" .. q .. " .. b" .. nl ..
    "print(g)" .. nl ..
    "return 42" .. nl

local ast = M8Parser.parse(M8Lexer.lex(seed))
local pe = M8Compiler.emit_binary(ast, "windows")
assert(pe[1] == 0x4D and pe[2] == 0x5A)
assert(io_write_binary("__EXE__", pe))

local ast2 = M8Parser.parse(M8Lexer.lex(seed))
local elf = M8Compiler.emit_binary(ast2, "linux")
assert(elf[1] == 0x7F and elf[2] == 0x45 and elf[3] == 0x4C and elf[4] == 0x46)
assert(io_write_binary("__ELF__", elf))

local layout = M8Compiler.memory_layout()
assert(layout.value_header.size == 32)
assert(layout.table.element_kind ~= nil)
assert(layout.allocator.windows ~= nil)
assert(layout.allocator.linux ~= nil)
assert(layout.move_rule ~= nil)

print("heap image ok")
"#,
    );
    src = src.replace("__EXE__", exe);
    src = src.replace("__ELF__", elf);
    src
}

fn assert_elf(bytes: &[u8]) {
    assert_eq!(&bytes[0..4], b"\x7FELF");
    assert_eq!(bytes[4], 2);
    assert_eq!(bytes[5], 1);
    assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 2);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), 0x3E);
    assert_eq!(
        u64::from_le_bytes(bytes[0x18..0x20].try_into().unwrap()),
        0x402000
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x20..0x28].try_into().unwrap()),
        0x40
    );
}

#[test]
fn native_heap_strings_tables_and_drops_execute() {
    let base = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&base).expect("temp dir");
    let exe = base.join("lusa_bootstrap_heap.exe");
    let elf = base.join("lusa_bootstrap_heap.elf");
    let exe_path = exe.to_string_lossy().replace('\\', "/");
    let elf_path = elf.to_string_lossy().replace('\\', "/");

    let src = program(&exe_path, &elf_path);
    let out = lusa::run_source(&src).expect("heap bootstrap failed");
    assert_eq!(out.join("\n"), "heap image ok");

    let pe = std::fs::read(&exe).expect("generated heap PE");
    let elf_bytes = std::fs::read(&elf).expect("generated heap ELF");
    assert_eq!(&pe[0..2], b"MZ");
    assert_elf(&elf_bytes);

    let run = std::process::Command::new(&exe)
        .output()
        .expect("run generated heap PE");
    assert_eq!(run.status.code(), Some(42), "heap PE exit: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    assert_eq!(
        stdout,
        "hello 5\n2 a b\nhe!\nhe!\n3 20\n10\nnllo\nz!\ncllo\ngllo\n"
    );

    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&elf);
}
