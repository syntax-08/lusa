//! Engine parity: the same program must print the same thing in the
//! tree-walking interpreter, the bytecode VM, and the C11 backend.
//!
//! Native runs skip cleanly when the host has no C compiler (CI always has
//! one).  Known, still-open divergences are kept as `#[ignore]` tests with the
//! reason attached, so `cargo test --test parity -- --ignored` shows exactly
//! where the engines disagree today.

use std::process::Command;

use lusa::cbackend;

fn interp(src: &str) -> String {
    lusa::run_source(src)
        .unwrap_or_else(|e| panic!("interpreter failed: {e:?}"))
        .join("\n")
}

fn vm(src: &str) -> String {
    lusa::vm::run_source_vm(src)
        .unwrap_or_else(|e| panic!("VM failed: {e:?}"))
        .join("\n")
}

/// Builds and runs `src` natively; `None` when no C compiler is available.
fn native(src: &str, tag: &str) -> Option<String> {
    if let Err(e) = cbackend::find_compiler(None) {
        eprintln!("skipping native parity run `{tag}`: {e}");
        return None;
    }
    let dir = std::env::temp_dir().join("lusa-parity");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let exe = dir.join(format!("{tag}-{}.exe", std::process::id()));
    let _ = std::fs::remove_file(&exe);
    cbackend::build(src, &exe, None)
        .unwrap_or_else(|e| panic!("native build `{tag}` failed:\n{e}"));
    let run = Command::new(&exe).output().expect("run native program");
    let _ = std::fs::remove_file(&exe);
    assert!(
        run.status.success(),
        "native program `{tag}` failed: {run:?}"
    );
    let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
    Some(stdout.trim_end_matches('\n').to_string())
}

/// All three engines must print `expected`.
fn assert_parity(tag: &str, src: &str, expected: &str) {
    assert_eq!(interp(src), expected, "interpreter output for `{tag}`");
    assert_eq!(vm(src), expected, "VM output for `{tag}`");
    if let Some(out) = native(src, tag) {
        assert_eq!(out, expected, "native output for `{tag}`");
    }
}

// ===== programs that agree everywhere =====

#[test]
fn integer_arithmetic_and_floor_semantics() {
    assert_parity(
        "ints",
        "local a = 17\n\
         local b = 5\n\
         print(a + b, a - b, a * b)\n\
         print(a // b, a % b)\n\
         print(-a // b, -a % b)\n\
         print(a // -b, a % -b)\n\
         print(2 ^ 10, 3 ^ 4)\n\
         print(a == b, a < b, a >= b)\n",
        "22 12 85\n3 2\n-4 3\n-4 -3\n1024 81\nfalse false true",
    );
}

#[test]
fn floor_modulo_at_the_bottom_of_the_i32_range() {
    // Native used to compute this as `a - (a // b) * b`, which overflows i32
    // (undefined behaviour in C).  It is now computed from `a % b` directly.
    assert_parity(
        "int_min_mod",
        "local m = -2147483647 - 1\n\
         print(m % 3, m // 3)\n\
         print(m % -3, m % 7)\n",
        "1 -715827883\n-2 5",
    );
}

#[test]
fn control_flow_and_recursion() {
    assert_parity(
        "flow",
        "function fib(n: i32): i32\n\
             if n < 2 then\n\
                 return n\n\
             end\n\
             return fib(n - 1) + fib(n - 2)\n\
         end\n\
         local total = 0\n\
         local i = 1\n\
         while i <= 10 do\n\
             total = total + i * i\n\
             i = i + 1\n\
         end\n\
         print(fib(20), total)\n\
         local s = \"lu\" .. \"sa\"\n\
         print(s, #s)\n",
        "6765 385\nlusa 4",
    );
}

#[test]
fn exactly_representable_decimals() {
    assert_parity(
        "floats",
        "local x = 1.5\n\
         local y = 0.25\n\
         print(x + y, x * 4, x - y)\n\
         print(10 / 4, 7 / 2, 1 / 8)\n",
        "1.75 6 1.25\n2.5 3.5 0.125",
    );
}

// ===== native-only: i32 edge cases must be defined, not UB =====

#[test]
fn native_i32_overflow_wraps_without_undefined_behaviour() {
    // The dynamic engines use f64 and do not overflow here (see the ignored
    // `i32_overflow` divergence below); native i32 arithmetic wraps in two's
    // complement.  Before, these expressions were signed overflow in C, so
    // an optimizing compiler was free to miscompile them, and
    // `INT32_MIN // -1` can trap with SIGFPE on x86.
    let src = "local m = -2147483647 - 1\n\
               local n = -1\n\
               print(m // n, m % n)\n\
               print(-m)\n\
               local big = 2147483647\n\
               print(big + 1, big * 2, big - m)\n";
    if let Some(out) = native(src, "i32_wrap") {
        assert_eq!(out, "-2147483648 0\n-2147483648\n-2147483648 -2 -1");
    }
    let c = cbackend::emit_c(src).expect("emit");
    assert!(
        c.contains("((int32_t)((uint32_t)(") && c.contains("((int32_t)(0u - (uint32_t)("),
        "i32 `+ - *` and unary `-` should wrap through uint32_t:\n{c}"
    );
}

// ===== known divergences (open design decisions) =====

#[test]
#[ignore = "open: native `/` on i32 variables truncates (3), the dynamic engines and native constant folding give 3.5"]
fn divergence_integer_division() {
    assert_parity("int_div", "local a = 7\nlocal b = 2\nprint(a / b)\n", "3.5");
}

#[test]
#[ignore = "open: untyped decimals are f32 in native code (0.300000012) but f64 in the dynamic engines"]
fn divergence_decimal_precision() {
    assert_parity(
        "f32_precision",
        "local c = 0.1\nlocal d = 0.2\nprint(c + d)\n",
        "0.30000000000000004",
    );
}

#[test]
#[ignore = "open: untyped integers are i32 in native code (wraps at 2^31) but f64 in the dynamic engines"]
fn divergence_i32_overflow() {
    assert_parity(
        "i32_overflow",
        "local big = 2147483647\nprint(big + 1)\n",
        "2147483648",
    );
}
