//! Language Server Protocol server for Lusa — JSON-RPC over stdio, `std` only.
//!
//! Started with `lusalang lsp`. Editors (Zed, VS Code, ...) send framed JSON,
//! the server answers with diagnostics produced by the real lexer+parser
//! (E0001/E0101...), so syntax errors appear live in the editor.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};

use crate::parser;

// ===== minimal JSON =====

/// A tiny JSON value with a matching parser and serializer.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// (key, value) pair that type-checks for `Json::Obj(vec![...])`.
    fn pair(key: &str, value: Json) -> (String, Json) {
        (key.to_string(), value)
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// Serializes to a compact JSON string (UTF-8 raw, control chars escaped).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9.0e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            Json::Str(s) => {
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            out.push_str(&format!("\\u{:04x}", c as u32));
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).write(out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Parses a JSON document. Returns None on malformed input.
    pub fn parse(input: &str) -> Option<Json> {
        let chars: Vec<char> = input.chars().collect();
        let mut p = P { c: chars, i: 0 };
        p.ws();
        let value = p.value(0)?;
        p.ws();
        if p.i >= p.c.len() {
            Some(value)
        } else {
            None
        }
    }
}

struct P {
    c: Vec<char>,
    i: usize,
}

impl P {
    fn ws(&mut self) {
        while self.i < self.c.len() && matches!(self.c[self.i], ' ' | '\t' | '\n' | '\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn eat(&mut self, want: char) -> bool {
        if self.peek() == Some(want) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > 100 {
            return None; // nesting sanity cap
        }
        self.ws();
        match self.peek()? {
            '{' => {
                self.i += 1;
                let mut pairs = Vec::new();
                self.ws();
                if self.eat('}') {
                    return Some(Json::Obj(pairs));
                }
                loop {
                    self.ws();
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(':') {
                        return None;
                    }
                    let val = self.value(depth + 1)?;
                    pairs.push((key, val));
                    self.ws();
                    match self.peek()? {
                        ',' => {
                            self.i += 1;
                        }
                        '}' => {
                            self.i += 1;
                            return Some(Json::Obj(pairs));
                        }
                        _ => return None,
                    }
                }
            }
            '[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(']') {
                    return Some(Json::Arr(items));
                }
                loop {
                    let val = self.value(depth + 1)?;
                    items.push(val);
                    self.ws();
                    match self.peek()? {
                        ',' => {
                            self.i += 1;
                        }
                        ']' => {
                            self.i += 1;
                            return Some(Json::Arr(items));
                        }
                        _ => return None,
                    }
                }
            }
            '"' => Some(Json::Str(self.string()?)),
            't' => self.lit("true", Json::Bool(true)),
            'f' => self.lit("false", Json::Bool(false)),
            'n' => self.lit("null", Json::Null),
            _ => self.number(),
        }
    }

    fn lit(&mut self, text: &str, value: Json) -> Option<Json> {
        for w in text.chars() {
            if self.peek() != Some(w) {
                return None;
            }
            self.i += 1;
        }
        Some(value)
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        if self.eat('-') {}
        let mut digits = 0;
        while matches!(self.peek(), Some('0'..='9')) {
            self.i += 1;
            digits += 1;
        }
        if digits == 0 {
            return None;
        }
        if self.eat('.') {
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.i += 1;
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some('0'..='9')) {
                self.i += 1;
            }
        }
        let text: String = self.c[start..self.i].iter().collect();
        text.parse::<f64>().ok().map(Json::Num)
    }

    fn string(&mut self) -> Option<String> {
        if !self.eat('"') {
            return None;
        }
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.i += 1;
            match c {
                '"' => return Some(out),
                '\\' => {
                    let esc = self.peek()?;
                    self.i += 1;
                    match esc {
                        '"' => out.push('"'),
                        '\\' => out.push('\\'),
                        '/' => out.push('/'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        'u' => {
                            let mut code = 0u32;
                            for _ in 0..4 {
                                let h = self.peek()?.to_digit(16)?;
                                self.i += 1;
                                code = code * 16 + h;
                            }
                            // surrogate pair?
                            if (0xD800..0xDC00).contains(&code) {
                                if self.peek() == Some('\\') {
                                    self.i += 1;
                                    if self.peek() == Some('u') {
                                        self.i += 1;
                                        let mut low = 0u32;
                                        for _ in 0..4 {
                                            let h = self.peek()?.to_digit(16)?;
                                            self.i += 1;
                                            low = low * 16 + h;
                                        }
                                        if (0xDC00..0xE000).contains(&low) {
                                            let combined = 0x10000
                                                + ((code - 0xD800) << 10)
                                                + (low - 0xDC00);
                                            out.push(char::from_u32(combined)?);
                                            continue;
                                        }
                                    }
                                }
                                out.push('\u{FFFD}'); // lone surrogate
                            } else {
                                out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return None,
                    }
                }
                c => out.push(c),
            }
        }
    }
}

// ===== diagnostics =====

/// LSP position from a Lusa char-offset
/// (line, character counted in UTF-16 units, both zero-based).
pub fn to_lsp_pos(src: &str, char_off: usize) -> (u32, u32) {
    let mut line = 0u32;
    let mut col16 = 0u32;
    let mut seen = 0usize;
    for c in src.chars() {
        if seen >= char_off {
            break;
        }
        if c == '\n' {
            line += 1;
            col16 = 0;
        } else {
            col16 += c.len_utf16() as u32;
        }
        seen += 1;
    }
    (line, col16)
}

/// Runs the current checker (lexer+parser) and returns LSP `Diagnostic` JSON
/// objects. Ownership errors (E1xxx) are runtime-detected today, so static
/// diagnostics cover lex/parse (E0001/E0101) for now.
pub fn diagnostics(src: &str) -> Vec<Json> {
    match parser::parse(src) {
        Ok(_) => vec![],
        Err(e) => {
            let start = e.span.map(|s| s.start).unwrap_or(0);
            let end = e.span.map(|s| s.end).unwrap_or(start);
            let (start_line, start_col) = to_lsp_pos(src, start);
            let (end_line, end_col) =
                to_lsp_pos(src, if end > start { end } else { start + 1 });
            vec![Json::Obj(vec![
                Json::pair("range", Json::Obj(vec![
                    Json::pair("start", Json::Obj(vec![
                        Json::pair("line", Json::Num(start_line as f64)),
                        Json::pair("character", Json::Num(start_col as f64)),
                    ])),
                    Json::pair("end", Json::Obj(vec![
                        Json::pair("line", Json::Num(end_line as f64)),
                        Json::pair("character", Json::Num(end_col as f64)),
                    ])),
                ])),
                Json::pair("severity", Json::Num(1.0)), // error
                Json::pair("code", Json::Str(e.code.to_string())),
                Json::pair("source", Json::Str("lusa".to_string())),
                Json::pair("message", Json::Str(e.msg.clone())),
            ])]
        }
    }
}

// ===== server loop =====

pub fn lsp_main() -> i32 {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut docs: HashMap<String, String> = HashMap::new();

    loop {
        // read one framed message
        let mut content_len: Option<usize> = None;
        loop {
            let mut line = String::new();
            match read_header_line(&mut reader, &mut line) {
                Ok(0) => return 0, // stdin closed
                Ok(_) => {}
                Err(_) => return 0,
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(v) = line_strip_ci(trimmed, "Content-Length:") {
                if let Ok(n) = v.trim().parse::<usize>() {
                    content_len = Some(n);
                }
            }
            // Content-Type and friends are ignored
        }
        let n = match content_len {
            Some(n) => n,
            None => return 0, // malformed framing
        };
        let mut buf = vec![0u8; n];
        if reader.read_exact(&mut buf).is_err() {
            return 0;
        }
        let msg = match Json::parse(&String::from_utf8_lossy(&buf)) {
            Some(m) => m,
            None => continue,
        };

        let method = msg.get("method").and_then(|m| m.as_str()).map(String::from);
        let id = msg.get("id").cloned();

        match method.as_deref() {
            Some("initialize") => {
                let mut capabilities = Json::Obj(vec![
                    Json::pair("capabilities", Json::Obj(vec![
                        Json::pair(
                            "textDocumentSync",
                            Json::Obj(vec![
                                Json::pair("openClose", Json::Bool(true)),
                                Json::pair("change", Json::Num(1.0)), // full sync
                            ]),
                        ),
                        Json::pair("positionEncoding", Json::Str("utf-16".into())),
                    ])),
                ]);
                let server_info = Json::Obj(vec![
                    Json::pair("name", Json::Str("lusa-lsp".into())),
                    Json::pair("version", Json::Str("0.1.0".into())),
                ]);
                let mut result = vec![];
                if let Json::Obj(ref mut pairs) = capabilities {
                    pairs.push(Json::pair("serverInfo", server_info));
                    result = std::mem::take(pairs);
                }
                if let Some(id) = id {
                    write_result(&stdout, &id, Json::Obj(result));
                }
            }
            Some("shutdown") => {
                if let Some(id) = id {
                    write_result(&stdout, &id, Json::Null);
                }
            }
            Some("exit") => return 0,
            Some("initialized") => {}           // notification: nothing to do
            Some("textDocument/didOpen") => {
                if let Some((uri, text)) = extract_text(&msg, /*use_document*/ true) {
                    docs.insert(uri.clone(), text);
                    publish(&stdout, &uri, diagnostics(docs.get(&uri).unwrap()));
                }
            }
            Some("textDocument/didChange") => {
                if let Some((uri, text)) = extract_text(&msg, /*use_document*/ false) {
                    docs.insert(uri.clone(), text);
                    publish(&stdout, &uri, diagnostics(docs.get(&uri).unwrap()));
                }
            }
            Some("textDocument/didSave") => {}  // content unchanged; nothing to do
            Some("textDocument/didClose") => {
                if let Some(u) = msg
                    .get("params")
                    .and_then(|p| p.get("textDocument"))
                    .and_then(|d| d.get("uri"))
                    .and_then(|u| u.as_str())
                {
                    let uri = u.to_string();
                    docs.remove(&uri);
                    publish(&stdout, &uri, vec![]);
                }
            }
            Some(other) => {
                if id.is_some() {
                    write_error(
                        &stdout,
                        id.as_ref().unwrap(),
                        -32601.0,
                        &format!("method not supported: {}", other),
                    );
                }
                // unknown notifications are ignored
            }
            None => {} // malformed; ignore
        }
    }
}

/// case-insensitive header prefix strip
fn line_strip_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len()
        && line[..prefix.len()].eq_ignore_ascii_case(prefix)
    {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

fn read_header_line(reader: &mut impl Read, out: &mut String) -> std::io::Result<usize> {
    let mut b = [0u8; 1];
    let mut count = 0usize;
    loop {
        let got = reader.read(&mut b)?;
        if got == 0 {
            return Ok(count);
        }
        count += got;
        if b[0] == b'\n' {
            return Ok(count);
        }
        // headers are ASCII by spec
        out.push(b[0] as char);
    }
}

/// Pulls (uri, full text) out of didOpen/didChange notifications.
fn extract_text(msg: &Json, use_document_text: bool) -> Option<(String, String)> {
    let params = msg.get("params")?;
    let uri = params
        .get("textDocument")?
        .get("uri")?
        .as_str()?
        .to_string();
    let text = if use_document_text {
        params.get("textDocument")?.get("text")?.as_str()?.to_string()
    } else {
        let changes = params.get("contentChanges")?.as_arr()?;
        let last = changes.last()?;
        match last.get("text") {
            Some(Json::Str(s)) => s.clone(),
            _ => return None, // range edits unsupported (full sync negotiated)
        }
    };
    Some((uri, text))
}

fn write_frame(mut w: impl Write, msg: &Json) {
    let body = msg.serialize();
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut bytes = header.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    let _ = w.write_all(&bytes);
    let _ = w.flush();
}

fn write_result(mut w: impl Write, id: &Json, result: Json) {
    write_frame(
        &mut w,
        &Json::Obj(vec![
            Json::pair("jsonrpc", Json::Str("2.0".into())),
            Json::pair("id", id.clone()),
            Json::pair("result", result),
        ]),
    );
}

fn write_error(mut w: impl Write, id: &Json, code: f64, message: &str) {
    write_frame(
        &mut w,
        &Json::Obj(vec![
            Json::pair("jsonrpc", Json::Str("2.0".into())),
            Json::pair("id", id.clone()),
            Json::pair(
                "error",
                Json::Obj(vec![
                    Json::pair("code", Json::Num(code)),
                    Json::pair("message", Json::Str(message.to_string())),
                ]),
            ),
        ]),
    );
}

/// `textDocument/publishDiagnostics` notification with fresh results.
fn publish(mut w: impl Write, uri: &str, diags: Vec<Json>) {
    write_notification(
        &mut w,
        "textDocument/publishDiagnostics",
        Json::Obj(vec![
            Json::pair("uri", Json::Str(uri.to_string())),
            Json::pair("diagnostics", Json::Arr(diags)),
        ]),
    );
}

fn write_notification(mut w: impl Write, method: &str, params: Json) {
    write_frame(
        &mut w,
        &Json::Obj(vec![
            Json::pair("jsonrpc", Json::Str("2.0".into())),
            Json::pair("method", Json::Str(method.to_string())),
            Json::pair("params", params),
        ]),
    );
}
