//! Lusa CLI: run `.lusa` files or fall into the REPL.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use lusa::ast;
use lusa::cbackend;
use lusa::error::{render, LusaError};
use lusa::interp::{Frame, Interp};
use lusa::parser;
use lusa::targets;

const BANNER: &str = concat!("Lusa ", env!("CARGO_PKG_VERSION"), " — Lua-like language with optional ownership (made in Rust)\ntype `:quit` to exit, finished blocks with `end` are evaluated as you go");

fn main() {
    lusa::update::clean_stale_backup();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("-h") | Some("--help") | Some("help") => {
            println!("lusalang — Lusa compiler and runtime (no cargo needed for daily use)");
            println!();
            println!(
                "  lusalang run <file.lusa>       run a program ([--vm] bytecode; [--native] C11)"
            );
            println!("  lusalang build <file.lusa>     compile with the host C compiler ([--target <name>] for Renode)");
            println!("  lusalang check <file.lusa>     validate (lex+parse; [--typed] runs the C11 type checker)");
            println!("  lusalang repl                  open the interactive REPL");
            println!("  lusalang new <name>            create a new project (scaffold)");
            println!("  lusalang install <user>/<repo> install a package from GitHub");
            println!("  lusalang packages              list installed packages");
            println!("  lusalang uninstall <name>      remove an installed package");
            println!("  lusalang where                 show where the runtime is installed");
            println!(
                "  lusalang lsp                   run the language server (editors contact it)"
            );
            println!("  lusalang self-update [--check] download and install the newest release");
            println!("  lusalang version               runtime version");
            println!();
            println!("  C backend: --cc <path> selects GCC/Clang/TCC/cl.exe; LUSA_CC and CC are honored.");
            println!("             --target <name> cross-builds bare metal (host, riscv32-virt, riscv64-virt,");
            println!("                              rp2040, stm32f4, esp32c3).");
            println!("             build --emit-c writes the intermediate C without invoking the C compiler.");
            println!();
            println!("  (short aliases: lusa [file.lusa] | lusa -e \"<code>\")");
        }
        Some("-v") | Some("--version") | Some("version") => {
            println!(concat!(
                "lusa ",
                env!("CARGO_PKG_VERSION"),
                " (interpreter + bytecode VM + C11 native backend)"
            ));
        }
        Some("repl") => {
            std::process::exit(repl());
        }
        Some("where") => {
            let exe = std::env::current_exe().ok();
            println!(
                "runtime: {}",
                exe.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "?".into())
            );
            println!(concat!("version: lusa ", env!("CARGO_PKG_VERSION")));
            println!("docs   : docs/LANGUAGE.md (language) + docs/TOOLCHAIN.md (toolchain)");
        }
        Some("run") => {
            std::process::exit(cmd_run(&args[2..]));
        }
        Some("build") => {
            std::process::exit(cmd_build(&args[2..]));
        }
        Some("check") => {
            std::process::exit(cmd_check(&args[2..]));
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
        Some("install") => {
            let spec = match args.get(2) {
                Some(s) => s.as_str(),
                None => {
                    eprintln!(
                        "usage: lusalang install <user>/<repo> (optionally `<user>/<repo>#<tag>`)"
                    );
                    std::process::exit(2);
                }
            };
            std::process::exit(pkg_install(spec));
        }
        Some("packages") => {
            std::process::exit(pkg_list());
        }
        Some("uninstall") => {
            let name = match args.get(2) {
                Some(s) => s,
                None => {
                    eprintln!("usage: lusalang uninstall <package-name>");
                    std::process::exit(2);
                }
            };
            std::process::exit(pkg_uninstall(name));
        }
        Some("lsp") => {
            std::process::exit(lusa::lsp::lsp_main());
        }
        Some("self-update") | Some("update") => {
            std::process::exit(lusa::update::cli(&args[2..]));
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
            std::process::exit(run_source_with_base(
                &src,
                path,
                Path::new(path).parent(),
            ));
        }
        None => {
            std::process::exit(repl());
        }
    }
}

fn read_source(path: &str) -> Result<String, i32> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("lusa: cannot read {}: {}", path, e);
        2
    })
}

/// `lusalang run <file> [--native|--vm] [--cc <cc>] [program args...]`.
fn cmd_run(args: &[String]) -> i32 {
    let mut want_vm = false;
    let mut want_native = false;
    let mut cc: Option<String> = None;
    let mut path: Option<String> = None;
    let mut program_args: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--vm" => want_vm = true,
            "--native" | "--c" => want_native = true,
            "--cc" => {
                i += 1;
                match args.get(i) {
                    Some(v) => cc = Some(v.clone()),
                    None => {
                        eprintln!("lusa: --cc requires a compiler path (or use LUSA_CC)");
                        return 2;
                    }
                }
            }
            s if s.starts_with("--cc=") => cc = Some(s["--cc=".len()..].to_string()),
            "-h" | "--help" => {
                println!("usage: lusalang run <file.lusa> [--native|--vm] [--cc <cc>] [args...]");
                return 0;
            }
            s if s.starts_with('-') && s != "-" => {
                eprintln!("lusa: unknown run option `{s}`");
                return 2;
            }
            _ => {
                if path.is_none() {
                    path = Some(a.clone());
                } else {
                    program_args.push(a.clone());
                }
            }
        }
        i += 1;
    }
    if want_native && want_vm {
        eprintln!("lusa: choose either --native or --vm");
        return 2;
    }
    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("usage: lusalang run <file.lusa> [--native|--vm] [--cc <cc>] [args...]");
            return 2;
        }
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };

    if want_native {
        return match cbackend::run_native_file(Path::new(&path), cc.as_deref(), &program_args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("lusa: C11 backend failed:\n{}", e);
                1
            }
        };
    }
    if want_vm {
        match lusa::vm::run_source_vm_with_base(&src, Path::new(&path).parent()) {
            Ok(lines) => {
                for line in lines {
                    println!("{}", line);
                }
                let _ = std::io::stdout().flush();
                println!("[bytecode vm] {} finished", path);
                return 0;
            }
            Err(e) => {
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().flush();
                eprint!(
                    "{}(bytecode vm)",
                    render(&e, &diagnostic_source(&src, Path::new(&path).parent()), &path)
                );
                return 1;
            }
        }
    }
    if !program_args.is_empty() {
        eprintln!("lusa: program arguments require --native");
    }
    run_source_with_base(&src, &path, Path::new(&path).parent())
}

/// `lusalang build <file> [-o <output>] [--target <name>] [--cc <cc>] [--emit-c]`.
fn cmd_build(args: &[String]) -> i32 {
    let mut input: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut cc: Option<String> = None;
    let mut target_name: Option<String> = None;
    let mut emit_c = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-o" | "--output" => {
                i += 1;
                match args.get(i) {
                    Some(v) => output = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("lusa: {a} requires an output path");
                        return 2;
                    }
                }
            }
            "--cc" => {
                i += 1;
                match args.get(i) {
                    Some(v) => cc = Some(v.clone()),
                    None => {
                        eprintln!("lusa: --cc requires a compiler path (or use LUSA_CC)");
                        return 2;
                    }
                }
            }
            s if s.starts_with("--cc=") => cc = Some(s["--cc=".len()..].to_string()),
            "--target" => {
                i += 1;
                match args.get(i) {
                    Some(v) => target_name = Some(v.clone()),
                    None => {
                        eprintln!(
                            "lusa: --target requires a target name ({})",
                            targets::target_names()
                        );
                        return 2;
                    }
                }
            }
            s if s.starts_with("--target=") => {
                target_name = Some(s["--target=".len()..].to_string())
            }
            "--emit-c" => emit_c = true,
            "-h" | "--help" => {
                println!(
                    "usage: lusalang build <file.lusa> [-o <output>] [--target <name>] [--cc <cc>] [--emit-c]"
                );
                println!("targets: {}", targets::target_names());
                return 0;
            }
            s if s.starts_with('-') => {
                eprintln!("lusa: unknown build option `{s}`");
                return 2;
            }
            _ => {
                if input.is_some() {
                    eprintln!("lusa: build accepts exactly one source file");
                    return 2;
                }
                input = Some(a.clone());
            }
        }
        i += 1;
    }
    let input = match input {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!(
                "usage: lusalang build <file.lusa> [-o <output>] [--target <name>] [--cc <cc>] [--emit-c]"
            );
            return 2;
        }
    };
    let target = match target_name.as_deref() {
        Some(name) => match targets::find(name) {
            Some(target) => target,
            None => {
                eprintln!(
                    "lusa: unknown target `{name}` (available: {})",
                    targets::target_names()
                );
                return 2;
            }
        },
        None => &targets::HOST,
    };
    let src = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("lusa: cannot read {}: {}", input.display(), e);
            return 2;
        }
    };

    if emit_c {
        let c_path = output.unwrap_or_else(|| input.with_extension("c"));
        return match cbackend::emit_c_file_for_target(&src, &c_path, input.parent(), target) {
            Ok(()) => {
                println!("C11 emitted: {}", c_path.display());
                0
            }
            Err(e) => {
                eprintln!("lusa: C11 backend failed:\n{}", e);
                1
            }
        };
    }

    let output = output.unwrap_or_else(|| cbackend::default_output_for(&input, target));
    match cbackend::build_with_target(&src, &output, cc.as_deref(), input.parent(), target) {
        Ok(compiler) => {
            if target.is_host() {
                println!(
                    "built {} with {}",
                    output.display(),
                    compiler.display_name()
                );
            } else {
                println!(
                    "built {} for {} with {}",
                    output.display(),
                    target.name,
                    compiler.display_name()
                );
            }
            0
        }
        Err(e) => {
            eprintln!("lusa: C11 build failed:\n{}", e);
            1
        }
    }
}

/// `lusalang check <file> [--typed]`: parse-only by default; `--typed` runs the
/// C11 backend's checker without producing an executable.
fn cmd_check(args: &[String]) -> i32 {
    let mut typed = false;
    let mut path: Option<String> = None;
    for a in args {
        match a.as_str() {
            "--typed" => typed = true,
            "-h" | "--help" => {
                println!("usage: lusalang check <file.lusa> [--typed]");
                return 0;
            }
            s if s.starts_with('-') => {
                eprintln!("lusa: unknown check option `{s}`");
                return 2;
            }
            _ => {
                if path.is_some() {
                    eprintln!("lusa: check accepts exactly one source file");
                    return 2;
                }
                path = Some(a.clone());
            }
        }
    }
    let path = match path {
        Some(p) => p,
        None => {
            eprintln!("usage: lusalang check <file.lusa> [--typed]");
            return 2;
        }
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    if typed {
        return match cbackend::emit_c_with_base(&src, Path::new(&path).parent()) {
            Ok(c) => {
                println!("check ok (typed): {} ({} bytes of C11)", path, c.len());
                0
            }
            Err(e) => {
                eprintln!("{}", e);
                1
            }
        };
    }
    match parser::parse_with_base(&src, Path::new(&path).parent()) {
        Ok(block) => {
            println!("check ok: {} ({} statements)", path, block.len());
            0
        }
        Err(e) => {
            eprint!("{}", render(&e, &diagnostic_source(&src, Path::new(&path).parent()), &path));
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
    if let Err(e) = std::fs::write(
        format!("{}{}main.lusa", name, std::path::MAIN_SEPARATOR),
        &main,
    ) {
        eprintln!("lusa: failed to write main.lusa: {}", e);
        return 2;
    }
    let readme = format!("# lusa: {}\n\nrun: lusalang run main.lusa\n", name);
    let _ = std::fs::write(
        format!("{}{}README.md", name, std::path::MAIN_SEPARATOR),
        readme,
    );
    println!(
        "project created: {}{}main.lusa",
        name,
        std::path::MAIN_SEPARATOR
    );
    println!(
        "next step      : lusalang run {}{}main.lusa",
        name,
        std::path::MAIN_SEPARATOR
    );
    0
}

// ===== package management (`install` / `packages` / `uninstall`) =====

fn packages_dir() -> std::path::PathBuf {
    if let Ok(base) = std::env::var("LUSA_PACKAGES") {
        if !base.is_empty() {
            return std::path::PathBuf::from(base);
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return std::path::PathBuf::from(local)
            .join("Lusa")
            .join("packages");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".lusa")
        .join("packages")
}

/// Reads `key = "value"` from a package's lusa.toml manifest, if present.
fn manifest_value(dir: &std::path::Path, key: &str) -> Option<String> {
    let txt = std::fs::read_to_string(dir.join("lusa.toml")).ok()?;
    for line in txt.lines() {
        let t = line.trim();
        if let Some(eq) = t.find('=') {
            if t[..eq].trim() == key {
                let v = t[eq + 1..].trim().trim_matches(|c| c == '"' || c == '\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

fn download(zip: &std::path::Path, url: &str) -> bool {
    std::process::Command::new("curl")
        .args(["-fsSL", "--retry", "3", "-o"])
        .arg(zip)
        .arg(url)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn copy_recursively(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_recursively(&e.path(), &dst.join(e.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// `lusalang install <user>/<repo>` (+ optional `#tag`): downloads the GitHub
/// archive with the built-in `curl` and unpacks it with the built-in `tar`.
fn pkg_install(spec: &str) -> i32 {
    let (repo, git_ref) = match spec.split_once('#') {
        Some((r, t)) => (r, Some(t)),
        None => (spec, None),
    };
    if repo.split('/').count() != 2 || repo.starts_with('/') || repo.ends_with('/') {
        eprintln!("lusa: expected `<user>/<repo>`, got \"{}\"", spec);
        return 2;
    }
    let branch_ref = match git_ref {
        Some(t) => format!("refs/tags/{}", t),
        None => "refs/heads/main".to_string(),
    };
    let repo_name = repo.rsplit('/').next().unwrap_or("pkg").to_string();
    let tmp = std::env::temp_dir().join(format!("lusa-pkg-{}-{}", repo_name, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = std::fs::create_dir_all(&tmp) {
        eprintln!("lusa: cannot create temp dir {}: {}", tmp.display(), e);
        return 1;
    }
    let zip = tmp.join("pkg.zip");
    let url = format!("https://codeload.github.com/{}/zip/{}", repo, branch_ref);
    println!("installing {} ...", spec);

    let mut ok = download(&zip, &url);
    if !ok && git_ref.is_none() {
        // default branch may be `master`
        let alt = format!("https://codeload.github.com/{}/zip/refs/heads/master", repo);
        ok = download(&zip, &alt);
    }
    if !ok {
        eprintln!("lusa: download failed: {}", url);
        let _ = std::fs::remove_dir_all(&tmp);
        return 1;
    }

    let status = std::process::Command::new("tar")
        .args(["-xf"])
        .arg(&zip)
        .arg("-C")
        .arg(&tmp)
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("lusa: cannot extract archive (tar)");
            let _ = std::fs::remove_dir_all(&tmp);
            return 1;
        }
    }

    // archive layout: <tmp>/<repo>-<default-branch>/
    let top = match std::fs::read_dir(&tmp) {
        Ok(mut rd) => loop {
            match rd.next() {
                Some(Ok(e)) if e.file_name() != "pkg.zip" => break e.path(),
                Some(_) => continue,
                None => {
                    eprintln!("lusa: empty archive");
                    let _ = std::fs::remove_dir_all(&tmp);
                    return 1;
                }
            }
        },
        Err(e) => {
            eprintln!("lusa: cannot read temp dir: {}", e);
            let _ = std::fs::remove_dir_all(&tmp);
            return 1;
        }
    };

    // The resident package name comes from lusa.toml `name` (require("name")
    // resolves against package folder names); falls back to the repo name.
    let name = manifest_value(&top, "name").unwrap_or_else(|| repo_name.clone());
    let dir = packages_dir().join(&name);

    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("lusa: cannot remove old package {}: {}", dir.display(), e);
            return 1;
        }
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("lusa: cannot create {}: {}", dir.display(), e);
        return 1;
    }
    if let Ok(list) = std::fs::read_dir(&top) {
        for e in list.flatten() {
            let target = dir.join(e.file_name());
            if std::fs::rename(e.path(), &target).is_err() {
                if let Err(copy_err) = copy_recursively(&e.path(), &target) {
                    eprintln!(
                        "lusa: cannot move {}: {}",
                        e.file_name().display(),
                        copy_err
                    );
                    let _ = std::fs::remove_dir_all(&dir);
                    let _ = std::fs::remove_dir_all(&tmp);
                    return 1;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);

    let version = manifest_value(&dir, "version").unwrap_or_else(|| "?".into());
    if dir.join("init.lusa").is_file() || dir.join("lusa.toml").is_file() {
        println!("installed: {} v{} -> {}", name, version, dir.display());
        println!(
            "use it   : local {} = require(\"{}\")",
            name.replace(['-', '.'], "_"),
            name
        );
    } else {
        println!("downloaded {} -> {}", name, dir.display());
        println!("note     : no init.lusa / lusa.toml entry point found; require() may fail");
    }
    0
}

/// `lusalang packages`: list installed packages.
fn pkg_list() -> i32 {
    let dir = packages_dir();
    if !dir.is_dir() {
        println!("packages root: {}", dir.display());
        println!("none installed yet — try: lusalang install syntax-08/lusa-ui");
        return 0;
    }
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                names.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    if names.is_empty() {
        println!("packages root: {}", dir.display());
        println!("none installed yet — try: lusalang install syntax-08/lusa-ui");
        return 0;
    }
    for n in &names {
        let version = manifest_value(&dir.join(n), "version").unwrap_or_else(|| "-".into());
        println!("{}  v{}", n, version);
    }
    println!("{} package(s) at {}", names.len(), dir.display());
    0
}

/// `lusalang uninstall <name>`.
fn pkg_uninstall(name: &str) -> i32 {
    let dir = packages_dir().join(name);
    if !dir.is_dir() {
        eprintln!(
            "lusa: \"{}\" is not installed (root: {})",
            name,
            packages_dir().display()
        );
        return 1;
    }
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        eprintln!("lusa: cannot remove {}: {}", dir.display(), e);
        return 1;
    }
    println!("uninstalled: {}", name);
    0
}

/// Runs a complete program in bytecode mode; 0 on success, 1 on error.
pub fn run_vm_source(src: &str, filename: &str) -> i32 {
    match lusa::vm::run_source_vm(src) {
        Ok(lines) => {
            for line in lines {
                println!("{}", line);
            }
            let _ = std::io::stdout().flush();
            0
        }
        Err(e) => {
            let _ = std::io::stdout().flush();
            eprint!("{}(bytecode vm)", render(&e, &diagnostic_source(src, None), filename));
            1
        }
    }
}

/// Text that diagnostic spans refer to.  Spans index the program after import
/// and module expansion (including a spliced `lusa::hal` crate); for programs
/// without modules that is `src` itself.
fn diagnostic_source(src: &str, base: Option<&Path>) -> String {
    lusa::imports::expand_imports(src, base).unwrap_or_else(|_| src.to_string())
}

/// Runs a complete program; 0 on success, 1 on error.
pub fn run_source(src: &str, filename: &str) -> i32 {
    run_source_with_base(src, filename, None)
}

/// Runs a complete program with imports resolved relative to `base`.
pub fn run_source_with_base(src: &str, filename: &str, base: Option<&Path>) -> i32 {
    match lusa::run_source_with_base(src, base) {
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
            eprint!("{}", render(&e, &diagnostic_source(src, base), filename));
            1
        }
    }
}

fn looks_incomplete(e: &LusaError, buf: &str) -> bool {
    let n = buf.chars().count();
    let near_end = matches!(e.span, Some(sp) if sp.start >= n.saturating_sub(1) || sp.end >= n.saturating_sub(1));
    near_end
        && (e.msg.contains("expected")
            || e.msg.contains("unterminated")
            || e.msg.contains("missing"))
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
