//! Module 25: modern embedded microcontroller targets.
//!
//! Static checks always run.  Cross-build and Renode execution skip cleanly
//! when the corresponding toolchain or emulator is unavailable, so the normal
//! `cargo test` matrix stays green.  `.github/workflows/renode.yml` installs
//! xPack Arm GCC, xPack RISC-V GCC, and Renode, and exports `LUSA_ARM_CC`,
//! `LUSA_ESP32_CC`, and `LUSA_RENODE` so every check executes there.
//!
//! The examples are separate images rather than one file because each board
//! exposes a different register map:
//!
//! * `modern_mcu_peripherals.lusa`  -- STM32F4: USART2, GPIOA, TIM2
//! * `modern_mcu_rp2040.lusa`       -- RP2040: PL011 UART0, SIO, SysTick
//! * `modern_mcu_esp32c3.lusa`      -- ESP32-C3: UART0, GPIO, TIMG T0

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lusa::cbackend;
use lusa::targets::{self, TargetKind, TargetSpec};

const STM32F4_BANNER: &str = "LUSA MODERN MCU OK\nstm32f4 gpio=ok timer=ok\n\n";
const RP2040_BANNER: &str = "LUSA MODERN MCU OK\nrp2040 gpio=ok systick=ok\n\n";
const ESP32C3_BANNER: &str = "LUSA MODERN MCU OK\nesp32c3 gpio=ok timer=ok\n\n";

const STM32F4_EXAMPLE: &str = "examples/modern_mcu_peripherals.lusa";
const RP2040_EXAMPLE: &str = "examples/modern_mcu_rp2040.lusa";
const ESP32C3_EXAMPLE: &str = "examples/modern_mcu_esp32c3.lusa";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_source(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lusa-module25-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 25 work directory");
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
            eprintln!("skipping cross build for `{}`: {error}", target.name);
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
    let script = target
        .renode_script
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

fn cross_build_example(target: &'static TargetSpec, example: &str, tag: &str) -> Option<(PathBuf, PathBuf)> {
    if skip_compiler(target) {
        return None;
    }
    let dir = workdir(tag);
    let elf = dir.join("firmware.elf");
    let compiler = cbackend::build_with_target(
        &example_source(example),
        &elf,
        None,
        Some(&manifest_dir().join("examples")),
        target,
    )
    .unwrap_or_else(|e| panic!("cross build `{example}` for `{}` failed:\n{e}", target.name));
    eprintln!(
        "module 25: built {} for {} with {}",
        elf.display(),
        target.name,
        compiler.display_name()
    );
    let bytes = std::fs::read(&elf).expect("read cross-built ELF");
    assert_eq!(&bytes[..4], b"\x7fELF", "{} must produce an ELF", target.name);
    Some((dir, elf))
}

fn run_example_on_renode(
    target: &'static TargetSpec,
    example: &str,
    banner: &str,
    tag: &str,
) {
    let Some((dir, elf)) = cross_build_example(target, example, tag) else {
        return;
    };
    let Some(renode) = skip_renode() else {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };

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
        .unwrap_or_else(|e| panic!("Renode run for `{}` failed: {e}", target.name));
    assert!(
        result.status.success(),
        "Renode exited with {} for `{}`\nstdout:\n{}\nstderr:\n{}",
        result.status,
        target.name,
        result.stdout,
        result.stderr
    );

    let output = std::fs::read_to_string(&uart)
        .unwrap_or_else(|e| panic!("read UART log {}: {e}", uart.display()))
        .replace("\r\n", "\n");
    assert_eq!(output, banner, "UART output from `{}`", target.name);
    if std::env::var_os("LUSA_KEEP_RENODE_ARTIFACTS").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn modern_mcu_targets_are_registered() {
    assert_eq!(targets::RP2040.kind, TargetKind::Rp2040);
    assert_eq!(targets::RP2040.triple, "thumbv6m-none-eabi");
    assert_eq!(targets::STM32F4.kind, TargetKind::Stm32F4);
    assert_eq!(targets::STM32F4.triple, "thumbv7em-none-eabi");
    assert_eq!(targets::ESP32C3.kind, TargetKind::Esp32C3);
    assert_eq!(targets::ESP32C3.triple, "riscv32-esp-elf");

    for name in ["rp2040", "stm32f4", "esp32c3"] {
        let target = targets::find(name).unwrap_or_else(|| panic!("target `{name}` missing"));
        assert!(
            targets::CROSS_TARGETS.iter().any(|t| t.name == target.name),
            "`{name}` must be listed in CROSS_TARGETS"
        );
        assert!(target.arch_flags.iter().any(|flag| flag.starts_with("-m")));
        assert!(!target.linker_script.is_empty());
        assert!(!target.startup.is_empty());
        assert!(!target.renode_platform.is_empty());
        assert!(!target.renode_script.is_empty());
        assert_eq!(target.output_extension, "elf");
    }

    // Register movement must not change the existing RISC-V virt mapping.
    assert_eq!(
        targets::find("riscv32-unknown-elf").unwrap().kind,
        TargetKind::RiscV32Virt
    );
}

#[test]
fn modern_mcu_memory_maps_match_hardware() {
    assert!(targets::RP2040.linker_script.contains("ORIGIN = 0x10000000"));
    assert!(targets::RP2040.linker_script.contains("LENGTH = 2M"));
    assert!(targets::RP2040.linker_script.contains("ORIGIN = 0x20000000"));
    assert!(targets::RP2040.linker_script.contains("LENGTH = 264K"));
    assert!(targets::RP2040.renode_platform.contains("cpuType: \"cortex-m0\""));
    assert!(targets::RP2040.renode_platform.contains("UART.PL011 @ sysbus 0x40034000"));
    assert!(targets::RP2040.renode_platform.contains("systickFrequency: 125000000"));

    assert!(targets::STM32F4.linker_script.contains("ORIGIN = 0x08000000"));
    assert!(targets::STM32F4.linker_script.contains("LENGTH = 1M"));
    assert!(targets::STM32F4.linker_script.contains("ORIGIN = 0x20000000"));
    assert!(targets::STM32F4.linker_script.contains("LENGTH = 128K"));
    assert!(targets::STM32F4.renode_platform.contains("cpuType: \"cortex-m4\""));
    assert!(targets::STM32F4.renode_platform.contains("UART.STM32_UART"));
    assert!(targets::STM32F4.renode_platform.contains("GPIOPort.STM32_GPIOPort"));
    assert!(targets::STM32F4.renode_platform.contains("Timers.STM32_Timer"));

    assert!(targets::ESP32C3.linker_script.contains("ORIGIN = 0x42000000"));
    assert!(targets::ESP32C3.linker_script.contains("LENGTH = 4M"));
    assert!(targets::ESP32C3.linker_script.contains("ORIGIN = 0x3FC80000"));
    assert!(targets::ESP32C3.linker_script.contains("LENGTH = 400K"));
    assert!(targets::ESP32C3.renode_platform.contains("CPU.RiscV32"));
    assert!(targets::ESP32C3.renode_platform.contains("rv32imc"));
    assert!(targets::ESP32C3.renode_platform.contains("UART.ESP32_UART"));
    assert!(targets::ESP32C3.renode_platform.contains("Timers.Arm_CMSDK_Timer"));
}

#[test]
fn modern_mcu_assets_exist() {
    for relative in [
        "renode/startup_arm.S",
        "renode/linker_arm_rp2040.ld",
        "renode/linker_arm_stm32f4.ld",
        "renode/lusa_rp2040.repl",
        "renode/lusa_rp2040.resc",
        "renode/lusa_stm32f4.repl",
        "renode/lusa_stm32f4.resc",
        "renode/lusa_esp32c3.repl",
        "renode/lusa_esp32c3.resc",
        "examples/modern_mcu_peripherals.lusa",
        "examples/modern_mcu_rp2040.lusa",
        "examples/modern_mcu_esp32c3.lusa",
    ] {
        assert!(
            manifest_dir().join(relative).is_file(),
            "missing module 25 asset {relative}"
        );
    }

    for target in [&targets::RP2040, &targets::STM32F4, &targets::ESP32C3] {
        assert!(target.startup.contains("lusa_entry"));
        assert!(target.startup.contains("memcpy"));
        assert!(target.renode_script.contains("@PLATFORM@"));
        assert!(target.renode_script.contains("@ELF@"));
        assert!(target.renode_script.contains("@UART_LOG@"));
        assert!(target.renode_script.contains("CreateFileBackend"));
    }

    assert!(targets::RP2040.renode_script.contains("cpu1 IsHalted true"));
    assert!(targets::STM32F4.renode_script.contains("emulation RunFor \"00:00:02\""));
    assert!(targets::ESP32C3.renode_script.contains("emulation RunFor \"00:00:02\""));
}

#[test]
fn examples_emit_freestanding_volatile_mmio() {
    let cases = [
        (STM32F4_EXAMPLE, "1073872896"), // 0x40020000 GPIOA
        (RP2040_EXAMPLE, "0xD0000000"),  // SIO base, kept as an exact literal
        (ESP32C3_EXAMPLE, "1610629120"), // 0x60004000 GPIO
    ];
    for (relative, address) in cases {
        let c = cbackend::emit_c_with_base(
            &example_source(relative),
            Some(&manifest_dir().join("examples")),
        )
        .unwrap_or_else(|e| panic!("emit {relative}: {e}"));
        assert!(
            c.starts_with("/* lusa:no_std */"),
            "{relative} must be #[no_std]"
        );
        assert!(c.contains("void _start(void)"), "{relative} entry wrapper");
        assert!(c.contains("volatile uint32_t *"), "{relative} volatile MMIO");
        assert!(c.contains(address), "{relative} MMIO base {address}");
        assert!(c.contains("while (1)"), "{relative} must park after the test");
        for forbidden in ["int main(void)", "printf(", "malloc(", "memcpy(", "memset("] {
            assert!(
                !c.contains(forbidden),
                "{relative} must not reference `{forbidden}`"
            );
        }
    }
}

#[test]
fn cross_builds_three_modern_mcus() {
    let cases = [
        (&targets::STM32F4, STM32F4_EXAMPLE, "stm32f4"),
        (&targets::RP2040, RP2040_EXAMPLE, "rp2040"),
        (&targets::ESP32C3, ESP32C3_EXAMPLE, "esp32c3"),
    ];
    for (target, example, tag) in cases {
        let Some((dir, _elf)) = cross_build_example(target, example, &format!("build-{tag}"))
        else {
            continue;
        };
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn cli_accepts_modern_mcu_target_names() {
    let cases = [
        ("stm32f4", STM32F4_EXAMPLE, "cli-stm32f4"),
        ("rp2040", RP2040_EXAMPLE, "cli-rp2040"),
        ("esp32c3", ESP32C3_EXAMPLE, "cli-esp32c3"),
    ];
    for (name, example, tag) in cases {
        let target = targets::find(name).expect("target registered");
        if skip_compiler(target) {
            continue;
        }
        let dir = workdir(tag);
        let elf = dir.join("firmware.elf");
        let output = Command::new(env!("CARGO_BIN_EXE_lusa"))
            .arg("build")
            .arg(manifest_dir().join(example))
            .arg("--target")
            .arg(name)
            .arg("-o")
            .arg(&elf)
            .output()
            .expect("run lusalang build");
        assert!(
            output.status.success(),
            "lusalang build --target {name} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("for {name}")),
            "build output should name the target"
        );
        let bytes = std::fs::read(&elf).expect("read ELF");
        assert_eq!(&bytes[..4], b"\x7fELF");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn renode_runs_stm32f4_peripheral_example() {
    run_example_on_renode(
        &targets::STM32F4,
        STM32F4_EXAMPLE,
        STM32F4_BANNER,
        "renode-stm32f4",
    );
}

#[test]
fn renode_runs_rp2040_gpio_and_systick_example() {
    run_example_on_renode(
        &targets::RP2040,
        RP2040_EXAMPLE,
        RP2040_BANNER,
        "renode-rp2040",
    );
}

#[test]
fn renode_runs_esp32c3_gpio_and_timer_example() {
    run_example_on_renode(
        &targets::ESP32C3,
        ESP32C3_EXAMPLE,
        ESP32C3_BANNER,
        "renode-esp32c3",
    );
}
