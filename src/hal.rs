//! `lusa::hal`: interrupt-driven UART, SPI, and I2C drivers (Module 27).
//!
//! The drivers are Lusa source embedded in the binary.  Each board family
//! (`TargetSpec::hal`) supplies the register-level half of every driver; the
//! public API, the ring buffers, and the ownership rules are shared.  The
//! module resolver splices the rendered crate into a program that mentions
//! `lusa::`, so `use lusa::hal::uart;` works in the interpreter, the VM, and
//! the C11 backend alike.
//!
//! A driver module is assembled as:
//!
//! 1. one ring-buffer instance per buffer (`ring.lusa` with `@R@`/`@CAP@`),
//! 2. the common API (`uart.lusa`, `spi.lusa`, `i2c.lusa`),
//! 3. the family backend (`<family>/uart.lusa`, ...), which implements the
//!    private `ll_*` hooks.
//!
//! Module-level state is declared before every function that touches it, so
//! the dynamic engines resolve it as module locals rather than globals.

use crate::error::LResult;
use crate::lexer;

const RING: &str = include_str!("hal/ring.lusa");
const UART: &str = include_str!("hal/uart.lusa");
const SPI: &str = include_str!("hal/spi.lusa");
const I2C: &str = include_str!("hal/i2c.lusa");

/// Every driver owns a 64-byte receive ring and a 64-byte transmit ring.
const DRIVER_RINGS: &[(&str, u32)] = &[("rx", 64), ("tx", 64)];

struct Backend {
    source: &'static str,
    /// Extra rings the backend needs (the host models use them for the
    /// simulated wire).
    rings: &'static [(&'static str, u32)],
}

struct Family {
    name: &'static str,
    /// Board support (MMIO and interrupt-controller helpers) placed in the
    /// `hal` module itself; drivers reach it through `super::`.
    board: &'static str,
    uart: Backend,
    spi: Backend,
    i2c: Backend,
}

const fn backend(source: &'static str) -> Backend {
    Backend { source, rings: &[] }
}

static FAMILIES: &[Family] = &[
    Family {
        name: "host",
        board: "",
        uart: Backend {
            source: include_str!("hal/host/uart.lusa"),
            rings: &[("line_in", 64), ("line_out", 128)],
        },
        spi: backend(include_str!("hal/host/spi.lusa")),
        i2c: backend(include_str!("hal/host/i2c.lusa")),
    },
    Family {
        name: "stm32f4",
        board: include_str!("hal/stm32f4/board.lusa"),
        uart: backend(include_str!("hal/stm32f4/uart.lusa")),
        spi: backend(include_str!("hal/stm32f4/spi.lusa")),
        i2c: backend(include_str!("hal/stm32f4/i2c.lusa")),
    },
    Family {
        name: "rp2040",
        board: include_str!("hal/rp2040/board.lusa"),
        uart: backend(include_str!("hal/rp2040/uart.lusa")),
        spi: backend(include_str!("hal/rp2040/spi.lusa")),
        i2c: backend(include_str!("hal/rp2040/i2c.lusa")),
    },
    Family {
        name: "esp32c3",
        board: include_str!("hal/esp32c3/board.lusa"),
        uart: backend(include_str!("hal/esp32c3/uart.lusa")),
        spi: backend(include_str!("hal/esp32c3/spi.lusa")),
        i2c: backend(include_str!("hal/esp32c3/i2c.lusa")),
    },
];

/// Names of the families that ship drivers.
pub fn families() -> Vec<&'static str> {
    FAMILIES.iter().map(|family| family.name).collect()
}

fn render_driver(out: &mut String, name: &str, common: &str, backend: &Backend) {
    out.push_str("pub mod ");
    out.push_str(name);
    out.push_str(" {\n");
    for (prefix, capacity) in DRIVER_RINGS.iter().chain(backend.rings) {
        let ring = RING
            .replace("@R@", prefix)
            .replace("@CAP@", &capacity.to_string());
        out.push_str(&ring);
        out.push('\n');
    }
    out.push_str(common);
    out.push('\n');
    out.push_str(backend.source);
    out.push_str("\n}\n");
}

/// Multi-line source of the `lusa` crate for `family`, or `None` when the
/// family ships no drivers.
pub fn crate_source(family: &str) -> Option<String> {
    let family = FAMILIES.iter().find(|candidate| candidate.name == family)?;
    let mut out = String::from("pub mod lusa {\npub mod hal {\n");
    out.push_str(family.board);
    out.push('\n');
    render_driver(&mut out, "uart", UART, &family.uart);
    render_driver(&mut out, "spi", SPI, &family.spi);
    render_driver(&mut out, "i2c", I2C, &family.i2c);
    out.push_str("}\n}\n");
    Some(out)
}

/// Re-renders `source` on a single line: tokens joined by spaces, comments
/// dropped.  Splicing the crate this way leaves every line number of the
/// user's program unchanged.
pub fn collapse(source: &str) -> LResult<String> {
    let chars: Vec<char> = source.chars().collect();
    let tokens = lexer::lex(source)?;
    let mut out = String::new();
    for token in &tokens {
        if matches!(token.kind, crate::tokens::TokenKind::Eof) {
            break;
        }
        let start = token.span.start.min(chars.len());
        let end = token.span.end.min(chars.len()).max(start);
        if start == end {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(&chars[start..end]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_renders_three_drivers() {
        for name in families() {
            let source = crate_source(name).expect("family");
            for driver in ["pub mod uart", "pub mod spi", "pub mod i2c"] {
                assert!(source.contains(driver), "{name}: {driver}");
            }
            assert!(!source.contains("@R@") && !source.contains("@CAP@"), "{name}");
            let line = collapse(&source).expect("collapse");
            assert!(!line.contains('\n'), "{name}");
        }
        assert!(crate_source("riscv32-virt").is_none());
    }
}
