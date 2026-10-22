//! Runs the self-hosted lexer (`lexer.lusa`) through both engines.
//!
//! The library entry point is stripped from the file so the test can append a
//! Lusa harness that inspects the returned token table.  This avoids changing
//! the process working directory or the global package search path.

const LEXER: &str = include_str!("../lexer.lusa");

fn program() -> String {
    let body = LEXER
        .trim_end()
        .strip_suffix("return move(Lexer)")
        .expect("lexer.lusa must end with `return move(Lexer)`");

    format!(
        r#"{body}

local dq = string.char(34)
local bs = string.char(92)
local nl = string.char(10)

local source =
    "local x = 0x1F + 1.5e-2" .. nl ..
    "if x >= 1 then" .. nl ..
    "    print(" .. dq .. "hi " .. bs .. "{{x}}" .. dq .. ")" .. nl ..
    "end" .. nl ..
    "--[[ block" .. nl .. "comment ]]" .. nl ..
    "move borrow clone" .. nl

local tokens = Lexer.lex(source)
assert(#tokens == 20)
assert(tokens[1].kind == "keyword")
assert(tokens[1].value == "local")
assert(tokens[4].kind == "number")
assert(tokens[4].value == "0x1F")
assert(tokens[6].value == "1.5e-2")
assert(tokens[9].value == ">=")
assert(tokens[14].kind == "string")
assert(tokens[14].interpolated == true)
assert(tokens[14].text == "hi " .. bs .. "{{x}}")
assert(tokens[14].line == 3)
assert(tokens[14].column == 11)
assert(tokens[17].value == "move")
assert(tokens[18].value == "borrow")
assert(tokens[19].value == "clone")
assert(tokens[20].kind == "eof")
assert(tokens[20].line == 8)

local bom_tokens = Lexer.lex(string.char(65279) .. "local x = 1")
assert(bom_tokens[1].kind == "keyword")
assert(bom_tokens[1].column == 1)
print("tokens ok")

local function expect_error(label, text)
    local ok, err = pcall(function() return Lexer.lex(text) end)
    assert(ok == false)
    assert(string.find(err, "lexer:", 1, true) ~= nil)
end

expect_error("string", "local x = " .. dq .. "oops")
expect_error("interpolation", "local x = " .. dq .. "oops " .. bs .. "{{a")
expect_error("character", "local x = @")
expect_error("hex", "local x = 0x")
expect_error("escape", "local x = " .. dq .. bs .. "q" .. dq)
print("errors ok")
"#
    )
}

#[test]
fn self_hosted_lexer_tokenizes_and_reports_errors() {
    let src = program();
    let expected = "tokens ok\nerrors ok";

    let interp = lusa::run_source(&src).expect("interpreter: lexer.lusa failed");
    assert_eq!(interp.join("\n"), expected);

    let vm = lusa::vm::run_source_vm(&src).expect("vm: lexer.lusa failed");
    assert_eq!(vm.join("\n"), expected);
}
