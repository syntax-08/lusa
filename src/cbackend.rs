//! C11 backend facade.
//!
//! The reference compiler (`compiler_bootstrap.lusa`) is embedded in the
//! binary so `lusalang build` and `lusalang run --native` work from a single
//! executable.  This module feeds user source to the self-hosted lexer,
//! parser, optimizer, type checker, and C emitter, then invokes the host C
//! compiler (GCC, Clang, TCC, or MSVC) to produce a native executable.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::LusaError;

const LEXER: &str = include_str!("../lexer.lusa");
const PARSER: &str = include_str!("../parser.lusa");
const BOOTSTRAP: &str = include_str!("../compiler_bootstrap.lusa");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompilerKind {
    /// GCC, Clang, TCC, and compatible command-line compilers.
    Gnu,
    /// Microsoft Visual C++ (`cl.exe`).
    Msvc,
}

#[derive(Clone, Debug)]
pub struct CCompiler {
    pub path: PathBuf,
    pub kind: CompilerKind,
    /// Visual Studio's `vcvars64.bat`, when `cl.exe` was discovered outside a
    /// Developer Command Prompt.  Running it gives `cl` its INCLUDE/LIB paths.
    pub env_script: Option<PathBuf>,
}

impl CCompiler {
    pub fn display_name(&self) -> String {
        self.path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

fn strip_module<'a>(source: &'a str, suffix: &str) -> Result<&'a str, String> {
    source
        .trim_end()
        .strip_suffix(suffix)
        .ok_or_else(|| format!("embedded module does not end with `{suffix}`"))
}

/// Encodes arbitrary source as a Lusa table of string chunks.  Characters that
/// cannot appear literally in a double-quoted Lusa string are emitted through
/// `string.char`, so user sources never need quote escaping.
fn source_chunks_literal(source: &str) -> String {
    let mut chunks: Vec<String> = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, chunks: &mut Vec<String>| {
        if !run.is_empty() {
            chunks.push(format!("\"{}\"", run));
            run.clear();
        }
    };

    for ch in source.chars() {
        let needs_escape =
            matches!(ch, '"' | '\\' | '\n' | '\r' | '\t') || ch == '\0' || ch.is_control();
        if needs_escape {
            flush(&mut run, &mut chunks);
            chunks.push(format!("string.char({})", ch as u32));
        } else {
            run.push(ch);
        }
    }
    flush(&mut run, &mut chunks);
    format!("{{{}}}", chunks.join(", "))
}

fn backend_program(source: &str) -> Result<String, String> {
    let lexer = strip_module(LEXER, "return move(Lexer)")?;
    let parser = strip_module(PARSER, "return move(Parser)")?;
    let bootstrap = strip_module(BOOTSTRAP, "return move(Compiler)")?;

    let mut program = String::with_capacity(
        lexer.len() + parser.len() + bootstrap.len() + source.len() / 8 + 512,
    );
    program.push_str("local M85Lexer, M85Parser, M85Compiler\n");
    program.push_str("do\n");
    program.push_str(lexer);
    program.push_str("\nM85Lexer = move(Lexer)\nend\n");
    program.push_str("do\n");
    program.push_str(parser);
    program.push_str("\nM85Parser = move(Parser)\nend\n");
    program.push_str("do\n");
    program.push_str(bootstrap);
    program.push_str("\nM85Compiler = move(Compiler)\nend\n");
    program.push_str("local __source_chunks = ");
    program.push_str(&source_chunks_literal(source));
    program.push('\n');
    program.push_str("local __source = table.concat(__source_chunks)\n");
    program.push_str("local __result = M85Compiler.bootstrap(M85Lexer, M85Parser, __source)\n");
    program.push_str("print(__result.c_source)\n");
    Ok(program)
}

fn format_backend_error(err: LusaError) -> String {
    if err.code == "E9000" {
        let msg = err.msg.trim();
        if msg.starts_with("error[") {
            return msg.to_string();
        }
        for (prefix, code) in [("parser:", "E0101"), ("lexer:", "E0001")] {
            if let Some(rest) = msg.strip_prefix(prefix) {
                let mut parts = rest.splitn(3, ':');
                if let (Some(line), Some(col), Some(text)) =
                    (parts.next(), parts.next(), parts.next())
                {
                    return format!(
                        "error[{code}]: {}\n  --> lusa:{}:{}",
                        text.trim(),
                        line.trim(),
                        col.trim()
                    );
                }
            }
        }
        if msg.starts_with("bootstrap:") {
            return format!("internal C backend error: {msg}");
        }
        return msg.to_string();
    }
    err.to_string()
}

/// Compiles Lusa source to a C11 translation unit.
pub fn emit_c(source: &str) -> Result<String, String> {
    let program = backend_program(source)?;
    let output = crate::run_source(&program).map_err(format_backend_error)?;
    if output.is_empty() {
        return Err("C backend produced no output".to_string());
    }
    Ok(output.concat())
}

/// Writes the generated C to `path`.
pub fn emit_c_file(source: &str, path: &Path) -> Result<(), String> {
    let c = emit_c(source)?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }
    std::fs::write(path, c).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Default executable path for a source file.
pub fn default_output(input: &Path) -> PathBuf {
    let mut output = input.to_path_buf();
    if cfg!(windows) {
        output.set_extension("exe");
    } else {
        output.set_extension("");
    }
    output
}

fn compiler_kind(path: &Path) -> CompilerKind {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name == "cl" || name == "clang-cl" {
        CompilerKind::Msvc
    } else {
        CompilerKind::Gnu
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let finder = if cfg!(windows) { "where" } else { "which" };
    let out = Command::new(finder).arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(PathBuf::from)
}

fn candidate_from_env() -> Vec<String> {
    ["LUSA_CC", "CC"]
        .iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
        .collect()
}

fn resolve_compiler(value: &str) -> Option<CCompiler> {
    let value = value.trim().trim_matches('"');
    if value.is_empty() {
        return None;
    }
    // A full path (including a Windows path with spaces) wins over PATH
    // lookup.  If the value contains arguments, only the first token is used;
    // extra compiler flags belong in the build configuration, not the name.
    let path = if Path::new(value).is_file() {
        PathBuf::from(value)
    } else {
        let first = value.split_whitespace().next().unwrap_or(value);
        if Path::new(first).is_file() {
            PathBuf::from(first)
        } else {
            find_on_path(first)?
        }
    };
    Some(make_compiler(path))
}

fn make_compiler(path: PathBuf) -> CCompiler {
    let kind = compiler_kind(&path);
    let env_script = if kind == CompilerKind::Msvc {
        msvc_env_script(&path)
    } else {
        None
    };
    CCompiler {
        path,
        kind,
        env_script,
    }
}

/// Finds Visual Studio's environment setup script by walking up from `cl.exe`.
/// Running `vcvars64.bat` supplies the include/library paths that are normally
/// only present in a Developer Command Prompt.
fn msvc_env_script(cl: &Path) -> Option<PathBuf> {
    for ancestor in cl.ancestors() {
        let build = ancestor.join("VC").join("Auxiliary").join("Build");
        for name in ["vcvars64.bat", "vcvarsall.bat"] {
            let script = build.join(name);
            if script.is_file() {
                return Some(script);
            }
        }
    }
    None
}

fn windows_msvc_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let program_files = std::env::var("ProgramFiles").ok();
    let program_files_x86 = std::env::var("ProgramFiles(x86)").ok();

    // The official locator is present with every Visual Studio/Build Tools
    // installation and avoids guessing versioned directory names.
    if let Some(pf86) = program_files_x86.as_ref() {
        let vswhere = Path::new(pf86)
            .join("Microsoft Visual Studio")
            .join("Installer")
            .join("vswhere.exe");
        if vswhere.is_file() {
            if let Ok(out) = Command::new(&vswhere)
                .args([
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                ])
                .output()
            {
                if out.status.success() {
                    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if !root.is_empty() {
                        found.extend(msvc_under(Path::new(&root)));
                    }
                }
            }
        }
    }

    for base in [program_files, program_files_x86].into_iter().flatten() {
        let vs = Path::new(&base).join("Microsoft Visual Studio");
        if let Ok(editions) = std::fs::read_dir(&vs) {
            for edition in editions.flatten() {
                found.extend(msvc_under(&edition.path()));
            }
        }
        let build_tools = Path::new(&base)
            .join("Microsoft Visual Studio")
            .join("2022")
            .join("BuildTools");
        found.extend(msvc_under(&build_tools));
    }
    found
}

fn msvc_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let tools = root.join("VC").join("Tools").join("MSVC");
    let Ok(versions) = std::fs::read_dir(tools) else {
        return out;
    };
    let mut versions: Vec<PathBuf> = versions.flatten().map(|e| e.path()).collect();
    versions.sort();
    versions.reverse();
    for version in versions {
        for host in ["Hostx64", "Hostx86"] {
            let cl = version.join("bin").join(host).join("x64").join("cl.exe");
            if cl.is_file() {
                out.push(cl);
            }
        }
    }
    out
}

/// Discovers or validates the host C compiler.
///
/// Order: explicit `--cc`, `LUSA_CC`, `CC`, PATH (`cc`, `gcc`, `clang`,
/// `tcc`, `cl`), then a Visual Studio installation found through `vswhere`.
pub fn find_compiler(explicit: Option<&str>) -> Result<CCompiler, String> {
    if let Some(value) = explicit {
        return resolve_compiler(value)
            .ok_or_else(|| format!("C compiler `{value}` was not found"));
    }
    for value in candidate_from_env() {
        if let Some(cc) = resolve_compiler(&value) {
            return Ok(cc);
        }
    }
    for name in ["cc", "gcc", "clang", "tcc", "cl"] {
        if let Some(path) = find_on_path(name) {
            return Ok(make_compiler(path));
        }
    }
    if cfg!(windows) {
        if let Some(path) = windows_msvc_candidates().into_iter().next() {
            return Ok(make_compiler(path));
        }
    }
    Err(
        "no C compiler found; install GCC, Clang, TCC, or Visual Studio Build Tools, \
         set LUSA_CC, or pass --cc <path>"
            .to_string(),
    )
}

fn run_compiler(mut command: Command, label: &str) -> Result<(), String> {
    let out = command
        .output()
        .map_err(|e| format!("cannot start {label}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut message = format!("{label} failed with status {}", out.status);
    if !stdout.trim().is_empty() {
        message.push_str("\n");
        message.push_str(stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        message.push_str("\n");
        message.push_str(stderr.trim_end());
    }
    Err(message)
}

fn compile_c_file(
    cc: &CCompiler,
    c_path: &Path,
    output: &Path,
    workdir: &Path,
) -> Result<(), String> {
    match cc.kind {
        CompilerKind::Gnu => {
            let mut command = Command::new(&cc.path);
            command
                .current_dir(workdir)
                .arg("-std=c11")
                .arg("-O2")
                .arg("-o")
                .arg(output)
                .arg(c_path);
            // libm is required by the C runtime's `powf`.  TCC on Windows has
            // no libm archive, and some Windows GCC/Clang distributions fold
            // math into the CRT; retry without `-lm` if linking rejects it.
            let with_math = !cc.display_name().to_ascii_lowercase().contains("tcc");
            if with_math {
                command.arg("-lm");
            }
            match run_compiler(command, "C compiler") {
                Ok(()) => Ok(()),
                Err(first) if with_math => {
                    let mut retry = Command::new(&cc.path);
                    retry
                        .current_dir(workdir)
                        .arg("-std=c11")
                        .arg("-O2")
                        .arg("-o")
                        .arg(output)
                        .arg(c_path);
                    run_compiler(retry, "C compiler (without -lm)").map_err(|_| first)
                }
                Err(e) => Err(e),
            }
        }
        CompilerKind::Msvc => {
            let obj = workdir.join("lusa-output.obj");
            let attempt = |with_std: bool| {
                if let Some(script) = &cc.env_script {
                    // `cl.exe` found through vswhere is not enough on its own:
                    // the Developer Command Prompt normally supplies INCLUDE
                    // and LIB.  Run the matching vcvars script first.
                    let bat = workdir.join("lusa-msvc.bat");
                    let mut text = String::from("@echo off\r\n");
                    text.push_str(&format!("call \"{}\"", script.display()));
                    if script
                        .file_name()
                        .map(|n| n.to_string_lossy().eq_ignore_ascii_case("vcvarsall.bat"))
                        .unwrap_or(false)
                    {
                        text.push_str(" x64");
                    }
                    text.push_str(" >nul\r\n");
                    text.push_str(&format!(
                        "\"{}\" /nologo /TC /O2 /Fo:\"{}\" /Fe:\"{}\" ",
                        cc.path.display(),
                        obj.display(),
                        output.display()
                    ));
                    if with_std {
                        text.push_str("/std:c11 ");
                    }
                    text.push_str(&format!("\"{}\"\r\n", c_path.display()));
                    std::fs::write(&bat, text)
                        .map_err(|e| format!("cannot write {}: {e}", bat.display()))?;
                    let mut command = Command::new("cmd");
                    command.current_dir(workdir).arg("/C").arg(&bat);
                    return run_compiler(command, "MSVC cl.exe");
                }
                let mut command = Command::new(&cc.path);
                command
                    .current_dir(workdir)
                    .arg("/nologo")
                    .arg("/TC")
                    .arg("/O2")
                    .arg(format!("/Fo:{}", obj.display()))
                    .arg(format!("/Fe:{}", output.display()));
                if with_std {
                    command.arg("/std:c11");
                }
                command.arg(c_path);
                run_compiler(command, "MSVC cl.exe")
            };
            match attempt(true) {
                Ok(()) => Ok(()),
                Err(first) => attempt(false).map_err(|_| first),
            }
        }
    }
}

fn build_c_source(
    c_source: &str,
    output: &Path,
    cc: Option<&str>,
    keep_c: Option<&Path>,
) -> Result<CCompiler, String> {
    let compiler = find_compiler(cc)?;
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve the current directory: {e}"))?
            .join(output)
    };
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
    }

    let workdir =
        std::env::temp_dir().join(format!("lusa-cbuild-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&workdir)
        .map_err(|e| format!("cannot create {}: {e}", workdir.display()))?;
    let c_path = workdir.join("program.c");
    let write_result = std::fs::write(&c_path, c_source);
    if let Err(e) = write_result {
        let _ = std::fs::remove_dir_all(&workdir);
        return Err(format!("cannot write generated C: {e}"));
    }

    let result = compile_c_file(&compiler, &c_path, &output, &workdir);
    if let Some(path) = keep_c {
        let _ = std::fs::write(path, c_source);
    }
    match result {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&workdir);
            Ok(compiler)
        }
        Err(error) => {
            let saved = output.with_extension("c");
            let _ = std::fs::write(&saved, c_source);
            let _ = std::fs::remove_dir_all(&workdir);
            Err(format!("{error}\ngenerated C saved to {}", saved.display()))
        }
    }
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Builds a native executable from Lusa source.
pub fn build(source: &str, output: &Path, cc: Option<&str>) -> Result<CCompiler, String> {
    let c = emit_c(source)?;
    build_c_source(&c, output, cc, None)
}

/// Builds a native executable from a source file.
pub fn build_file(input: &Path, output: &Path, cc: Option<&str>) -> Result<CCompiler, String> {
    let source = std::fs::read_to_string(input)
        .map_err(|e| format!("cannot read {}: {e}", input.display()))?;
    build(&source, output, cc)
}

/// Runs Lusa source through the C backend and executes the resulting native
/// program, inheriting the caller's stdin/stdout/stderr.  Returns the program's
/// exit code.
pub fn run_native(source: &str, cc: Option<&str>, args: &[String]) -> Result<i32, String> {
    let c = emit_c(source)?;
    let exe_name = if cfg!(windows) {
        "lusa-run.exe"
    } else {
        "lusa-run"
    };
    let dir = std::env::temp_dir().join(format!("lusa-crun-{}-{}", std::process::id(), nanos()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let exe = dir.join(exe_name);
    let build_result = build_c_source(&c, &exe, cc, None);
    if let Err(e) = build_result {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    let status = Command::new(&exe)
        .args(args)
        .status()
        .map_err(|e| format!("cannot run {}: {e}", exe.display()));
    let _ = std::fs::remove_dir_all(&dir);
    let status = status?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_round_trip_quotes_and_newlines() {
        let source = "local s = \"hi\"\nprint(s, 'x')\n";
        let literal = source_chunks_literal(source);
        assert!(literal.contains("string.char(34)"));
        assert!(literal.contains("string.char(10)"));
        assert!(!literal.contains("\\\""));
    }

    #[test]
    fn emits_c_for_typed_program() {
        let c = emit_c("local x: u32 = 1\nprint(x)\nreturn 0\n").expect("emit C");
        assert!(c.contains("uint32_t"));
        assert!(c.contains("int main(void)"));
    }

    #[test]
    fn rejects_mismatched_types_with_cargo_style_diagnostic() {
        let err = emit_c("local x: u32 = \"bad\"\nreturn 0\n").expect_err("type error");
        assert!(err.contains("error[E"), "diagnostic: {err}");
        assert!(err.contains("--> lusa:"), "diagnostic: {err}");
    }
}
