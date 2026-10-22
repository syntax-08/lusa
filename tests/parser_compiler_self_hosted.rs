//! Runs the self-hosted parser and compiler through both engines.
//!
//! Each module is wrapped in its own `do ... end` block so their top-level
//! locals (including the local `move` helpers) cannot shadow one another.  The
//! harness asserts AST shape, an exact byte image, traversal ownership, jump
//! targets, and the parser/compiler error paths.

const LEXER: &str = include_str!("../lexer.lusa");
const PARSER: &str = include_str!("../parser.lusa");
const COMPILER: &str = include_str!("../compiler.lusa");

fn strip_module<'a>(source: &'a str, suffix: &str) -> &'a str {
    source
        .trim_end()
        .strip_suffix(suffix)
        .unwrap_or_else(|| panic!("module must end with `{suffix}`"))
}

fn program() -> String {
    let lexer = strip_module(LEXER, "return move(Lexer)");
    let parser = strip_module(PARSER, "return move(Parser)");
    let compiler = strip_module(COMPILER, "return move(Compiler)");

    let mut src = String::from("local M6Lexer, M6Parser, M6Compiler\n");
    src.push_str("do\n");
    src.push_str(lexer);
    src.push_str("\nM6Lexer = move(Lexer)\nend\n");
    src.push_str("do\n");
    src.push_str(parser);
    src.push_str("\nM6Parser = move(Parser)\nend\n");
    src.push_str("do\n");
    src.push_str(compiler);
    src.push_str("\nM6Compiler = move(Compiler)\nend\n");

    src.push_str(
        r#"
local nl = string.char(10)
local dq = string.char(34)
local bs = string.char(92)

-- ===== AST shape and precedence =====

local source =
    "local x = 1 + 2 * 3" .. nl ..
    "if x >= 7 then" .. nl ..
    "    x = x - 1" .. nl ..
    "elseif x < 0 then" .. nl ..
    "    x = x + 1" .. nl ..
    "else" .. nl ..
    "    x = 0" .. nl ..
    "end" .. nl ..
    "while x < 10 do" .. nl ..
    "    x = x + 2" .. nl ..
    "end" .. nl ..
    "repeat" .. nl ..
    "    x = x - 1" .. nl ..
    "until x <= 0" .. nl ..
    "print(x)" .. nl ..
    "return x" .. nl

local ast = M6Parser.parse(M6Lexer.lex(source))
assert(ast.tag == "Program")
assert(#ast.body == 6)
assert(ast.body[1].tag == "Local")
assert(ast.body[1].names[1] == "x")
assert(ast.body[1].values[1].tag == "Binary")
assert(ast.body[1].values[1].op == "+")
assert(ast.body[1].values[1].lhs.value == 1)
assert(ast.body[1].values[1].rhs.tag == "Binary")
assert(ast.body[1].values[1].rhs.op == "*")
assert(ast.body[1].values[1].rhs.lhs.value == 2)
assert(ast.body[1].values[1].rhs.rhs.value == 3)

assert(ast.body[2].tag == "If")
assert(#ast.body[2].branches == 2)
assert(ast.body[2].branches[1].cond.op == ">=")
assert(ast.body[2].branches[2].cond.op == "<")
assert(ast.body[2].else_body ~= nil)
assert(ast.body[2].else_body[1].tag == "Assign")

assert(ast.body[3].tag == "While")
assert(ast.body[3].cond.op == "<")
assert(ast.body[3].body[1].tag == "Assign")

assert(ast.body[4].tag == "Repeat")
assert(ast.body[4].cond.op == "<=")
assert(ast.body[4].body[1].tag == "Assign")

assert(ast.body[5].tag == "CallStmt")
assert(ast.body[5].value.tag == "Call")
assert(ast.body[5].value.callee.name == "print")
assert(ast.body[5].value.args[1].tag == "Name")
assert(ast.body[5].value.args[1].name == "x")

assert(ast.body[6].tag == "Return")
assert(ast.body[6].values[1].tag == "Name")
assert(ast.body[6].values[1].name == "x")

local hex = M6Parser.parse(M6Lexer.lex("return 0x1F"))
assert(hex.body[1].values[1].tag == "Number")
assert(hex.body[1].values[1].value == 31)

local escaped = M6Parser.parse(M6Lexer.lex(
    "return " .. dq .. bs .. "u{41}" .. dq))
assert(escaped.body[1].values[1].tag == "String")
assert(escaped.body[1].values[1].value == "A")
print("ast ok")

-- ===== exact image for `local a = 1 + 2 return a` =====

local simple = M6Parser.parse(M6Lexer.lex(
    "local a = 1 + 2" .. nl .. "return a"))
local saved = clone simple
local image = M6Compiler.compile(clone simple)

assert(image.version == 1)
assert(image.nlocals == 1)
assert(#image.code == 19)
assert(#image.lines == #image.code)
assert(#image.constants == 2)
assert(image.constants[1].kind == "number")
assert(image.constants[1].value == 1)
assert(image.constants[2].kind == "number")
assert(image.constants[2].value == 2)

local expected = {
    0, 0, 0,       -- LoadConst 0 (1)
    0, 0, 1,       -- LoadConst 1 (2)
    21,            -- Add
    14, 0, 0,      -- SetLoc slot 0
    2, 0, 0,       -- GetLoc slot 0
    61, 0, 1,      -- ReturnFixed 1
    61, 0, 0       -- implicit ReturnFixed 0
}
assert(#image.code == #expected)
for i = 1, #expected do
    assert(image.code[i] == expected[i],
        "byte " .. i .. " = " .. tostring(image.code[i]))
end

-- The compiler consumed `clone simple`, not `saved`.
assert(saved.tag == "Program")
assert(saved.body[1].tag == "Local")
assert(saved.body[1].names[1] == "a")
assert(saved.body[2].values[1].name == "a")
print("image ok")

-- ===== instruction decoder / jump validation =====

local operand_bytes = {
    [0] = 2, [1] = 0, [2] = 2, [4] = 2,
    [13] = 2, [14] = 2,
    [21] = 0, [22] = 0, [23] = 0, [24] = 0, [25] = 0, [26] = 0,
    [27] = 0, [28] = 0, [29] = 0, [30] = 0, [31] = 0, [32] = 0,
    [33] = 0, [34] = 0, [35] = 0, [36] = 0, [37] = 0,
    [38] = 0, [39] = 0,
    [42] = 4,
    [53] = 2, [54] = 2, [55] = 2, [56] = 2,
    [59] = 0, [61] = 2
}
local is_jump = {
    [53] = true, [54] = true, [55] = true, [56] = true
}

local function validate_image(img)
    assert(#img.code == #img.lines)
    for i = 1, #img.code do
        assert(img.code[i] >= 0 and img.code[i] <= 255)
    end
    local pc = 1
    while pc <= #img.code do
        local op = img.code[pc]
        local width = operand_bytes[op]
        assert(width ~= nil, "unknown opcode " .. tostring(op))
        if is_jump[op] then
            local target = img.code[pc + 1] * 256 + img.code[pc + 2]
            assert(target >= 1 and target <= #img.code + 1,
                "jump target " .. tostring(target))
        end
        pc = pc + 1 + width
    end
    assert(pc == #img.code + 1)
end

validate_image(image)

-- A small `if` fixes the branch target semantics: the false path must land
-- after the assignment, not at the end of the whole program.
local if_source =
    "local x = 1" .. nl ..
    "if x then x = 2 end" .. nl ..
    "return x"
local if_image = M6Compiler.compile(M6Parser.parse(M6Lexer.lex(if_source)))
local if_expected = {
    0, 0, 0,       -- LoadConst 0 (1)
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- GetLoc 0 (condition)
    54, 0, 19,     -- JumpIfFalse -> GetLoc after the body
    0, 0, 1,       -- LoadConst 1 (2)
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- GetLoc 0
    61, 0, 1,      -- ReturnFixed 1
    61, 0, 0       -- implicit ReturnFixed 0
}
assert(#if_image.code == #if_expected)
for i = 1, #if_expected do
    assert(if_image.code[i] == if_expected[i],
        "if byte " .. i .. " = " .. tostring(if_image.code[i]))
end
validate_image(if_image)

local while_source =
    "local x = 0" .. nl ..
    "while x < 2 do x = x + 1 end" .. nl ..
    "return x"
local while_image = M6Compiler.compile(M6Parser.parse(M6Lexer.lex(while_source)))
local while_expected = {
    0, 0, 0,       -- LoadConst 0 (0)
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- condition: GetLoc 0
    0, 0, 1,       -- LoadConst 1 (2)
    31,            -- Lt
    54, 0, 30,     -- JumpIfFalse -> after the loop
    2, 0, 0,       -- body: GetLoc 0
    0, 0, 2,       -- LoadConst 2 (1)
    21,            -- Add
    14, 0, 0,      -- SetLoc 0
    53, 0, 7,      -- Jump back to the condition
    2, 0, 0,       -- GetLoc 0
    61, 0, 1,      -- ReturnFixed 1
    61, 0, 0       -- implicit ReturnFixed 0
}
assert(#while_image.code == #while_expected)
for i = 1, #while_expected do
    assert(while_image.code[i] == while_expected[i],
        "while byte " .. i .. " = " .. tostring(while_image.code[i]))
end
validate_image(while_image)

local repeat_source =
    "local x = 0" .. nl ..
    "repeat x = x + 1 until x >= 2" .. nl ..
    "return x"
local repeat_image = M6Compiler.compile(M6Parser.parse(M6Lexer.lex(repeat_source)))
local repeat_expected = {
    0, 0, 0,       -- LoadConst 0 (0)
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- body: GetLoc 0
    0, 0, 1,       -- LoadConst 1 (1)
    21,            -- Add
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- condition: GetLoc 0
    0, 0, 2,       -- LoadConst 2 (2)
    34,            -- Ge
    54, 0, 7,      -- JumpIfFalse back to the body
    2, 0, 0,       -- GetLoc 0
    61, 0, 1,      -- ReturnFixed 1
    61, 0, 0       -- implicit ReturnFixed 0
}
assert(#repeat_image.code == #repeat_expected)
for i = 1, #repeat_expected do
    assert(repeat_image.code[i] == repeat_expected[i],
        "repeat byte " .. i .. " = " .. tostring(repeat_image.code[i]))
end
validate_image(repeat_image)

-- Logical expressions use the VM's keep-jump opcodes and must not emit an
-- extra Pop on the evaluation path.
local logical_source =
    "local a = true and false or true" .. nl ..
    "return a"
local logical_image = M6Compiler.compile(M6Parser.parse(M6Lexer.lex(logical_source)))
local logical_expected = {
    0, 0, 0,       -- LoadConst 0 (true)
    56, 0, 10,     -- and: keep false / pop true
    0, 0, 1,       -- LoadConst 1 (false)
    55, 0, 16,     -- or: keep true / pop false
    0, 0, 0,       -- LoadConst 0 (true)
    14, 0, 0,      -- SetLoc 0
    2, 0, 0,       -- GetLoc 0
    61, 0, 1,      -- ReturnFixed 1
    61, 0, 0       -- implicit ReturnFixed 0
}
assert(#logical_image.code == #logical_expected)
for i = 1, #logical_expected do
    assert(logical_image.code[i] == logical_expected[i],
        "logical byte " .. i .. " = " .. tostring(logical_image.code[i]))
end
validate_image(logical_image)

-- Compile the representative control-flow AST through a clone; `if`,
-- `elseif`, `else`, `while`, `repeat` and calls all emit jumps here.
local control_image = M6Compiler.compile(clone ast)
assert(control_image.nlocals == 1)
validate_image(control_image)
assert(ast.body[2].tag == "If")
assert(ast.body[4].tag == "Repeat")

-- ===== extended nodes and ownership =====

local extended_source =
    "local t = nil" .. nl ..
    "local flag = true and false or true" .. nl ..
    "do" .. nl ..
    "    while true do" .. nl ..
    "        break" .. nl ..
    "    end" .. nl ..
    "end" .. nl ..
    "local s = " .. dq .. "hi" .. bs .. "n" .. dq .. nl ..
    "local b = borrow t" .. nl ..
    "return clone t" .. nl

local extended = M6Parser.parse(M6Lexer.lex(extended_source))
assert(#extended.body == 6)
assert(extended.body[2].values[1].tag == "Logical")
assert(extended.body[2].values[1].op == "or")
assert(extended.body[2].values[1].lhs.tag == "Logical")
assert(extended.body[2].values[1].lhs.op == "and")
assert(extended.body[3].tag == "Do")
assert(extended.body[3].body[1].tag == "While")
assert(extended.body[3].body[1].body[1].tag == "Break")
assert(extended.body[4].values[1].tag == "String")
assert(extended.body[4].values[1].value == "hi" .. nl)
assert(extended.body[4].values[1].interpolated == false)
assert(extended.body[5].values[1].tag == "Borrow")
assert(extended.body[6].values[1].tag == "Clone")

local extended_image = M6Compiler.compile(clone extended)
assert(extended_image.nlocals == 4)
validate_image(extended_image)

-- `extended` was cloned for compilation and remains intact.
assert(extended.body[1].tag == "Local")
assert(extended.body[6].values[1].tag == "Clone")
print("extended ok")

-- ===== errors =====

local function expect_parser_error(text)
    local ok, err = pcall(function()
        return M6Parser.parse(M6Lexer.lex(text))
    end)
    assert(ok == false)
    assert(string.find(err, "parser:1:", 1, true) ~= nil, err)
end

local table_ast = M6Parser.parse(M6Lexer.lex("local x = {}"))
local table_node = table_ast.body[1].values[1]
assert(table_node.tag == "Table")
assert(#table_node.items == 0)

local index_ast = M6Parser.parse(M6Lexer.lex("local x = t[1]"))
local index_node = index_ast.body[1].values[1]
assert(index_node.tag == "Index")
local index_table = index_node.table
assert(index_table.tag == "Name")

local field_ast = M6Parser.parse(M6Lexer.lex("local x = a.b"))
assert(field_ast.body[1].values[1].tag == "Field")
assert(field_ast.body[1].values[1].field_name == "b")
local keyed_ast = M6Parser.parse(M6Lexer.lex("local x = { id = 1, name = 7 }"))
assert(keyed_ast.body[1].values[1].tag == "Table")
assert(keyed_ast.body[1].values[1].keys[1] == "id")
assert(keyed_ast.body[1].values[1].keys[2] == "name")
expect_parser_error("t[1] = 2")

local ok, err = pcall(function()
    local a = M6Parser.parse(M6Lexer.lex(
        "local s = " .. dq .. bs .. "{x}" .. dq))
    return M6Compiler.compile(clone a)
end)
assert(ok == false)
assert(string.find(err, "compiler:1:", 1, true) ~= nil, err)

local ok2, err2 = pcall(function()
    return M6Compiler.compile({})
end)
assert(ok2 == false)
assert(string.find(err2, "compiler:", 1, true) ~= nil, err2)

local ok3, err3 = pcall(function()
    local a = M6Parser.parse(M6Lexer.lex("break"))
    return M6Compiler.compile(clone a)
end)
assert(ok3 == false)
assert(string.find(err3, "compiler:1:", 1, true) ~= nil, err3)
print("errors ok")
"#,
    );

    src
}

#[test]
fn self_hosted_parser_and_compiler_build_an_image() {
    let src = program();
    let expected = "ast ok\nimage ok\nextended ok\nerrors ok";

    let interp = lusa::run_source(&src).expect("interpreter: parser/compiler failed");
    assert_eq!(interp.join("\n"), expected);

    let vm = lusa::vm::run_source_vm(&src).expect("vm: parser/compiler failed");
    assert_eq!(vm.join("\n"), expected);
}
