//! Recursion-descent parser with Lua-style precedence.
//!
//!   or < and < comparisons < .. (right) < + - < * / // % < unary < ^ (right)

use crate::ast::*;
use crate::error::{LResult, LusaError, Span, E_PARSE};
use crate::lexer;
use crate::tokens::{StrSeg, Token, TokenKind as TK};
use std::collections::HashSet;
use std::path::Path;

pub fn parse(src: &str) -> LResult<Block> {
    parse_with_base(src, None)
}

/// Parses source with imports resolved relative to `base`.
pub fn parse_with_base(src: &str, base: Option<&Path>) -> LResult<Block> {
    let expanded = crate::imports::expand_imports(src, base)?;
    parse_expanded(&expanded)
}

/// Parses a file, resolving imports relative to that file.
pub fn parse_file(path: &Path) -> LResult<Block> {
    let src = std::fs::read_to_string(path).map_err(|e| {
        LusaError::new(
            E_PARSE,
            None,
            format!("cannot read {}: {e}", path.display()),
        )
    })?;
    let expanded = crate::imports::expand_imports_file(&src, path)?;
    parse_expanded(&expanded)
}

fn parse_expanded(src: &str) -> LResult<Block> {
    let toks = lexer::lex(src)?;
    Parser::new(toks).parse_file()
}

const STMT_ENDS: &[TK] = &[TK::End, TK::Else, TK::Elseif, TK::Until, TK::Eof];

/// Attributes collected before a declaration or statement.
#[derive(Default)]
struct Attributes {
    packed: bool,
    align: Option<u32>,
    no_std: bool,
    entry: bool,
    global_allocator: bool,
    link_section: Option<String>,
    interrupt: Option<InterruptAttr>,
}

/// Linker section names are emitted inside a C string literal and passed to a
/// compiler attribute.  Keep the accepted set deliberately small so user text
/// can never break out of the literal or add another attribute.
fn validate_section_name(name: &str, span: Span) -> LResult<()> {
    if name.is_empty() {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "`link_section` cannot be empty",
        )
        .help("use a section such as `.text.boot` or `.data.boot`"));
    }
    if name.len() > 255 {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "section name is too long (maximum 255 bytes)",
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'$'))
    {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "invalid character in `link_section` name",
        )
        .help("section names may contain ASCII letters, digits, `_`, `.`, `-`, and `$`"));
    }
    Ok(())
}

/// Interrupt vector names become linker/assembly symbols, so they are limited
/// to a portable identifier spelling.  Target membership is validated later,
/// when the cross-build knows which board is being produced.
fn validate_interrupt_name(name: &str, span: Span) -> LResult<()> {
    if name.is_empty() {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "interrupt vector name cannot be empty",
        )
        .help("write `#[interrupt(\"TIM2\")]` or use a bare `#[interrupt]`"));
    }
    if name.len() > 64 {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "interrupt vector name is too long (maximum 64 bytes)",
        ));
    }
    let mut bytes = name.bytes();
    let first = bytes.next().unwrap_or(b'_');
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "invalid character in interrupt vector name",
        )
        .help("vector names may contain ASCII letters, digits, and `_`, and must start with a letter or `_`"));
    }
    if name.eq_ignore_ascii_case("default") || name.starts_with('@') {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "`default` is reserved for the bare `#[interrupt]` timer vector",
        ));
    }
    Ok(())
}


const PREC_OR: u8 = 1;
const PREC_AND: u8 = 2;
const PREC_CMP: u8 = 3;
const PREC_CONCAT: u8 = 4;
const PREC_ADD: u8 = 5;
const PREC_MUL: u8 = 6;
const PREC_UNARY: u8 = 7;
const PREC_POW: u8 = 8;

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    /// Struct names declared before the current expression.  Struct literals
    /// only become unambiguous once the name is known, exactly like the
    /// native backend's one-pass declaration order.
    structs: HashSet<String>,
    match_seq: usize,
}

fn merge(a: Span, b: Span) -> Span {
    a.merge(b)
}

fn expr_var(name: String, span: Span) -> Expr {
    Expr { kind: ExprKind::Var(Box::new(name)), span }
}

fn expr_field(obj: Expr, name: String, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Field { obj: Box::new(obj), name: Box::new(name) },
        span,
    }
}

/// Validates the raw text accepted by `asm!("...")`.  The C emitter places the
/// text inside a C string literal, so quotes, backslashes, and line breaks are
/// rejected before either backend sees the program.
fn validate_asm_text(text: &str, span: Span) -> LResult<()> {
    if text.trim().is_empty() {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "`asm!` requires a non-empty assembly string",
        )
        .help("use `asm!(\"nop\")` or a semicolon-separated instruction string"));
    }
    if text
        .chars()
        .any(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '\0'))
    {
        return Err(LusaError::new(
            E_PARSE,
            Some(span),
            "`asm!` text must not contain quotes, backslashes, or newlines",
        ));
    }
    Ok(())
}

/// Maps the Rust-style pointer method sugar (`ptr.read_volatile()`) to the
/// shared intrinsic name.  Both parsers desugar here so the interpreter, VM,
/// and C backend see exactly one call shape.
fn volatile_builtin(name: &str) -> Option<&'static str> {
    match name {
        "read_volatile" => Some("volatile_read"),
        "write_volatile" => Some("volatile_write"),
        _ => None,
    }
}

enum MatchPattern {
    Literal(Expr),
    Binding(String, Span),
    Wildcard,
    /// `Ok(x)`, `Err(e)`, `Some(x)`, or `None`.  `binding` is `None` for
    /// `None`, `Ok(_)`, and the empty `Ok()` spelling.
    Constructor {
        name: String,
        binding: Option<String>,
        span: Span,
    },
}

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Parser {
            toks,
            pos: 0,
            structs: HashSet::new(),
            match_seq: 0,
        }
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }
    fn peek_at(&self, off: usize) -> &Token {
        &self.toks[(self.pos + off).min(self.toks.len() - 1)]
    }
    fn at(&self, k: TK) -> bool {
        self.peek().kind == k
    }
    fn bump(&mut self) -> Token {
        let t = self.peek().clone();
        self.pos = (self.pos + 1).min(self.toks.len() - 1);
        t
    }
    fn eat(&mut self, k: TK) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, k: TK, what: &str) -> LResult<Token> {
        if self.at(k) {
            Ok(self.bump())
        } else {
            let tk = self.peek().clone();
            Err(LusaError::new(
                E_PARSE,
                Some(tk.span),
                format!("expected {}, found {}", what, tk.desc()),
            ))
        }
    }
    fn unexpected_here(&self, what: &str) -> LusaError {
        let tk = self.peek().clone();
        LusaError::new(
            E_PARSE,
            Some(tk.span),
            format!("expected {}, found {}", what, tk.desc()),
        )
    }

    fn expect_name(&mut self) -> LResult<(String, Span)> {
        let tk = self.peek().clone();
        match tk.kind.clone() {
            TK::Name(n) => {
                self.bump();
                Ok((n, tk.span))
            }
            _ => Err(self.unexpected_here("a name")),
        }
    }

    fn expect_member_name(&mut self) -> LResult<(String, Span)> {
        let tk = self.peek().clone();
        if let TK::Name(n) = tk.kind.clone() {
            self.bump();
            return Ok((n, tk.span));
        }
        if let Some(text) = tk.kind.keyword_text() {
            self.bump();
            return Ok((text.to_string(), tk.span));
        }
        Err(self.unexpected_here("a member name"))
    }

    // ===== statements =====

    fn parse_file(&mut self) -> LResult<Block> {
        self.parse_block()
    }

    fn parse_block(&mut self) -> LResult<Block> {
        self.parse_block_until(&[])
    }

    /// Parses statements until one of the normal block terminators or an
    /// extra terminator (used by `match` arms, which end at `|`).
    fn parse_block_until(&mut self, extra_ends: &[TK]) -> LResult<Block> {
        let mut block = Vec::new();
        loop {
            if STMT_ENDS.contains(&self.peek().kind) || extra_ends.contains(&self.peek().kind) {
                break;
            }
            if self.eat(TK::Semi) {
                continue;
            }
            if self.at(TK::Return) {
                let r = self.parse_return()?;
                block.push(r);
                self.eat(TK::Semi);
                break;
            }
            let st = self.parse_stmt()?;
            block.push(st);
            self.eat(TK::Semi);
            if self.at(TK::Return) {
                // return must be last (we still allow parsing it here)
                let r = self.parse_return()?;
                block.push(r);
                self.eat(TK::Semi);
                break;
            }
        }
        Ok(block)
    }

    fn parse_return(&mut self) -> LResult<St> {
        let ret = self.bump();
        let mut values = Vec::new();
        if !STMT_ENDS.contains(&self.peek().kind) && !self.at(TK::Semi) {
            values = self.parse_exprlist()?;
        }
        let span = match values.last() {
            Some(e) => merge(ret.span, e.span),
            None => ret.span,
        };
        Ok(St { kind: StKind::Return { values, span: ret.span }, span })
    }

    fn parse_stmt(&mut self) -> LResult<St> {
        let tk = self.peek().clone();
        let st = match tk.kind {
            TK::Local => self.parse_local_st(None)?,
            TK::If => self.parse_if()?,
            TK::While => self.parse_while()?,
            TK::Repeat => self.parse_repeat()?,
            TK::For => self.parse_for()?,
            TK::Do => {
                self.bump();
                let body = self.parse_block()?;
                self.expect(TK::End, "`end` to close the `do`")?;
                St { kind: StKind::Do { body }, span: tk.span }
            }
            TK::Unsafe => {
                self.bump();
                // `unsafe do ... end` and the shorter `unsafe ... end` both
                // work.  The dynamic engines track the block so raw pointer
                // dereferences and intrinsics can enforce their sandbox rules.
                self.eat(TK::Do);
                let body = self.parse_block()?;
                self.expect(TK::End, "`end` to close the `unsafe` block")?;
                St { kind: StKind::Unsafe { body }, span: tk.span }
            }
            TK::Struct => self.parse_struct_st(false, None)?,
            TK::Len if self.peek_at(1).kind == TK::LBracket => self.parse_attributed_stmt()?,
            TK::Match => self.parse_match()?,
            TK::Import | TK::Mod => {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(tk.span),
                    "unresolved import statement",
                )
                .help("imports are resolved before parsing; check the path and that the driver received a base directory"));
            }
            TK::Function => self.parse_func_st()?,
            TK::Async => self.parse_async_func_st()?,
            TK::Break => {
                self.bump();
                St { kind: StKind::Break, span: tk.span }
            }
            TK::Borrow | TK::Clone | TK::Move | TK::LParen | TK::Num(_) | TK::Int(_) | TK::Str(_)
            | TK::Nil | TK::True | TK::False | TK::Minus | TK::Not | TK::Len
            | TK::DotDotDot | TK::LBrace | TK::LBracket | TK::Amp | TK::Await => {
                let e = self.parse_expr()?;
                St { kind: StKind::ExprSt(e), span: tk.span }
            }
            TK::Name(_) => self.parse_assign_or_expr()?,
            TK::Star | TK::StarLine => self.parse_assign_or_expr()?,
            other => {
                let tk = self.peek().clone();
                let _ = other;
                return Err(LusaError::new(
                    E_PARSE,
                    Some(tk.span),
                    format!("unexpected {} at the start of a statement", tk.desc()),
                ));
            }
        };
        Ok(st)
    }

    fn parse_assign_or_expr(&mut self) -> LResult<St> {
        let start = self.peek().span;
        let first = self.parse_prefix()?;
        if self.at(TK::Assign) || self.at(TK::Comma) {
            let mut targets = vec![first.clone()];
            while self.eat(TK::Comma) {
                let t = self.parse_prefix()?;
                match &t.kind {
                    ExprKind::Var(_)
                    | ExprKind::Field { .. }
                    | ExprKind::Index { .. }
                    | ExprKind::Un { op: UnOp::Deref, .. } => {}
                    _ => {
                        return Err(LusaError::new(E_PARSE, Some(t.span), "invalid assignment target")
                            .help("valid targets are names (`x`), fields (`t.k`), indices (`t[k]`) and dereferences (`*ptr`)"));
                    }
                }
                targets.push(t);
            }
            self.expect(TK::Assign, "`=`")?;
            let values = self.parse_exprlist()?;
            let end = values.last().map(|e| e.span).unwrap_or(start);
            for t in &targets {
                match &t.kind {
                    ExprKind::Var(_)
                    | ExprKind::Field { .. }
                    | ExprKind::Index { .. }
                    | ExprKind::Un { op: UnOp::Deref, .. } => {}
                    _ => {
                        return Err(LusaError::new(E_PARSE, Some(t.span), "invalid assignment target")
                            .help("valid targets are names (`x`), fields (`t.k`), indices (`t[k]`) and dereferences (`*ptr`)"));
                    }
                }
            }
            return Ok(St {
                kind: StKind::Assign { targets, values, link_section: None },
                span: merge(start, end),
            });
        }
        // otherwise it must be a call/method expression statement
        match &first.kind {
            ExprKind::Call { .. } | ExprKind::Method { .. } | ExprKind::Await { .. } => {}
            _ => {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(first.span),
                    "this expression cannot start a statement",
                )
                .help("only calls (`f(x)`) and method calls (`o:m()`) may appear as bare statements"));
            }
        }
        let first_span = first.span;
        Ok(St { kind: StKind::ExprSt(first), span: first_span })
    }

    /// Consumes a type annotation and returns its spelling.  The dynamic
    /// engines use it for the struct/pointer emulation layer; the C11 backend
    /// performs the actual static checking on the same spellings.
    fn parse_type_annotation(&mut self) -> LResult<String> {
        if self.eat(TK::LBracket) {
            let element = self.parse_type_annotation()?;
            if self.eat(TK::Semi) {
                let size = self.parse_array_size()?;
                self.expect(TK::RBracket, "`]` to close the array type")?;
                return Ok(format!("array<{element},{size}>"));
            }
            self.expect(TK::RBracket, "`]` to close the table type")?;
            return Ok(format!("table<{element}>"));
        }
        if self.eat(TK::Star) {
            let (qual, _) = self.expect_name()?;
            if qual != "mut" && qual != "const" {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(self.toks[self.pos - 1].span),
                    format!("expected `mut` or `const` after `*` in a pointer type, found `{qual}`"),
                ));
            }
            let (elem, _) = self.expect_name()?;
            return Ok(format!("*{qual} {elem}"));
        }
        let (name, _) = self.expect_name()?;
        if name == "table" && self.eat(TK::Lt) {
            let inner = self.parse_type_annotation()?;
            self.expect(TK::Gt, "`>` to close the table type")?;
            return Ok(format!("table<{inner}>"));
        }
        if matches!(name.as_str(), "Vec" | "vec" | "Box" | "box") && self.eat(TK::Lt) {
            let inner = self.parse_type_annotation()?;
            self.expect(TK::Gt, "`>` to close the generic type")?;
            let tag = if matches!(name.as_str(), "Vec" | "vec") {
                "vec"
            } else {
                "box"
            };
            return Ok(format!("{tag}<{inner}>"));
        }
        if matches!(name.as_str(), "Result" | "result") && self.eat(TK::Lt) {
            let ok = self.parse_type_annotation()?;
            self.expect(TK::Comma, "`,` between the `Result` success and error types")?;
            let err = self.parse_type_annotation()?;
            self.expect(TK::Gt, "`>` to close the `Result` type")?;
            return Ok(format!("result<{ok},{err}>"));
        }
        if matches!(name.as_str(), "Option" | "option") && self.eat(TK::Lt) {
            let inner = self.parse_type_annotation()?;
            self.expect(TK::Gt, "`>` to close the `Option` type")?;
            return Ok(format!("option<{inner}>"));
        }
        if matches!(name.as_str(), "Task" | "task") && self.eat(TK::Lt) {
            let inner = self.parse_type_annotation()?;
            self.expect(TK::Gt, "`>` to close the `Task` type")?;
            return Ok(format!("task<{inner}>"));
        }
        Ok(name)
    }

    /// Parses the `N` in `[T; N]`.  Only literal sizes are accepted: the C11
    /// backend needs the bound at declaration time.
    fn parse_array_size(&mut self) -> LResult<String> {
        let tk = self.peek().clone();
        let size = match tk.kind.clone() {
            TK::Int(text) => text,
            TK::Num(n) => {
                if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
                    return Err(LusaError::new(
                        E_PARSE,
                        Some(tk.span),
                        "array length must be a non-negative integer literal",
                    ));
                }
                format!("{}", n as u64)
            }
            _ => {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(tk.span),
                    format!("expected an array length, found {}", tk.desc()),
                )
                .help("fixed arrays need a literal length, e.g. `[u8; 16]`"));
            }
        };
        self.bump();
        Ok(size)
    }

    /// Parses zero or more `#[...]` attributes.
    ///
    /// Supported attributes are `#[packed]`, `#[repr(align(N))]`,
    /// `#[no_std]`, `#[entry]`, `#[global_allocator]`, `#[interrupt]`,
    /// `#[interrupt("VECTOR")]`, and `#[link_section = "..."]`.  The values are
    /// validated here, before the AST reaches either backend, so the dynamic
    /// engines and the C emitter agree on what is legal.
    fn parse_attributes(&mut self) -> LResult<Attributes> {
        const MAX_ALIGN: u32 = 4096;
        let mut attrs = Attributes::default();
        let mut stop_after = false;
        while self.at(TK::Len) && self.peek_at(1).kind == TK::LBracket {
            self.bump(); // #
            self.bump(); // [
            let (attr, attr_span) = self.expect_name()?;
            match attr.as_str() {
                "packed" => {
                    if attrs.packed {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[packed]` attribute",
                        ));
                    }
                    attrs.packed = true;
                }
                "no_std" => {
                    if attrs.no_std {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[no_std]` attribute",
                        ));
                    }
                    attrs.no_std = true;
                    // `#[no_std]` is a standalone file-level statement; do not
                    // absorb attributes that belong to the next declaration.
                    stop_after = true;
                }
                "entry" => {
                    if attrs.entry {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[entry]` attribute",
                        ));
                    }
                    attrs.entry = true;
                }
                "global_allocator" => {
                    if attrs.global_allocator {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[global_allocator]` attribute",
                        ));
                    }
                    attrs.global_allocator = true;
                }
                "link_section" => {
                    if attrs.link_section.is_some() {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[link_section = \"...\"]` attribute",
                        ));
                    }
                    self.expect(TK::Assign, "`=` after `link_section`")?;
                    let tk = self.peek().clone();
                    let name = match &tk.kind {
                        TK::Str(segs) => {
                            let mut text = String::new();
                            for seg in segs {
                                match seg {
                                    StrSeg::Lit(lit) => text.push_str(lit),
                                    StrSeg::Expr(_) => {
                                        return Err(LusaError::new(
                                            E_PARSE,
                                            Some(tk.span),
                                            "`link_section` requires a plain string literal",
                                        ));
                                    }
                                }
                            }
                            self.bump();
                            text
                        }
                        _ => {
                            return Err(LusaError::new(
                                E_PARSE,
                                Some(tk.span),
                                format!(
                                    "expected a section name string, found {}",
                                    tk.desc()
                                ),
                            )
                            .help("write `#[link_section = \".text.boot\"]`"));
                        }
                    };
                    validate_section_name(&name, tk.span)?;
                    attrs.link_section = Some(name);
                }
                "interrupt" => {
                    if attrs.interrupt.is_some() {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[interrupt]` attribute",
                        ));
                    }
                    let mut vector = None;
                    if self.eat(TK::LParen) {
                        let tk = self.peek().clone();
                        let name = match &tk.kind {
                            TK::Str(segs) => {
                                let mut text = String::new();
                                for seg in segs {
                                    match seg {
                                        StrSeg::Lit(lit) => text.push_str(lit),
                                        StrSeg::Expr(_) => {
                                            return Err(LusaError::new(
                                                E_PARSE,
                                                Some(tk.span),
                                                "`interrupt` requires a plain vector-name string",
                                            ));
                                        }
                                    }
                                }
                                self.bump();
                                text
                            }
                            _ => {
                                return Err(LusaError::new(
                                    E_PARSE,
                                    Some(tk.span),
                                    format!(
                                        "expected an interrupt vector name, found {}",
                                        tk.desc()
                                    ),
                                )
                                .help("write `#[interrupt(\"TIM2\")]` or use a bare `#[interrupt]`"));
                            }
                        };
                        validate_interrupt_name(&name, tk.span)?;
                        self.expect(TK::RParen, "`)` after the interrupt vector name")?;
                        vector = Some(name);
                    }
                    attrs.interrupt = Some(InterruptAttr { vector });
                }
                "repr" => {
                    self.expect(TK::LParen, "`(` after `repr`")?;
                    let (inner, inner_span) = self.expect_name()?;
                    if inner != "align" {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(inner_span),
                            format!("unsupported representation attribute `repr({inner})`"),
                        )
                        .help("only `#[repr(align(N))]` is supported"));
                    }
                    self.expect(TK::LParen, "`(` after `repr(align`")?;
                    let tk = self.peek().clone();
                    let value: u64 = match &tk.kind {
                        TK::Int(text) => text.parse().unwrap_or(u64::MAX),
                        TK::Num(n)
                            if n.is_finite()
                                && *n >= 0.0
                                && n.fract() == 0.0
                                && *n <= u32::MAX as f64 =>
                        {
                            *n as u64
                        }
                        _ => {
                            return Err(LusaError::new(
                                E_PARSE,
                                Some(tk.span),
                                format!(
                                    "expected an alignment integer, found {}",
                                    tk.desc()
                                ),
                            )
                            .help("use a power of two such as `#[repr(align(8))]`"));
                        }
                    };
                    self.bump();
                    self.expect(TK::RParen, "`)` to close `repr(align(`")?;
                    self.expect(TK::RParen, "`)` to close `repr(`")?;
                    if value == 0 || value > MAX_ALIGN as u64 {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(tk.span),
                            format!("alignment {value} must be between 1 and 4096"),
                        )
                        .help("alignment must be a power of two between 1 and 4096"));
                    }
                    if !(value as u32).is_power_of_two() {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(tk.span),
                            format!("alignment {value} is not a power of two"),
                        )
                        .help("alignment must be a power of two between 1 and 4096"));
                    }
                    if attrs.align.is_some() {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(attr_span),
                            "duplicate `#[repr(align(N))]` attribute",
                        ));
                    }
                    attrs.align = Some(value as u32);
                }
                other => {
                    return Err(LusaError::new(
                        E_PARSE,
                        Some(attr_span),
                        format!("unsupported attribute `#{other}`"),
                    )
                    .help(
                        "supported attributes are `#[packed]`, `#[repr(align(N))]`, \
                         `#[no_std]`, `#[entry]`, `#[global_allocator]`, \
                         `#[interrupt]`, `#[interrupt(\"VECTOR\")]`, and \
                         `#[link_section = \"...\"]`",
                    ));
                }
            }
            self.expect(TK::RBracket, "`]` to close the attribute")?;
            if stop_after {
                break;
            }
        }
        Ok(attrs)
    }

    /// Handles `#[...]` prefixes and dispatches to the declaration they
    /// annotate.  Misplaced attributes are rejected here with a parser
    /// diagnostic; the C backend repeats placement checks for anything the
    /// dynamic engines accept.
    fn parse_attributed_stmt(&mut self) -> LResult<St> {
        let start = self.peek().span;
        let attrs = self.parse_attributes()?;

        if attrs.no_std {
            if attrs.packed
                || attrs.align.is_some()
                || attrs.entry
                || attrs.global_allocator
                || attrs.interrupt.is_some()
                || attrs.link_section.is_some()
            {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(start),
                    "`#[no_std]` must be written on its own",
                )
                .help("place `#[no_std]` before the rest of the program and use a separate attribute for declarations"));
            }
            return Ok(St { kind: StKind::NoStd, span: start });
        }

        if self.at(TK::Struct) {
            if attrs.entry
                || attrs.link_section.is_some()
                || attrs.global_allocator
                || attrs.interrupt.is_some()
            {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(start),
                    "`#[entry]` and `#[link_section]` are only supported on functions or globals; `#[global_allocator]` and `#[interrupt]` are only supported on functions",
                ));
            }
            return self.parse_struct_st(attrs.packed, attrs.align);
        }

        if self.at(TK::Function) || self.at(TK::Async) {
            if attrs.packed || attrs.align.is_some() {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(start),
                    "`#[packed]` and `#[repr(align(N))]` are only supported on structs",
                ));
            }
            let mut st = if self.at(TK::Async) {
                self.parse_async_func_st()?
            } else {
                self.parse_func_st()?
            };
            if let StKind::FuncSt {
                entry,
                global_allocator,
                link_section,
                interrupt,
                ..
            } = &mut st.kind
            {
                *entry = attrs.entry;
                *global_allocator = attrs.global_allocator;
                *link_section = attrs.link_section;
                *interrupt = attrs.interrupt;
            }
            return Ok(st);
        }

        if self.at(TK::Local) {
            if attrs.packed
                || attrs.align.is_some()
                || attrs.entry
                || attrs.global_allocator
                || attrs.interrupt.is_some()
            {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(start),
                    "`#[packed]`, `#[repr(align(N))]`, `#[entry]`, and `#[interrupt]` cannot annotate a local; `#[global_allocator]` is only supported on functions",
                ));
            }
            return self.parse_local_st(attrs.link_section);
        }

        if matches!(self.peek().kind, TK::Name(_) | TK::Star | TK::StarLine) {
            if attrs.packed
                || attrs.align.is_some()
                || attrs.entry
                || attrs.global_allocator
                || attrs.interrupt.is_some()
            {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(start),
                    "`#[packed]`, `#[repr(align(N))]`, `#[entry]`, and `#[interrupt]` cannot annotate an assignment; `#[global_allocator]` is only supported on functions",
                ));
            }
            let mut st = self.parse_assign_or_expr()?;
            match &mut st.kind {
                StKind::Assign { link_section, .. } => *link_section = attrs.link_section,
                _ => {
                    if attrs.link_section.is_some() {
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(start),
                            "`#[link_section]` is only supported on functions and global variables",
                        ));
                    }
                }
            }
            return Ok(st);
        }

        Err(LusaError::new(
            E_PARSE,
            Some(self.peek().span),
            "this statement cannot carry attributes",
        )
        .help(
            "attributes are supported on `struct`, `function`, top-level `local`, and global assignments",
        ))
    }

    fn parse_struct_st(&mut self, packed: bool, align: Option<u32>) -> LResult<St> {
        let kw = self.bump();
        let (name, name_span) = self.expect_name()?;
        let mut fields = Vec::new();
        if self.eat(TK::LBrace) {
            while !self.at(TK::RBrace) {
                let (fname, fspan) = self.expect_name()?;
                self.expect(TK::Colon, "`:` after the field name")?;
                let ty = self.parse_type_annotation()?;
                fields.push(StructField { name: fname, ty, span: fspan });
                if !self.eat(TK::Comma) {
                    self.eat(TK::Semi);
                    if !matches!(self.peek().kind, TK::Name(_)) {
                        break;
                    }
                }
            }
            self.expect(TK::RBrace, "`}` to close the struct")?;
        } else {
            // Bare form: `struct Point x: i32, y: i32 end`.
            while !self.at(TK::End) {
                let (fname, fspan) = self.expect_name()?;
                self.expect(TK::Colon, "`:` after the field name")?;
                let ty = self.parse_type_annotation()?;
                fields.push(StructField { name: fname, ty, span: fspan });
                self.eat(TK::Comma);
                self.eat(TK::Semi);
            }
            self.expect(TK::End, "`end` to close the struct")?;
        }
        self.structs.insert(name.clone());
        let span = merge(kw.span, name_span);
        Ok(St { kind: StKind::Struct { name, fields, packed, align }, span })
    }

    /// `Point{x: 1, y: 2}` — the current token is `{`.
    fn parse_struct_lit(&mut self, name: String, start: Span) -> LResult<Expr> {
        self.expect(TK::LBrace, "`{` to open the struct literal")?;
        let mut fields = Vec::new();
        while !self.at(TK::RBrace) {
            let (fname, fspan) = self.expect_name()?;
            if !self.eat(TK::Colon) {
                self.expect(TK::Assign, "`:` or `=` after the struct field name")?;
            }
            let value = self.parse_expr()?;
            fields.push(StructLitField { name: fname, value, span: fspan });
            if !self.eat(TK::Comma) {
                if self.at(TK::RBrace) {
                    break;
                }
                let continues = matches!(self.peek().kind, TK::Name(_))
                    && matches!(self.peek_at(1).kind, TK::Colon | TK::Assign);
                if !continues {
                    break;
                }
            }
        }
        let close = self.expect(TK::RBrace, "`}` to close the struct literal")?;
        Ok(Expr {
            kind: ExprKind::StructLit { name, fields },
            span: merge(start, close.span),
        })
    }

    /// `match expr with | pattern => body | _ => default end`
    ///
    /// Lowered to an ordinary `do` block plus a hidden local and `if`/`elseif`
    /// chain.  Because the existing checker scopes each branch independently,
    /// ownership errors inside an arm are caught statically with no runtime
    /// match machinery.
    fn parse_match(&mut self) -> LResult<St> {
        let kw = self.bump();
        let scrutinee = self.parse_expr()?;
        self.expect(TK::With, "`with` after the match value")?;
        self.match_seq += 1;
        let temp = format!("__match_{}", self.match_seq);
        let temp_span = kw.span;
        let temp_var = || expr_var(temp.clone(), temp_span);
        let mut branches: Vec<(Expr, Block)> = Vec::new();
        let mut else_block: Option<Block> = None;
        let mut saw_default = false;
        let mut arm_count = 0usize;

        while !self.at(TK::End) && !self.at(TK::Eof) {
            self.eat(TK::Pipe);
            arm_count += 1;
            let pattern = self.parse_match_pattern()?;
            let arrow = self.expect(TK::FatArrow, "`=>` after the match pattern")?;
            let body = self.parse_block_until(&[TK::Pipe])?;
            match pattern {
                MatchPattern::Wildcard => {
                    else_block = Some(body);
                    saw_default = true;
                }
                MatchPattern::Binding(name, bspan) => {
                    let bind = St {
                        kind: StKind::Local {
                            targets: vec![name],
                            values: vec![Expr {
                                kind: ExprKind::Move { e: Box::new(temp_var()) },
                                span: bspan,
                            }],
                            annotations: vec![None],
                            link_section: None,
                        },
                        span: bspan,
                    };
                    let mut wrapped = vec![bind];
                    wrapped.extend(body);
                    else_block = Some(wrapped);
                    saw_default = true;
                }
                MatchPattern::Literal(lit) => {
                    let cond = Expr {
                        kind: ExprKind::Bin {
                            op: BinOp::Eq,
                            l: Box::new(temp_var()),
                            r: Box::new(lit),
                        },
                        span: merge(arrow.span, temp_span),
                    };
                    branches.push((cond, body));
                }
                MatchPattern::Constructor { name, binding, span: bspan } => {
                    let (method, bind_method) = match name.as_str() {
                        "Ok" => ("is_ok", Some("unwrap")),
                        "Err" => ("is_err", Some("unwrap_err")),
                        "Some" => ("is_some", Some("unwrap")),
                        _ => ("is_none", None),
                    };
                    let cond = Expr {
                        kind: ExprKind::Method {
                            obj: Box::new(temp_var()),
                            name: Box::new(method.to_string()),
                            args: Vec::new(),
                            span_args_end: bspan,
                        },
                        span: bspan,
                    };
                    let mut wrapped = Vec::new();
                    if let (Some(bind), Some(bind_method)) = (binding, bind_method) {
                        let value = Expr {
                            kind: ExprKind::Method {
                                obj: Box::new(temp_var()),
                                name: Box::new(bind_method.to_string()),
                                args: Vec::new(),
                                span_args_end: bspan,
                            },
                            span: bspan,
                        };
                        wrapped.push(St {
                            kind: StKind::Local {
                                targets: vec![bind],
                                values: vec![value],
                                annotations: vec![None],
                                link_section: None,
                            },
                            span: bspan,
                        });
                    }
                    wrapped.extend(body);
                    branches.push((cond, wrapped));
                }
            }
            if saw_default {
                break;
            }
        }
        if arm_count == 0 {
            return Err(LusaError::new(
                E_PARSE,
                Some(kw.span),
                "`match` requires at least one arm",
            )
            .help("write `match value with | pattern => body | _ => body end`"));
        }
        self.expect(TK::End, "`end` to close the `match`")?;
        let local = St {
            kind: StKind::Local {
                targets: vec![temp],
                values: vec![scrutinee],
                annotations: vec![None],
                link_section: None,
            },
            span: kw.span,
        };
        let if_stmt = St {
            kind: StKind::If { branches, else_block },
            span: kw.span,
        };
        Ok(St {
            kind: StKind::Do { body: vec![local, if_stmt] },
            span: kw.span,
        })
    }

    fn parse_match_pattern(&mut self) -> LResult<MatchPattern> {
        let tk = self.peek().clone();
        if let TK::Name(ref name) = tk.kind {
            if name == "_" {
                self.bump();
                return Ok(MatchPattern::Wildcard);
            }
            if matches!(name.as_str(), "Ok" | "Err" | "Some" | "None") {
                let name = name.clone();
                self.bump();
                if name == "None" {
                    if self.eat(TK::LParen) {
                        self.expect(TK::RParen, "`)` after `None`")?;
                    }
                    return Ok(MatchPattern::Constructor {
                        name,
                        binding: None,
                        span: tk.span,
                    });
                }
                if !self.at(TK::LParen) {
                    return Ok(MatchPattern::Binding(name, tk.span));
                }
                self.bump();
                let mut binding = None;
                if !self.at(TK::RParen) {
                    let inner = self.peek().clone();
                    if let TK::Name(ref b) = inner.kind {
                        let b = b.clone();
                        self.bump();
                        if b != "_" {
                            binding = Some(b);
                        }
                    } else {
                        return Err(self
                            .unexpected_here("a binding name or `_` in the sum-type pattern")
                            .help(
                                "`Ok`, `Err`, `Some`, and `None` patterns bind a single name, e.g. `Ok(value)`",
                            ));
                    }
                }
                self.expect(TK::RParen, "`)` to close the sum-type pattern")?;
                return Ok(MatchPattern::Constructor { name, binding, span: tk.span });
            }
            self.bump();
            return Ok(MatchPattern::Binding(name.clone(), tk.span));
        }
        let lit = match tk.kind {
            TK::Str(_) | TK::Num(_) | TK::Int(_) | TK::Nil | TK::True | TK::False => {
                self.parse_primary()?
            }
            TK::Minus => {
                let minus = self.bump();
                let inner = self.parse_primary()?;
                let span = merge(minus.span, inner.span);
                Expr {
                    kind: ExprKind::Un { op: UnOp::Neg, e: Box::new(inner) },
                    span,
                }
            }
            _ => {
                return Err(self.unexpected_here("a literal, a binding name, or `_`").help(
                    "match patterns support numbers, strings, booleans, nil, `_`, and a single binding name",
                ));
            }
        };
        Ok(MatchPattern::Literal(lit))
    }

    fn parse_local_st(&mut self, link_section: Option<String>) -> LResult<St> {
        let kw = self.bump();
        if self.at(TK::Function) {
            self.bump();
            let (name, span) = self.expect_name()?;
            let func = self.parse_funclit()?;
            let fexpr = Expr { kind: ExprKind::Func(func), span: merge(kw.span, span) };
            return Ok(St {
                kind: StKind::Local {
                    targets: vec![name],
                    values: vec![fexpr],
                    annotations: vec![None],
                    link_section,
                },
                span: kw.span,
            });
        }
        let (n0, _) = self.expect_name()?;
        let mut annotations = vec![if self.eat(TK::Colon) {
            Some(self.parse_type_annotation()?)
        } else {
            None
        }];
        let mut targets = vec![n0];
        while self.eat(TK::Comma) {
            let (n, _) = self.expect_name()?;
            annotations.push(if self.eat(TK::Colon) {
                Some(self.parse_type_annotation()?)
            } else {
                None
            });
            targets.push(n);
        }
        let mut values = Vec::new();
        if self.eat(TK::Assign) {
            values = self.parse_exprlist()?;
        }
        Ok(St {
            kind: StKind::Local { targets, values, annotations, link_section },
            span: kw.span,
        })
    }

    fn parse_if(&mut self) -> LResult<St> {
        let kw = self.bump();
        let mut branches = Vec::new();
        let cond = self.parse_expr()?;
        self.expect(TK::Then, "`then`")?;
        let body = self.parse_block()?;
        branches.push((cond, body));
        let mut else_block = None;
        loop {
            if self.eat(TK::Elseif) {
                let c = self.parse_expr()?;
                self.expect(TK::Then, "`then`")?;
                let b = self.parse_block()?;
                branches.push((c, b));
            } else if self.eat(TK::Else) {
                else_block = Some(self.parse_block()?);
                break;
            } else {
                break;
            }
        }
        self.expect(TK::End, "`end` to close the `if`")?;
        Ok(St { kind: StKind::If { branches, else_block }, span: kw.span })
    }

    fn parse_while(&mut self) -> LResult<St> {
        let kw = self.bump();
        let cond = self.parse_expr()?;
        self.expect(TK::Do, "`do`")?;
        let body = self.parse_block()?;
        self.expect(TK::End, "`end` to close the `while`")?;
        Ok(St { kind: StKind::While { cond, body }, span: kw.span })
    }

    fn parse_repeat(&mut self) -> LResult<St> {
        let kw = self.bump();
        let body = self.parse_block()?;
        self.expect(TK::Until, "`until`")?;
        let cond = self.parse_expr()?;
        Ok(St { kind: StKind::Repeat { body, cond }, span: kw.span })
    }

    fn parse_for(&mut self) -> LResult<St> {
        let kw = self.bump();
        let (var0, _) = self.expect_name()?;
        if self.at(TK::Assign) {
            self.bump();
            let start = self.parse_expr()?;
            self.expect(TK::Comma, "`,` between the start and limit of the numeric `for`")?;
            let limit = self.parse_expr()?;
            let step = if self.eat(TK::Comma) { Some(self.parse_expr()?) } else { None };
            self.expect(TK::Do, "`do`")?;
            let body = self.parse_block()?;
            self.expect(TK::End, "`end` to close the `for`")?;
            Ok(St {
                kind: StKind::ForNum { var: var0, start, limit, step, body },
                span: kw.span,
            })
        } else {
            let mut vars = vec![var0];
            while self.eat(TK::Comma) {
                let (n, _) = self.expect_name()?;
                vars.push(n);
            }
            self.expect(TK::In, "`in`")?;
            let values = self.parse_exprlist()?;
            self.expect(TK::Do, "`do`")?;
            let body = self.parse_block()?;
            self.expect(TK::End, "`end` to close the `for`")?;
            Ok(St { kind: StKind::ForIter { vars, values, body }, span: kw.span })
        }
    }

    /// `function name(.name)*(m) params body end`
    fn parse_func_st(&mut self) -> LResult<St> {
        self.parse_func_st_inner(false)
    }

    /// `async function name(...) ... end`.  `async fn name(...)` is accepted
    /// as a compact alias and produces the same AST.
    fn parse_async_func_st(&mut self) -> LResult<St> {
        let async_kw = self.bump();
        let is_fn_alias = matches!(&self.peek().kind, TK::Name(n) if n == "fn");
        if !self.at(TK::Function) && !is_fn_alias {
            return Err(LusaError::new(
                E_PARSE,
                Some(self.peek().span),
                format!(
                    "expected `function` after `async`, found {}",
                    self.peek().desc()
                ),
            )
            .help("write `async function name(...) ... end`"));
        }
        // The shared function parser consumes one leading token before the
        // name, so `fn` plays the same role as the `function` keyword here.
        let mut st = self.parse_func_st_inner(true)?;
        st.span = merge(async_kw.span, st.span);
        Ok(st)
    }

    fn parse_func_st_inner(&mut self, is_async: bool) -> LResult<St> {
        let kw = self.bump();
        let (n0, sp0) = self.expect_name()?;
        let mut target = expr_var(n0, sp0);
        let mut method_name: Option<String> = None;
        loop {
            if self.at(TK::Colon) {
                self.bump();
                let (m, _) = self.expect_member_name()?;
                method_name = Some(m);
                break;
            } else if self.at(TK::Dot) || self.at(TK::ColonColon) {
                self.bump();
                let (n, nsp) = self.expect_member_name()?;
                target = expr_field(target, n, merge(sp0, nsp));
            } else {
                break;
            }
        }
        let mut params = Vec::new();
        if method_name.is_some() {
            params.push("self".to_string());
        }
        let mut func = self.parse_funclit_from(params, is_async)?;
        if let Some(m) = method_name {
            target = expr_field(target, m, sp0);
        }
        func.span_body = merge(kw.span, func.span_body);
        Ok(St {
            kind: StKind::FuncSt {
                target,
                func,
                entry: false,
                global_allocator: false,
                link_section: None,
                interrupt: None,
            },
            span: kw.span,
        })
    }

    /// parses `(params) body end` â€” the `function` keyword and optional name were consumed.
    fn parse_funclit(&mut self) -> LResult<FuncLit> {
        self.parse_funclit_from(Vec::new(), false)
    }

    fn parse_funclit_from(&mut self, mut params: Vec<String>, is_async: bool) -> LResult<FuncLit> {
        self.expect(TK::LParen, "`(` to open the parameter list")?;
        let mut has_vararg = false;
        if !self.at(TK::RParen) {
            loop {
                if self.at(TK::DotDotDot) {
                    self.bump();
                    has_vararg = true;
                    break;
                }
                let (n, _) = self.expect_name()?;
                if self.eat(TK::Colon) {
                    let _ = self.parse_type_annotation()?;
                }
                params.push(n);
                if !self.eat(TK::Comma) {
                    break;
                }
            }
        }
        self.expect(TK::RParen, "`)`")?;
        if self.eat(TK::Colon) {
            let _ = self.parse_type_annotation()?;
        }
        let body = self.parse_block()?;
        let end = self.expect(TK::End, "`end` to close the `function`")?;
        Ok(FuncLit { params, has_vararg, is_async, body, span_body: end.span })
    }

    // ===== expressions =====

    fn parse_expr(&mut self) -> LResult<Expr> {
        self.subexpr(1)
    }

    fn parse_exprlist(&mut self) -> LResult<Vec<Expr>> {
        let mut out = vec![self.parse_expr()?];
        while self.eat(TK::Comma) {
            out.push(self.parse_expr()?);
        }
        Ok(out)
    }

    fn bin_from(&self, k: &TK) -> Option<BinOp> {
        Some(match k {
            TK::Plus => BinOp::Add,
            TK::Minus => BinOp::Sub,
            TK::Star => BinOp::Mul,
            TK::Slash => BinOp::Div,
            TK::FloorDiv => BinOp::FloorDiv,
            TK::Percent => BinOp::Mod,
            TK::Caret => BinOp::Pow,
            TK::Eq => BinOp::Eq,
            TK::Ne => BinOp::Ne,
            TK::Lt => BinOp::Lt,
            TK::Gt => BinOp::Gt,
            TK::Le => BinOp::Le,
            TK::Ge => BinOp::Ge,
            TK::DotDot => BinOp::Concat,
            _ => return None,
        })
    }

    fn subexpr(&mut self, limit: u8) -> LResult<Expr> {
        let mut lhs = self.parse_prefix()?;
        loop {
            let tk = self.peek().clone();
            // A `*` at the start of a line is a dereference, not a
            // continuation of a multiplication.
            if tk.kind == TK::StarLine {
                break;
            }
            let (prec, right) = match tk.kind {
                TK::Or => (PREC_OR, false),
                TK::And => (PREC_AND, false),
                TK::Ne | TK::Eq | TK::Lt | TK::Gt | TK::Le | TK::Ge => (PREC_CMP, false),
                TK::DotDot => (PREC_CONCAT, true),
                TK::Plus | TK::Minus => (PREC_ADD, false),
                TK::Star | TK::Slash | TK::FloorDiv | TK::Percent => (PREC_MUL, false),
                TK::Caret => (PREC_POW, true),
                _ => break,
            };
            if (prec as i32) < (limit as i32) {
                break;
            }
            self.bump();
            if tk.kind == TK::Or || tk.kind == TK::And {
                let op = if tk.kind == TK::And { LogOp::And } else { LogOp::Or };
                let r = self.subexpr(prec)?;
                let span = merge(lhs.span, r.span);
                lhs = Expr {
                    kind: ExprKind::Log { op, l: Box::new(lhs), r: Box::new(r) },
                    span,
                };
                continue;
            }
            let op = self.bin_from(&tk.kind).unwrap();
            let rprec = if right { prec } else { prec + 1 };
            let r = self.subexpr(rprec)?;
            let span = merge(lhs.span, r.span);
            lhs = Expr {
                kind: ExprKind::Bin { op, l: Box::new(lhs), r: Box::new(r) },
                span,
            };
        }
        Ok(lhs)
    }

    /// unary operators then primary with suffixes.
    fn parse_prefix(&mut self) -> LResult<Expr> {
        let tk = self.peek().clone();
        match tk.kind {
            TK::Await => {
                self.bump();
                // `await` binds tighter than binary operators; `.await` is the
                // postfix spelling and is handled in the suffix loop.
                let e = self.subexpr(PREC_UNARY)?;
                let span = merge(tk.span, e.span);
                let mut ex = Expr {
                    kind: ExprKind::Await { value: Box::new(e) },
                    span,
                };
                ex = self.parse_suffixes_applicable(ex)?;
                Ok(ex)
            }
            TK::Not
            | TK::Minus
            | TK::Len
            | TK::Star
            | TK::StarLine
            | TK::Amp
            | TK::Borrow
            | TK::Clone
            | TK::Move => {
                let op = tk.kind.clone();
                self.bump();
                let e = self.subexpr(PREC_UNARY)?;
                let span = merge(tk.span, e.span);
                let kind = match op {
                    TK::Not => ExprKind::Un { op: UnOp::Not, e: Box::new(e) },
                    TK::Minus => ExprKind::Un { op: UnOp::Neg, e: Box::new(e) },
                    TK::Len => ExprKind::Un { op: UnOp::Len, e: Box::new(e) },
                    TK::Star | TK::StarLine => ExprKind::Un { op: UnOp::Deref, e: Box::new(e) },
                    TK::Amp => ExprKind::AddrOf(Box::new(e)),
                    TK::Borrow => ExprKind::Borrow { e: Box::new(e), is_mut: false },
                    TK::Move => ExprKind::Move { e: Box::new(e) },
                    _ => ExprKind::Clone { e: Box::new(e) },
                };
                let mut ex = Expr { kind, span };
                ex = self.parse_suffixes_applicable(ex)?;
                Ok(ex)
            }
            _ => {
                let prim = self.parse_primary()?;
                self.parse_suffixes_applicable(prim)
            }
        }
    }

    /// suffixes (for the leftmost expression only â€” calls, fields, indexes)
    fn parse_suffixes_applicable(&mut self, mut e: Expr) -> LResult<Expr> {
        loop {
            let t = self.peek().clone();
            match t.kind {
                TK::Dot | TK::ColonColon => {
                    self.bump();
                    if self.at(TK::Await)
                        && self.peek_at(1).kind != TK::LParen
                    {
                        // Postfix `task.await` / `task::await`.  The call form
                        // `task.await(t)` keeps `await` as a member name.
                        let tok = self.bump();
                        let span = merge(e.span, tok.span);
                        e = Expr {
                            kind: ExprKind::Await { value: Box::new(e) },
                            span,
                        };
                        continue;
                    }
                    let (n, nsp) = self.expect_member_name()?;
                    let span = merge(e.span, nsp);
                    e = expr_field(e, n, span);
                }
                TK::LBracket => {
                    self.bump();
                    let key = self.parse_expr()?;
                    self.expect(TK::RBracket, "`]`")?;
                    let span = e.span;
                    e = Expr {
                        kind: ExprKind::Index { obj: Box::new(e), key: Box::new(key) },
                        span,
                    };
                }
                TK::Colon => {
                    self.bump();
                    let (m, msp) = self.expect_member_name()?;
                    self.expect(TK::LParen, "`(` to call the method")?;
                    let args = self.parse_args()?;
                    let span = merge(e.span, msp);
                    if let Some(builtin) = volatile_builtin(&m) {
                        let mut call_args = Vec::with_capacity(args.len() + 1);
                        call_args.push(e);
                        call_args.extend(args);
                        e = Expr {
                            kind: ExprKind::Call {
                                callee: Box::new(expr_var(builtin.to_string(), span)),
                                args: call_args,
                                span_args_end: span,
                            },
                            span,
                        };
                    } else {
                        e = Expr {
                            kind: ExprKind::Method { obj: Box::new(e), name: Box::new(m), args, span_args_end: span },
                            span,
                        };
                    }
                }
                TK::LParen => {
                    self.bump();
                    let args = self.parse_args()?;
                    let span = merge(e.span, self.toks[self.pos - 1].span);
                    let builtin = match &e.kind {
                        ExprKind::Field { name, .. } => volatile_builtin(name),
                        _ => None,
                    };
                    if let Some(builtin) = builtin {
                        let obj = match e.kind {
                            ExprKind::Field { obj, .. } => obj,
                            _ => unreachable!("checked above"),
                        };
                        let mut call_args = Vec::with_capacity(args.len() + 1);
                        call_args.push(*obj);
                        call_args.extend(args);
                        e = Expr {
                            kind: ExprKind::Call {
                                callee: Box::new(expr_var(builtin.to_string(), span)),
                                args: call_args,
                                span_args_end: span,
                            },
                            span,
                        };
                    } else {
                        e = Expr {
                            kind: ExprKind::Call { callee: Box::new(e), args, span_args_end: span },
                            span,
                        };
                    }
                }
                TK::LBrace => {
                    if let ExprKind::Var(name) = &e.kind {
                        if self.structs.contains(name.as_str()) {
                            let name = (**name).clone();
                            let span = e.span;
                            e = self.parse_struct_lit(name, span)?;
                            continue;
                        }
                    }
                    let arg = self.parse_primary()?; // table literal
                    let span = merge(e.span, arg.span);
                    e = Expr {
                        kind: ExprKind::Call { callee: Box::new(e), args: vec![arg], span_args_end: span },
                        span,
                    };
                }
                TK::Question => {
                    self.bump();
                    let span = merge(e.span, t.span);
                    e = Expr { kind: ExprKind::Try { value: Box::new(e) }, span };
                }
                _ => break,
            }
        }
        Ok(e)
    }

    /// Parses `asm!("...")` after the `asm` name has been consumed.
    ///
    /// The macro syntax desugars to `asm("...")`, so the interpreter, VM, and
    /// C11 backend share one call shape, one argument check, and one `unsafe`
    /// gate.  The C emitter turns the call into `LUSA_ASM("...")`.
    fn parse_asm(&mut self, name_span: Span) -> LResult<Expr> {
        self.expect(TK::Bang, "`!` after `asm`")?;
        self.expect(TK::LParen, "`(` after `asm!`")?;

        let (text, text_span) = match self.peek().kind.clone() {
            TK::Str(segs) => {
                let span = self.peek().span;
                self.bump();
                let mut text = String::new();
                for seg in segs {
                    match seg {
                        StrSeg::Lit(lit) => text.push_str(&lit),
                        StrSeg::Expr(_) => {
                            return Err(LusaError::new(
                                E_PARSE,
                                Some(span),
                                "`asm!` requires a plain string literal (no interpolation)",
                            ));
                        }
                    }
                }
                (text, span)
            }
            TK::RParen => {
                return Err(LusaError::new(
                    E_PARSE,
                    Some(self.peek().span),
                    "`asm!` requires an assembly string",
                )
                .help("write `asm!(\"nop\")` for a no-op instruction"));
            }
            _ => {
                let t = self.peek().clone();
                return Err(LusaError::new(
                    E_PARSE,
                    Some(t.span),
                    format!("`asm!` expects a string literal, found {}", t.desc()),
                ));
            }
        };
        validate_asm_text(&text, text_span)?;

        let close_span = self.peek().span;
        self.expect(TK::RParen, "`)` to close `asm!`")?;
        let span = merge(name_span, close_span);
        let arg = Expr { kind: ExprKind::Str(vec![StrAstSeg::Lit(text)]), span: text_span };
        Ok(Expr {
            kind: ExprKind::Call {
                callee: Box::new(expr_var("asm".to_string(), name_span)),
                args: vec![arg],
                span_args_end: span,
            },
            span,
        })
    }

    /// parse primary expression (literals, names, parens, tables, functions)
    fn parse_primary(&mut self) -> LResult<Expr> {
        let tk = self.bump();
        let kind = match tk.kind.clone() {
            TK::Nil => ExprKind::Nil,
            TK::True => ExprKind::True,
            TK::False => ExprKind::False,
            TK::Num(n) => ExprKind::Num(n),
            TK::Int(s) => ExprKind::Int(s),
            TK::Str(segs) => self.parse_str_segs(segs, tk.span)?,
            TK::DotDotDot => ExprKind::Vararg,
            TK::Name(n) => {
                if n == "asm" && self.at(TK::Bang) {
                    return self.parse_asm(tk.span);
                }
                if n == "None" && !self.at(TK::LParen) {
                    // Bare `None` is sugar for the zero-argument constructor;
                    // the call keeps the value fresh for both dynamic engines.
                    let callee = expr_var("None".to_string(), tk.span);
                    return Ok(Expr {
                        kind: ExprKind::Call {
                            callee: Box::new(callee),
                            args: Vec::new(),
                            span_args_end: tk.span,
                        },
                        span: tk.span,
                    });
                }
                ExprKind::Var(Box::new(n))
            }
            TK::LParen => {
                let e = self.parse_expr()?;
                self.expect(TK::RParen, "`)`")?;
                ExprKind::Paren(Box::new(e))
            }
            TK::LBrace => return self.parse_table_ctor(tk.span),
            TK::LBracket => {
                let mut elems = Vec::new();
                if !self.at(TK::RBracket) {
                    loop {
                        elems.push(self.parse_expr()?);
                        if !self.eat(TK::Comma) {
                            break;
                        }
                        if self.at(TK::RBracket) {
                            break;
                        }
                    }
                }
                self.expect(TK::RBracket, "`]` to close the array literal")?;
                ExprKind::ArrayLit(elems)
            }
            TK::Function => {
                let f = self.parse_funclit()?;
                ExprKind::Func(f)
            }
            TK::Async => {
                let is_fn_alias =
                    matches!(&self.peek().kind, TK::Name(n) if n == "fn");
                if !self.at(TK::Function) && !is_fn_alias {
                    return Err(LusaError::new(
                        E_PARSE,
                        Some(self.peek().span),
                        format!(
                            "expected `function` after `async`, found {}",
                            self.peek().desc()
                        ),
                    )
                    .help("write `async function(...) ... end`"));
                }
                self.bump();
                let f = self.parse_funclit_from(Vec::new(), true)?;
                ExprKind::Func(f)
            }
            other => {
                let _ = other;
                return Err(LusaError::new(
                    E_PARSE,
                    Some(tk.span),
                    format!("unexpected {} in an expression", tk.desc()),
                )
                .help("check for a missing operator, comma, or `end` here"));
            }
        };
        Ok(Expr { kind, span: tk.span })
    }

    fn parse_str_segs(&mut self, segs: Vec<StrSeg>, span: Span) -> LResult<ExprKind> {
        let mut out = Vec::new();
        for s in segs {
            match s {
                StrSeg::Lit(l) => out.push(StrAstSeg::Lit(l)),
                StrSeg::Expr(toks) => {
                    let mut toks = toks;
                    toks.push(Token {
                        kind: TK::Eof,
                        span: Span::new(span.end, span.end),
                    });
                    let mut sub = Parser::new(toks);
                    let e = sub.parse_expr()?;
                    if !sub.at(TK::Eof) {
                        let t = sub.peek().clone();
                        return Err(LusaError::new(
                            E_PARSE,
                            Some(merge(t.span, span)),
                            "unexpected token inside `{...}` interpolation",
                        ));
                    }
                    out.push(StrAstSeg::Interp(Box::new(e)));
                }
            }
        }
        Ok(ExprKind::Str(out))
    }

    /// parses argument list after `(` has been consumed (up to and including `)`)
    fn parse_args(&mut self) -> LResult<Vec<Expr>> {
        let mut args = Vec::new();
        if !self.at(TK::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.eat(TK::Comma) {
                    break;
                }
            }
        }
        self.expect(TK::RParen, "`)`")?;
        Ok(args)
    }

    fn parse_table_ctor(&mut self, open_span: Span) -> LResult<Expr> {
        let mut entries: Vec<TableEntry> = Vec::new();
        loop {
            if self.at(TK::RBrace) {
                self.bump();
                break;
            }
            let start = self.peek().span;
            if self.at(TK::LBracket) {
                self.bump();
                let key = self.parse_expr()?;
                self.expect(TK::RBracket, "`]`")?;
                self.expect(TK::Assign, "`=`")?;
                let value = self.parse_expr()?;
                entries.push(TableEntry { key: Some(key), value, span: start });
            } else if matches!(self.peek().kind, TK::Name(_)) && self.peek_at(1).kind == TK::Assign {
                let (n, nsp) = self.expect_name()?;
                self.bump(); // =
                let value = self.parse_expr()?;
                let key = Expr {
                    kind: ExprKind::Str(vec![StrAstSeg::Lit(n)]),
                    span: nsp,
                };
                entries.push(TableEntry { key: Some(key), value, span: start });
            } else {
                let value = self.parse_expr()?;
                entries.push(TableEntry { key: None, value, span: start });
            }
            if !self.eat(TK::Comma) {
                self.expect(TK::RBrace, "`}`")?;
                break;
            }
        }
        let close = self.toks[self.pos - 1].span;
        Ok(Expr { kind: ExprKind::Table(entries), span: merge(open_span, close) })
    }
}

