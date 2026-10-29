//! Module 27: safe, interrupt-driven peripheral drivers (`lusa::hal`).
//!
//! Static checks always run.  The host simulation drivers run in the
//! interpreter and the bytecode VM.  Cross builds and Renode execution skip
//! cleanly when the toolchain or emulator is missing, matching modules 25
//! and 26; the Renode workflow exports `LUSA_ARM_CC`,
//! `LUSA_ESP32_CC`/`LUSA_RISCV_CC`, and `LUSA_RENODE`, so every check runs in
//! CI.
//!
//! Each board runs two images under Renode:
//!
//! * the UART echo server (`examples/interrupt_uart_echo.lusa`, with the
//!   board's UART vector), fed `hello lusa`, `ping`, and `!` through the
//!   emulated UART;
//! * a driver check that claims all three blocks, pushes a burst through the
//!   SPI loopback, writes to and reads back from the I2C echo device at
//!   0x55, and expects a NACK from the empty address 0x22.  Renode's
//!   DesignWare I2C model (RP2040) does not model address NACKs, so that
//!   board skips the NACK step.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lusa::ast::StKind;
use lusa::cbackend;
use lusa::targets::{self, TargetSpec};

const ECHO_EXAMPLE: &str = "examples/interrupt_uart_echo.lusa";

/// What the echo server prints for the scripted session.
const ECHO_TRANSCRIPT: &str =
    "LUSA ECHO READY\nhello lusa\n[ 10 bytes]\nping\n[ 4 bytes]\nbye l=2\n";

/// Lines typed into the UART, one `WriteChar` per byte.
const ECHO_INPUT: &[&str] = &["hello lusa\n", "ping\n", "!"];

const DRIVERS_BANNER: &str =
    "LUSA DRIVERS OK\nclaims=ok spi=ok i2c=ok nack=ok\n";
const DRIVERS_BANNER_NO_NACK: &str = "LUSA DRIVERS OK\nclaims=ok spi=ok i2c=ok\n";

/// Interrupt handlers of the driver check, per board.
const STM32F4_HANDLERS: &str = r#"
#[interrupt("USART2")]
function serial_irq(): void
    uart::on_interrupt()
end

#[interrupt("SPI1")]
function spi_irq(): void
    spi::on_interrupt()
end

#[interrupt("I2C1_EV")]
function i2c_event_irq(): void
    i2c::on_interrupt()
end

#[interrupt("I2C1_ER")]
function i2c_error_irq(): void
    i2c::on_interrupt()
end
"#;

const RP2040_HANDLERS: &str = r#"
#[interrupt("UART0")]
function serial_irq(): void
    uart::on_interrupt()
end

#[interrupt("SPI0")]
function spi_irq(): void
    spi::on_interrupt()
end

#[interrupt("I2C0")]
function i2c_irq(): void
    i2c::on_interrupt()
end
"#;

/// UART0, SPI2, and I2C share CPU interrupt 11 on the ESP32-C3.
const ESP32C3_HANDLERS: &str = r#"
#[interrupt("machine_external")]
function external_irq(): void
    uart::on_interrupt()
    spi::on_interrupt()
    i2c::on_interrupt()
end
"#;

/// Driver check body.  `@NACK@` is replaced by the NACK step or by nothing.
const DRIVER_CHECK: &str = r##"
function say(port: uart::Uart, text: *mut u32, count: u32): void
    local i: u32 = 0
    unsafe do
        local cursor: *mut u32 = text
        while i < count do
            if uart::write(borrow port, *cursor) then
                cursor = cursor + 1
                i = i + 1
            end
        end
    end
end

function verdict(port: uart::Uart, label: *mut u32, count: u32, ok: bool): void
    local pass: [u32; 2] = [111, 107]
    local fail: [u32; 4] = [102, 97, 105, 108]
    say(borrow port, label, count)
    if ok then
        say(borrow port, &pass[0], 2)
    else
        say(borrow port, &fail[0], 4)
    end
end

function spi_next(bus: spi::Spi, expected: u32): bool
    match spi::read(borrow bus) with
    | Some(value) => return value == expected
    | None => return false
    end
end

function i2c_next(wire: i2c::I2c, expected: u32): bool
    match i2c::read(borrow wire) with
    | Some(value) => return value == expected
    | None => return false
    end
end

#[entry]
function main(): void
    local port: uart::Uart = uart::take():unwrap()
    local bus: spi::Spi = spi::take():unwrap()
    local wire: i2c::I2c = i2c::take():unwrap()
    uart::configure(borrow port, 115200)
    spi::configure(borrow bus, 1000000, 0)
    i2c::configure(borrow wire, 100000)
    interrupts_enable()

    -- Every block can be claimed once.
    local claims_ok: bool = false
    if uart::take():is_none() then
        if spi::take():is_none() then
            claims_ok = i2c::take():is_none()
        end
    end

    -- SPI: a three-byte burst through the MISO/MOSI loopback.
    spi::transfer(borrow bus, 159)
    spi::transfer(borrow bus, 1)
    spi::transfer(borrow bus, 2)
    spi::flush(borrow bus)
    local spi_ok: bool = false
    if spi::available(borrow bus) == 3 then
        if spi_next(borrow bus, 159) then
            if spi_next(borrow bus, 1) then
                spi_ok = spi_next(borrow bus, 2)
            end
        end
    end

    -- I2C: write three bytes to the echo device at 0x55 and read them back.
    i2c::queue(borrow wire, 16)
    i2c::queue(borrow wire, 32)
    i2c::queue(borrow wire, 48)
    local i2c_ok: bool = false
    if i2c::start_write(borrow wire, 85) then
        if i2c::wait(borrow wire) == 0 then
            if i2c::start_read(borrow wire, 85, 3) then
                if i2c::wait(borrow wire) == 0 then
                    if i2c_next(borrow wire, 16) then
                        if i2c_next(borrow wire, 32) then
                            i2c_ok = i2c_next(borrow wire, 48)
                        end
                    end
                end
            end
        end
    end

    local banner: [u32; 16] = [
        76, 85, 83, 65, 32, 68, 82, 73, 86, 69, 82, 83, 32, 79, 75, 10
    ]
    local claims: [u32; 7] = [99, 108, 97, 105, 109, 115, 61]
    local spi_label: [u32; 5] = [32, 115, 112, 105, 61]
    local i2c_label: [u32; 5] = [32, 105, 50, 99, 61]
    local newline: [u32; 1] = [10]
    say(borrow port, &banner[0], 16)
    verdict(borrow port, &claims[0], 7, claims_ok)
    verdict(borrow port, &spi_label[0], 5, spi_ok)
    verdict(borrow port, &i2c_label[0], 5, i2c_ok)
@NACK@
    say(borrow port, &newline[0], 1)
    uart::flush(borrow port)
    while true do
    end
end
"##;

/// NACK step: nothing answers at 0x22, so the write must end with status 1.
const NACK_STEP: &str = r#"
    i2c::queue(borrow wire, 1)
    local nack_ok: bool = false
    if i2c::start_write(borrow wire, 34) then
        nack_ok = i2c::wait(borrow wire) == 1
    end
    local nack_label: [u32; 6] = [32, 110, 97, 99, 107, 61]
    verdict(borrow port, &nack_label[0], 6, nack_ok)
"#;

fn driver_check(handlers: &str, nack: bool) -> String {
    let body = DRIVER_CHECK.replace("@NACK@", if nack { NACK_STEP } else { "" });
    format!(
        "#[no_std]\nuse lusa::hal::uart;\nuse lusa::hal::spi;\nuse lusa::hal::i2c;\n{handlers}\n{body}"
    )
}

/// The echo example with the UART handler moved to `vector`.
fn echo_for(vector: &str) -> String {
    example_source(ECHO_EXAMPLE).replace(
        "#[interrupt(\"USART2\")]",
        &format!("#[interrupt(\"{vector}\")]"),
    )
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn example_source(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lusa-module27-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 27 work directory");
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

fn cross_build(target: &'static TargetSpec, source: &str, tag: &str) -> Option<(PathBuf, PathBuf)> {
    if skip_compiler(target) {
        return None;
    }
    let dir = workdir(tag);
    let elf = dir.join("firmware.elf");
    let compiler = cbackend::build_with_target(source, &elf, None, None, target)
        .unwrap_or_else(|e| panic!("cross build for `{}` failed:\n{e}", target.name));
    eprintln!(
        "module 27: built {} for {} with {}",
        elf.display(),
        target.name,
        compiler.display_name()
    );
    let bytes = std::fs::read(&elf).expect("read cross-built ELF");
    assert_eq!(&bytes[..4], b"\x7fELF", "{} must produce an ELF", target.name);
    Some((dir, elf))
}

/// Renders the board's monitor script.  `input` lines are typed into the
/// UART after the firmware has had time to start, one byte at a time.
fn render_script(target: &TargetSpec, dir: &Path, elf: &Path, input: &[&str]) -> (PathBuf, PathBuf) {
    let platform = dir.join("platform.repl");
    std::fs::write(&platform, target.renode_platform).expect("write Renode platform");
    let uart = dir.join("uart.log");
    let models = target
        .write_renode_models(dir)
        .expect("write Renode peripheral models");
    let mut run = String::from("emulation RunFor \"0.2\"\n");
    for line in input {
        for byte in line.bytes() {
            run.push_str(&format!("sysbus.uart0 WriteChar {byte}\n"));
        }
        run.push_str("emulation RunFor \"0.1\"\n");
    }
    run.push_str("emulation RunFor \"0.8\"");
    let script = target
        .renode_script
        .replace("@MODELS@", &models)
        .replace("@PLATFORM@", &format!("@{}", slash_path(&platform)))
        .replace("@ELF@", &format!("@{}", slash_path(elf)))
        .replace("@UART_LOG@", &slash_path(&uart))
        .replace("emulation RunFor \"00:00:02\"", &run);
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
                return Ok(ChildResult { status, stdout, stderr });
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

fn run_on_renode(target: &'static TargetSpec, source: &str, input: &[&str], expected: &str, tag: &str) {
    let Some((dir, elf)) = cross_build(target, source, tag) else {
        return;
    };
    let Some(renode) = find_renode() else {
        eprintln!("skipping Renode execution: set LUSA_RENODE or put renode on PATH");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    };
    let (script, uart) = render_script(target, &dir, &elf, input);
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
    assert_eq!(output, expected, "UART output from `{}`", target.name);
    if std::env::var_os("LUSA_KEEP_RENODE_ARTIFACTS").is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
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

fn engine_errors(source: &str) -> (String, String, String) {
    let interp = lusa::run_source(source).expect_err("interpreter must reject");
    let vm = lusa::vm::run_source_vm(source).expect_err("VM must reject");
    let native = cbackend::emit_c(source).expect_err("C11 backend must reject");
    (interp.code.to_string(), vm.code.to_string(), native)
}

// ---------------------------------------------------------------------------
// `#[peripheral]` resource tokens
// ---------------------------------------------------------------------------

#[test]
fn peripheral_attribute_parses_and_rejects_misuse() {
    let block = lusa::parser::parse("#[peripheral]\nstruct Port { base: u32 }\nstruct Plain { a: u32 }\n")
        .expect("parse #[peripheral]");
    let flags: Vec<(String, bool)> = block
        .iter()
        .filter_map(|st| match &st.kind {
            StKind::Struct { name, peripheral, .. } => Some((name.clone(), *peripheral)),
            _ => None,
        })
        .collect();
    assert_eq!(flags, vec![("Port".to_string(), true), ("Plain".to_string(), false)]);

    let cases = [
        ("#[peripheral]\nfunction f(): void\nend\n", "only supported on structs"),
        ("#[peripheral]\nlocal x = 1\n", "only supported on structs"),
        (
            "#[peripheral]\n#[peripheral]\nstruct Port { base: u32 }\n",
            "duplicate `#[peripheral]` attribute",
        ),
    ];
    for (source, needle) in cases {
        let error = lusa::parser::parse(source).expect_err("must reject #[peripheral] misuse");
        assert!(error.msg.contains(needle), "expected `{needle}` in: {}", error.msg);
        let native = cbackend::emit_c(source).expect_err("self-hosted parser must reject");
        assert!(native.contains(needle), "expected `{needle}` in: {native}");
    }
}

#[test]
fn peripheral_tokens_move_in_every_engine() {
    let base = "#[peripheral]\n\
                struct Port { base: u32 }\n\
                function address(port: Port): u32\n\
                    return port.base\n\
                end\n\
                local port = Port{base: 1073759232}\n";
    // Lending the token keeps it with its owner.
    let lent = format!("{base}print(address(borrow port))\nprint(address(borrow port))\n");
    assert_eq!(both_engines(&lent), "1073759232\n1073759232");
    cbackend::emit_c(&lent).expect("borrowed token compiles natively");

    // A scalar-only struct would be copied; a token moves.
    let moved = format!("{base}local other = port\nprint(address(borrow port))\n");
    let (interp, vm, native) = engine_errors(&moved);
    assert_eq!(interp, "E1001");
    assert_eq!(vm, "E1001");
    assert!(native.contains("E1001"), "{native}");

    let passed = format!("{base}address(port)\nprint(address(borrow port))\n");
    let (interp, vm, native) = engine_errors(&passed);
    assert_eq!(interp, "E1001");
    assert_eq!(vm, "E1001");
    assert!(native.contains("E1001"), "{native}");
}

#[test]
fn peripheral_tokens_cannot_be_cloned() {
    let direct = "#[peripheral]\nstruct Port { base: u32 }\nlocal port = Port{base: 1}\nlocal copy = clone port\n";
    let nested = "#[peripheral]\nstruct Port { base: u32 }\nstruct Board { serial: Port }\n\
                  local board = Board{serial: Port{base: 1}}\nlocal copy = clone board\n";
    for source in [direct, nested] {
        let (interp, vm, native) = engine_errors(source);
        assert_eq!(interp, "E1006", "{source}");
        assert_eq!(vm, "E1006", "{source}");
        assert!(native.contains("E1006") && native.contains("`#[peripheral]`"), "{native}");
    }
}

// ---------------------------------------------------------------------------
// The bundled `lusa` crate
// ---------------------------------------------------------------------------

#[test]
fn hal_crate_resolves_from_any_module() {
    let source = r#"
use lusa::hal::uart;

mod app {
    use lusa::hal::spi;

    pub function spi_claimed(): bool
        local bus = spi::take():unwrap()
        return spi::take():is_none()
    end
}

local port = lusa::hal::uart::take():unwrap()
print(uart::take():is_none(), app::spi_claimed())
"#;
    assert_eq!(both_engines(source), "true true");
    // No `lusa::` path: nothing is spliced in.
    let plain = lusa::imports::expand_imports("print(1)\n", None).expect("expand");
    assert_eq!(plain, "print(1)\n");
}

#[test]
fn hal_crate_keeps_line_numbers_and_no_std_first() {
    let source = "#[no_std]\n-- serial\nuse lusa::hal::uart;\n\n#[entry]\nfunction main(): void\n    local port = uart::take():unwrap()\nend\n";
    let expanded = lusa::imports::expand_imports(source, None).expect("expand");
    assert!(expanded.starts_with("#[no_std]"), "{}", &expanded[..40]);
    assert_eq!(expanded.lines().count(), source.lines().count());
    let line = expanded.lines().nth(6).unwrap();
    assert!(line.contains("__lusa_m_lusa_hal_uart__take()"), "{line}");

    // A diagnostic in user code points at the user's own line.
    let moved = "use lusa::hal::uart;\n\nlocal port = uart::take():unwrap()\nuart::configure(port, 9600)\nprint(uart::baud(borrow port))\n";
    let error = lusa::run_source(moved).expect_err("moved token");
    assert_eq!(error.code, "E1001");
    let expanded = lusa::imports::expand_imports(moved, None).expect("expand");
    let at = error.span.expect("span").start;
    let line = expanded.chars().take(at).filter(|&c| c == '\n').count() + 1;
    assert_eq!(line, 5);
}

#[test]
fn hal_crate_requires_a_supported_board() {
    let source = "#[no_std]\nuse lusa::hal::uart;\n#[entry]\nfunction main(): void\nend\n";
    let error = cbackend::emit_c_for_target(source, None, &targets::RISCV32_VIRT)
        .expect_err("riscv32-virt ships no HAL");
    assert!(error.contains("`lusa::hal` has no drivers for target `riscv32-virt`"), "{error}");
    for target in [&targets::HOST, &targets::STM32F4, &targets::RP2040, &targets::ESP32C3] {
        assert!(!target.hal.is_empty(), "{}", target.name);
        let source = lusa::hal::crate_source(target.hal).expect("family");
        for module in ["pub mod uart", "pub mod spi", "pub mod i2c", "#[peripheral]"] {
            assert!(source.contains(module), "{}: {module}", target.name);
        }
    }
}

// ---------------------------------------------------------------------------
// Host simulation drivers (interpreter and VM)
// ---------------------------------------------------------------------------

#[test]
fn host_uart_queues_bytes_for_the_interrupt_handler() {
    let source = r#"
use lusa::hal::uart;

local port = uart::take():unwrap()
uart::configure(borrow port, 115200)
print("claimed", uart::take():is_none(), uart::baud(borrow port))

-- Transmit: queued bytes wait for the handler.
uart::write(borrow port, 79)
uart::write(borrow port, 75)
print("queued", uart::pending(borrow port), uart::is_busy(borrow port), uart::sim_irq_pending())
while uart::sim_irq_pending() do
    uart::on_interrupt()
end
local sent = ""
local more = true
while more do
    match uart::sim_sent() with
    | Some(byte) => sent = sent .. string.char(byte)
    | None => more = false
    end
end
print("sent", sent, uart::sim_transmitted(), uart::is_busy(borrow port))

-- Receive: the handler fills the ring; a full ring counts overruns.
print("empty", uart::read(borrow port):is_none())
local i = 0
while i < 64 do
    uart::sim_receive(65)
    i = i + 1
end
uart::on_interrupt()
i = 0
while i < 6 do
    uart::sim_receive(66)
    i = i + 1
end
uart::on_interrupt()
print("received", uart::available(borrow port), uart::overruns(borrow port))
print("first", uart::read(borrow port):unwrap())

-- Loopback: transmitted bytes come back on the receive line.
uart::configure(borrow port, 9600)
uart::sim_loopback(true)
uart::write(borrow port, 90)
uart::flush(borrow port)
uart::idle(borrow port)
print("loopback", uart::read(borrow port):unwrap())
"#;
    assert_eq!(
        both_engines(source),
        "claimed true 115200\n\
         queued 2 true true\n\
         sent OK 2 false\n\
         empty true\n\
         received 64 6\n\
         first 65\n\
         loopback 90"
    );
}

#[test]
fn host_spi_exchanges_full_duplex_bursts() {
    let source = r#"
use lusa::hal::spi;

local bus = spi::take():unwrap()
spi::configure(borrow bus, 4000000, 3)
spi::sim_reply_offset(1)
print("config", spi::mode(borrow bus), spi::frequency(borrow bus))
spi::transfer(borrow bus, 10)
spi::transfer(borrow bus, 20)
spi::transfer(borrow bus, 255)
print("busy", spi::is_busy(borrow bus), spi::pending(borrow bus))
spi::flush(borrow bus)
local replies = ""
local more = true
while more do
    match spi::read(borrow bus) with
    | Some(byte) => replies = replies .. byte .. " "
    | None => more = false
    end
end
print("replies", replies, spi::sim_frames(), spi::is_busy(borrow bus))
"#;
    assert_eq!(
        both_engines(source),
        "config 3 4000000\nbusy true 2\nreplies 11 21 0  3 false"
    );
}

#[test]
fn host_i2c_runs_writes_reads_and_nacks() {
    let source = r#"
use lusa::hal::i2c;

local wire = i2c::take():unwrap()
i2c::configure(borrow wire, 400000)
print("limits", i2c::max_read(borrow wire), i2c::max_write(borrow wire))
print("empty write", i2c::start_write(borrow wire, 85))
i2c::queue(borrow wire, 7)
i2c::queue(borrow wire, 8)
print("started", i2c::start_write(borrow wire, 85), i2c::is_busy(borrow wire))
print("busy queue", i2c::queue(borrow wire, 9), i2c::start_read(borrow wire, 85, 1))
print("write", i2c::wait(borrow wire), i2c::sim_events(), i2c::is_busy(borrow wire))
print("zero read", i2c::start_read(borrow wire, 85, 0))
i2c::start_read(borrow wire, 85, 2)
print("read", i2c::wait(borrow wire), i2c::available(borrow wire))
print("bytes", i2c::read(borrow wire):unwrap(), i2c::read(borrow wire):unwrap())
i2c::queue(borrow wire, 1)
i2c::start_write(borrow wire, 34)
print("nack", i2c::wait(borrow wire), i2c::status(borrow wire))
i2c::sim_attach(34)
i2c::queue(borrow wire, 3)
i2c::start_write(borrow wire, 34)
print("moved", i2c::wait(borrow wire))
"#;
    assert_eq!(
        both_engines(source),
        "limits 32 32\n\
         empty write false\n\
         started true true\n\
         busy queue false false\n\
         write 0 4 false\n\
         zero read false\n\
         read 0 2\n\
         bytes 7 8\n\
         nack 1 1\n\
         moved 0"
    );
}

// ---------------------------------------------------------------------------
// Boards: vectors, platforms, examples, and cross builds
// ---------------------------------------------------------------------------

#[test]
fn driver_vectors_match_hardware() {
    assert_eq!(targets::STM32F4.vector("i2c1_ev").unwrap().number, 47);
    assert_eq!(targets::STM32F4.vector("I2C1_ER").unwrap().number, 48);
    assert_eq!(targets::STM32F4.vector("spi1").unwrap().number, 51);
    assert_eq!(targets::STM32F4.vector("usart2").unwrap().number, 54);
    assert_eq!(targets::RP2040.vector("spi0").unwrap().number, 34);
    assert_eq!(targets::RP2040.vector("uart0").unwrap().number, 36);
    assert_eq!(targets::RP2040.vector("i2c0").unwrap().number, 39);
    assert_eq!(targets::ESP32C3.vector("machine_external").unwrap().number, 11);

    let words = |target: &TargetSpec| -> Vec<String> {
        target
            .startup_source()
            .lines()
            .filter(|line| line.trim_start().starts_with(".word"))
            .map(str::to_string)
            .collect()
    };
    let stm32 = words(&targets::STM32F4);
    for (index, name) in [(47, "i2c1_ev"), (48, "i2c1_er"), (51, "spi1")] {
        assert!(stm32[index].contains(&format!("lusa_isr_{name}")), "{}", stm32[index]);
    }
    let rp2040 = words(&targets::RP2040);
    assert_eq!(rp2040.len(), 40);
    assert!(rp2040[34].contains("lusa_isr_spi0"));
    assert!(rp2040[39].contains("lusa_isr_i2c0"));
}

#[test]
fn renode_platforms_wire_the_driver_peripherals() {
    let stm32 = targets::STM32F4.renode_platform;
    for needle in [
        "SPI.STM32SPI @ sysbus 0x40013000",
        "SPI.SPILoopback @ spi1",
        "I2C.STM32F1_I2C @ sysbus 0x40005400",
        "Mocks.EchoI2CDevice @ i2c1 0x55",
    ] {
        assert!(stm32.contains(needle), "STM32F4: {needle}");
    }
    let rp2040 = targets::RP2040.renode_platform;
    for needle in [
        "SPI.LusaRp2040Spi @ sysbus 0x4003C000",
        "I2C.DesignWare_APB_I2C @ sysbus 0x40044000",
        "Mocks.EchoI2CDevice @ i2c0 0x55",
    ] {
        assert!(rp2040.contains(needle), "RP2040: {needle}");
    }
    let esp32 = targets::ESP32C3.renode_platform;
    for needle in [
        "UART.LusaEsp32C3Uart @ sysbus 0x60000000",
        "SPI.LusaEsp32C3Spi @ sysbus 0x60024000",
        "I2C.LusaEsp32C3I2c @ sysbus 0x60013000",
        "Miscellaneous.CombinedInput",
        "rv32imc_zicsr",
    ] {
        assert!(esp32.contains(needle), "ESP32-C3: {needle}");
    }

    let dir = workdir("models");
    for (target, count) in [(&targets::STM32F4, 0), (&targets::RP2040, 1), (&targets::ESP32C3, 3)] {
        let lines = target.write_renode_models(&dir).expect("write models");
        assert_eq!(lines.lines().filter(|l| l.starts_with("include @")).count(), count, "{}", target.name);
        for model in target.renode_models {
            assert!(dir.join(model.file).is_file(), "{}", model.file);
            assert!(model.source.contains("namespace Antmicro.Renode.Peripherals"));
        }
        if count > 0 {
            assert!(target.renode_script.contains("@MODELS@"), "{}", target.name);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn echo_example_exists_and_is_documented() {
    let example = example_source(ECHO_EXAMPLE);
    for needle in ["#[no_std]", "use lusa::hal::uart;", "#[interrupt(\"USART2\")]", "uart::take()", "borrow port"] {
        assert!(example.contains(needle), "example lacks {needle}");
    }
    let readme = example_source("README.md");
    let language = example_source("docs/LANGUAGE.md");
    for doc in [&readme, &language] {
        assert!(doc.contains("interrupt_uart_echo.lusa"));
        assert!(doc.contains("#[peripheral]"));
        assert!(doc.contains("lusa::hal::uart"));
    }
}

#[test]
fn native_logic_and_constant_folding_compile() {
    // Regression: the self-hosted checker moved AST fields out of `and`/`or`
    // operands and folded literals, failing with E1001 inside the compiler.
    let source = "function f(a: u32, b: u32): bool\n\
                      return a == 1 and b == 0 or a > 2\n\
                  end\n\
                  local mask: u32 = 4 + 256 + 512\n\
                  print(f(1, 0), mask)\n";
    let c = cbackend::emit_c(source).expect("logic and folding compile");
    assert!(c.contains("772"), "constant folded");
}

#[test]
fn drivers_cross_build_for_every_board() {
    for (target, handlers, tag) in [
        (&targets::STM32F4, STM32F4_HANDLERS, "build-stm32f4"),
        (&targets::RP2040, RP2040_HANDLERS, "build-rp2040"),
        (&targets::ESP32C3, ESP32C3_HANDLERS, "build-esp32c3"),
    ] {
        if let Some((dir, _)) = cross_build(target, &driver_check(handlers, true), tag) {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
    // The host family also lowers to C (no hardware access).
    let host = driver_check("", true);
    cbackend::emit_c_for_target(&host, None, &targets::HOST).expect("host HAL emits C");
}

// ---------------------------------------------------------------------------
// Renode: the echo server and the driver check on every board
// ---------------------------------------------------------------------------

#[test]
fn renode_runs_uart_echo_on_stm32f4() {
    run_on_renode(
        &targets::STM32F4,
        &example_source(ECHO_EXAMPLE),
        ECHO_INPUT,
        ECHO_TRANSCRIPT,
        "echo-stm32f4",
    );
}

#[test]
fn renode_runs_uart_echo_on_rp2040() {
    run_on_renode(&targets::RP2040, &echo_for("UART0"), ECHO_INPUT, ECHO_TRANSCRIPT, "echo-rp2040");
}

#[test]
fn renode_runs_uart_echo_on_esp32c3() {
    run_on_renode(
        &targets::ESP32C3,
        &echo_for("machine_external"),
        ECHO_INPUT,
        ECHO_TRANSCRIPT,
        "echo-esp32c3",
    );
}

#[test]
fn renode_runs_spi_and_i2c_drivers_on_stm32f4() {
    run_on_renode(
        &targets::STM32F4,
        &driver_check(STM32F4_HANDLERS, true),
        &[],
        DRIVERS_BANNER,
        "drivers-stm32f4",
    );
}

#[test]
fn renode_runs_spi_and_i2c_drivers_on_rp2040() {
    run_on_renode(
        &targets::RP2040,
        &driver_check(RP2040_HANDLERS, false),
        &[],
        DRIVERS_BANNER_NO_NACK,
        "drivers-rp2040",
    );
}

#[test]
fn renode_runs_spi_and_i2c_drivers_on_esp32c3() {
    run_on_renode(
        &targets::ESP32C3,
        &driver_check(ESP32C3_HANDLERS, true),
        &[],
        DRIVERS_BANNER,
        "drivers-esp32c3",
    );
}
