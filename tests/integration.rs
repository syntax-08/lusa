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
