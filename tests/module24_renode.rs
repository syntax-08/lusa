//! Module 24: bare-metal emulation with Renode.
//!
//! The static tests always run.  The real cross-compile + Renode checks skip
//! cleanly when no RISC-V compiler or Renode executable is available, so the
//! normal three-OS `cargo test` matrix stays green.  The dedicated
//! `.github/workflows/renode.yml` job installs both tools and sets
//! `LUSA_RISCV_CC` / `LUSA_RENODE` so these tests execute the firmware.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lusa::cbackend;
use lusa::targets::{self, TargetSpec};

const BANNER: &str = "LUSA RENODE UART OK\nlusa virt v2.0.0\n\n";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_source() -> String {
    std::fs::read_to_string(manifest_dir().join("examples/baremetal_renode_uart.lusa"))
        .expect("read bare-metal UART example")
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lusa-module24-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 24 work directory");
    dir
}

fn slash_path(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
        .replace('\\', "/")
}

fn find_renode() -> Option<PathBuf> {
    for key in ["LUSA_RENODE", "RENODE"] {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim().trim_matches('"');
            if value.is_empty() {
                continue;
            }
            let path = Path::new(value);
            if path.is_file() {
                return Some(path.to_path_buf());
            }
            if path.is_dir() {
                for name in ["renode.exe", "renode", "Renode.exe"] {
                    let candidate = path.join(name);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    let finder = if cfg!(windows) { "where" } else { "which" };
    for name in ["renode", "Renode"] {
        let output = Command::new(finder).arg(name).output().ok()?;
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
            {
                return Some(PathBuf::from(line));
            }
        }
    }
    None
}

fn skip_compiler(target: &TargetSpec) -> bool {
    match cbackend::find_target_compiler(target, None) {
        Ok(_) => false,
        Err(error) => {
            eprintln!("skipping Renode test for `{}`: {error}", target.name);
            true
        }
    }
}

fn skip_renode() -> Option<PathBuf> {
    match find_renode() {
        Some(path) => Some(path),
        None => {
            eprintln!("skipping Renode execution: set LUSA_RENODE or put renode on PATH");
            None
        }
    }
}

fn render_script(target: &TargetSpec, dir: &Path, elf: &Path) -> (PathBuf, PathBuf) {
    let platform = dir.join("platform.repl");
    std::fs::write(&platform, target.renode_platform).expect("write Renode platform");
    let uart = dir.join("uart.log");
    let models = target
        .write_renode_models(dir)
        .expect("write Renode peripheral models");
    let script = target
        .renode_script
        .replace("@MODELS@", &models)
        .replace("@PLATFORM@", &format!("@{}", slash_path(&platform)))
        .replace("@ELF@", &format!("@{}", slash_path(elf)))
        .replace("@UART_LOG@", &slash_path(&uart));
    let path = dir.join("run.resc");
    std::fs::write(&path, script).expect("write Renode script");
    (path, uart)
}

struct ChildResult {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_with_timeout(mut command: Command, timeout: Duration) -> Result<ChildResult, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", command.get_program().to_string_lossy()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    pipe.read_to_string(&mut stdout).ok();
                }
                if let Some(mut pipe) = child.stderr.take() {
                    pipe.read_to_string(&mut stderr).ok();
                }
                return Ok(ChildResult {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Renode did not finish within {timeout:?}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("cannot wait for Renode: {e}")),
        }
    }
}

fn run_example_on_renode(target: &'static TargetSpec, tag: &str) {
    if skip_compiler(target) {
        return;
    }
    let Some(renode) = skip_renode() else {
        return;
    };
    let dir = workdir(tag);
    let elf = dir.join("firmware.elf");
    let compiler = cbackend::build_with_target(
        &example_source(),
        &elf,
        None,
        Some(&manifest_dir().join("examples")),
        target,
    )
    .unwrap_or_else(|e| panic!("cross build for `{}` failed:\n{e}", target.name));
    eprintln!(
        "module 24: built {} for {} with {}",
        elf.display(),
        target.name,
        compiler.display_name()
    );

    let (script, uart) = render_script(target, &dir, &elf);
    let mut command = Command::new(&renode);
    command
        .current_dir(&dir)
        .arg("--disable-gui")
        .arg("--console")
        .arg("--plain")
        .arg("--hide-log")
        .arg(&script);
    let result = run_with_timeout(command, Duration::from_secs(120))
        .unwrap_or_else(|e| panic!("Renode run failed: {e}"));
    assert!(
        result.status.success(),
        "Renode exited with {}\nstdout:\n{}\nstderr:\n{}",
        result.status,
        result.stdout,
        result.stderr
    );

    let output = std::fs::read_to_string(&uart)
        .unwrap_or_else(|e| panic!("read UART log {}: {e}", uart.display()))
        .replace("\r\n", "\n");
    assert_eq!(output, BANNER, "UART output from `{}`", target.name);
    if std::env::var_os("LUSA_KEEP_RENODE_ARTIFACTS").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn renode_assets_define_memory_and_uart() {
    let rv32 = &targets::RISCV32_VIRT;
    assert!(rv32.renode_platform.contains("Memory.MappedMemory @ sysbus 0x80000000"));
    assert!(rv32.renode_platform.contains("UART.NS16550 @ sysbus 0x10000000"));
    assert!(rv32.renode_platform.contains("CPU.RiscV32"));
    assert!(rv32.renode_platform.contains("rv32imac"));
    assert!(rv32.renode_script.contains("sysbus LoadELF @ELF@"));
    assert!(rv32.renode_script.contains("CreateFileBackend \"@UART_LOG@\" true"));
    assert!(rv32.renode_script.contains("emulation RunFor \"00:00:01\""));

    let rv64 = &targets::RISCV64_VIRT;
    assert!(rv64.renode_platform.contains("CPU.RiscV64"));
    assert!(rv64.renode_platform.contains("rv64imac"));
    assert!(rv64.renode_script.contains("sysbus LoadELF @ELF@"));

    for relative in [
        "renode/lusa_riscv32_virt.repl",
        "renode/lusa_riscv64_virt.repl",
        "renode/lusa_riscv32_virt.resc",
        "renode/lusa_riscv64_virt.resc",
        "renode/startup_riscv.S",
        "renode/linker_riscv.ld",
    ] {
        assert!(
            manifest_dir().join(relative).is_file(),
            "missing Renode asset {relative}"
        );
    }
}

#[test]
fn example_emits_freestanding_uart_writes() {
    let c = cbackend::emit_c(&example_source()).expect("emit example C");
    assert!(c.starts_with("/* lusa:no_std */"), "example must be no_std");
    assert!(c.contains("void _start(void)"));
    assert!(c.contains("lusa_program_body"));
    assert!(c.contains("(volatile uint32_t *)"));
    assert!(c.contains("268435456"), "NS16550 base 0x10000000");
    assert!(c.contains("[38]"), "38-byte banner array");
    for forbidden in ["int main(void)", "printf(", "malloc(", "memcpy(", "memset("] {
        assert!(
            !c.contains(forbidden),
            "freestanding image must not contain `{forbidden}`"
        );
    }
}

#[test]
fn cross_target_rejects_hosted_programs() {
    let err = cbackend::build_with_target(
        "print(\"hosted\")\nreturn 0\n",
        &std::env::temp_dir().join("lusa-module24-hosted.elf"),
        None,
        None,
        &targets::RISCV32_VIRT,
    )
    .expect_err("hosted program must not cross-build");
    assert!(err.contains("requires a `#[no_std]` program"), "{err}");
}

#[test]
fn cross_target_requires_an_entry_function() {
    let err = cbackend::build_with_target(
        "#[no_std]\nlocal counter: u32 = 1\n",
        &std::env::temp_dir().join("lusa-module24-noentry.elf"),
        None,
        None,
        &targets::RISCV32_VIRT,
    )
    .expect_err("no_std program without entry must not cross-build");
    assert!(err.contains("requires an `#[entry]` function"), "{err}");
}

#[test]
fn cli_cross_build_writes_elf() {
    if skip_compiler(&targets::RISCV32_VIRT) {
        return;
    }
    let dir = workdir("cli");
    let elf = dir.join("firmware.elf");
    let output = Command::new(env!("CARGO_BIN_EXE_lusa"))
        .arg("build")
        .arg(manifest_dir().join("examples/baremetal_renode_uart.lusa"))
        .arg("--target")
        .arg("riscv32-virt")
        .arg("-o")
        .arg(&elf)
        .output()
        .expect("run lusalang build");
    assert!(
        output.status.success(),
        "lusalang build failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("for riscv32-virt"));
    let bytes = std::fs::read(&elf).expect("read ELF");
    assert_eq!(&bytes[..4], b"\x7fELF", "output must be an ELF image");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn renode_runs_riscv32_uart_example() {
    run_example_on_renode(&targets::RISCV32_VIRT, "renode32");
}

#[test]
fn renode_runs_riscv64_uart_example() {
    run_example_on_renode(&targets::RISCV64_VIRT, "renode64");
}
