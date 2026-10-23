//! Recursion-descent parser with Lua-style precedence.
//!
//!   or < and < comparisons < .. (right) < + - < * / // % < unary < ^ (right)

use crate::ast::*;
use crate::error::{LResult, LusaError, Span, E_PARSE};
use crate::lexer;
use crate::tokens::{StrSeg, Token, TokenKind as TK};

pub fn parse(src: &str) -> LResult<Block> {
    let toks = lexer::lex(src)?;
    Parser::new(toks).parse_file()
}

const STMT_ENDS: &[TK] = &[TK::End, TK::Else, TK::Elseif, TK::Until, TK::Eof];

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

impl Parser {
    fn new(toks: Vec<Token>) -> Self {
        Parser { toks, pos: 0 }
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

    // ===== statements =====

    fn parse_file(&mut self) -> LResult<Block> {
        self.parse_block()
    }

    fn parse_block(&mut self) -> LResult<Block> {
        let mut block = Vec::new();
        loop {
            if STMT_ENDS.contains(&self.peek().kind) {
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
            TK::Local => self.parse_local_st()?,
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
            TK::Struct => self.parse_struct_st()?,
            TK::Function => self.parse_func_st()?,
            TK::Break => {
                self.bump();
                St { kind: StKind::Break, span: tk.span }
            }
            TK::Borrow | TK::Clone | TK::LParen | TK::Num(_) | TK::Int(_) | TK::Str(_)
            | TK::Nil | TK::True | TK::False | TK::Minus | TK::Not | TK::Len
            | TK::DotDotDot | TK::LBrace => {
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
            return Ok(St { kind: StKind::Assign { targets, values }, span: merge(start, end) });
        }
        // otherwise it must be a call/method expression statement
        match &first.kind {
            ExprKind::Call { .. } | ExprKind::Method { .. } => {}
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
        Ok(name)
    }

    fn parse_struct_st(&mut self) -> LResult<St> {
        let kw = self.bump();
        let (name, name_span) = self.expect_name()?;
        self.expect(TK::LBrace, "`{` to open the struct fields")?;
        let mut fields = Vec::new();
        while !self.at(TK::RBrace) {
            let (fname, fspan) = self.expect_name()?;
            self.expect(TK::Colon, "`:` after the field name")?;
            let ty = self.parse_type_annotation()?;
            fields.push(StructField { name: fname, ty, span: fspan });
            if !self.eat(TK::Comma) && !self.at(TK::Semi) {
                break;
            }
            self.eat(TK::Semi);
        }
        self.expect(TK::RBrace, "`}` to close the struct")?;
        let span = merge(kw.span, name_span);
        Ok(St { kind: StKind::Struct { name, fields }, span })
    }

    fn parse_local_st(&mut self) -> LResult<St> {
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
        Ok(St { kind: StKind::Local { targets, values, annotations }, span: kw.span })
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
        let kw = self.bump();
        let (n0, sp0) = self.expect_name()?;
        let mut target = expr_var(n0, sp0);
        let mut method_name: Option<String> = None;
        loop {
            if self.at(TK::Colon) {
                self.bump();
                let (m, _) = self.expect_name()?;
                method_name = Some(m);
                break;
            } else if self.at(TK::Dot) {
                self.bump();
                let (n, nsp) = self.expect_name()?;
                target = expr_field(target, n, merge(sp0, nsp));
            } else {
                break;
            }
        }
        let mut params = Vec::new();
        if method_name.is_some() {
            params.push("self".to_string());
        }
        let mut func = self.parse_funclit_from(params)?;
        if let Some(m) = method_name {
            target = expr_field(target, m, sp0);
        }
        func.span_body = merge(kw.span, func.span_body);
        Ok(St { kind: StKind::FuncSt { target, func }, span: kw.span })
    }

    /// parses `(params) body end` â€” the `function` keyword and optional name were consumed.
    fn parse_funclit(&mut self) -> LResult<FuncLit> {
        self.parse_funclit_from(Vec::new())
    }

    fn parse_funclit_from(&mut self, mut params: Vec<String>) -> LResult<FuncLit> {
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
        Ok(FuncLit { params, has_vararg, body, span_body: end.span })
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
            TK::Not | TK::Minus | TK::Len | TK::Star | TK::StarLine | TK::Borrow | TK::Clone => {
                let op = tk.kind.clone();
                self.bump();
                let e = self.subexpr(PREC_UNARY)?;
                let span = merge(tk.span, e.span);
                let kind = match op {
                    TK::Not => ExprKind::Un { op: UnOp::Not, e: Box::new(e) },
                    TK::Minus => ExprKind::Un { op: UnOp::Neg, e: Box::new(e) },
                    TK::Len => ExprKind::Un { op: UnOp::Len, e: Box::new(e) },
                    TK::Star | TK::StarLine => ExprKind::Un { op: UnOp::Deref, e: Box::new(e) },
                    TK::Borrow => ExprKind::Borrow { e: Box::new(e), is_mut: false },
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
                TK::Dot => {
                    self.bump();
                    let (n, nsp) = self.expect_name()?;
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
                    let (m, msp) = self.expect_name()?;
                    self.expect(TK::LParen, "`(` to call the method")?;
                    let args = self.parse_args()?;
                    let span = merge(e.span, msp);
                    e = Expr {
                        kind: ExprKind::Method { obj: Box::new(e), name: Box::new(m), args, span_args_end: span },
                        span,
                    };
                }
                TK::LParen => {
                    self.bump();
                    let args = self.parse_args()?;
                    let span = merge(e.span, self.toks[self.pos - 1].span);
                    e = Expr {
                        kind: ExprKind::Call { callee: Box::new(e), args, span_args_end: span },
                        span,
                    };
                }
                TK::LBrace => {
                    let arg = self.parse_primary()?; // table literal
                    let span = merge(e.span, arg.span);
                    e = Expr {
                        kind: ExprKind::Call { callee: Box::new(e), args: vec![arg], span_args_end: span },
                        span,
                    };
                }
                _ => break,
            }
        }
        Ok(e)
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
            TK::Name(n) => ExprKind::Var(Box::new(n)),
            TK::LParen => {
                let e = self.parse_expr()?;
                self.expect(TK::RParen, "`)`")?;
                ExprKind::Paren(Box::new(e))
            }
            TK::LBrace => return self.parse_table_ctor(tk.span),
            TK::Function => {
                let f = self.parse_funclit()?;
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

