//! Module 13: one Lua-style pattern engine for the interpreter, VM, and C backend.

use std::path::PathBuf;
use std::process::{Command, Output};

use lusa::cbackend;

fn emit(source: &str) -> String {
    cbackend::emit_c(source).unwrap_or_else(|e| panic!("C backend failed:\n{e}"))
}

fn emit_error(source: &str, code: &str) {
    let err = cbackend::emit_c(source).expect_err("emission must fail");
    assert!(
        err.contains(&format!("error[{code}]")),
        "expected {code}, got: {err}"
    );
}

fn native_exe(label: &str) -> PathBuf {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    out_dir.join(format!(
        "lusa-module13-{label}-{}{}",
        std::process::id(),
        std::env::consts::EXE_SUFFIX
    ))
}

fn build_native(label: &str, source: &str) -> Option<PathBuf> {
    let compiler = match cbackend::find_compiler(None) {
        Ok(cc) => cc,
        Err(e) => {
            eprintln!("skipping native Module 13 test: {e}");
            return None;
        }
    };
    let exe = native_exe(label);
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None).unwrap_or_else(|e| {
        panic!("native build with {} failed:\n{e}", compiler.display_name())
    });
    Some(exe)
}

fn run_exe(exe: &PathBuf) -> Output {
    let output = Command::new(exe).output().expect("run native program");
    let _ = std::fs::remove_file(exe);
    output
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n")
}

const PARITY_SOURCE: &str = r#"
local s = "abc 123 xyz"
print("find", string.find(s, "%d+"))
print("find-init", string.find(s, "%d+", -6))
print("plain", string.find("a.b", ".", 1, true))
print("method", s:find("%a+"))
print("match", string.match(s, "(%a+)%s(%d+)"))
print("position", string.match(s, "()(%d+)()"))
print("missing", string.match(s, "%d+", 20))
local text, count = string.gsub("a=1 b=2", "(%a+)=(%d+)", "%2:%1")
print("gsub", text, count)
local limited, limited_count = string.gsub("aa aa", "a", "b", 1)
print("limit", limited, limited_count)
for w in string.gmatch("one two", "%a+") do print("word", w) end
for k, v in string.gmatch("a=1 b=2", "(%a+)=(%d+)") do print("pair", k, v) end
print("balanced", string.match("(a(b)c)", "%b()"))
print("frontier", string.match("THE (quick) fox", "%f[%a]%a+"))
print("anchor", string.match("zabc", "^a"))
print("backref", string.match("abc abc", "(%a+) %1"))
print("backtrack", string.match("aaab", "(a*)(b)c"))
print("unicode", string.match("café", "caf(.)"))
print("unicode-find", string.find("café", "é"))
print("emoji", string.match("a😀", "a(.)"))
print("emoji-find", string.find("a😀", "😀"))
local ok, err = pcall(string.match, "abc", "[")
print("caught", ok, err)
local ok2, err2 = pcall(string.gsub, "abc", "%a+", "%2")
print("bad-replacement", ok2, err2)
"#;

const PARITY_EXPECTED: &str = "find 5 7\n\
find-init 6 7\n\
plain 2 2\n\
method 1 3\n\
match abc 123\n\
position 5 123 8\n\
missing nil\n\
gsub 1:a 2:b 2\n\
limit ba aa 1\n\
word one\n\
word two\n\
pair a 1\n\
pair b 2\n\
balanced (a(b)c)\n\
frontier THE\n\
anchor nil\n\
backref abc\n\
backtrack nil\n\
unicode é\n\
unicode-find 4 4\n\
emoji 😀\n\
emoji-find 2 2\n\
caught false E2010: malformed pattern: missing ']' in character set\n\
bad-replacement false E2010: replacement references capture %2 but the pattern has 0 capture(s)\n";

#[test]
fn interpreter_and_vm_share_one_pattern_engine() {
    let interp = lusa::run_source(PARITY_SOURCE).expect("interpreter failed");
    let vm = lusa::vm::run_source_vm(PARITY_SOURCE).expect("VM failed");
    assert_eq!(format!("{}\n", interp.join("\n")), PARITY_EXPECTED);
    assert_eq!(format!("{}\n", vm.join("\n")), PARITY_EXPECTED);
}

const NATIVE_SOURCE: &str = r#"
local s: string = "abc 123 xyz"
local start, finish = string.find(s, "%d+")
print("find", start, finish)

local plain_start, plain_finish = string.find("a.b", ".", 1, true)
print("plain", plain_start, plain_finish)

local first: string = string.match(s, "(%a+)%s(%d+)")
print("match", first)

local text, count = string.gsub("a=1 b=2", "(%a+)=(%d+)", "%2:%1")
print("gsub", text, count)

local limited, limited_count = string.gsub("aa aa", "a", "b", 1)
print("limit", limited, limited_count)

for word in string.gmatch(s, "%a+") do
    print("word", word)
end

for key, value in string.gmatch("a=1 b=2", "(%a+)=(%d+)") do
    print("pair", key, value)
end

local balanced: string = string.match("(a(b)c)", "%b()")
print("balanced", balanced)

local frontier: string = string.match("THE (quick) fox", "%f[%a]%a+")
print("frontier", frontier)

local back: string = string.match("abc abc", "(%a+) %1")
print("back", back)

local absent: string = string.match("aaab", "(a*)(b)c")
print("absent", absent)

local unicode_text: string = string.match("café", "caf(.)")
print("unicode", unicode_text)
local unicode_start, unicode_finish = string.find("café", "é")
print("unicode-find", unicode_start, unicode_finish)
local emoji_text: string = string.match("a😀", "a(.)")
print("emoji", emoji_text)
local emoji_start, emoji_finish = string.find("a😀", "😀")
print("emoji-find", emoji_start, emoji_finish)

local missing: i32 = string.find(s, "%d+", 20)
print("missing", missing)
return 0
"#;

const NATIVE_EXPECTED: &str = "find 5 7\n\
plain 2 2\n\
match abc\n\
gsub 1:a 2:b 2\n\
limit ba aa 1\n\
word abc\n\
word xyz\n\
pair a 1\n\
pair b 2\n\
balanced (a(b)c)\n\
frontier THE\n\
back abc\n\
absent nil\n\
unicode é\n\
unicode-find 4 4\n\
emoji 😀\n\
emoji-find 2 2\n\
missing 0\n";

#[test]
fn native_pattern_direct_calls_match_the_dynamic_engines() {
    let Some(exe) = build_native("parity", NATIVE_SOURCE) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(stdout(&output), NATIVE_EXPECTED);
}

#[test]
fn native_pattern_emission_tracks_gmatch_and_temporary_lifetimes() {
    let c = emit(NATIVE_SOURCE);
    assert!(c.contains("lusa_string_find_values("));
    assert!(c.contains("lusa_string_gsub("));
    assert!(c.contains("lusa_string_gmatch_next("));
    assert!(c.contains("LusaString *gmatch_captures"));
    assert!(c.contains("lusa_drop_gmatch(&"));
    assert!(c.contains("LUSA_PAT_MAX_STEPS"));

    let direct = emit(
        r#"
print("matched", string.match("a=1", "(%a+)=(%d+)"))
print("rewritten", string.gsub("a=1", "a", "b"))
return 0
"#,
    );
    assert!(
        direct.contains("LusaString *print_text"),
        "pattern temporaries printed directly must be bound for RAII drops"
    );
    assert!(
        direct.contains("lusa_drop_string(&print_text"),
        "printed pattern temporaries must be dropped immediately"
    );
}

#[test]
fn native_static_diagnostics_reject_bad_patterns_and_captures() {
    emit_error(
        r#"
local value: string = string.match("a", "[")
print(value)
return 0
"#,
        "E0323",
    );
    emit_error(
        r#"
local text = string.gsub("a", "(a)", "%2")
print(text)
return 0
"#,
        "E0324",
    );
    emit_error(
        r#"
for left, right in string.gmatch("a", "(a)") do
    print(left, right)
end
return 0
"#,
        "E0324",
    );
    emit_error(
        r#"
local iterator = string.gmatch("a", "%a+")
print(iterator)
return 0
"#,
        "E0317",
    );
}

#[test]
fn native_runtime_pattern_errors_are_catchable_with_pcall() {
    let source = r#"
function malformed(): i32
    local pattern: string = "["
    local value: string = string.match("abc", pattern)
    print("unreachable", value)
    return 0
end

function expensive(): i32
    local pattern: string = "a*a*a*a*a*a*a*a*a*a*b"
    local value: string = string.match("aaaaaaaaaaaaaaaaaaaaaaaa", pattern)
    print("unreachable", value)
    return 0
end

local ok, err = pcall(malformed)
print("caught", ok, err)
local ok2, err2 = pcall(expensive)
print("budget", ok2, err2)
return 0
"#;
    let Some(exe) = build_native("pcall-pattern", source) else {
        return;
    };
    let output = run_exe(&exe);
    assert!(output.status.success(), "native program failed: {output:?}");
    assert_eq!(
        stdout(&output),
        "caught false E2010: malformed pattern: missing ']' in character set\n\
         budget false E2011: pattern exceeded the bounded matching budget; simplify nested quantifiers\n"
    );
}
