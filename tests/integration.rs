//! Integration tests: run whole `.lusa` programs through the library API.
//! Success paths compare printed output; failure paths compare error codes.

use lusa::run_source;

fn out(src: &str) -> String {
    run_source(src).expect("expected success").join("\n")
}

fn err_code(src: &str) -> String {
    run_source(src).expect_err("expected an error").code.to_string()
}

// ===== basics =====

#[test]
fn numbers_and_operators() {
    assert_eq!(out("print(1 + 2 * 3)"), "7");
    assert_eq!(out("print(7 / 2, 7 // 2, 7 % 3, 2 ^ 3, -(3 + 1))"), "3.5 3 1 8 -4");
    assert_eq!(out("print(#\"abcd\")"), "4");
    assert_eq!(out("print(-10 // 3)"), "-4"); // floor division
}

#[test]
fn strings_and_interpolation() {
    assert_eq!(out("print(\"hello, world\")"), "hello, world");
    assert_eq!(out("print(\"n: \\{1+2} s: \\{\"x\"}\")"), "n: 3 s: x");
    assert_eq!(out("print(\"a\\tb\\u{26a1}\")"), "a\tb⚡");
    assert_eq!(out("print(\"x\" .. \"y\" .. 1)"), "xy1");
}

#[test]
fn truthiness_and_if() {
    let src = r#"
if not false and nil then
    print("a")
elseif 0 then
    print("b")
else
    print("c")
end
"#;
    assert_eq!(out(src).trim(), "b"); // 0 is truthy in Lua
}

#[test]
fn loops() {
    assert_eq!(out("for i = 1, 3 do print(i) end"), "1\n2\n3");
    assert_eq!(out("for i = 10, 4, -2 do print(i) end"), "10\n8\n6\n4");
    let w = "local i = 0 while i < 3 do i = i + 1 end print(i)";
    assert_eq!(out(w), "3");
    let r = "local i = 0 repeat i = i + 1 until i >= 2 print(i)";
    assert_eq!(out(r), "2");
    let b = "for i = 1, 5 do if i == 3 then break end print(i) end";
    assert_eq!(out(b), "1\n2");
}

#[test]
fn tables_and_fields() {
    let t = "local t = {1, 2, 3, name = \"ana\", [\"k\"] = 9} print(t[2], t.name, t.k, #t)";
    assert_eq!(out(t), "2 ana 9 3");
    let n = "local n = {} n.x = 1 n[\"y\"] = 2 print(n.x, n.y)";
    assert_eq!(out(n), "1 2");
}

#[test]
fn functions_and_multiple_returns() {
    let f = "function f(a,b) return a+b, a*b end print(f(3,4))";
    assert_eq!(out(f), "7 12");
    let v = "local function v(...) return ... end print(v(1, 2, 3))";
    assert_eq!(out(v), "1 2 3");
    assert_eq!(out("local a, b, c = 1, 2 print(a, b, c)"), "1 2 nil");
}

#[test]
fn closures_capture() {
    let c = r#"
function contador()
    local n = 0
    return function() n = n + 1 return n end
end
local k = contador()
k() k()
print(k())
"#;
    assert_eq!(out(c), "3");
}

#[test]
fn methods() {
    let m = r#"
local o = {n = 10}
function o:soma(v) self.n = self.n + v return self.n end
print(o:soma(5), o:soma(2), o.n)
"#;
    assert_eq!(out(m), "15 17 17");
}

// ===== stdlib =====

#[test]
fn stdlib_basics() {
    assert_eq!(out("print(type(1), type(nil), type(\"\"), type({}), type(print))")
        , "number nil string table function");
    assert_eq!(out("print(tostring(1.5), tostring(false), tonumber(\"42\"), tonumber(\"no\"))")
        , "1.5 false 42 nil");
    assert_eq!(out("print(math.floor(9.9), math.abs(-3), math.max(2, 9))"), "9 3 9");
    assert_eq!(out("local t = {\"b\", \"a\"} table.sort(t) print(table.concat(t, \",\"))"), "a,b");
    assert_eq!(out("print(string.upper(\"ok\"), string.sub(\"wiki\", 1, 2))"), "OK wi");
    assert_eq!(out("print(string.format(\"%.2f\", 1.239))"), "1.24");
}

#[test]
fn iterators() {
    let ip = "for i, v in ipairs({\"x\", \"y\"}) do print(i, v) end";
    assert_eq!(out(ip), "1 x\n2 y");
    let pr = "for k, v in pairs({z = 1}) do print(k, v) end";
    assert_eq!(out(pr), "z 1");
}

// ===== ownership =====

#[test]
fn move_then_use_is_E1001() {
    assert_eq!(err_code("local t = {1} local u = t print(t)"), "E1001");
    assert_eq!(err_code("local d = {1} local f = print local e = f(d) print(d)"), "E1001");
}

#[test]
fn move_while_borrowed_is_E1002() {
    assert_eq!(err_code("local t = {1} local a = borrow t local b = t"), "E1002");
    assert_eq!(err_code("local t = {1} local a = borrow t function g(x) end g(t)"), "E1002");
}

#[test]
fn write_through_imm_borrow_is_E1004() {
    assert_eq!(err_code("local t = {x = 1} local a = borrow t a.x = 2"), "E1004");
}

#[test]
fn clone_gives_independent_copy() {
    let c = "local t = {1} local u = clone t u[1] = 9 print(t[1], u[1])";
    assert_eq!(out(c), "1 9");
}

#[test]
fn borrow_reads_directly() {
    let b = "local t = {k = 5} local a = borrow t print(a.k)";
    assert_eq!(out(b), "5");
}

#[test]
fn methods_take_a_mut_borrow_of_self() {
    let m = r#"
local o = {n = 0}
function o:inc() self.n = self.n + 1 return self.n end
local a = o:inc()
local b = o:inc()
print(a, b, o.n)
"#;
    assert_eq!(out(m), "1 2 2");
}

#[test]
fn scalars_copy_freely() {
    let s = "local a = 1 local b = a local c = b print(a, b, c)";
    assert_eq!(out(s), "1 1 1");
    let t = "local a = \"x\" local b = a print(b)";
    assert_eq!(out(t), "x");
}

// ===== errors =====

#[test]
fn error_and_assert_codes() {
    assert_eq!(err_code("error(\"boom\")"), "E9000");
    assert_eq!(err_code("assert(false, \"nope\")"), "E9100");
    assert_eq!(err_code("local x = 1 + {}"), "E2001");
    assert_eq!(err_code("local x = {} .. \"a\""), "E2002");
    // reading an undefined global is Lua-forgiving: nil, not an error
    assert_eq!(out("print(missing)"), "nil");
}

#[test]
fn moved_field_reads_give_nil_after_move() {
    // A HEAP field read into a local moves the slot out: after that,
    // plain reads of the field come back as `nil` (Lua forgiveness)...
    let f = "local t = {a = {}} local x = t.a print(t.a)";
    assert_eq!(out(f), "nil");
    // ...but another *moving* read surfaces E1001-style rigor.
    assert_eq!(err_code("local t = {a = {}} local x = t.a local y = t.a"), "E1001");
}

// ===== package system (`require`) =====

#[test]
fn require_loads_package_and_exports() {
    // build a fake package tree in a temp dir and point LUSA_PACKAGES at it
    let base = std::env::temp_dir().join(format!("lusa-pkgtest-{}", std::process::id()));
    let pkg = base.join("fib");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("lusa.toml"),
        "name = \"fib\"\nversion = \"0.1.0\"\nentry = \"fib.lusa\"\n",
    )
    .unwrap();
    std::fs::write(
        pkg.join("fib.lusa"),
        r#"-- fib package: exports fib(n)
local function fib(n)
    if n < 2 then return n end
    return fib(n - 1) + fib(n - 2)
end

return {
    fib = clone fib,
    name = "fib",
}
"#,
    )
    .unwrap();

    std::env::set_var("LUSA_PACKAGES", &base);
    let result = run_source(
        r#"local f = require("fib")
local fib = f.fib
print(f.name, fib(10))
"#,
    );
    assert_eq!(result.expect("require should succeed").join("\n"), "fib 55");

    // missing package -> E9000 with install hint
    let miss = run_source("local x = require(\"nope\")");
    assert_eq!(miss.expect_err("missing package must error").code.to_string(), "E9000");

    std::env::remove_var("LUSA_PACKAGES");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn require_is_cached_across_module_nesting() {
    let base = std::env::temp_dir().join(format!("lusa-pkgtest2-{}", std::process::id()));
    let pkg = base.join("counter");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("init.lusa"),
        r#"-- prints once per LOAD; require() should cache, so this shows up exactly once
print("load: counter")
return { tag = "counter" }
"#,
    )
    .unwrap();
    std::env::set_var("LUSA_PACKAGES", &base);
    let result = run_source(
        r#"local a = require("counter")
local b = require("counter")
print(a.tag, b.tag)
"#,
    );
    assert_eq!(
        result.expect("require should succeed").join("\n"),
        "load: counter\ncounter counter"
    );
    std::env::remove_var("LUSA_PACKAGES");
    let _ = std::fs::remove_dir_all(&base);
}

// ===== pcall (protected calls) =====

#[test]
fn pcall_ok_and_results() {
    let src = r#"
local function f(x) return x * 2, x + 1 end
local ok, a, b = pcall(f, 5)
print(ok, a, b)
"#;
    assert_eq!(out(src), "true 10 6");
}

#[test]
fn pcall_catches_user_errors_and_resumes() {
    let src = r#"
local function boom(x) error("boom " .. x) end
local ok, err = pcall(boom, 42)
print(ok, err)
print("resumed after the error")
"#;
    assert_eq!(out(src), "false E9000: boom 42\nresumed after the error");
}

#[test]
fn pcall_catches_runtime_and_ownership_errors() {
    // runtime type error, inspected via the E-code prefix of the message
    let t1 = r#"
local ok, e = pcall(function() return 1 + {} end)
print(ok, string.sub(e, 1, 5))
"#;
    assert_eq!(out(t1), "false E2001");

    // moved-value error inside the protected call is caught too
    let t2 = r#"
local ok, e = pcall(function() local t = {a = {}} local x = t.a local y = t.a return y end)
print(ok, string.sub(e, 1, 5))
"#;
    assert_eq!(out(t2), "false E1001");
}

#[test]
fn pcall_survives_stack_overflow() {
    let src = r#"
local function inf() return inf() end
local ok, e = pcall(inf)
print(ok, string.sub(e, 1, 5))
"#;
    assert_eq!(out(src), "false E9999");
}

#[test]
fn pcall_rejects_uncallable_and_supports_rethrow() {
    assert_eq!(out("local ok, e = pcall(42) print(ok, string.sub(e, 1, 5))"), "false E2005");

    // catch-then-rethrow: error() inside restores the throw with a new message
    let src = r#"
local ok, err = pcall(function() error("disk full") end)
if not ok then
    error("critical: " .. err)
end
"#;
    assert_eq!(err_code(src), "E9000");

    // missing callee is a loud programming error (not a protected miss)
    assert_eq!(err_code("pcall()"), "E9000");
}

// ===== language server (lsp diagnostics) =====

#[test]
fn lsp_diagnostics_report_positions_and_codes() {
    let diags = lusa::lsp::diagnostics("local x = (1 +\nprint(x)\n");
    assert_eq!(diags.len(), 1);
    let s = diags[0].serialize();
    assert!(s.contains("\"code\":\"E0101\""), "missing code: {}", s);
    assert!(s.contains("\"severity\":1"), "missing severity: {}", s);
    assert!(s.contains("\"line\":1,\"character\":8"), "missing error position: {}", s);
    assert!(s.contains("\"source\":\"lusa\""), "missing source: {}", s);

    // clean source produces no diagnostics
    assert_eq!(lusa::lsp::diagnostics("local x = 1\nprint(x)\n").len(), 0);

    // multiline: the line index counts newlines before the error
    let diags2 = lusa::lsp::diagnostics("local a = 1\nlocal b = 2\nlocal c = (1 +\n");
    assert_eq!(diags2.len(), 1);
    assert!(diags2[0].serialize().contains("\"line\":2"), "expected line 2: {}", diags2[0].serialize());
}

// ===== json =====

#[test]
fn json_encode_roundtrip() {
    let src = r#"
local obj = { name = "lusa", version = 1, tags = {"fast", "rust"}, nested = { ok = true } }
local enc = json.encode(obj)
print(enc)
local back = json.decode(enc)
print(back.name, back.version, back.tags[2], back.nested.ok)
"#;
    assert_eq!(
        out(src),
        "{\"name\":\"lusa\",\"version\":1,\"tags\":[\"fast\",\"rust\"],\"nested\":{\"ok\":true}}\nlusa 1 rust true"
    );
}

#[test]
fn json_scalars_and_arrays() {
    assert_eq!(
        out("print(json.encode(nil), json.encode(true), json.encode(2.5), json.encode(\"a\\nb\"))"),
        "null true 2.5 \"a\\nb\""
    );
    assert_eq!(out("print(json.encode({10, 20, 30}))"), "[10,20,30]");
    assert_eq!(out("print(json.encode({}))"), "[]");
    let src = "local a = json.decode(\"[1, 2.5, -3e2, null, true]\")\nprint(#a, a[2], a[3], a[4], a[5])";
    assert_eq!(out(src), "5 2.5 -300 nil true");
}

#[test]
fn json_escapes() {
    // quotes live fine inside single-quoted Lusa strings; \n is a real newline
    assert_eq!(out(r#"print(json.encode('say "hi"\nl3'))"#), "\"say \\\"hi\\\"\\nl3\"");
    // decode resolves escapes; JSON \\ (two backslashes) becomes a single one
    let src = r#"local s = json.decode('"a\\\\nb"')
print(#s, s)"#;
    assert_eq!(out(src), "4 a\\nb");
    // raw control characters inside JSON strings are invalid
    assert_eq!(err_code("json.decode(\"a\\nb\")"), "E9000");
}

#[test]
fn json_errors() {
    assert_eq!(err_code("json.encode(print)"), "E9000");
    assert_eq!(err_code("json.decode(\"{bad}\")"), "E9000");
    assert_eq!(err_code("json.decode(\"[1, 2\")"), "E9000");
    assert_eq!(err_code("json.decode(\"1 2\")"), "E9000");
    assert_eq!(err_code("json.decode('\"unterminated')"), "E9000");
    assert_eq!(err_code("json.encode({ f = math.floor })"), "E9000");
    // ownership: storing the table in itself moves it; encoding the moved slot errors
    assert_eq!(err_code("local t = {}; t.self = t; json.encode(t)"), "E1001");
    assert_eq!(err_code("json.encode(math.huge)"), "E9000");
    let r = out("local ok, e = pcall(function() return json.decode(\"x\") end)\nprint(ok, string.sub(e, 1, 5))");
    assert_eq!(r, "false E9000");
}

#[test]
fn json_object_vs_array_keys() {
    // dense integer keys 1..n => JSON array
    assert_eq!(out("print(json.encode({[1] = \"a\", [2] = \"b\"}))"), "[\"a\",\"b\"]");
    // mixed/string keys => JSON object with stringified numeric keys
    assert_eq!(
        out("print(json.encode({[1] = \"a\", [2] = \"b\", x = 1}))"),
        "{\"1\":\"a\",\"2\":\"b\",\"x\":1}"
    );
    // holes break the array shape
    assert_eq!(out("local t = {10, 20}; t[4] = 40\nprint(json.encode(t))"), "{\"1\":10,\"2\":20,\"4\":40}");
}


// ===== bytecode VM parity =====
//
// Every program runs through BOTH engines (tree-walking interpreter and the
// bytecode VM) and must produce identical output — or identical error codes.

fn vm_out(src: &str) -> String {
    lusa::vm::run_source_vm(src)
        .expect("vm: expected success")
        .join("\n")
}

fn vm_err_code(src: &str) -> String {
    lusa::vm::run_source_vm(src)
        .expect_err("vm: expected error")
        .code
        .to_string()
}

/// Runs the same program through both engines and requires identical output.
fn assert_parity(src: &str, expected: &str) {
    let a = out(src);
    let b = vm_out(src);
    assert_eq!(a, expected, "interpreter output mismatch for:\n{}", src);
    assert_eq!(b, expected, "vm output mismatch for:\n{}", src);
}

/// Both engines must fail with the same E-code.
fn assert_err_parity(src: &str, code: &str) {
    assert_eq!(err_code(src), code, "interpreter code mismatch for:\n{}", src);
    assert_eq!(vm_err_code(src), code, "vm code mismatch for:\n{}", src);
}

#[test]
fn vm_parity_numbers_strings_interpolation() {
    assert_parity("print(1 + 2 * 3)", "7");
    assert_parity("print(7 / 2, 7 // 2, 7 % 3, 2 ^ 3, -(3 + 1))", "3.5 3 1 8 -4");
    assert_parity("print(#\"abcd\")", "4");
    assert_parity("print(-10 // 3)", "-4");
    assert_parity("print(\"x\" .. \"y\" .. 1)", "xy1");
    assert_parity("print(\"n: \\{1+2} s: \\{\"x\"}\")", "n: 3 s: x");
}

#[test]
fn vm_parity_control_flow() {
    assert_parity(
        r#"
if not false and 1 + 1 == 2 then
    print("branch a")
elseif 2 > 3 then
    print("branch b")
else
    print("branch c")
end
"#,
        "branch a",
    );
    assert_parity(
        r#"
local i = 0
while i < 3 do
    i = i + 1
    print(i)
end
"#,
        "1\n2\n3",
    );
    assert_parity(
        r#"
local n = 0
repeat
    n = n + 1
until n >= 3
print(n)
"#,
        "3",
    );
    assert_parity(
        r#"
for i = 1, 4 do
    if i == 3 then
        break
    end
    print(i)
end
"#,
        "1\n2",
    );
    // descending numeric loop
    assert_parity(
        r#"
for i = 10, 1, -3 do
    print(i)
end
"#,
        "10\n7\n4\n1",
    );
    // for-in over a literal table (insertion order is deterministic)
    assert_parity(
        r#"
local t = {a = 1, b = 2}
for k, v in pairs(t) do
    print(k .. "=" .. v)
end
"#,
        "a=1\nb=2",
    );
}

#[test]
fn vm_parity_functions_closures_recursion() {
    assert_parity(
        r#"
function fib(n)
    if n < 2 then
        return n
    end
    return fib(n - 1) + fib(n - 2)
end
print(fib(12))
"#,
        "144",
    );
    // multi-value returns + truncation of mid-list expansion
    assert_parity(
        r#"
function multi() return 1, 2, 3 end
local a = multi()
print(a)
local b, c = multi()
print(b, c)
print(multi())
local x, y = multi(), 9
print(x, y)
"#,
        "1\n1 2\n1 2 3\n1 9",
    );
    // closure counter: the capture survives across calls
    assert_parity(
        r#"
function counter()
    local n = 0
    return function() n = n + 1 return n end
end
local c = counter()
print(c(), c(), c())
"#,
        "1 2 3",
    );
    // two closures sharing one captured slot
    assert_parity(
        r#"
function mkr()
    local n = 0
    local inc = function() n = n + 1 return n end
    local get = function() return n end
    return inc, get
end
local inc, get = mkr()
inc()
inc()
print(get())
"#,
        "2",
    );
}

#[test]
fn vm_parity_tables_fields_methods() {
    assert_parity(
        r#"
local t = {1, 2, 3, name = "ana", ["k"] = 9}
print(t[2], t.name, t.k, #t)
t.x = 42
print(t.x)
t[10] = "ten"
print(t[10])
"#,
        "2 ana 9 3\n42\nten",
    );
    // method call: the receiver arrives as the first parameter (`self`)
    assert_parity(
        r#"
local p = {x = 3}
function p.add(self, n)
    return self.x + n
end
print(p:add(4))
"#,
        "7",
    );
}

#[test]
fn vm_parity_pcall() {
    assert_parity(
        r#"
local function f(x) return x * 2, x + 1 end
local ok, a, b = pcall(f, 5)
print(ok, a, b)
"#,
        "true 10 6",
    );
    assert_parity(
        r#"
local function boom(x) error("boom " .. x) end
local ok, err = pcall(boom, 42)
print(ok, err)
print("resumed after the error")
"#,
        "false E9000: boom 42\nresumed after the error",
    );
    assert_parity(
        r#"
local ok, e = pcall(function() return 1 + {} end)
print(ok, string.sub(e, 1, 5))
"#,
        "false E2001",
    );
    assert_parity(
        r#"
local ok, e = pcall(function() local t = {a = {}} local x = t.a local y = t.a return y end)
print(ok, string.sub(e, 1, 5))
"#,
        "false E1001",
    );
    assert_parity(
        r#"
local function inf() return inf() end
local ok, e = pcall(inf)
print(ok, string.sub(e, 1, 5))
"#,
        "false E9999",
    );
    assert_parity(
        "local ok, e = pcall(42) print(ok, string.sub(e, 1, 5))",
        "false E2005",
    );
}

#[test]
fn vm_parity_json() {
    assert_parity(
        r#"
local obj = { name = "lusa", version = 1, tags = {"fast", "rust"}, nested = { ok = true } }
local enc = json.encode(obj)
print(enc)
local back = json.decode(enc)
print(back.name, back.version, back.tags[2], back.nested.ok)
"#,
        "{\"name\":\"lusa\",\"version\":1,\"tags\":[\"fast\",\"rust\"],\"nested\":{\"ok\":true}}\nlusa 1 rust true",
    );
    assert_parity(
        "print(json.encode(nil), json.encode(true), json.encode(2.5))",
        "null true 2.5",
    );
}

#[test]
fn vm_parity_varargs() {
    assert_parity(
        r#"
function f(a, ...) return a, ... end
print(f(1, 2, 3))
local n1, n2 = f(7, 8, 9)
print(n1, n2)
function g(...) return {...} end
local tt = g(4, 5, 6)
print(#tt, tt[1])
function h(...) return 0, ... end
print(h(1, 2))
"#,
        "1 2 3\n7 8\n3 4\n0 1 2",
    );
}

#[test]
fn vm_parity_ownership() {
    // moved variables are unusable below (E1001 on both engines)
    assert_err_parity("local t = {1} local u = t print(t)", "E1001");
    assert_err_parity("local d = {1} local f = print local e = f(d) print(d)", "E1001");
    assert_err_parity("local t = {a = {}} local x = t.a local y = t.a", "E1001");
    // clone keeps the source table alive
    assert_parity("local t = {1} local u = clone t u[1] = 9 print(t[1], u[1])", "1 9");
    // scalars are copied, no move happens
    assert_parity("local a = 5 local b = a print(a, b)", "5 5");
}

#[test]
fn vm_parity_stdlib() {
    assert_parity(
        "print(string.sub(\"hello\", 2, 4) .. \"|\" .. math.floor(2.7))",
        "ell|2",
    );
}

#[test]
fn vm_parity_error_codes() {
    assert_err_parity("local x = 1 + {}", "E2001");
    assert_err_parity("missingfn()", "E2005");
    assert_err_parity("local t = nil print(t.x)", "E2006");
    assert_err_parity("local t = {} local x = t.nope.deeper", "E2006");
}
