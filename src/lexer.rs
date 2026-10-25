//! Lexer — turns source text into tokens.
//!
//! Features: line comments `--`, block comments `--[[ ... ]]`,
//! numeric literals (decimal, hex, exponent), strings with escapes and
//! interpolation `\{expr}` (re-lexed recursively), names/keywords, operators.

use crate::error::{LResult, LusaError, Span, E_LEX};
use crate::tokens::{StrSeg, Token, TokenKind};

pub fn lex(src: &str) -> LResult<Vec<Token>> {
    Lexer::new(src).run()
}

const KEYWORDS: &[(&str, TokenKind)] = &[
    ("local", TokenKind::Local),
    ("function", TokenKind::Function),
    ("end", TokenKind::End),
    ("if", TokenKind::If),
    ("then", TokenKind::Then),
    ("elseif", TokenKind::Elseif),
    ("else", TokenKind::Else),
    ("for", TokenKind::For),
    ("in", TokenKind::In),
    ("do", TokenKind::Do),
    ("while", TokenKind::While),
    ("repeat", TokenKind::Repeat),
    ("until", TokenKind::Until),
    ("unsafe", TokenKind::Unsafe),
    ("struct", TokenKind::Struct),
    ("return", TokenKind::Return),
    ("break", TokenKind::Break),
    ("and", TokenKind::And),
    ("or", TokenKind::Or),
    ("not", TokenKind::Not),
    ("borrow", TokenKind::Borrow),
    ("clone", TokenKind::Clone),
    ("move", TokenKind::Move),
    ("match", TokenKind::Match),
    ("with", TokenKind::With),
    ("import", TokenKind::Import),
    ("mod", TokenKind::Mod),
    ("nil", TokenKind::Nil),
    ("true", TokenKind::True),
    ("false", TokenKind::False),
];

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    last_num: Option<f64>,
    /// Exact digits when the last scanned number was a pure integer.
    last_int: Option<String>,
    /// `true` when only whitespace (including at least one newline) separates
    /// this position from the previous token.  A `*` in this position starts
    /// a pointer dereference statement instead of continuing a multiplication.
    at_line_start: bool,
}

pub fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}
pub fn is_name_cont(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Lexer {
    fn new(src: &str) -> Self {
        let mut chars: Vec<char> = src.chars().collect();
        if let Some(&'\u{FEFF}') = chars.first() {
            chars.remove(0);
        }
        Lexer {
            chars,
            pos: 0,
            last_num: None,
            last_int: None,
            at_line_start: true,
        }
    }

    fn peek(&self) -> char {
        *self.chars.get(self.pos).unwrap_or(&'\0')
    }
    fn peek2(&self) -> char {
        *self.chars.get(self.pos + 1).unwrap_or(&'\0')
    }
    fn bump(&mut self) -> char {
        let c = self.peek();
        self.pos += 1;
        c
    }

    fn take_number(&mut self) -> TokenKind {
        if let Some(text) = self.last_int.take() {
            TokenKind::Int(text)
        } else {
            TokenKind::Num(self.last_num.take().unwrap_or(0.0))
        }
    }

    fn run(mut self) -> LResult<Vec<Token>> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.pos >= self.chars.len() {
                out.push(Token {
                    kind: TokenKind::Eof,
                    span: Span::new(self.pos.saturating_sub(1), self.pos),
                });
                break;
            }
            let start = self.pos;
            let c = self.peek();
            let line_start = self.at_line_start;
            self.at_line_start = false;
            if is_name_start(c) {
                let kind = self.scan_name();
                out.push(Token {
                    kind,
                    span: Span::new(start, self.pos),
                });
            } else if c.is_ascii_digit() {
                self.scan_number()?;
                let kind = self.take_number();
                out.push(Token {
                    kind,
                    span: Span::new(start, self.pos),
                });
            } else {
                let kind = self.scan_symbol()?;
                let kind = match kind {
                    TokenKind::Star if line_start => TokenKind::StarLine,
                    other => other,
                };
                out.push(Token {
                    kind,
                    span: Span::new(start, self.pos),
                });
            }
        }
        Ok(out)
    }

    fn skip_trivia(&mut self) -> LResult<()> {
        loop {
            let c = self.peek();
            if c == '\n' {
                self.at_line_start = true;
                self.pos += 1;
            } else if c.is_whitespace() {
                self.pos += 1;
            } else if c == '-' && self.peek2() == '-' {
                self.pos += 2;
                if self.peek() == '[' && self.peek2() == '[' {
                    self.pos += 2;
                    loop {
                        if self.pos >= self.chars.len() {
                            return Err(LusaError::new(
                                E_LEX,
                                Some(Span::new(self.pos - 1, self.pos)),
                                "unterminated block comment (expected `]]`)",
                            )
                            .help("block comments start with `--[[` and end with `]]`"));
                        }
                        let cc = self.bump();
                        if cc == ']' && self.peek() == ']' {
                            self.pos += 1;
                            break;
                        }
                    }
                } else {
                    while self.pos < self.chars.len() && self.peek() != '\n' {
                        self.pos += 1;
                    }
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn scan_name(&mut self) -> TokenKind {
        let start = self.pos;
        while is_name_cont(self.peek()) {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        for (kw, kind) in KEYWORDS {
            if *kw == word {
                return kind.clone();
            }
        }
        TokenKind::Name(word)
    }

    fn scan_symbol(&mut self) -> LResult<TokenKind> {
        let c = self.bump();
        let k = match c {
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => {
                if self.peek() == '/' {
                    self.pos += 1;
                    TokenKind::FloorDiv
                } else {
                    TokenKind::Slash
                }
            }
            '%' => TokenKind::Percent,
            '#' => TokenKind::Len,
            '&' => TokenKind::Amp,
            '^' => TokenKind::Caret,
            '.' => {
                if self.peek() == '.' {
                    self.pos += 1;
                    if self.peek() == '.' {
                        self.pos += 1;
                        TokenKind::DotDotDot
                    } else {
                        TokenKind::DotDot
                    }
                } else {
                    TokenKind::Dot
                }
            }
            '~' => {
                if self.peek() == '=' {
                    self.pos += 1;
                    TokenKind::Ne
                } else {
                    return Err(self.err_char("unexpected character `~` (did you mean `~=`?)"));
                }
            }
            '=' => {
                if self.peek() == '=' {
                    self.pos += 1;
                    TokenKind::Eq
                } else if self.peek() == '>' {
                    self.pos += 1;
                    TokenKind::FatArrow
                } else {
                    TokenKind::Assign
                }
            }
            '|' => TokenKind::Pipe,
            '<' => {
                if self.peek() == '=' {
                    self.pos += 1;
                    TokenKind::Le
                } else {
                    TokenKind::Lt
                }
            }
            '>' => {
                if self.peek() == '=' {
                    self.pos += 1;
                    TokenKind::Ge
                } else {
                    TokenKind::Gt
                }
            }
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            ':' => TokenKind::Colon,
            '"' | '\'' => return self.scan_string(c),
            other => {
                return Err(self.err_char(&format!(
                    "unexpected character `{}`",
                    other.escape_default().to_string()
                )));
            }
        };
        Ok(k)
    }

    fn err_char(&self, msg: &str) -> LusaError {
        LusaError::new(E_LEX, Some(Span::new(self.pos - 1, self.pos)), msg)
    }

    fn err_at(&self, span: Span, msg: &str) -> LusaError {
        LusaError::new(E_LEX, Some(span), msg)
    }

    fn scan_number(&mut self) -> LResult<()> {
        self.last_num = None;
        self.last_int = None;
        let start = self.pos;

        if self.peek() == '0' && (self.peek2() == 'x' || self.peek2() == 'X') {
            self.pos += 2;
            let dstart = self.pos;
            while self.peek().is_ascii_hexdigit() {
                self.pos += 1;
            }
            if dstart == self.pos {
                return Err(self.err_at(
                    Span::new(start, self.pos),
                    "invalid hex literal (expected digits after `0x`)",
                ));
            }
            // Keep the raw spelling: converting through u64/f64 would lose
            // precision for 128-bit constants.  The C backend parses this
            // exact text into lusa_i128.
            let raw: String = self.chars[start..self.pos].iter().collect();
            self.last_int = Some(raw);
            return Ok(());
        }

        let mut text = String::new();
        let mut is_float = false;
        while self.peek().is_ascii_digit() {
            text.push(self.bump());
        }
        if self.peek() == '.' && self.chars.get(self.pos + 1).is_some_and(|c| c.is_ascii_digit())
        {
            is_float = true;
            text.push(self.bump());
            while self.peek().is_ascii_digit() {
                text.push(self.bump());
            }
        }
        if (self.peek() == 'e' || self.peek() == 'E') && {
            let save = self.pos;
            self.pos += 1;
            if self.peek() == '+' || self.peek() == '-' {
                self.pos += 1;
            }
            let has_digits = self.peek().is_ascii_digit();
            self.pos = save;
            has_digits
        } {
            is_float = true;
            text.push(self.bump()); // e/E
            if self.peek() == '+' || self.peek() == '-' {
                text.push(self.bump());
            }
            while self.peek().is_ascii_digit() {
                text.push(self.bump());
            }
        }

        if is_float {
            let v: f64 = text
                .parse()
                .map_err(|_| self.err_at(Span::new(start, self.pos), "invalid number literal"))?;
            self.last_num = Some(v);
        } else {
            self.last_int = Some(text);
        }
        Ok(())
    }

    fn scan_string(&mut self, quote: char) -> LResult<TokenKind> {
        let start = self.pos; // pos of the opening quote
        let mut segs: Vec<StrSeg> = Vec::new();
        let mut lit = String::new();
        loop {
            if self.pos >= self.chars.len() {
                return Err(self.err_at(
                    Span::new(start, self.pos),
                    "unterminated string (missing closing quote)",
                ));
            }
            let c = self.bump();
            if c == quote {
                break;
            }
            if c == '\\' {
                let ne = self.bump();
                match ne {
                    'n' => lit.push('\n'),
                    't' => lit.push('\t'),
                    'r' => lit.push('\r'),
                    '\\' => lit.push('\\'),
                    '\n' => lit.push('\n'),
                    '{' => {
                        // interpolation: `\{expr}` (re-lexed recursively)
                        let txt = std::mem::take(&mut lit);
                        if !txt.is_empty() {
                            segs.push(StrSeg::Lit(txt));
                        }
                        let toks = self.scan_braced_expr(start)?;
                        segs.push(StrSeg::Expr(toks));
                    }
                    '0' => lit.push('\0'),
                    'a' => lit.push('\u{7}'),
                    'u' => {
                        if self.peek() == '{' {
                            self.pos += 1;
                            let hex_start = self.pos;
                            while self.peek().is_ascii_hexdigit() {
                                self.pos += 1;
                            }
                            let digits: String = self.chars[hex_start..self.pos].iter().collect();
                            if self.peek() == '}' {
                                self.pos += 1;
                            } else {
                                return Err(self.err_at(Span::new(hex_start, self.pos), "missing `}` after `\\u{`"));
                            }
                            let cp = u32::from_str_radix(&digits, 16)
                                .ok()
                                .and_then(char::from_u32)
                                .ok_or_else(|| {
                                    self.err_at(Span::new(hex_start, self.pos), "invalid unicode escape")
                                })?;
                            lit.push(cp);
                        } else {
                            return Err(
                                self.err_at(Span::new(self.pos - 1, self.pos), "`\\u` must be followed by `{...}`")
                            );
                        }
                    }
                    other => {
                        return Err(
                            self.err_at(
                                Span::new(self.pos - 1, self.pos),
                                &format!("unknown escape `\\{}`", other.escape_default().to_string()),
                            )
                            .help("supported: \\\\ \\n \\t \\r \\0 \\a \\u{XXXX}; `{`/`}` are literal; interpolation is \\{expr}"),
                        );
                    }
                }
                continue;
            }
            // `{` / `}` are literal characters; interpolation is only `\{...}`
            lit.push(c);
        }
        let txt = lit;
        if !txt.is_empty() || segs.is_empty() {
            segs.push(StrSeg::Lit(txt));
        }
        Ok(TokenKind::Str(segs))
    }

    fn scan_braced_expr(&mut self, _start: usize) -> LResult<Vec<Token>> {
        let mut depth = 1usize;
        let mut toks = Vec::new();
        loop {
            self.skip_trivia()?;
            if self.pos >= self.chars.len() {
                return Err(self.err_at(
                    Span::new(self.pos.saturating_sub(1), self.pos),
                    "unterminated interpolation (missing `}`)",
                ));
            }
            let tstart = self.pos;
            let c = self.peek();
            if c == '}' {
                depth -= 1;
                self.pos += 1;
                if depth == 0 {
                    return Ok(toks);
                }
                toks.push(Token {
                    kind: TokenKind::RBrace,
                    span: Span::new(tstart, tstart + 1),
                });
                continue;
            }
            if is_name_start(c) {
                let kind = self.scan_name();
                toks.push(Token {
                    kind,
                    span: Span::new(tstart, self.pos),
                });
            } else if c.is_ascii_digit() {
                self.scan_number()?;
                let kind = self.take_number();
                toks.push(Token {
                    kind,
                    span: Span::new(tstart, self.pos),
                });
            } else if c == '"' || c == '\'' {
                self.pos += 1; // consume the opening quote (scan_string expects it gone)
                let kind = self.scan_string(c)?;
                toks.push(Token {
                    kind,
                    span: Span::new(tstart, self.pos),
                });
            } else {
                let kind = self.scan_symbol()?;
                if kind == TokenKind::LBrace {
                    depth += 1;
                }
                toks.push(Token {
                    kind,
                    span: Span::new(tstart, self.pos),
                });
            }
        }
    }
}
