//! Lusa library entry point: modules plus `run_source`, which executes a
//! whole program (used by the CLI and the integration tests).

pub mod ast;
pub mod builtins;
pub mod cbackend;
pub mod compiler;
pub mod coroutine;
pub mod error;
pub mod eval;
pub mod imports;
pub mod interp;
pub mod lexer;
pub mod lsp;
pub mod modules;
pub mod parser;
pub mod pattern;
pub mod scope;
pub mod targets;
pub mod task;
pub mod tokens;
pub mod update;
pub mod value;
pub mod vm;

use error::LResult;
use std::path::Path;

/// Parses and runs a source file.
/// Returns the lines `print` produced; errors bubble up as `LusaError`.
pub fn run_source(src: &str) -> LResult<Vec<String>> {
    run_source_with_base(src, None)
}

/// Parses and runs a source file, resolving imports relative to `base`.
pub fn run_source_with_base(src: &str, base: Option<&Path>) -> LResult<Vec<String>> {
    // The evaluator recurses in Rust frames (one call level ≈ several
    // thousand stack bytes), and the logical `MAX_CALL_DEPTH` guard only
    // kicks in after those frames accumulate. Hosts run `run_source` from
    // threads with small stacks (2 MiB test workers), so evaluation gets a
    // dedicated honest stack here — one spawn per program, not per call.
    let joined = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(s, move || {
                let ast = parser::parse_with_base(src, base)?;
                let mut interp = interp::Interp::new();
                interp.install_stdlib();
                let result = interp.run(&ast, "<source>");
                let out = std::mem::take(&mut interp.out);
                result.map(|_| out)
            })
            .expect("failed to spawn interpreter thread")
            .join()
    });
    joined.expect("interpreter thread panicked")
}
