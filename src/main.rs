//! Lusa CLI: run `.lusa` files or fall into the REPL.

use std::io::{BufRead, Write};

use lusa::ast;
use lusa::error::{render, LusaError};
use lusa::interp::{Frame, Interp};
use lusa::parser;

const BANNER: &str = "Lusa 0.1.0 — Lua-like language with optional ownership (made in Rust)\ntype `:quit` to exit, finished blocks with `end` are evaluated as you go";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("-h") | Some("--help") | Some("help") => {
            println!("lusalang — Lusa runtime manager (no cargo needed for daily use)");
            println!();
            println!("  lusalang run <file.lusa>       run a program");
            println!("  lusalang check <file.lusa>     validate (lex+parse) without running");
            println!("  lusalang repl                  open the interactive REPL");
            println!("  lusalang new <name>            create a new project (scaffold)");
            println!("  lusalang where                 show where the runtime is installed");
            println!("  lusalang version               runtime version");
            println!();
            println!("  (short aliases: lusa [file.lusa] | lusa -e \"<code>\")");
        }
        Some("-v") | Some("--version") | Some("version") => {
            println!("lusa 0.1.0 (runtime RC1: release turbo)");
        }
        Some("repl") => {
            std::process::exit(repl());
        }
        Some("where") => {
            let exe = std::env::current_exe().ok();
            println!("runtime: {}", exe.map(|p| p.display().to_string()).unwrap_or_else(|| "?".into()));
            println!("version: lusa 0.1.0");
            println!("docs   : docs/LANGUAGE.md (language) + docs/TOOLCHAIN.md (toolchain)");
        }
        Some("run") => {
            let path = match args.get(2) {
                Some(p) => p,
                None => {
                    eprintln!("usage: lusalang run <file.lusa>");
                    std::process::exit(2);
                }
            };
            let src = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("lusa: cannot read {}: {}", path, e);
                    std::process::exit(2);
                }
            };
            std::process::exit(run_source(&src, path));
        }
        Some("check") => {
            let path = match args.get(2) {
                Some(p) => p,
                None => {
                    eprintln!("usage: lusalang check <file.lusa>");
                    std::process::exit(2);
                }
            };
            std::process::exit(check(path));
        }
        Some("new") => {
            let name = match args.get(2) {
                Some(p) => p,
                None => {
                    eprintln!("usage: lusalang new <project-name>");
                    std::process::exit(2);
                }
            };
            std::process::exit(new_project(name));
        }
        Some("-e") => {
            let code = args[2..].join(" ");
            std::process::exit(run_source(&code, "<cmdline>"));
        }
        Some(path) => {
            let src = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("lusa: cannot read {}: {}", path, e);
                    std::process::exit(2);
                }
            };
            std::process::exit(run_source(&src, path));
        }
        None => {
            std::process::exit(repl());
        }
    }
}

/// `lusa check <file>`: lex+parse only; reports diagnostics without running.
fn check(path: &str) -> i32 {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lusa: cannot read {}: {}", path, e);
            return 2;
        }
    };
    match parser::parse(&src) {
        Ok(block) => {
            println!("check ok: {} ({} statements)", path, block.len());
            0
        }
        Err(e) => {
            eprint!("{}", render(&e, &src, path));
            1
        }
    }
}

const SCAFFOLD_MAIN: &str = r#"-- {name}/main.lusa — Lusa project
-- run with:  lusalang run main.lusa

print("== {name} ==")

-- gradient render (photocraft starter)
local CHARS = ".:-=+*#%@"
for y = 1, 8 do
    local row = ""
    for x = 1, 16 do
        local v = (x * y) // 6 % #CHARS
        row = row .. string.sub(CHARS, v + 1, v + 1)
    end
    print(row)
end

-- table data
local cases = {
    {x = 2, y = 3},
    {x = 4, y = 5},
}
for _, c in ipairs(cases) do
    print(string.format("(%d)^2 + 1 = %d", c.x, c.x * c.x + 1))
end
"#;

/// `lusa new <name>`: project scaffold.
fn new_project(name: &str) -> i32 {
    if name.is_empty() || name.contains(['\\', '/']) {
        eprintln!("lusa: invalid project name ({})", name);
        return 2;
    }
    if std::fs::metadata(name).is_ok() {
        eprintln!("lusa: \"{}\" already exists; pick another name", name);
        return 2;
    }
    if let Err(e) = std::fs::create_dir_all(name) {
        eprintln!("lusa: failed to create {}: {}", name, e);
        return 2;
    }
    let main = SCAFFOLD_MAIN.replace("{name}", name);
    if let Err(e) = std::fs::write(format!("{}{}main.lusa", name, std::path::MAIN_SEPARATOR), &main) {
        eprintln!("lusa: failed to write main.lusa: {}", e);
        return 2;
    }
    let readme = format!("# lusa: {}\n\nrun: lusalang run main.lusa\n", name);
    let _ = std::fs::write(format!("{}{}README.md", name, std::path::MAIN_SEPARATOR), readme);
    println!("project created: {}{}main.lusa", name, std::path::MAIN_SEPARATOR);
    println!("next step      : lusalang run {}{}main.lusa", name, std::path::MAIN_SEPARATOR);
    0
}

/// Runs a complete program; 0 on success, 1 on error.
pub fn run_source(src: &str, filename: &str) -> i32 {
    match lusa::run_source(src) {
        Ok(lines) => {
            for line in lines {
                println!("{}", line);
            }
            let _ = std::io::stdout().flush();
            0
        }
        Err(e) => {
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            eprint!("{}", render(&e, src, filename));
            1
        }
    }
}

fn looks_incomplete(e: &LusaError, buf: &str) -> bool {
    let n = buf.chars().count();
    let near_end = matches!(e.span, Some(sp) if sp.start >= n.saturating_sub(1) || sp.end >= n.saturating_sub(1));
    near_end
        && (e.msg.contains("expected") || e.msg.contains("unterminated") || e.msg.contains("missing"))
}

/// Interactive REPL with multi-line continuation (Lua-style).
fn repl() -> i32 {
    println!("{}", BANNER);
    let mut interp = Interp::new();
    interp.install_stdlib();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        print!("lusa> ");
        let _ = std::io::stdout().flush();
        let first = match lines.next() {
            None => break,
            Some(Err(_)) => break,
            Some(Ok(s)) => s,
        };
        let trim = first.trim();
        if trim.is_empty() {
            continue;
        }
        if matches!(trim, ":quit" | ":q" | ":exit") {
            break;
        }

        // accumulate until the parse is complete (covers `end`-blocks etc.)
        let mut buf = String::from(first);
        let parsed: Result<ast::Block, LusaError> = loop {
            match parser::parse(&buf) {
                Ok(a) => break Ok(a),
                Err(e) => {
                    if looks_incomplete(&e, &buf) {
                        print!("  ...> ");
                        let _ = std::io::stdout().flush();
                        match lines.next() {
                            None => break Err(e),
                            Some(Err(_)) => break Err(e),
                            Some(Ok(m)) => {
                                buf.push('\n');
                                buf.push_str(&m);
                            }
                        }
                    } else {
                        break Err(e);
                    }
                }
            }
        };

        match parsed {
            Err(e) => {
                let _ = std::io::stderr().flush();
                eprint!("{}", render(&e, &buf, "<repl>"));
            }
            Ok(block) => {
                let mut frame = Frame::new(None, Vec::new());
                let run = interp.exec_block(&mut frame, &block);
                let out = std::mem::take(&mut interp.out);
                for line in out {
                    println!("{}", line);
                }
                if let Err(e) = run {
                    let _ = std::io::stdout().flush();
                    let _ = std::io::stderr().flush();
                    eprint!("{}", render(&e, &buf, "<repl>"));
                }
            }
        }
    }
    println!("bye!");
    0
}
