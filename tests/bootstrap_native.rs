//! Module 7: ownership-safe optimizer and direct PE32+/ELF64 emission.
//!
//! The suite composes the self-hosted modules in separate `do ... end` blocks,
//! verifies optimizer/ownership invariants, compiles scalar programs to real
//! Windows and Linux images, runs both engines, and executes the generated PE.

const LEXER: &str = include_str!("../lexer.lusa");
const PARSER: &str = include_str!("../parser.lusa");
const BOOTSTRAP: &str = include_str!("../compiler_bootstrap.lusa");

fn strip_module<'a>(source: &'a str, suffix: &str) -> &'a str {
    source
        .trim_end()
        .strip_suffix(suffix)
        .unwrap_or_else(|| panic!("module must end with `{suffix}`"))
}

fn program(
    exe_path: &str,
    elf_path: &str,
    feature_path: &str,
    gap_path: &str,
    self_ok_path: &str,
    access: &str,
) -> String {
    let lexer = strip_module(LEXER, "return move(Lexer)");
    let parser = strip_module(PARSER, "return move(Parser)");
    let bootstrap = strip_module(BOOTSTRAP, "return move(Compiler)");

    let mut src = String::from("local M7Lexer, M7Parser, M7Compiler\n");
    src.push_str("do\n");
    src.push_str(lexer);
    src.push_str("\nM7Lexer = move(Lexer)\nend\n");
    src.push_str("do\n");
    src.push_str(parser);
    src.push_str("\nM7Parser = move(Parser)\nend\n");
    src.push_str("do\n");
    src.push_str(bootstrap);
    src.push_str("\nM7Compiler = move(Compiler)\nend\n");

    src.push_str(
        r#"
local nl = string.char(10)
local q = string.char(34)

-- A seed that stays inside the native scalar subset.
local seed =
    "local x = 20" .. nl ..
    "local y = 22" .. nl ..
    "if x + y == 42 then" .. nl ..
    "    print(" .. q .. "native ok" .. q .. ")" .. nl ..
    "end" .. nl ..
    "return x + y" .. nl

local ast = M7Parser.parse(M7Lexer.lex(seed))
local saved = clone ast
local optimized = M7Compiler.optimize(clone ast)

-- optimize consumed only the clone; the original AST is untouched.
assert(saved.tag == "Program")
assert(saved.body[1].tag == "Local")
assert(saved.body[1].names[1] == "x")
assert(saved.body[1].values[1].tag == "Number")
assert(saved.body[1].values[1].value == 20)
assert(saved.body[3].tag == "If")
assert(saved.body[4].values[1].tag == "Binary")

-- __ALIAS_TEST__

-- emit_binary must be read-only for its input.
local pe = M7Compiler.emit_binary(__ACCESS__, "windows")
assert(#pe > 512)
assert(pe[1] == 0x4D and pe[2] == 0x5A)
assert(pe[0x3D] == 0x40) -- e_lfanew = 0x40 (1-based table index 0x3D)
assert(pe[0x41] == 0x50 and pe[0x42] == 0x45)
assert(saved.body[1].names[1] == "x")
assert(io_write_binary("__EXE__", pe))
local pe_back = io_read_binary("__EXE__")
assert(#pe_back == #pe)
assert(pe_back[1] == pe[1] and pe_back[2] == pe[2] and pe_back[#pe_back] == pe[#pe])

local elf = M7Compiler.emit_binary(clone saved, "linux")
assert(#elf > 512)
assert(elf[1] == 0x7F and elf[2] == 0x45 and elf[3] == 0x4C and elf[4] == 0x46)
assert(io_write_binary("__ELF__", elf))

-- bootstrap() lexes/parses the source and must match the AST pipeline.
local boot = M7Compiler.bootstrap(clone M7Lexer, clone M7Parser, seed, "windows")
assert(boot.target == "windows")
assert(boot.source_bytes == #seed)
assert(#boot.image == #pe)
local bi = 1
while bi <= #pe do
    assert(boot.image[bi] == pe[bi])
    bi = bi + 1
end

local fix = M7Compiler.verify_fixpoint(clone M7Lexer, clone M7Parser, seed, "linux")
assert(fix.equal)
assert(#fix.first.image == #elf)
assert(#fix.second.image == #elf)

-- A richer scalar program exercises loops, globals, assignments, logical
-- expressions, bool/nil printing, and multiple print arguments.
local feature_seed =
    "local a = 5" .. nl ..
    "local b = 7" .. nl ..
    "local total = 0" .. nl ..
    "local i = 0" .. nl ..
    "while i < 3 do" .. nl ..
    "    total = total + a + b" .. nl ..
    "    i = i + 1" .. nl ..
    "end" .. nl ..
    "repeat" .. nl ..
    "    total = total - 1" .. nl ..
    "until total == 33" .. nl ..
    "local j = 0" .. nl ..
    "while true do" .. nl ..
    "    j = j + 1" .. nl ..
    "    if j == 2 then break end" .. nl ..
    "end" .. nl ..
    "do" .. nl ..
    "    local flag = (a < b) and not false" .. nl ..
    "    if total ~= 33 then" .. nl ..
    "        print(" .. q .. "bad" .. q .. ")" .. nl ..
    "    elseif flag then" .. nl ..
    "        print(" .. q .. "sum" .. q .. ", total, a + b, 1 + 2 * 3)" .. nl ..
    "    else" .. nl ..
    "        print(" .. q .. "bad" .. q .. ")" .. nl ..
    "    end" .. nl ..
    "end" .. nl ..
    "print(" .. q .. "j" .. q .. ", j)" .. nl ..
    "print(true, false, nil)" .. nl ..
    "return total" .. nl
local feature_ast = M7Parser.parse(M7Lexer.lex(feature_seed))
local feature = M7Compiler.emit_binary(feature_ast, "windows")
assert(io_write_binary("__FEATURE__", feature))

-- The optimizer must fold constants and remove dead statements.
local fold_ast = M7Parser.parse(M7Lexer.lex(
    "local a = 1 + 2 * 3" .. nl .. "return a"))
local folded = M7Compiler.optimize(fold_ast)
assert(folded.body[1].values[1].tag == "Number")
assert(folded.body[1].values[1].value == 7)

-- A false head must not swallow a later, still-dynamic condition.
local elseif_ast = M7Parser.parse(M7Lexer.lex(
    "if false then print(" .. q .. "a" .. q .. ")" .. nl ..
    "elseif x == 1 then print(" .. q .. "b" .. q .. ")" .. nl ..
    "else print(" .. q .. "c" .. q .. ") end"))
local elseif_opt = M7Compiler.optimize(elseif_ast)
assert(elseif_opt.body[1].tag == "If")
assert(#elseif_opt.body[1].branches == 1)
assert(elseif_opt.body[1].branches[1].cond.tag == "Binary")

local true_ast = M7Parser.parse(M7Lexer.lex(
    "if true then print(" .. q .. "yes" .. q .. ") else print(" .. q .. "no" .. q .. ") end"))
local true_opt = M7Compiler.optimize(true_ast)
local false_ast = M7Parser.parse(M7Lexer.lex("if false then print(" .. q .. "yes" .. q .. ") else print(" .. q .. "no" .. q .. ") end"))
local false_opt = M7Compiler.optimize(false_ast)
assert(false_opt.body[1].tag == "Do")
assert(false_opt.body[1].body[1].tag == "CallStmt")
assert(true_opt.body[1].tag == "Do")
assert(true_opt.body[1].body[1].tag == "CallStmt")

local while_ast = M7Parser.parse(M7Lexer.lex(
    "while false do print(" .. q .. "never" .. q .. ") end"))
local while_opt = M7Compiler.optimize(while_ast)
assert(while_opt.body[1].tag == "Do")
assert(#while_opt.body[1].body == 0)

local dead_ast = {
    tag = "Program", line = 1, column = 1,
    body = {
        {tag = "Return", line = 1, column = 1, values = {
            {tag = "Number", value = 1, line = 1, column = 1}
        }},
        {tag = "Local", line = 2, column = 1, names = {"x"}, values = {
            {tag = "Number", value = 2, line = 2, column = 1}
        }}
    }
}
local dead = M7Compiler.optimize(dead_ast)
assert(#dead.body == 1)
assert(dead.body[1].tag == "Return")

local mem = M7Compiler.memory_layout()
assert(mem.scalar ~= nil)
assert(mem.value_header.length ~= nil)
assert(mem.table ~= nil)
assert(mem.closure ~= nil)
assert(mem.drop_rule ~= nil)
assert(mem.clone_rule ~= nil)
assert(mem.borrow_rule ~= nil)

local ok_target, err_target = pcall(function()
    M7Compiler.emit_binary(clone saved, "macos")
end)
assert(ok_target == false)
assert(string.find(err_target, "unsupported native target", 1, true) ~= nil)

local bad_ast = {
    tag = "Program", line = 1, column = 1,
    body = {
        {tag = "Local", line = 1, column = 1, names = {"t"}, values = {
            {tag = "Table", line = 1, column = 1, items = {}}
        }}
    }
}
local ok_node, err_node = pcall(function()
    M7Compiler.emit_binary(bad_ast, "windows")
end)
assert(ok_node == false)
assert(string.find(err_node, "native backend does not support AST node Table",
    1, true) ~= nil)

local gap_ok, gap_result = pcall(function()
    return M7Compiler.self_compile(clone M7Lexer, clone M7Parser, "windows", "__GAP__")
end)
assert(gap_ok == true)
assert(gap_result.ok == false)
assert(gap_result.stage == "frontend")
assert(string.find(gap_result.error, "parser:", 1, true) ~= nil)

local self_ok, self_image = pcall(function()
    return M7Compiler.self_compile(clone M7Lexer, clone M7Parser, "windows", "__SELF_OK__")
end)
assert(self_ok == true)
assert(self_image.ok == true)
assert(self_image.target == "windows")
assert(self_image.image_bytes == #self_image.image)
assert(#self_image.image > 512)
assert(self_image.image[1] == 0x4D and self_image.image[2] == 0x5A)

print("native image ok")
"#,
    );
    src = src.replace("__ACCESS__", access);
    // `borrow` is interpreter-only, so exercise deep-cloning through a
    // top-level alias only on that engine.
    let alias_test = if access.contains("borrow") {
        "-- Cloning through a top-level borrow used to re-enter the registry.\n\
         local alias = borrow saved\n\
         local copied = clone alias\n\
         assert(copied.tag == \"Program\")\n\
         assert(copied.body[1].names[1] == \"x\")"
    } else {
        "-- The VM has no borrow opcode."
    };
    src = src.replace("-- __ALIAS_TEST__", alias_test);
    src = src.replace("__EXE__", exe_path);
    src = src.replace("__ELF__", elf_path);
    src = src.replace("__FEATURE__", feature_path);
    src = src.replace("__GAP__", gap_path);
    src = src.replace("__SELF_OK__", self_ok_path);

    src
}

fn assert_elf(bytes: &[u8]) {
    assert_eq!(&bytes[0..4], b"\x7FELF");
    assert_eq!(bytes[4], 2, "ELFCLASS64");
    assert_eq!(bytes[5], 1, "little endian");
    assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), 2, "ET_EXEC");
    assert_eq!(
        u16::from_le_bytes([bytes[18], bytes[19]]),
        0x3E,
        "EM_X86_64"
    );
    assert_eq!(
        u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
        0x402000,
        "entry at code_base"
    );
    assert_eq!(
        u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
        0x40,
        "program header offset"
    );
    assert_eq!(u16::from_le_bytes([bytes[54], bytes[55]]), 56);
    assert_eq!(u16::from_le_bytes([bytes[56], bytes[57]]), 1);
    assert_eq!(
        u32::from_le_bytes(bytes[0x40..0x44].try_into().unwrap()),
        1,
        "PT_LOAD"
    );
    assert_eq!(
        u32::from_le_bytes(bytes[0x44..0x48].try_into().unwrap()),
        7,
        "R|W|X"
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x48..0x50].try_into().unwrap()),
        0
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x50..0x58].try_into().unwrap()),
        0x400000
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x58..0x60].try_into().unwrap()),
        0x400000
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x60..0x68].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x68..0x70].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(
        u64::from_le_bytes(bytes[0x70..0x78].try_into().unwrap()),
        0x1000
    );
}

#[test]
fn bootstrap_emits_runnable_pe_and_well_formed_elf_on_both_engines() {
    let base = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&base).expect("temp dir");
    let exe = base.join("lusa_bootstrap_native.exe");
    let elf = base.join("lusa_bootstrap_native.elf");
    let feature = base.join("lusa_bootstrap_features.exe");
    // Full self_compile() intentionally exercises the front-end gap.  Use a
    // tiny unsupported source instead of the 50 KiB compiler: the Module-5
    // lexer is quadratic over long sources, and the parser rejects the table
    // constructor immediately anyway.
    let gap = base.join("lusa_self_compile_gap.lusa");
    std::fs::write(&gap, "local Compiler = {}\nreturn Compiler\n").expect("write gap source");
    let self_ok = base.join("lusa_self_compile_ok.lusa");
    std::fs::write(&self_ok, "print(\"self ok\")\nreturn 0\n").expect("write supported source");
    let exe_path = exe.to_string_lossy().replace('\\', "/");
    let elf_path = elf.to_string_lossy().replace('\\', "/");
    let feature_path = feature.to_string_lossy().replace('\\', "/");
    let gap_path = gap.to_string_lossy().replace('\\', "/");
    let self_ok_path = self_ok.to_string_lossy().replace('\\', "/");

    // Interpreter: exercise the public borrowed-AST contract.
    let interp_src = program(
        &exe_path,
        &elf_path,
        &feature_path,
        &gap_path,
        &self_ok_path,
        "borrow saved",
    );
    let interp = lusa::run_source(&interp_src).expect("interpreter: bootstrap failed");
    assert_eq!(interp.join("\n"), "native image ok");
    let interp_pe = std::fs::read(&exe).expect("interpreter PE");
    let interp_elf = std::fs::read(&elf).expect("interpreter ELF");
    let interp_feature = std::fs::read(&feature).expect("interpreter feature PE");

    // VM: `borrow` is not executable there, so use an owned clone.  The
    // generated images must be byte-identical because emit_binary snapshots
    // and optimizes exactly the same body.
    let vm_src = program(
        &exe_path,
        &elf_path,
        &feature_path,
        &gap_path,
        &self_ok_path,
        "clone saved",
    );
    let vm = lusa::vm::run_source_vm(&vm_src).expect("vm: bootstrap failed");
    assert_eq!(vm.join("\n"), "native image ok");
    let vm_pe = std::fs::read(&exe).expect("vm PE");
    let vm_elf = std::fs::read(&elf).expect("vm ELF");
    let vm_feature = std::fs::read(&feature).expect("vm feature PE");
    assert_eq!(interp_pe, vm_pe, "PE output must be deterministic across engines");
    assert_eq!(interp_elf, vm_elf, "ELF output must be deterministic across engines");
    assert_eq!(interp_feature, vm_feature);

    // PE32+ headers, imports, and a real Windows process.
    assert_eq!(&interp_pe[0..2], b"MZ");
    assert_eq!(&interp_pe[0x3C..0x40], &0x40u32.to_le_bytes());
    assert_eq!(&interp_pe[0x40..0x44], b"PE\0\0");
    assert_eq!(u16::from_le_bytes([interp_pe[0x44], interp_pe[0x45]]), 0x8664);
    assert_eq!(
        u16::from_le_bytes([interp_pe[0x58], interp_pe[0x59]]),
        0x20B,
        "PE32+ optional header"
    );
    assert_eq!(
        u32::from_le_bytes(interp_pe[0x44 + 20 + 112 + 8..][..4].try_into().unwrap()),
        0x3000,
        "import directory RVA"
    );

    let run = std::process::Command::new(&exe)
        .output()
        .expect("run generated PE");
    assert_eq!(
        run.status.code(),
        Some(42),
        "generated PE must return the seed's exit value: {run:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n"),
        "native ok\n"
    );

    let feature_run = std::process::Command::new(&feature)
        .output()
        .expect("run generated feature PE");
    assert_eq!(
        feature_run.status.code(),
        Some(33),
        "feature PE exit value: {feature_run:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&feature_run.stdout).replace("\r\n", "\n"),
        "sum 33 12 7\nj 2\ntrue false nil\n"
    );

    // ELF64 header and the single RX load segment.
    assert_elf(&interp_elf);
    assert_elf(&vm_elf);

    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&elf);
    let _ = std::fs::remove_file(&feature);
    let _ = std::fs::remove_file(&gap);
    let _ = std::fs::remove_file(&self_ok);
}
