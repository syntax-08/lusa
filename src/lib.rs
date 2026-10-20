//! Lusa library entry point: modules plus `run_source`, which executes a
//! whole program (used by the CLI and the integration tests).

pub mod ast;
pub mod builtins;
pub mod error;
pub mod eval;
pub mod interp;
pub mod lexer;
pub mod parser;
pub mod scope;
pub mod tokens;
pub mod value;

use error::LResult;

/// Parses and runs a source file.
/// Returns the lines `print` produced; errors bubble up as `LusaError`.
pub fn run_source(src: &str) -> LResult<Vec<String>> {
    let ast = parser::parse(src)?;
    let mut interp = interp::Interp::new();
    interp.install_stdlib();
    let result = interp.run(&ast, "<source>");
    let out = std::mem::take(&mut interp.out);
    result.map(|_| out)
}
