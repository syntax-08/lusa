//! Module 28: the embedded async executor and cooperative task scheduler.
//!
//! The executor semantics (round-robin polling, `task.wait`/`task.wake`,
//! `task.sleep` on the tick clock, parking through awaited children, and
//! blocking `await`) are checked in the interpreter and the bytecode VM, and
//! natively whenever a host C compiler is available.  The async `lusa::hal`
//! wrappers run against the host models in both dynamic engines.
//!
//! Cross builds and Renode execution skip cleanly when the toolchain or the
//! emulator is missing, matching modules 25 through 27; the Renode workflow
//! exports `LUSA_ARM_CC`, `LUSA_ESP32_CC`/`LUSA_RISCV_CC`, and `LUSA_RENODE`,
//! so every check runs in CI.  Each board runs two images:
//!
//! * `examples/async_multitasking_firmware.lusa` (with the board's vectors):
//!   an LED blinker on the tick timer, a UART echo console, and a reporter
//!   woken by an application event, fed `hello lusa`, `ping`, and `!`;
//! * a scheduler check: two tasks that yield must alternate, and two
//!   timer-driven tasks must wake in deadline order.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use lusa::cbackend;
use lusa::targets::{self, TargetSpec};

const EXAMPLE: &str = "examples/async_multitasking_firmware.lusa";

/// The example's handlers as shipped (STM32F4 vectors).
const STM32F4_EXAMPLE_HANDLERS: &str = r#"#[interrupt("TIM2")]
function tick_irq(): void
    timer::on_interrupt()
end

#[interrupt("USART2")]
function serial_irq(): void
    uart::on_interrupt()
end
"#;

const RP2040_HANDLERS: &str = r#"#[interrupt("SYSTICK")]
function tick_irq(): void
    timer::on_interrupt()
end

#[interrupt("UART0")]
function serial_irq(): void
    uart::on_interrupt()
end
"#;

/// The tick timer and UART0 share CPU interrupt 11 on the ESP32-C3.
const ESP32C3_HANDLERS: &str = r#"#[interrupt("machine_external")]
function external_irq(): void
    timer::on_interrupt()
    uart::on_interrupt()
end
"#;

/// What the example prints for the scripted session.
const EXAMPLE_TRANSCRIPT: &str = "LUSA ASYNC READY\nhello lusa\n[ 10 bytes]\nping\n[ 4 bytes]\n\
                                  bye lines=2 blinks=ok led=ok tasks=ok\n";

/// Lines typed into the UART, one `WriteChar` per byte.
const EXAMPLE_INPUT: &[&str] = &["hello lusa\n", "ping\n", "!"];

const SCHEDULER_BANNER: &str = "LUSA SCHED OK\nrr=xyxyxy timed=aabaa runs=ok\n";

/// Scheduler check.  `@HANDLERS@` is replaced by the board's handlers.
const SCHEDULER_CHECK: &str = r##"#[no_std]
use lusa::hal::uart;
use lusa::hal::timer;

@HANDLERS@

local trace: [u32; 16]
local marks: u32 = 0

function mark(letter: u32): void
    if marks < 16 then
        trace[marks] = letter
        marks = marks + 1
    end
end

-- Round robin: append a letter, then let the other task run.
async function spinner(letter: u32, rounds: u32): u32
    local i: u32 = 0
    while i < rounds do
        mark(letter)
        task.yield_now()
        i = i + 1
    end
    return rounds
end

-- Timer driven: append a letter every `period` milliseconds.
async function ticker(clock: timer::Timer, letter: u32, period: u32, rounds: u32): u32
    local i: u32 = 0
    while i < rounds do
        await timer::delay_ms(borrow clock, period)
        mark(letter)
        i = i + 1
    end
    return rounds
end

function send(port: uart::Uart, text: *mut u32, count: u32): void
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

#[entry]
function main(): void
    local port: uart::Uart = uart::take():unwrap()
    local clock: timer::Timer = timer::take():unwrap()
    uart::configure(borrow port, 115200)
    timer::configure(borrow clock, 1000)
    interrupts_enable()

    task.spawn(spinner(120, 3))
    task.spawn(spinner(121, 3))
    local first: i32 = task.run()
    local split: u32 = marks
    task.spawn(ticker(borrow clock, 97, 20, 4))
    task.spawn(ticker(borrow clock, 98, 50, 1))
    local second: i32 = task.run()

    local banner: [u32; 14] = [76, 85, 83, 65, 32, 83, 67, 72, 69, 68, 32, 79, 75, 10]
    local rr: [u32; 3] = [114, 114, 61]
    local timed: [u32; 7] = [32, 116, 105, 109, 101, 100, 61]
    local runs: [u32; 6] = [32, 114, 117, 110, 115, 61]
    local pass: [u32; 2] = [111, 107]
    local fail: [u32; 4] = [102, 97, 105, 108]
    send(borrow port, &banner[0], 14)
    send(borrow port, &rr[0], 3)
    send(borrow port, &trace[0], split)
    send(borrow port, &timed[0], 7)
    send(borrow port, &trace[split], marks - split)
    send(borrow port, &runs[0], 6)
    if first == 2 and second == 2 then
        send(borrow port, &pass[0], 2)
    else
        send(borrow port, &fail[0], 4)
    end
    send(borrow port, &banner[13], 1)
    uart::flush(borrow port)
    while true do
    end
end
"##;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo(relative: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(relative))
        .unwrap_or_else(|e| panic!("read {relative}: {e}"))
}

/// The example with its STM32F4 handlers replaced by `handlers`.
fn example_for(handlers: &str) -> String {
    let source = read_repo(EXAMPLE);
    assert!(
        source.contains(STM32F4_EXAMPLE_HANDLERS),
        "the example's handler block changed; update STM32F4_EXAMPLE_HANDLERS"
    );
    source.replace(STM32F4_EXAMPLE_HANDLERS, handlers)
}

fn scheduler_for(handlers: &str) -> String {
    SCHEDULER_CHECK.replace("@HANDLERS@", handlers)
}

fn boards() -> [(&'static TargetSpec, &'static str, &'static str); 3] {
    [
        (&targets::STM32F4, STM32F4_EXAMPLE_HANDLERS, "stm32f4"),
        (&targets::RP2040, RP2040_HANDLERS, "rp2040"),
        (&targets::ESP32C3, ESP32C3_HANDLERS, "esp32c3"),
    ]
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
            eprintln!("skipping native Module 28 run: {e}");
            None
        }
    }
}

fn native_exe(tag: &str) -> PathBuf {
    let out_dir = std::env::temp_dir().join("opencode");
    std::fs::create_dir_all(&out_dir).expect("temp dir");
    let exe = out_dir.join(format!("lusa-module28-{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    exe
}

/// Builds and runs a hosted program; `None` without a host C compiler.
fn native_output(source: &str, tag: &str) -> Option<String> {
    let compiler = native_compiler()?;
    let exe = native_exe(tag);
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "native program failed: {run:?}");
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    Some(stdout.trim_end_matches('\n').to_string())
}

/// Runs `source` in the interpreter, the VM, and natively, and checks all
/// three print `expected`.
fn assert_all_engines(source: &str, tag: &str, expected: &str) {
    assert_eq!(both_engines(source), expected, "dynamic engines for `{tag}`");
    if let Some(native) = native_output(source, tag) {
        assert_eq!(native, expected, "native output for `{tag}`");
    }
}

// ---------------------------------------------------------------------------
// Executor semantics
// ---------------------------------------------------------------------------

/// Three tasks that yield.  Two of them yield inside an awaited child: the
/// child's yield suspends its parent too, so the tasks interleave.
const SCHEDULE: &str = r#"
async function child(id: i32, steps: i32): i32
    local i: i32 = 0
    while i < steps do
        print(id, i)
        task.yield_now()
        i = i + 1
    end
    return steps
end

async function worker(id: i32, steps: i32): i32
    local finished: i32 = await child(id, steps)
    print(id, "done", finished)
    return finished
end

local a = task.spawn(worker(1, 3))
local b = task.spawn(worker(2, 2))
local c = task.spawn(child(3, 1))
local polled: i32 = task.run()
print("polled", polled)
print("results", await a, await b, await c)
"#;

const SCHEDULE_OUTPUT: &str = "1 0\n2 0\n3 0\n1 1\n2 1\n1 2\n2 done 2\n1 done 3\npolled 3\nresults 3 2 1";

#[test]
fn executor_round_robins_tasks_through_awaited_children() {
    assert_all_engines(SCHEDULE, "schedule", SCHEDULE_OUTPUT);
}

/// Events: every waiter of an event wakes, a wake that lands before the park
/// is kept, and `task.run()` returns while only event waiters remain.
const EVENTS: &str = r#"
async function listener(id: i32, rounds: i32): i32
    local heard: i32 = 0
    while heard < rounds do
        task.wait(7)
        heard = heard + 1
        print("listener", id, "heard", heard, "at", task.now())
    end
    return heard
end

async function signaller(): i32
    task.sleep(5)
    task.wake(7)
    task.sleep(5)
    task.wake(7)
    task.wake(7)
    return 0
end

async function self_waker(): i32
    task.wake(9)
    task.wait(9)
    print("self wake at", task.now())
    return 1
end

local one = task.spawn(listener(1, 2))
local two = task.spawn(listener(2, 1))
local own = task.spawn(self_waker())
local sig = task.spawn(signaller())
local polled: i32 = task.run()
print("polled", polled, "now", task.now())
print(task.status(one), task.status(two), task.status(own), task.status(sig))

local late = task.spawn(listener(3, 1))
local idle: i32 = task.run()
print("idle run", idle, task.status(late))
task.wake(7)
local woken: i32 = task.run()
print("woken run", woken, task.status(late))
"#;

const EVENTS_OUTPUT: &str = "self wake at 0\n\
                             listener 1 heard 1 at 5\n\
                             listener 2 heard 1 at 5\n\
                             listener 1 heard 2 at 10\n\
                             polled 4 now 10\n\
                             done done done done\n\
                             idle run 1 parked\n\
                             listener 3 heard 1 at 10\n\
                             woken run 0 done";

#[test]
fn wait_and_wake_park_tasks_until_their_event_fires() {
    assert_all_engines(EVENTS, "events", EVENTS_OUTPUT);
}

/// Sleepers wake in deadline order on the host's virtual clock; a blocking
/// `await` of a sleeping task advances the clock too.
const SLEEP: &str = r#"
async function nap(ticks: u32): u32
    task.sleep(ticks)
    return task.now()
end

async function sleeper(id: i32, period: u32, rounds: i32): i32
    local i: i32 = 0
    while i < rounds do
        local woke: u32 = await nap(period)
        print("sleeper", id, "woke at", woke)
        i = i + 1
    end
    return i
end

local fast = task.spawn(sleeper(1, 20, 4))
local slow = task.spawn(sleeper(2, 50, 1))
local polled: i32 = task.run()
print("polled", polled, "now", task.now())
local blocking: u32 = await nap(15)
print("blocking await woke at", blocking)
task.tick()
task.tick()
print("ticked to", task.now())
"#;

const SLEEP_OUTPUT: &str = "sleeper 1 woke at 20\n\
                            sleeper 1 woke at 40\n\
                            sleeper 2 woke at 50\n\
                            sleeper 1 woke at 60\n\
                            sleeper 1 woke at 80\n\
                            polled 2 now 80\n\
                            blocking await woke at 95\n\
                            ticked to 97";

#[test]
fn sleepers_wake_in_deadline_order_on_the_tick_clock() {
    assert_all_engines(SLEEP, "sleep", SLEEP_OUTPUT);
}

#[test]
fn blocking_await_runs_other_tasks_while_its_task_is_parked() {
    let source = r#"
async function receive(): i32
    task.wait(4)
    print("received at", task.now())
    return 42
end

async function sender(): i32
    task.sleep(30)
    print("sending at", task.now())
    task.wake(4)
    return 0
end

task.spawn(sender())
local value: i32 = await receive()
print("value", value)
"#;
    assert_all_engines(
        source,
        "blocking",
        "sending at 30\nreceived at 30\nvalue 42",
    );
}

#[test]
fn awaiting_a_task_nothing_can_wake_is_a_deadlock() {
    let source = r#"
async function forever(): i32
    task.wait(3)
    return 1
end

local handle = forever()
local ok, err = pcall(function() return await handle end)
print(ok, err)
print(task.status(handle))
"#;
    assert_eq!(
        both_engines(source),
        "false E2012: the awaited task is parked on event 3 and nothing is left to wake it\nfailed"
    );
}

#[test]
fn dynamic_engines_reject_misplaced_suspensions() {
    let cases = [
        ("task.wait(1)\n", "E9999", "`task.wait` can only be used inside an async function"),
        ("task.sleep(1)\n", "E9999", "`task.sleep` can only be used inside an async function"),
        // An error inside a task fails the task; awaiting it raises the
        // task's error text.
        (
            "async function f(): i32\n    task.wait(32)\n    return 1\nend\nprint(await f())\n",
            "E9999",
            "E2005: `task.wait` operand 32 is out of range",
        ),
        ("task.wake(-1)\n", "E2005", "out of range"),
        ("task.tick(1)\n", "E2005", "`task.tick` takes no arguments"),
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
fn native_checker_types_the_executor_api() {
    let cases = [
        ("task.wait(1)\n", "E0312", "can only be used inside an `async function`"),
        (
            "async function f(): i32\n    task.sleep()\n    return 1\nend\n",
            "E0311",
            "`task.sleep` takes one integer",
        ),
        (
            "async function f(): i32\n    task.sleep(true)\n    return 1\nend\n",
            "",
            "`task.sleep` argument",
        ),
        ("local t: u32 = task.now(1)\n", "E0311", "`task.now` takes no arguments"),
        ("task.wake(true)\n", "", "`task.wake` argument"),
        (
            "async function g(): void\nend\nasync function f(): i32\n    local x: i32 = await g()\n    return 1\nend\n",
            "E0312",
            "cannot await a `task<void>` value here",
        ),
    ];
    for (source, code, needle) in cases {
        let error = cbackend::emit_c(source).expect_err("C11 backend must reject");
        assert!(error.contains(needle), "expected `{needle}` in:\n{error}");
        if !code.is_empty() {
            assert!(error.contains(&format!("error[{code}]")), "expected {code} in:\n{error}");
        }
    }

    // `await` of a `task<void>` is fine as a statement, in and out of async
    // code, and `task.wake`/`task.tick` work from plain functions.
    let accepted = r#"
async function settle(ticks: u32): void
    task.sleep(ticks)
end

async function run(): i32
    await settle(2)
    return 1
end

function isr(): void
    task.tick()
    task.wake(5)
end

await settle(1)
isr()
print(await run(), task.now())
"#;
    let c = cbackend::emit_c(accepted).expect("void awaits and wakes compile");
    assert!(c.contains("lusa_task_wake((uint32_t)"), "{c}");
    assert_all_engines(accepted, "void-await", "1 4");
}

#[test]
fn native_runtime_parks_wakes_and_recycles_tasks() {
    let c = cbackend::emit_c(EVENTS).expect("events program compiles");
    for marker in [
        "LUSA_TASK_PARK",
        "static void lusa_task_collect(void)",
        "static int lusa_task_poll(LusaTask *task)",
        "static bool lusa_task_idle(void)",
        "static void lusa_task_wake(uint32_t event)",
        "static volatile uint32_t lusa_task_signals[LUSA_TASK_EVENTS];",
        "return lusa_task_wait_event(base, (uint32_t)(",
        "return lusa_task_sleep_for(base, (uint32_t)(",
    ] {
        assert!(c.contains(marker), "missing {marker:?}");
    }
    let c = cbackend::emit_c(SLEEP).expect("sleep program compiles");
    assert!(c.contains("lusa_task_release(base->awaiting);"), "awaited temporaries are recycled");

    // A program that only signals the executor from an interrupt handler
    // still gets the runtime.
    let isr_only = "#[no_std]\n\
                    #[interrupt]\n\
                    function tick(): void\n    task.tick()\n    task.wake(2)\nend\n\
                    #[entry]\n\
                    function main(): void\nend\n";
    let c = cbackend::emit_c(isr_only).expect("ISR-only program compiles");
    for marker in [
        "static void lusa_task_tick(void)",
        "static void lusa_task_wake(uint32_t event)",
        "#define LUSA_TASK_WFI 1",
        "__asm__ __volatile__(\"wfi\" : : : \"memory\");",
        "lusa_task_arena[LUSA_TASK_ARENA_BYTES]",
    ] {
        assert!(c.contains(marker), "missing {marker:?}");
    }
    for banned in ["malloc(", "free(", "lusa_task_status", "lusa_string_new", "pthread_create"] {
        assert!(!c.contains(banned), "freestanding executor must not contain {banned:?}");
    }
}

#[test]
fn freestanding_awaits_run_in_constant_arena_space() {
    // Each round allocates a fresh child task.  Without recycling, 3000 rounds
    // would need several hundred KiB; the default arena is 16 KiB.  A wrong
    // total writes through a null pointer, so the process fails.
    let source = r#"
#[no_std]

async function step(n: u32): u32
    task.yield_now()
    return n + 1
end

async function counter(rounds: u32): u32
    local value: u32 = 0
    while value < rounds do
        value = await step(value)
    end
    return value
end

#[entry]
function main(): void
    local job: task<u32> = counter(3000)
    task.spawn(job)
    task.run()
    local total: u32 = await job
    local direct: u32 = 0
    local i: u32 = 0
    while i < 3000 do
        direct = await step(direct)
        i = i + 1
    end
    if total ~= 3000 or direct ~= 3000 then
        unsafe do
            local null: *mut u32 = unsafe_ptr_cast(0)
            null.write_volatile(1)
        end
    end
end
"#;
    let c = cbackend::emit_c(source).expect("freestanding program compiles");
    assert!(c.starts_with("/* lusa:no_std */"));
    assert!(c.contains("lusa_task_release("), "{c}");
    let Some(compiler) = native_compiler() else {
        return;
    };
    let exe = native_exe("arena");
    cbackend::build(source, &exe, None)
        .unwrap_or_else(|e| panic!("native build with {} failed:\n{e}", compiler.display_name()));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(run.status.success(), "3000 awaits must fit in the arena: {run:?}");
}

// ---------------------------------------------------------------------------
// Async `lusa::hal` drivers on the host models
// ---------------------------------------------------------------------------

#[test]
fn host_tasks_blink_the_led_while_echoing_the_uart() {
    let source = r#"
use lusa::hal::uart
use lusa::hal::timer
use lusa::hal::led

async function blink(clock: timer::Timer, light: led::Led, times: u32): u32
    local i: u32 = 0
    while i < times do
        local now: u32 = await timer::delay_ms(borrow clock, 250)
        led::toggle(borrow light)
        print("blink", now, led::is_on(borrow light))
        i = i + 1
    end
    return led::toggles(borrow light)
end

async function echo(port: uart::Uart, count: u32): u32
    local seen: u32 = 0
    while seen < count do
        local byte: u32 = await uart::read_async(borrow port)
        await uart::write_async(borrow port, byte)
        print("echo", byte, "at", task.now())
        seen = seen + 1
    end
    await uart::flush_async(borrow port)
    return seen
end

async function peer(clock: timer::Timer): u32
    await timer::delay_ms(borrow clock, 300)
    uart::sim_receive(104)
    uart::sim_receive(105)
    await timer::delay_ms(borrow clock, 400)
    uart::sim_receive(33)
    return 3
end

local clock = timer::take():unwrap()
local light = led::take():unwrap()
local port = uart::take():unwrap()
timer::configure(borrow clock, 1000)
led::configure(borrow light)
uart::configure(borrow port, 115200)
print("claims", timer::take():is_none(), led::take():is_none())

local b = task.spawn(blink(borrow clock, borrow light, 4))
local e = task.spawn(echo(borrow port, 3))
local p = task.spawn(peer(borrow clock))
local ran: i32 = task.run()
print("ran", ran, "millis", timer::millis(borrow clock))
local sent = ""
local more = true
while more do
    match uart::sim_sent() with
    | Some(byte) => sent = sent .. string.char(byte)
    | None => more = false
    end
end
print("sent", sent, "toggles", await b, "echoed", await e, await p, uart::is_busy(borrow port))
"#;
    assert_eq!(
        both_engines(source),
        "claims true true\n\
         blink 250 true\n\
         echo 104 at 300\n\
         echo 105 at 300\n\
         blink 500 false\n\
         echo 33 at 700\n\
         blink 750 true\n\
         blink 1000 false\n\
         ran 3 millis 1000\n\
         sent hi! toggles 4 echoed 3 3 false"
    );
}

#[test]
fn host_async_uart_moves_whole_buffers() {
    let source = r#"
use lusa::hal::uart

async function exchange(port: uart::Uart, inbox: *mut u32, outbox: *mut u32): u32
    local got: u32 = await uart::read_exact_async(borrow port, inbox, 3)
    local sent: u32 = await uart::write_all_async(borrow port, outbox, 2)
    await uart::flush_async(borrow port)
    return got * 10 + sent
end

local port = uart::take():unwrap()
uart::configure(borrow port, 9600)
-- The sandbox models raw buffers with `unsafe_alloc`; native code can pass
-- `&array[0]` instead.
unsafe do
    local inbox: *mut u32 = unsafe_alloc(64)
    local outbox: *mut u32 = unsafe_alloc(64)
    *outbox = 79
    *(outbox + 1) = 75
    local job = task.spawn(exchange(borrow port, inbox, outbox))
    uart::sim_receive(76)
    local first: i32 = task.run()
    print("partial", first, task.status(job), uart::available(borrow port))
    uart::sim_receive(85)
    uart::sim_receive(83)
    local second: i32 = task.run()
    print("complete", second, task.status(job), await job)
    print("inbox", *inbox, *(inbox + 1), *(inbox + 2))
end
local out = ""
local more = true
while more do
    match uart::sim_sent() with
    | Some(byte) => out = out .. string.char(byte)
    | None => more = false
    end
end
print("sent", out, uart::event())
"#;
    assert_eq!(
        both_engines(source),
        "partial 1 parked 0\ncomplete 0 done 32\ninbox 76 85 83\nsent OK 24"
    );
}

#[test]
fn host_async_spi_and_i2c_wait_for_their_interrupts() {
    let source = r#"
use lusa::hal::spi
use lusa::hal::i2c

async function bus_work(bus: spi::Spi, wire: i2c::I2c): u32
    local first: u32 = await spi::transfer_async(borrow bus, 10)
    local second: u32 = await spi::transfer_async(borrow bus, 20)
    spi::transfer(borrow bus, 30)
    await spi::flush_async(borrow bus)
    print("spi", first, second, spi::read(borrow bus):unwrap(), spi::is_busy(borrow bus))
    i2c::queue(borrow wire, 7)
    i2c::queue(borrow wire, 9)
    i2c::start_write(borrow wire, 85)
    local wrote: u32 = await i2c::wait_async(borrow wire)
    i2c::start_read(borrow wire, 85, 2)
    local got: u32 = await i2c::wait_async(borrow wire)
    print("i2c", wrote, got, i2c::read(borrow wire):unwrap(), i2c::read(borrow wire):unwrap())
    i2c::queue(borrow wire, 1)
    i2c::start_write(borrow wire, 34)
    local nack: u32 = await i2c::wait_async(borrow wire)
    print("nack", nack, spi::event(), i2c::event())
    return nack
end

local bus = spi::take():unwrap()
local wire = i2c::take():unwrap()
spi::configure(borrow bus, 1000000, 0)
spi::sim_reply_offset(1)
i2c::configure(borrow wire, 100000)
local job = task.spawn(bus_work(borrow bus, borrow wire))
print("ran", task.run(), await job)
"#;
    assert_eq!(
        both_engines(source),
        "spi 11 21 31 false\ni2c 0 0 7 9\nnack 1 25 26\nran 1 1"
    );
}

#[test]
fn host_timer_converts_milliseconds_and_plays_interrupts() {
    let source = r#"
use lusa::hal::timer

local clock = timer::take():unwrap()
timer::configure(borrow clock, 300)
print("hz", timer::hz(borrow clock), timer::ticks_for(borrow clock, 10), timer::ticks_for(borrow clock, 2000))
timer::sim_advance(450)
print("ticks", timer::ticks(borrow clock), "millis", timer::millis(borrow clock))
local woke: u32 = await timer::delay_ticks(borrow clock, 6)
print("delay_ticks", woke)
timer::configure(borrow clock, 0)
print("clamped", timer::hz(borrow clock))
"#;
    assert_eq!(
        both_engines(source),
        "hz 300 3 600\nticks 450 millis 1500\ndelay_ticks 456\nclamped 1"
    );
}

#[test]
fn every_board_ships_the_timer_and_led_backends() {
    let checks: [(&TargetSpec, &[&str]); 3] = [
        (&targets::STM32F4, &["1073741824 + word * 4", "nvic_enable(28)", "1073872920"]),
        (&targets::RP2040, &["3758153748", "16777215", "3489660944"]),
        (&targets::ESP32C3, &["1610756096 + word * 4", "route_interrupt(37)", "1610629124"]),
    ];
    for (target, needles) in checks {
        let source = lusa::hal::crate_source(target.hal).expect("family");
        for module in ["pub mod timer", "pub mod led", "async function delay_ms", "async function read_async"] {
            assert!(source.contains(module), "{}: {module}", target.name);
        }
        for needle in needles {
            assert!(source.contains(needle), "{}: {needle}", target.name);
        }
    }
    let host = lusa::hal::crate_source("host").expect("host family");
    assert!(host.contains("pub function sim_advance"));
}

// ---------------------------------------------------------------------------
// Boards: vectors, platforms, emitted firmware, and cross builds
// ---------------------------------------------------------------------------

#[test]
fn tick_vectors_match_the_timer_backends() {
    assert_eq!(targets::STM32F4.vector("tim2").unwrap().number, 44);
    assert_eq!(targets::RP2040.vector("systick").unwrap().number, 15);
    assert_eq!(targets::ESP32C3.vector("machine_external").unwrap().number, 11);
}

#[test]
fn esp32c3_platform_models_the_system_timer() {
    let platform = targets::ESP32C3.renode_platform;
    for needle in [
        "systimer: Timers.LusaEsp32C3Systimer @ sysbus 0x60023000",
        "-> externalIRQ@3",
        "numberOfInputs: 4",
    ] {
        assert!(platform.contains(needle), "ESP32-C3 platform lacks {needle}");
    }
    let model = targets::ESP32C3
        .renode_models
        .iter()
        .find(|model| model.file == "LusaEsp32C3Systimer.cs")
        .expect("SYSTIMER model registered");
    for needle in [
        "namespace Antmicro.Renode.Peripherals.Timers",
        "class LusaEsp32C3Systimer",
        "LimitTimer",
        "InterruptStatus = 0x70",
    ] {
        assert!(model.source.contains(needle), "SYSTIMER model lacks {needle}");
    }
}

#[test]
fn example_exists_and_is_documented() {
    let example = read_repo(EXAMPLE);
    for needle in [
        "#[no_std]",
        "use lusa::hal::timer;",
        "use lusa::hal::led;",
        "uart::read_async",
        "timer::delay_ms",
        "task.wait(1)",
        "task.run()",
    ] {
        assert!(example.contains(needle), "example lacks {needle}");
    }
    for doc in ["README.md", "docs/LANGUAGE.md", "docs/ROADMAP.md", "docs/RELEASE_NOTES.md", ".github/release-notes/v2.4.0.md"] {
        let text = read_repo(doc);
        for needle in ["async_multitasking_firmware.lusa", "task.wait", "lusa::hal::timer"] {
            assert!(text.contains(needle), "{doc} lacks {needle}");
        }
    }
    let notes = read_repo(".github/release-notes/v2.4.0.md");
    for heading in ["## Impact Introduction", "## Feature Highlights", "## How to Use"] {
        assert!(notes.contains(heading), "release notes lack {heading}");
    }
}

#[test]
fn firmware_emits_c_with_the_board_vectors() {
    for (target, handlers, _) in boards() {
        for (what, source) in [("example", example_for(handlers)), ("scheduler", scheduler_for(handlers))] {
            let c = cbackend::emit_c_for_target(&source, None, target)
                .unwrap_or_else(|e| panic!("{what} for {}: {e}", target.name));
            cbackend::validate_interrupt_vectors(&c, target)
                .unwrap_or_else(|e| panic!("{what} vectors for {}: {e}", target.name));
            for marker in ["#define LUSA_TASK_WFI 1", "static int32_t lusa_task_run(void)", "lusa_task_sleep_for(base"] {
                assert!(c.contains(marker), "{what} for {}: missing {marker:?}", target.name);
            }
        }
    }
    // The STM32F4 vectors do not exist on the other boards.
    let wrong = cbackend::emit_c_for_target(&read_repo(EXAMPLE), None, &targets::RP2040)
        .expect("emits");
    assert!(cbackend::validate_interrupt_vectors(&wrong, &targets::RP2040).is_err());
}

fn workdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lusa-module28-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("module 28 work directory");
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

fn cross_build(target: &'static TargetSpec, source: &str, tag: &str) -> Option<(PathBuf, PathBuf)> {
    if let Err(error) = cbackend::find_target_compiler(target, None) {
        eprintln!("skipping cross build for `{}`: {error}", target.name);
        return None;
    }
    let dir = workdir(tag);
    let elf = dir.join("firmware.elf");
    let compiler = cbackend::build_with_target(source, &elf, None, None, target)
        .unwrap_or_else(|e| panic!("cross build for `{}` failed:\n{e}", target.name));
    eprintln!("module 28: built {} for {} with {}", elf.display(), target.name, compiler.display_name());
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
    let models = target.write_renode_models(dir).expect("write Renode peripheral models");
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

#[test]
fn firmware_cross_builds_for_every_board() {
    for (target, handlers, name) in boards() {
        for (what, source) in [("example", example_for(handlers)), ("scheduler", scheduler_for(handlers))] {
            if let Some((dir, _)) = cross_build(target, &source, &format!("build-{what}-{name}")) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Renode: the multitasking example and the scheduler check on every board
// ---------------------------------------------------------------------------

#[test]
fn renode_runs_async_multitasking_on_stm32f4() {
    run_on_renode(
        &targets::STM32F4,
        &read_repo(EXAMPLE),
        EXAMPLE_INPUT,
        EXAMPLE_TRANSCRIPT,
        "example-stm32f4",
    );
}

#[test]
fn renode_runs_async_multitasking_on_rp2040() {
    run_on_renode(
        &targets::RP2040,
        &example_for(RP2040_HANDLERS),
        EXAMPLE_INPUT,
        EXAMPLE_TRANSCRIPT,
        "example-rp2040",
    );
}

#[test]
fn renode_runs_async_multitasking_on_esp32c3() {
    run_on_renode(
        &targets::ESP32C3,
        &example_for(ESP32C3_HANDLERS),
        EXAMPLE_INPUT,
        EXAMPLE_TRANSCRIPT,
        "example-esp32c3",
    );
}

#[test]
fn renode_schedules_tasks_in_order_on_stm32f4() {
    run_on_renode(
        &targets::STM32F4,
        &scheduler_for(STM32F4_EXAMPLE_HANDLERS),
        &[],
        SCHEDULER_BANNER,
        "sched-stm32f4",
    );
}

#[test]
fn renode_schedules_tasks_in_order_on_rp2040() {
    run_on_renode(&targets::RP2040, &scheduler_for(RP2040_HANDLERS), &[], SCHEDULER_BANNER, "sched-rp2040");
}

#[test]
fn renode_schedules_tasks_in_order_on_esp32c3() {
    run_on_renode(&targets::ESP32C3, &scheduler_for(ESP32C3_HANDLERS), &[], SCHEDULER_BANNER, "sched-esp32c3");
}
