//! Module 29: the host software ecosystem.
//!
//! `HashMap<K, V>`, `String`, the new `Vec<T>` methods, and native `tostring`
//! are checked in the interpreter, the bytecode VM, and native host builds.
//! `lusa::fs` runs against a scratch directory, and `lusa::net` talks to
//! itself over TCP and UDP loopback.  `examples/host_http_server.lusa` is
//! started on a free port and answers real HTTP requests in every engine.
//! Native runs skip cleanly when the host has no C compiler.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use lusa::cbackend;
use lusa::targets;

const EXAMPLE: &str = "examples/host_http_server.lusa";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file with `\n` line endings (Git on Windows checks
/// out CRLF).
fn read_repo(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
        .replace("\r\n", "\n")
}

/// A fresh scratch directory, as a forward-slash path that Lusa string
/// literals can hold.
fn scratch(tag: &str) -> String {
    let dir = std::env::temp_dir().join(format!("lusa-module29-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 29 scratch directory");
    dir.display().to_string().replace('\\', "/")
}

fn both_engines(source: &str) -> String {
    let interp = lusa::run_source(source)
        .unwrap_or_else(|e| panic!("interpreter failed: {e:?}"))
        .join("\n");
    let vm = lusa::vm::run_source_vm(source)
        .unwrap_or_else(|e| panic!("VM failed: {e:?}"))
        .join("\n");
    assert_eq!(interp, vm, "interpreter and VM disagree");
    interp
}

fn native_compiler() -> Option<cbackend::CCompiler> {
    match cbackend::find_compiler(None) {
        Ok(cc) => Some(cc),
        Err(e) => {
            eprintln!("skipping native Module 29 run: {e}");
            None
        }
    }
}

fn native_exe(source: &str, tag: &str) -> Option<PathBuf> {
    let compiler = native_compiler()?;
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module29-{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    Some(exe)
}

/// Builds and runs a hosted program; `None` without a host C compiler.
fn native_output(source: &str, tag: &str) -> Option<String> {
    let exe = native_exe(source, tag)?;
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    Some(stdout.trim_end_matches('\n').to_string())
}

/// Runs `source` in the interpreter, the VM, and natively.
fn assert_all_engines(source: &str, tag: &str, expected: &str) {
    assert_eq!(both_engines(source), expected, "dynamic engines for `{tag}`");
    if let Some(native) = native_output(source, tag) {
        assert_eq!(native, expected, "native output for `{tag}`");
    }
}

// ---------------------------------------------------------------------------
// Collections
// ---------------------------------------------------------------------------

const COLLECTIONS: &str = r#"
use lusa::collections::HashMap
use lusa::collections::String
use lusa::collections::Vec

local ages: HashMap<string, i32> = {}
ages:insert("ada", 36)
ages:insert("alan", 41)
ages:insert("grace", 85)
ages:insert("ada", 37)
print("len", ages:len(), ages:contains_key("alan"), ages:contains_key("bob"))
match ages:get("ada") with
| Some(age) => print("ada", age)
| None => print("no ada")
end
print("missing", ages:get("bob"), ages:get_or("bob", -1))
print("removed", ages:remove("alan"), ages:remove("alan"), ages:len())
ages:insert("alan", 42)
for name, age in ages:iter() do
    print(name, age)
end

local squares: HashMap<u32, string> = {}
squares:insert(3, "nine")
squares:insert(1, "one")
print(squares:get_or(3, "?"), squares:get(1), squares:is_empty())
local copy: HashMap<u32, string> = clone squares
squares:clear()
print(squares:len(), squares:is_empty(), copy:len(), copy:get_or(1, "?"))

local flags: HashMap<i32, bool> = {}
flags:insert(-7, true)
flags:insert(8, false)
print(flags:get_or(-7, false), flags:get_or(8, true), flags:get_or(9, true))

local text: String = "lusa"
text:push_str(" 2.5")
text:push(33)
print(text, text:len(), text:contains("2.5"), text:starts_with("lu"), text:ends_with("!"))
print("pop", text:pop(), text:as_str())
local empty: String = ""
print(empty:is_empty(), empty:len())
text:clear()
print("[" .. text:as_str() .. "]", text:is_empty())

local v: Vec<i32> = [10, 20, 30]
v:insert(1, 15)
v:insert(4, 40)
print("vec", v:len(), v[0], v[1], v[2], v[3], v[4])
print("removed", v:remove(0), v:len(), v[0])
print("get", v:get(1), v:get(9), v:get(-1), v:contains(30), v:contains(99))
for i, value in v:iter() do
    print(i, value)
end
local words: Vec<string> = ["red", "green"]
words:insert(0, "blue")
print(words:contains("green"), words:contains("teal"), words:remove(1), words:get(0))

local built: String = ""
local n: i32 = 0
while n < 1000 do
    built:push_str(tostring(n % 10))
    n = n + 1
end
print(built:len(), tostring(3.5), tostring(true), tostring(-12), tostring("s"))
"#;

const COLLECTIONS_OUTPUT: &str = "len 3 true false\n\
                                  ada 37\n\
                                  missing None -1\n\
                                  removed true false 2\n\
                                  ada 37\n\
                                  grace 85\n\
                                  alan 42\n\
                                  nine Some(one) false\n\
                                  0 true 2 one\n\
                                  true false true\n\
                                  lusa 2.5! 9 true true true\n\
                                  pop 33 lusa 2.5\n\
                                  true 0\n\
                                  [] true\n\
                                  vec 5 10 15 20 30 40\n\
                                  removed 10 4 15\n\
                                  get Some(20) None None true false\n\
                                  0 15\n\
                                  1 20\n\
                                  2 30\n\
                                  3 40\n\
                                  true false red Some(blue)\n\
                                  1000 3.5 true -12 s";

#[test]
fn collections_behave_the_same_in_every_engine() {
    assert_all_engines(COLLECTIONS, "collections", COLLECTIONS_OUTPUT);
}

#[test]
fn moved_heap_locals_survive_constructors() {
    // Regression: the C backend cleared a moved local before the
    // constructor read it, so `Some(line)` held NULL.
    let source = r#"
function word(): string
    return "x" .. "y"
end

function wrapped(): Option<string>
    local line: string = word()
    return Some(line)
end

function boxed(): Result<string, string>
    local line: string = word()
    return Ok(line)
end

local held: string = word()
local maybe: Option<string> = None
print(wrapped(), boxed(), maybe:unwrap_or(held))
local items: [string; 2] = [word(), word()]
print(items[0], items[1])
"#;
    assert_all_engines(source, "moved-locals", "Some(xy) Ok(xy) xy\nxy xy");
}

#[test]
fn dynamic_engines_report_collection_misuse() {
    let cases = [
        ("local m: HashMap<string, i32> = {}\nm:insert({}, 1)\n", "E2007", "a HashMap key must be a string or an integer"),
        ("local s: String = \"a\"\ns:push(200)\n", "E2005", "ASCII code"),
        ("local v: Vec<i32> = [1]\nv:remove(3)\n", "E2006", "out of bounds"),
        ("local v: Vec<i32> = [1]\nv:insert(5, 2)\n", "E2006", "out of bounds"),
        ("local s: String = 5\n", "E2005", "a `String` is built from a string"),
    ];
    for (source, code, needle) in cases {
        for (engine, result) in [
            ("interpreter", lusa::run_source(source)),
            ("VM", lusa::vm::run_source_vm(source)),
        ] {
            let error = result.expect_err("must reject");
            assert_eq!(error.code, code, "{engine}: {source}");
            assert!(error.msg.contains(needle), "{engine}: expected `{needle}` in {}", error.msg);
        }
    }
}

#[test]
fn native_checker_types_collections_and_host_io() {
    let cases = [
        ("local m: HashMap<f32, i32> = {}\n", "E0301", "HashMap keys must be"),
        ("local m: HashMap<string, table> = {}\n", "E0301", "HashMap values must be"),
        ("local m: HashMap<string, i32> = {a = 1}\n", "E0308", "starts from an empty `{}`"),
        ("local m: HashMap<string, i32> = {}\nm:insert(1, 2)\n", "E0308", "HashMap:insert key"),
        ("local m: HashMap<string, i32> = {}\nm:push(1)\n", "E0318", "unknown HashMap method"),
        ("local m: HashMap<string, i32> = {}\nlocal it = m:iter()\n", "E0312", "drives a `for ... in` loop"),
        ("local s: String = 5\n", "E0308", "`String` initializer"),
        ("local s: String = \"a\"\ns:push_str(5)\n", "E0308", "String:push_str text"),
        ("local v: Vec<i32> = [1]\nprint(v:get(\"x\"))\n", "E0308", "Vec:get index"),
        ("local t = {}\nprint(tostring(t))\n", "E0308", "native `tostring` converts"),
        (
            "#[no_std]\n#[entry]\nfunction main(): void\n    local s: String = \"a\"\nend\n",
            "E0312",
            "needs the hosted runtime",
        ),
        (
            "#[no_std]\n#[entry]\nfunction main(): void\n    local ok: bool = __fs_exists(\"a\")\nend\n",
            "E0312",
            "host I/O is unavailable in `#[no_std]` programs",
        ),
    ];
    for (source, code, needle) in cases {
        let error = cbackend::emit_c(source).expect_err("C11 backend must reject");
        assert!(error.contains(needle), "expected `{needle}` in:\n{error}");
        assert!(error.contains(&format!("error[{code}]")), "expected {code} in:\n{error}");
    }
}

#[test]
fn native_runtimes_are_emitted_only_when_used() {
    let plain = cbackend::emit_c("print(1)\n").expect("plain program");
    for absent in ["LusaMap", "lusa_strbuf_", "lusa_host_", "/* lusa:net */", "lusa_vec_rotate_in"] {
        assert!(!plain.contains(absent), "plain program must not contain {absent:?}");
    }
    let c = cbackend::emit_c(COLLECTIONS).expect("collections program");
    for marker in [
        "typedef struct LusaMap {",
        "static void lusa_map_insert(LusaMap *map, uint64_t key, uint64_t value)",
        "static void lusa_strbuf_push_str(LusaString *s, LusaString *text)",
        "static bool lusa_vec_rotate_out(LusaVec *vec, uint32_t size, int64_t index)",
        "static LusaString *lusa_tostring_i32(int32_t value)",
        "lusa_map_next(",
    ] {
        assert!(c.contains(marker), "missing {marker:?}");
    }
    assert!(!c.contains("/* lusa:net */"), "collections need no sockets");

    let io = cbackend::emit_c("use lusa::fs\nprint(fs::exists(\"x\"))\n").expect("fs program");
    for marker in ["/* lusa:net */", "#include <winsock2.h>", "ws2_32.lib", "static LusaString *lusa_host_fs_read(LusaString *path)"] {
        assert!(io.contains(marker), "missing {marker:?}");
    }
    let windows_header = io.find("#include <windows.h>").expect("coroutine runtime");
    let lean = io.find("#define WIN32_LEAN_AND_MEAN").expect("lean Windows header");
    assert!(lean < windows_header, "WIN32_LEAN_AND_MEAN must precede <windows.h>");
}

#[test]
fn std_modules_are_spliced_for_hosted_programs_only() {
    let hosted = lusa::imports::expand_imports("use lusa::fs\nprint(1)\n", None).expect("hosted");
    assert!(hosted.contains("__lusa_m_lusa_fs__read_to_string"), "hosted programs get lusa::fs");
    assert!(!hosted.contains("__lusa_m_lusa_hal_uart__"), "lusa::fs alone needs no drivers");
    let both = lusa::imports::expand_imports("use lusa::hal::uart\nuse lusa::net\nprint(1)\n", None)
        .expect("drivers and sockets");
    assert!(both.contains("__lusa_m_lusa_hal_uart__") && both.contains("__lusa_m_lusa_net__listen"));

    let no_std = "#[no_std]\nuse lusa::fs\n#[entry]\nfunction main(): void\nend\n";
    let error = lusa::imports::expand_imports(no_std, None).expect_err("no fs without an OS");
    assert!(error.msg.contains("lusa::fs"), "{}", error.msg);
    let board = cbackend::emit_c_for_target(
        "#[no_std]\nuse lusa::net\n#[entry]\nfunction main(): void\nend\n",
        None,
        &targets::STM32F4,
    )
    .expect_err("boards have no sockets");
    assert!(board.contains("lusa::net"), "{board}");
    for family in lusa::hal::families() {
        let source = lusa::hal::crate_source_for(family, false, &["fs"]).expect("family");
        assert_eq!(source.contains("pub mod fs"), family == "host", "{family}");
        assert!(!source.contains("pub mod hal"), "{family}");
    }
}

// ---------------------------------------------------------------------------
// lusa::fs
// ---------------------------------------------------------------------------

const FILES: &str = r#"
use lusa::collections::HashMap
use lusa::fs

local dir = "@DIR@/nested/deeper"
print("mkdir", fs::create_dir_all(dir), fs::is_dir(dir), fs::exists(dir .. "/none"))
local path = dir .. "/notes.txt"
print("write", fs::write(path, "alpha\nbeta\r\n"))
print("append", fs::append(path, "gamma"))
print("size", fs::size(path), fs::exists(path), fs::is_dir(path))
match fs::read_to_string(path) with
| Ok(text) =>
    local clean: string = string.gsub(text, "\r", "")
    print("read", #text, clean)
| Err(reason) => print("read failed", reason)
end
match fs::open(path) with
| Ok(file) =>
    local lines: HashMap<i32, string> = {}
    local count: i32 = 0
    local more = true
    while more do
        match fs::read_line(borrow file) with
        | Some(line) =>
            count = count + 1
            lines:insert(count, line)
        | None => more = false
        end
    end
    fs::close(file)
    for n, line in lines:iter() do
        print("line", n, "[" .. line .. "]")
    end
| Err(reason) => print("open failed", reason)
end
match fs::create(dir .. "/log.txt") with
| Ok(log) =>
    print("handle", fs::write_str(borrow log, "one "), fs::write_str(borrow log, "two"))
    fs::close(log)
| Err(reason) => print("create failed", reason)
end
match fs::open_append(dir .. "/log.txt") with
| Ok(log) =>
    fs::write_str(borrow log, " three")
    fs::close(log)
| Err(reason) => print("append failed", reason)
end
match fs::open(dir .. "/log.txt") with
| Ok(log) =>
    print("read_all", fs::read_all(borrow log))
    fs::close(log)
| Err(reason) => print("open failed", reason)
end
match fs::read_to_string(dir .. "/missing.txt") with
| Ok(text) => print("unexpected", text)
| Err(reason) => print("missing", #reason > 0)
end
local opened: bool = false
match fs::open(dir .. "/missing.txt") with
| Ok(file) =>
    opened = true
    fs::close(file)
| Err(reason) => opened = false
end
print("opened missing", opened, fs::create_dir(dir):is_err(), fs::remove_dir(dir):is_err())
print("cleanup", fs::remove_file(path), fs::remove_file(dir .. "/log.txt"), fs::remove_dir(dir), fs::exists(dir))
print("again", fs::remove_file(path):is_err())
"#;

const FILES_OUTPUT: &str = "mkdir Ok(true) true false\n\
                            write Ok(12)\n\
                            append Ok(5)\n\
                            size Ok(17) true false\n\
                            read 17 alpha\nbeta\ngamma\n\
                            line 1 [alpha]\n\
                            line 2 [beta]\n\
                            line 3 [gamma]\n\
                            handle Ok(4) Ok(3)\n\
                            read_all Ok(one two three)\n\
                            missing true\n\
                            opened missing false true true\n\
                            cleanup Ok(true) Ok(true) Ok(true) false\n\
                            again true";

#[test]
fn files_and_directories_work_in_every_engine() {
    let dir = scratch("files");
    let source = FILES.replace("@DIR@", &dir);
    assert_all_engines(&source, "files", FILES_OUTPUT);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// lusa::net
// ---------------------------------------------------------------------------

const SOCKETS: &str = r#"
use lusa::net

local listener = net::listen("127.0.0.1:0"):unwrap()
local port = net::local_port(borrow listener)
print("port", port > 0)
local client = net::connect(net::address("127.0.0.1", port)):unwrap()
local server = net::accept(borrow listener):unwrap()
print("peer", net::peer(borrow server) ~= "", net::peer(borrow client) == net::address("127.0.0.1", port))
print("sent", net::send(borrow client, "GET /hello HTTP/1.0\r\nHost: x\r\n\r\n"))
print("line", net::recv_line(borrow server))
print("line", net::recv_line(borrow server))
print("blank", net::recv_line(borrow server))
net::send(borrow server, "pong")
net::close(server)
print("reply", net::recv(borrow client, 64))
print("closed", net::recv(borrow client, 64), net::recv_line(borrow client))
net::close(client)
net::close_listener(listener)
print("refused", net::connect(net::address("127.0.0.1", port)):is_err())
print("bad address", net::listen("nowhere"):is_err())

local a = net::udp_bind("127.0.0.1:0"):unwrap()
local b = net::udp_bind("127.0.0.1:0"):unwrap()
print("udp sent", net::send_to(borrow a, "ping", net::address("127.0.0.1", net::udp_port(borrow b))))
print("udp got", net::recv_from(borrow b, 32), net::last_sender(borrow b) == net::address("127.0.0.1", net::udp_port(borrow a)))
net::close_udp(a)
net::close_udp(b)
"#;

const SOCKETS_OUTPUT: &str = "port true\n\
                              peer true true\n\
                              sent Ok(32)\n\
                              line Some(GET /hello HTTP/1.0)\n\
                              line Some(Host: x)\n\
                              blank Some()\n\
                              reply Ok(pong)\n\
                              closed Ok() None\n\
                              refused true\n\
                              bad address true\n\
                              udp sent Ok(4)\n\
                              udp got Ok(ping) true";

#[test]
fn sockets_talk_over_loopback_in_every_engine() {
    assert_all_engines(SOCKETS, "sockets", SOCKETS_OUTPUT);
}

// ---------------------------------------------------------------------------
// The HTTP server example
// ---------------------------------------------------------------------------

fn free_port() -> u16 {
    let probe = TcpListener::bind("127.0.0.1:0").expect("probe port");
    probe.local_addr().expect("probe address").port()
}

/// The example on `port` serving `limit` requests from `root`.
fn server_source(port: u16, root: &str, limit: u32) -> String {
    let source = read_repo(EXAMPLE);
    for line in ["local ADDRESS = \"127.0.0.1:8080\"", "local ROOT = \"www\"", "local MAX_REQUESTS: i32 = 0"] {
        assert!(source.contains(line), "the example no longer declares {line}");
    }
    source
        .replace("local ADDRESS = \"127.0.0.1:8080\"", &format!("local ADDRESS = \"127.0.0.1:{port}\""))
        .replace("local ROOT = \"www\"", &format!("local ROOT = \"{root}\""))
        .replace("local MAX_REQUESTS: i32 = 0", &format!("local MAX_REQUESTS: i32 = {limit}"))
}

fn http_get(port: u16, path: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stream = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => break stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("cannot reach the example server: {e}"),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    write!(stream, "GET {path} HTTP/1.0\r\nHost: localhost\r\n\r\n").expect("send request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

fn response(status: &str, kind: &str, body: &str) -> String {
    format!(
        "HTTP/1.0 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// The four requests the tests make, and what the server must answer.
fn exercise(port: u16) {
    assert_eq!(http_get(port, "/"), response("200 OK", "text/plain", "Hello from Lusa!\n"));
    assert_eq!(
        http_get(port, "/files/index.html"),
        response("200 OK", "text/html", "<h1>Hello from Lusa</h1>\n")
    );
    assert_eq!(
        http_get(port, "/files/..secret"),
        response("400 Bad Request", "text/plain", "bad file name\n")
    );
    assert_eq!(
        http_get(port, "/stats"),
        response("200 OK", "text/plain", "/ 1\n/files/index.html 1\n/files/..secret 1\n/stats 1\n")
    );
}

fn check_log(root: &str) {
    let log = std::fs::read_to_string(format!("{root}/access.log")).expect("access log");
    assert_eq!(log.replace("\r\n", "\n"), "/\n/files/index.html\n/files/..secret\n/stats\n");
}

#[test]
fn http_server_example_serves_requests_in_the_dynamic_engines() {
    for engine in ["interpreter", "vm"] {
        let root = scratch(&format!("www-{engine}"));
        let port = free_port();
        let source = server_source(port, &root, 4);
        let server = std::thread::spawn(move || {
            let result = if engine == "vm" {
                lusa::vm::run_source_vm(&source)
            } else {
                lusa::run_source(&source)
            };
            result.map_err(|e| format!("{e:?}"))
        });
        exercise(port);
        let output = server.join().expect("server thread").expect("server program");
        assert_eq!(
            output.join("\n"),
            format!("listening on 127.0.0.1:{port}\nserved 4 requests"),
            "{engine}"
        );
        check_log(&root);
        let _ = std::fs::remove_dir_all(&root);
    }
}

struct ServerProcess(Child);

impl Drop for ServerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn http_server_example_serves_requests_natively() {
    let root = scratch("www-native");
    let port = free_port();
    let Some(exe) = native_exe(&server_source(port, &root, 4), "server") else {
        return;
    };
    let child = Command::new(&exe)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start native server");
    let mut server = ServerProcess(child);
    exercise(port);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = server.0.try_wait().expect("wait for server") {
            break status;
        }
        assert!(Instant::now() < deadline, "the native server did not stop after four requests");
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    server.0.stdout.take().expect("stdout").read_to_string(&mut stdout).ok();
    assert!(status.success(), "native server failed: {status:?}");
    assert_eq!(
        stdout.replace("\r\n", "\n").trim_end(),
        format!("listening on 127.0.0.1:{port}\nserved 4 requests")
    );
    check_log(&root);
    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn example_exists_and_is_documented() {
    let example = read_repo(EXAMPLE);
    for needle in ["use lusa::fs", "use lusa::net", "HashMap<string, i32>", "local report: String", "net::accept", "fs::read_to_string"] {
        assert!(example.contains(needle), "example lacks {needle}");
    }
    for doc in ["README.md", "docs/LANGUAGE.md", "docs/ROADMAP.md", "docs/RELEASE_NOTES.md", ".github/release-notes/v2.5.0.md"] {
        let text = read_repo(doc);
        for needle in ["host_http_server.lusa", "lusa::fs", "lusa::net", "HashMap<K, V>"] {
            assert!(text.contains(needle), "{doc} lacks {needle}");
        }
    }
    let notes = read_repo(".github/release-notes/v2.5.0.md");
    for heading in ["## Impact Introduction", "## Feature Highlights", "## How to Use"] {
        assert!(notes.contains(heading), "release notes lack {heading}");
    }
}
