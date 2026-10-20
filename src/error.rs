//! Error codes, spans and cargo-style diagnostics for Lusa.
//!
//! Spans are char-offset based so Unicode sources slice cleanly.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize, // char index
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LusaError {
    /// e.g. "E1001"
    pub code: &'static str,
    pub msg: String,
    pub span: Option<Span>,
    pub notes: Vec<(String, Option<Span>)>, // (text, location)
    pub help: Vec<String>,
}

impl LusaError {
    pub fn new(code: &'static str, span: Option<Span>, msg: impl Into<String>) -> Self {
        LusaError {
            code,
            msg: msg.into(),
            span,
            notes: Vec::new(),
            help: Vec::new(),
        }
    }
    pub fn note(mut self, text: impl Into<String>, span: Option<Span>) -> Self {
        self.notes.push((text.into(), span));
        self
    }
    pub fn help(mut self, text: impl Into<String>) -> Self {
        self.help.push(text.into());
        self
    }
}

impl fmt::Display for LusaError {
    // Display without source; `render` below uses the actual source text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "error[{}]: {}", self.code, self.msg)
    }
}

pub type LResult<T> = Result<T, LusaError>;

pub const E_LEX: &str = "E0001";
pub const E_PARSE: &str = "E0101";
/// reserved: `break` outside a loop is a parse error (reported as E0101 for now)
#[allow(dead_code)]
pub const E_BREAK: &str = "E0102";

// ownership & borrow codes
pub const E_MOVED: &str = "E1001"; // use of moved value
pub const E_BORROW_CONFLICT: &str = "E1002"; // cannot move/mutate while borrowed
/// reserved: borrows read moved slots as errors via E1001 paths
#[allow(dead_code)]
pub const E_BORROW_MOVED: &str = "E1003";
pub const E_MUT_IMMUT: &str = "E1004"; // mutate through immutable borrow
/// reserved: aliases are Copy (shared refs), so moving through them is allowed
#[allow(dead_code)]
pub const E_MOVE_ALIAS: &str = "E1005";

// runtime type errors
pub const E_ARITH: &str = "E2001";
pub const E_CONCAT: &str = "E2002";
pub const E_CMP: &str = "E2003";
pub const E_LEN: &str = "E2004";
pub const E_CALL: &str = "E2005";
pub const E_INDEX: &str = "E2006";
pub const E_BAD_KEY: &str = "E2007";
pub const E_NUM: &str = "E2008";

// user raised
pub const E_ERROR: &str = "E9000"; // error()
pub const E_ASSERT: &str = "E9100"; // assert()
pub const E_RUNTIME: &str = "E9999"; // fallback

/// Render an error cargo-style against the source text.
pub fn render(err: &LusaError, source: &str, filename: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::new();
    out.push_str(&format!("error[{}]: {}\n", err.code, err.msg));

    if let Some(sp) = err.span {
        out.push_str(&format!(" --> {}:{}:{}\n", filename, line_of(&chars, sp.start), col_of(&chars, sp.start)));
        out.push_str(&gutter_snippet(&chars, sp, filename));
    } else {
        out.push_str(&format!(" --> {}\n", filename));
    }

    for (text, nspan) in &err.notes {
        match nspan {
            Some(ns) => {
                out.push_str(&format!(
                    "note: {}\n --> {}:{}:{}\n",
                    text, filename, line_of(&chars, ns.start), col_of(&chars, ns.start)
                ));
                out.push_str(&gutter_snippet(&chars, *ns, filename));
            }
            None => out.push_str(&format!("note: {}\n", text)),
        }
    }
    for h in &err.help {
        out.push_str(&format!("help: {}\n", h));
    }
    out
}

fn line_of(chars: &[char], at: usize) -> usize {
    1 + chars[..at.min(chars.len())].iter().filter(|&&c| c == '\n').count()
}

fn col_of(chars: &[char], at: usize) -> usize {
    let mut col = 1;
    for &c in &chars[..at.min(chars.len())] {
        if c == '\n' {
            col = 1;
        } else {
            col += 1;
        }
    }
    col
}

/// One snippet block with a gutter and an underline under the span.
fn gutter_snippet(chars: &[char], sp: Span, _filename: &str) -> String {
    let start = sp.start.min(chars.len());
    let end = sp.end.min(chars.len());
    let line_start = chars[..start]
        .iter()
        .rposition(|&c| c == '\n')
        .map(|p| p + 1)
        .unwrap_or(0);
    let line_end = {
        let e = chars[end..]
            .iter()
            .position(|&c| c == '\n')
            .map(|p| end + p)
            .unwrap_or(chars.len());
        e
    };
    let line: String = chars[line_start..line_end].iter().collect();
    let ln = line_of(chars, start);
    let col = col_of(chars, start);
    let width = (end.saturating_sub(start)).max(1);

    let numstr = ln.to_string();
    let pad = numstr.len();
    let mut out = String::new();
    out.push_str(&format!("{} |\n", " ".repeat(pad)));
    out.push_str(&format!("{} | {}\n", numstr, line));
    let spaces_len = line[..(col - 1).min(line.chars().count())]
        .chars()
        .count();
    let caret = if width <= 1 {
        "^".to_string()
    } else {
        "-".repeat(width - 1) + "^"
    };
    out.push_str(&format!(
        "{} | {}{}\n",
        " ".repeat(pad),
        " ".repeat(spaces_len),
        caret
    ));
    out
}
