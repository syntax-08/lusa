//! Module 26: hardware interrupts and exception vectors.
//!
//! Static checks always run.  Cross builds and Renode execution skip cleanly
//! when the toolchain or emulator is unavailable, matching module 25.  The
//! Renode workflow exports `LUSA_ARM_CC`, `LUSA_ESP32_CC`/`LUSA_RISCV_CC`, and
//! `LUSA_RENODE`, so every check executes in CI.
//!
//! Each board has its own interrupt source:
//!
//! * STM32F4 -- `examples/interrupt_driven_timer.lusa` uses TIM2 at vector 44
//! * RP2040  -- architectural SysTick at exception 15
//! * ESP32-C3 -- CLINT machine timer at `mcause` 7
//!
//! The inline images below print a fixed banner only after the ISR has run at
//! least three times and mutated both the counter and the `fired` flag.  The
//! main program then verifies the `interrupts_disable`/`restore` protocol.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lusa::ast::{ExprKind, StKind};
use lusa::cbackend;
use lusa::targets::{self, TargetSpec};

const STM32F4_BANNER: &str =
    "LUSA INTERRUPT OK\nstm32f4 irq=ok count=ok flag=ok state=ok restored=ok\n\n";
const RP2040_BANNER: &str =
    "LUSA INTERRUPT OK\nrp2040 irq=ok count=ok flag=ok state=ok restored=ok\n\n";
const ESP32C3_BANNER: &str =
    "LUSA INTERRUPT OK\nesp32c3 irq=ok count=ok flag=ok state=ok restored=ok\n\n";

const STM32F4_EXAMPLE: &str = "examples/interrupt_driven_timer.lusa";

/// RP2040 firmware: bare `#[interrupt]` resolves to SysTick, which the NVIC
/// model treats as exception 15.  The ISR publishes the counter/flag through
/// volatile accesses so the polling loop cannot be optimized away.
const RP2040_FIRMWARE: &str = r#"
#[no_std]

local ticks: u32 = 0
local fired: u32 = 0

function uart_init(): void
    unsafe do
        local uart: *mut u32 = 0x40034000
        local ibrd: *mut u32 = uart + 9
        local fbrd: *mut u32 = uart + 10
        local lcr: *mut u32 = uart + 11
        local control: *mut u32 = uart + 12
        ibrd.write_volatile(8)
        fbrd.write_volatile(44)
        lcr.write_volatile(0x70)
        control.write_volatile(0x301)
    end
end

function uart_putc(byte: u32): void
    unsafe do
        local uart: *mut u32 = 0x40034000
        local data: *mut u32 = uart
        local flags: *mut u32 = uart + 6
        while (flags.read_volatile() // 32) % 2 ~= 0 do
        end
        data.write_volatile(byte)
    end
end

function uart_puts(message: *mut u32, length: u32): void
    local i: u32 = 0
    unsafe do
        local cursor: *mut u32 = message
        while i < length do
            uart_putc(cursor.read_volatile())
            cursor = cursor + 1
            i = i + 1
        end
    end
end

#[interrupt]
function systick_isr(): void
    unsafe do
        local next: u32 = volatile_read(&ticks) + 1
        volatile_write(&ticks, next)
        volatile_write(&fired, 1)
    end
end

#[entry]
function _start(): void
    uart_init()
    unsafe do
        -- 0xE000E010 as decimal: hex tokens containing `e` parse as floats.
        local systick: *mut u32 = 3758153744
        local control: *mut u32 = systick
        local reload: *mut u32 = systick + 1
        local value: *mut u32 = systick + 2
        reload.write_volatile(124999) -- 125 MHz / 125000 = 1 kHz
        value.write_volatile(0)
        control.write_volatile(7)     -- CLKSOURCE | TICKINT | ENABLE
    end
    interrupts_enable()

    unsafe do
        while volatile_read(&ticks) < 3 do
        end
    end

    local before: u32 = interrupts_disable()
    local nested: u32 = interrupts_disable()
    interrupts_restore(nested)
    local count: u32 = 0
    local flag: u32 = 0
    unsafe do
        count = volatile_read(&ticks)
        flag = volatile_read(&fired)
    end
    interrupts_restore(before)
    local restored: u32 = interrupts_disable()
    interrupts_enable()

    local prefix: [u32; 18] = [
        76, 85, 83, 65, 32, 73, 78, 84, 69, 82, 82, 85, 80, 84, 32, 79,
        75, 10
    ]
    local board: [u32; 11] = [114, 112, 50, 48, 52, 48, 32, 105, 114, 113, 61]
    local count_label: [u32; 7] = [32, 99, 111, 117, 110, 116, 61]
    local flag_label: [u32; 6] = [32, 102, 108, 97, 103, 61]
    local state_label: [u32; 7] = [32, 115, 116, 97, 116, 101, 61]
    local restored_label: [u32; 10] = [
        32, 114, 101, 115, 116, 111, 114, 101, 100, 61
    ]
    local ok: [u32; 2] = [111, 107]
    local fail: [u32; 4] = [102, 97, 105, 108]
    local newline: [u32; 2] = [10, 10]

    local state_ok: bool = false
    if before == 1 then
        if nested == 0 then
            state_ok = true
        end
    end

    uart_puts(&prefix[0], 18)
    uart_puts(&board[0], 11)
    if count > 0 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&count_label[0], 7)
    if count >= 3 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&flag_label[0], 6)
    if flag == 1 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&state_label[0], 7)
    if state_ok then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&restored_label[0], 10)
    if restored == 1 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&newline[0], 2)

    while true do
    end
end
"#;

/// ESP32-C3 firmware: bare `#[interrupt]` resolves to the CLINT machine timer
/// (`mcause` 7).  The CLINT lives at 0x20008000; `mtimecmp` is at +0x4000 and
/// `mtime` at +0xBFF8, so the pointer offsets are expressed in u32 words.
const ESP32C3_FIRMWARE: &str = r#"
#[no_std]

local ticks: u32 = 0
local fired: u32 = 0

function uart_putc(byte: u32): void
    unsafe do
        local fifo: *mut u32 = 0x60000000
        fifo.write_volatile(byte)
    end
end

function uart_puts(message: *mut u32, length: u32): void
    local i: u32 = 0
    unsafe do
        local cursor: *mut u32 = message
        while i < length do
            uart_putc(cursor.read_volatile())
            cursor = cursor + 1
            i = i + 1
        end
    end
end

#[interrupt]
function machine_timer_isr(): void
    unsafe do
        -- Program the next compare before acknowledging the current one.
        local clint: *mut u32 = 536903680 -- 0x20008000
        local compare_low: *mut u32 = clint + 4096
        local time_low: *mut u32 = clint + 12286
        compare_low.write_volatile(time_low.read_volatile() + 160000)
        local next: u32 = volatile_read(&ticks) + 1
        volatile_write(&ticks, next)
        volatile_write(&fired, 1)
    end
end

#[entry]
function _start(): void
    unsafe do
        local clint: *mut u32 = 536903680
        local compare_low: *mut u32 = clint + 4096
        local compare_high: *mut u32 = clint + 4097
        local time_low: *mut u32 = clint + 12286
        -- 16 MHz / 160000 = 100 Hz.  Keep the high word clear for this image.
        compare_high.write_volatile(4294967295)
        compare_low.write_volatile(time_low.read_volatile() + 160000)
        compare_high.write_volatile(0)
    end
    interrupts_enable()

    unsafe do
        while volatile_read(&ticks) < 3 do
        end
    end

    local before: u32 = interrupts_disable()
    local nested: u32 = interrupts_disable()
    interrupts_restore(nested)
    local count: u32 = 0
    local flag: u32 = 0
    unsafe do
        count = volatile_read(&ticks)
        flag = volatile_read(&fired)
    end
    interrupts_restore(before)
    local restored: u32 = interrupts_disable()
    interrupts_enable()

    local prefix: [u32; 18] = [
        76, 85, 83, 65, 32, 73, 78, 84, 69, 82, 82, 85, 80, 84, 32, 79,
        75, 10
    ]
    local board: [u32; 12] = [
        101, 115, 112, 51, 50, 99, 51, 32, 105, 114, 113, 61
    ]
    local count_label: [u32; 7] = [32, 99, 111, 117, 110, 116, 61]
    local flag_label: [u32; 6] = [32, 102, 108, 97, 103, 61]
    local state_label: [u32; 7] = [32, 115, 116, 97, 116, 101, 61]
    local restored_label: [u32; 10] = [
        32, 114, 101, 115, 116, 111, 114, 101, 100, 61
    ]
    local ok: [u32; 2] = [111, 107]
    local fail: [u32; 4] = [102, 97, 105, 108]
    local newline: [u32; 2] = [10, 10]

    local state_ok: bool = false
    if before == 1 then
        if nested == 0 then
            state_ok = true
        end
    end

    uart_puts(&prefix[0], 18)
    uart_puts(&board[0], 12)
    if count > 0 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&count_label[0], 7)
    if count >= 3 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&flag_label[0], 6)
    if flag == 1 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&state_label[0], 7)
    if state_ok then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&restored_label[0], 10)
    if restored == 1 then
        uart_puts(&ok[0], 2)
    else
        uart_puts(&fail[0], 4)
    end
    uart_puts(&newline[0], 2)

    while true do
    end
end
"#;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_source(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lusa-module26-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 26 work directory");
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

fn cross_build(
    target: &'static TargetSpec,
    source: &str,
    tag: &str,
) -> Option<(PathBuf, PathBuf)> {
    if skip_compiler(target) {
        return None;
    }
    let dir = workdir(tag);
    let elf = dir.join("firmware.elf");
    let compiler = cbackend::build_with_target(source, &elf, None, None, target)
        .unwrap_or_else(|e| panic!("cross build for `{}` failed:\n{e}", target.name));
    eprintln!(
        "module 26: built {} for {} with {}",
        elf.display(),
        target.name,
        compiler.display_name()
    );
    let bytes = std::fs::read(&elf).expect("read cross-built ELF");
    assert_eq!(&bytes[..4], b"\x7fELF", "{} must produce an ELF", target.name);
    Some((dir, elf))
}

fn run_on_renode(
    target: &'static TargetSpec,
    source: &str,
    banner: &str,
    tag: &str,
) {
    let Some((dir, elf)) = cross_build(target, source, tag) else {
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

fn both_engines(source: &str) -> (String, String) {
    let interp = lusa::run_source(source)
        .expect("interpreter failed")
        .join("\n");
    let vm = lusa::vm::run_source_vm(source)
        .expect("VM failed")
        .join("\n");
    (interp, vm)
}

fn function_name(st: &lusa::ast::St) -> Option<&str> {
    match &st.kind {
        StKind::FuncSt { target, .. } => match &target.kind {
            ExprKind::Var(name) => Some(name),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn interrupt_attribute_parses_bare_and_explicit_vectors() {
    let block = lusa::parser::parse(
        "#[no_std]\n\
         #[interrupt]\n\
         function beat(): void\n\
         end\n\
         #[interrupt(\"TIM2\")]\n\
         function tick(): void\n\
         end\n",
    )
    .expect("parse interrupt attributes");

    let mut seen = Vec::new();
    for st in &block {
        if let StKind::FuncSt { interrupt, .. } = &st.kind {
            seen.push((function_name(st).unwrap().to_string(), interrupt.clone()));
        }
    }
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "beat");
    assert!(seen[0].1.as_ref().unwrap().vector.is_none());
    assert_eq!(seen[1].0, "tick");
    assert_eq!(
        seen[1].1.as_ref().unwrap().vector.as_deref(),
        Some("TIM2")
    );
}

#[test]
fn interrupt_attribute_rejects_misuse_at_parse_time() {
    let cases = [
        (
            "local f = 1\n#[interrupt]\nlocal x = 1\n",
            "cannot annotate a local",
        ),
        (
            "#[interrupt]\nstruct Pair { a: u32 }\n",
            "only supported on functions or globals",
        ),
        (
            "#[interrupt(\"default\")]\nfunction bad(): void\nend\n",
            "reserved",
        ),
        (
            "#[interrupt(\"TIM-2\")]\nfunction bad(): void\nend\n",
            "invalid character in interrupt vector name",
        ),
        (
            "#[interrupt(5)]\nfunction bad(): void\nend\n",
            "expected an interrupt vector name",
        ),
        (
            "#[interrupt]\n#[interrupt(\"tim2\")]\nfunction bad(): void\nend\n",
            "duplicate `#[interrupt]` attribute",
        ),
    ];
    for (source, needle) in cases {
        let error = lusa::parser::parse(source).expect_err("must reject interrupt misuse");
        assert!(
            error.msg.contains(needle),
            "expected `{needle}` in: {}",
            error.msg
        );
    }
}

#[test]
fn interrupt_functions_require_isr_signatures() {
    let cases = [
        (
            "#[interrupt]\nfunction bad(value: u32): void\nend\n",
            "cannot take parameters",
        ),
        (
            "#[interrupt]\nfunction bad(): u32\n    return 1\nend\n",
            "must return `void`",
        ),
        (
            "#[interrupt]\nasync function bad(): void\nend\n",
            "cannot be `async`",
        ),
        (
            "#[interrupt]\n#[entry]\nfunction bad(): void\nend\n",
            "`#[entry]` and `#[interrupt]`",
        ),
        (
            "#[interrupt]\n#[global_allocator]\nfunction bad(): void\nend\n",
            "`#[global_allocator]` and `#[interrupt]`",
        ),
        (
            "#[interrupt]\nfunction bad:run(): void\nend\n",
            "cannot annotate a method",
        ),
        (
            "#[interrupt(\"tim2\")]\nfunction first(): void\nend\n\
             #[interrupt(\"TIM2\")]\nfunction second(): void\nend\n",
            "duplicate `#[interrupt]` for vector `tim2`",
        ),
    ];
    for (source, needle) in cases {
        let error = cbackend::emit_c(source).expect_err("typed checker must reject ISR");
        assert!(
            error.contains(needle),
            "expected `{needle}` in:\n{error}"
        );
    }
}

#[test]
fn target_vector_tables_match_hardware() {
    assert_eq!(targets::STM32F4.vector("tim2").unwrap().number, 44);
    assert_eq!(targets::STM32F4.vector("usart2").unwrap().number, 54);
    assert_eq!(targets::RP2040.vector("systick").unwrap().number, 15);
    assert_eq!(targets::RP2040.vector("uart0").unwrap().number, 36);
    assert_eq!(
        targets::ESP32C3.vector("machine_timer").unwrap().number,
        7
    );
    assert_eq!(
        targets::ESP32C3.vector("machine_external").unwrap().number,
        11
    );
    assert_eq!(targets::STM32F4.default_vector, "tim2");
    assert_eq!(targets::RP2040.default_vector, "systick");
    assert_eq!(targets::ESP32C3.default_vector, "machine_timer");
    assert!(targets::HOST.default_vector.is_empty());
    assert!(targets::HOST.vectors.is_empty());

    // The rendered ARM table always has one word per exception from 2 through
    // the highest device vector, after the initial SP and reset words.
    let stm32 = targets::STM32F4.startup_source();
    let words: Vec<&str> = stm32
        .lines()
        .filter(|line| line.trim_start().starts_with(".word"))
        .collect();
    assert_eq!(words.len(), 55, "STM32F4 vector table size");
    assert!(words[44].contains("lusa_isr_tim2"), "{}", words[44]);
    assert!(words[54].contains("lusa_isr_usart2"), "{}", words[54]);
    assert!(words[15].contains("lusa_isr_systick"), "{}", words[15]);

    let rp2040 = targets::RP2040.startup_source();
    let words: Vec<&str> = rp2040
        .lines()
        .filter(|line| line.trim_start().starts_with(".word"))
        .collect();
    assert_eq!(words.len(), 37, "RP2040 vector table size");
    assert!(words[15].contains("lusa_isr_systick"), "{}", words[15]);
    assert!(words[36].contains("lusa_isr_uart0"), "{}", words[36]);

    let esp32 = targets::ESP32C3.startup_source();
    let words: Vec<&str> = esp32
        .lines()
        .filter(|line| line.trim_start().starts_with(".word"))
        .collect();
    assert_eq!(words.len(), 12, "ESP32-C3 trap table size");
    assert!(words[3].contains("lusa_isr_machine_software"));
    assert!(words[7].contains("lusa_isr_machine_timer"));
    assert!(words[11].contains("lusa_isr_machine_external"));

    for (target, vector) in [
        (&targets::STM32F4, "tim2"),
        (&targets::RP2040, "systick"),
        (&targets::ESP32C3, "machine_timer"),
    ] {
        let linker = target.linker_source();
        assert!(
            linker.contains(&format!(
                "PROVIDE(lusa_isr_{vector} = _default_handler);"
            )),
            "{} linker fallback",
            target.name
        );
    }
}

#[test]
fn interrupt_vector_validation_resolves_defaults_and_duplicates() {
    let stm32_c = "/* lusa:interrupt vector=default */\n\
                   void lusa_isr_tim2(void) {}\n";
    assert!(cbackend::validate_interrupt_vectors(stm32_c, &targets::STM32F4).is_ok());

    let upper = "/* lusa:interrupt vector=TIM2 */\n";
    assert!(cbackend::validate_interrupt_vectors(upper, &targets::STM32F4).is_ok());

    let wrong_board = "/* lusa:interrupt vector=tim2 */\n";
    let error = cbackend::validate_interrupt_vectors(wrong_board, &targets::RP2040)
        .expect_err("TIM2 is not an RP2040 vector");
    assert!(error.contains("unknown interrupt vector `tim2`"), "{error}");
    assert!(error.contains("systick"), "available vectors: {error}");

    let duplicate = "/* lusa:interrupt vector=default */\n\
                     /* lusa:interrupt vector=tim2 */\n";
    let error = cbackend::validate_interrupt_vectors(duplicate, &targets::STM32F4)
        .expect_err("bare and explicit TIM2 collide");
    assert!(error.contains("duplicate `#[interrupt]`"), "{error}");

    let host = "/* lusa:interrupt vector=default */\n";
    let error = cbackend::validate_interrupt_vectors(host, &targets::HOST)
        .expect_err("host has no default vector");
    assert!(error.contains("does not define a default interrupt vector"), "{error}");
}

#[test]
fn interrupt_intrinsics_track_the_global_mask() {
    let source = "local first: u32 = interrupts_disable()\n\
                  local nested: u32 = interrupts_disable()\n\
                  interrupts_restore(nested)\n\
                  local still: u32 = interrupts_disable()\n\
                  interrupts_restore(first)\n\
                  local again: u32 = interrupts_disable()\n\
                  interrupts_restore(again)\n\
                  print(first, nested, still, again)\n";
    let (interp, vm) = both_engines(source);
    assert_eq!(interp, "1 0 0 1");
    assert_eq!(vm, interp);

    let source = "interrupts_enable()\nprint(interrupts_disable())\n";
    let (interp, vm) = both_engines(source);
    assert_eq!(interp, "1");
    assert_eq!(vm, interp);
}

#[test]
fn interrupt_c_backend_emits_wrappers_and_metadata() {
    let source = "#[no_std]\n\
                  local ticks: u32 = 0\n\
                  #[interrupt]\n\
                  function timer_tick(): void\n\
                      ticks = ticks + 1\n\
                  end\n\
                  #[interrupt(\"TIM2\")]\n\
                  function tim2_tick(): void\n\
                      ticks = ticks + 1\n\
                  end\n\
                  #[entry]\n\
                  function _start(): void\n\
                      timer_tick()\n\
                      while true do\n\
                      end\n\
                  end\n";
    let c = cbackend::emit_c(source).expect("emit interrupt wrappers");
    assert!(c.contains("/* lusa:interrupt vector=default */"), "{c}");
    assert!(c.contains("/* lusa:interrupt vector=tim2 */"), "{c}");
    assert!(
        c.contains("void LUSA_ISR_NAME(LUSA_DEFAULT_VECTOR)(void)"),
        "{c}"
    );
    assert!(c.contains("void lusa_isr_tim2(void)"), "{c}");
    assert!(c.contains("__attribute__((used))"), "{c}");
    assert!(c.contains("cpsid i"), "ARM critical section:\n{c}");
    assert!(c.contains("csrci mstatus, 8"), "RISC-V critical section:\n{c}");

    let example = example_source(STM32F4_EXAMPLE);
    let c = cbackend::emit_c(&example).expect("emit interrupt example");
    assert!(c.contains("/* lusa:interrupt vector=tim2 */"), "{c}");
    assert!(c.contains("void lusa_isr_tim2(void)"), "{c}");
    assert!(!c.contains("void lusa_isr_default(void)"), "{c}");

    // The example itself must be target-valid, and the other boards must
    // reject its explicit TIM2 vector before invoking their compilers.
    assert!(cbackend::validate_interrupt_vectors(&c, &targets::STM32F4).is_ok());
    let error = cbackend::validate_interrupt_vectors(&c, &targets::RP2040)
        .expect_err("TIM2 example is not an RP2040 image");
    assert!(error.contains("unknown interrupt vector `tim2`"), "{error}");
}

#[test]
fn interrupt_example_exists_and_is_documented() {
    let path = manifest_dir().join(STM32F4_EXAMPLE);
    assert!(path.is_file(), "missing {STM32F4_EXAMPLE}");
    let source = example_source(STM32F4_EXAMPLE);
    assert!(source.contains("#[no_std]"));
    assert!(source.contains("#[interrupt(\"TIM2\")]"));
    assert!(source.contains("interrupts_disable()"));
    assert!(source.contains("interrupts_restore("));
    assert!(source.contains("volatile_read(&ticks)"));
    assert!(source.contains("local psc: *mut u32 = tim + 10"));
    assert!(source.contains("local arr: *mut u32 = tim + 11"));
}

#[test]
fn cross_builds_interrupt_images_for_all_boards() {
    let cases = [
        (&targets::STM32F4, example_source(STM32F4_EXAMPLE), "build-stm32f4"),
        (&targets::RP2040, RP2040_FIRMWARE.to_string(), "build-rp2040"),
        (&targets::ESP32C3, ESP32C3_FIRMWARE.to_string(), "build-esp32c3"),
    ];
    for (target, source, tag) in cases {
        let Some((dir, _elf)) = cross_build(target, &source, tag) else {
            continue;
        };
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn renode_runs_stm32f4_timer_interrupt() {
    run_on_renode(
        &targets::STM32F4,
        &example_source(STM32F4_EXAMPLE),
        STM32F4_BANNER,
        "renode-stm32f4",
    );
}

#[test]
fn renode_runs_rp2040_systick_interrupt() {
    run_on_renode(
        &targets::RP2040,
        RP2040_FIRMWARE,
        RP2040_BANNER,
        "renode-rp2040",
    );
}

#[test]
fn renode_runs_esp32c3_machine_timer_interrupt() {
    run_on_renode(
        &targets::ESP32C3,
        ESP32C3_FIRMWARE,
        ESP32C3_BANNER,
        "renode-esp32c3",
    );
}
