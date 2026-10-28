//! Abstract syntax tree for Lusa.

use crate::error::Span;

pub type Block = Vec<St>;

/// Splits a canonical `array<T,N>` annotation into its element spelling and
/// length.  Both parsers emit this spelling, so the dynamic engines and the
/// self-hosted C backend agree on the representation.
pub fn array_annotation(ann: &str) -> Option<(&str, usize)> {
    let rest = ann.strip_prefix("array<")?.strip_suffix('>')?;
    let (element, size) = rest.rsplit_once(',')?;
    let length = size.parse().ok()?;
    Some((element, length))
}

/// Splits a canonical `vec<T>` annotation into its element spelling.  Vec is
/// simulated as a 0-based table by the dynamic engines and lowered to a
/// growable buffer by the C backend.
pub fn vec_annotation(ann: &str) -> Option<&str> {
    ann.strip_prefix("vec<")?.strip_suffix('>')
}

/// Splits a canonical `box<T>` annotation into its element spelling.  The
/// dynamic engines tag boxed values so `*b` can read and write the pointee
/// without the raw-pointer `unsafe` gate.
pub fn box_annotation(ann: &str) -> Option<&str> {
    ann.strip_prefix("box<")?.strip_suffix('>')
}

/// Splits a canonical `result<T,E>` annotation into its success and error
/// spellings.  The payload is separated by the first top-level comma so nested
/// generic types such as `result<box<i32>,string>` stay intact.
pub fn result_annotation(ann: &str) -> Option<(&str, &str)> {
    let rest = ann.strip_prefix("result<")?.strip_suffix('>')?;
    let mut depth = 0usize;
    for (i, ch) in rest.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let ok = &rest[..i];
                let err = &rest[i + 1..];
                if ok.is_empty() || err.is_empty() {
                    return None;
                }
                return Some((ok, err));
            }
            _ => {}
        }
    }
    None
}

/// Splits a canonical `option<T>` annotation into its payload spelling.
pub fn option_annotation(ann: &str) -> Option<&str> {
    ann.strip_prefix("option<")?.strip_suffix('>')
}

/// Splits a canonical `task<T>` annotation into the awaited result spelling.
pub fn task_annotation(ann: &str) -> Option<&str> {
    ann.strip_prefix("task<")?.strip_suffix('>')
}

#[derive(Clone, Debug)]
pub struct St {
    pub kind: StKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StKind {
    /// `local` declarations
    Local {
        targets: Vec<String>,
        values: Vec<Expr>,
        /// static type annotations (`u32`, `*mut u32`, struct names) captured
        /// for the dynamic engines' safe emulation layer.
        annotations: Vec<Option<String>>,
        /// `#[link_section = "..."]` on a top-level declaration.  The C
        /// backend promotes such a local to a file-scope global.
        link_section: Option<String>,
    },
    /// `name = value` and `t.a = value` assignments
    Assign {
        targets: Vec<Expr>,
        values: Vec<Expr>,
        /// `#[link_section = "..."]` on a top-level implicit global.
        link_section: Option<String>,
    },
    /// `struct Name { field: type, ... }` declaration.  The native backend
    /// lowers it to a C struct; the dynamic engines simulate it as a
    /// fixed-field buffer.
    ///
    /// `packed` and `align` carry `#[packed]` / `#[repr(align(N))]`
    /// representation attributes.  The dynamic engines ignore the physical
    /// layout; only the C11 backend needs it.
    Struct {
        name: String,
        fields: Vec<StructField>,
        packed: bool,
        align: Option<u32>,
    },
    /// `unsafe do ... end` block (raw pointers and intrinsics only).
    Unsafe { body: Block },
    /// expression statement (usually a call)
    ExprSt(Expr),
    If {
        branches: Vec<(Expr, Block)>,
        else_block: Option<Block>,
    },
    While { cond: Expr, body: Block },
    Repeat {
        body: Block,
        cond: Expr, // evaluated with body vars in scope
    },
    ForNum {
        var: String,
        start: Expr,
        limit: Expr,
        step: Option<Expr>,
        body: Block,
    },
    ForIter {
        vars: Vec<String>,
        values: Vec<Expr>,
        body: Block,
    },
    /// `function name/path(args) body end` sugar
    FuncSt {
        target: Expr,
        func: FuncLit,
        /// `#[entry]`: emit this function as the program entry symbol.
        entry: bool,
        /// `#[global_allocator]`: use this function as the allocator or
        /// deallocator half of the global heap contract.
        global_allocator: bool,
        /// `#[link_section = "..."]` on the function.
        link_section: Option<String>,
    },
    /// `#[no_std]`: file-level bare-metal mode marker.  The dynamic engines
    /// accept and ignore it; the C backend disables the hosted runtime.
    NoStd,
    /// `do ... end` standalone block (new scope)
    Do { body: Block },
    Return { values: Vec<Expr>, span: Span },
    Break,
}

/// String literal segments — literal text or an interpolated `{expr}` AST node.
#[derive(Clone, Debug)]
pub enum StrAstSeg {
    Lit(String),
    Interp(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    Nil,
    True,
    False,
    Num(f64),
    /// Exact pure-integer literal (decimal or `0x` hex digits preserved).
    Int(String),
    Str(Vec<StrAstSeg>),
    /// plain variable / global
    Var(Box<String>),
    /// t.k
    Field { obj: Box<Expr>, name: Box<String> },
    /// t[expr]
    Index { obj: Box<Expr>, key: Box<Expr> },
    /// call(args...)
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span_args_end: Span,
    },
    /// obj:name(args)
    Method {
        obj: Box<Expr>,
        name: Box<String>,
        args: Vec<Expr>,
        span_args_end: Span,
    },
    Table(Vec<TableEntry>),
    /// `[a, b, c]` fixed-size array literal.  The native backend emits a C
    /// compound literal; the dynamic engines build a 0-based table.
    ArrayLit(Vec<Expr>),
    /// `&place` — address of a variable, field, or index expression.
    AddrOf(Box<Expr>),
    /// `Point{x: 10, y: 20}` static struct literal.
    StructLit {
        name: String,
        fields: Vec<StructLitField>,
    },
    Func(FuncLit),
    /// ordinary binary operators
    Bin { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    /// `a and b` / `a or b` (kept distinct for short-circuit eval)
    Log { op: LogOp, l: Box<Expr>, r: Box<Expr> },
    Un { op: UnOp, e: Box<Expr> },
    /// `borrow expr` / `clone expr` / `move expr` / `borrow mut expr`
    Borrow { e: Box<Expr>, is_mut: bool },
    Clone { e: Box<Expr> },
    /// Explicit ownership transfer (`move expr`).
    Move { e: Box<Expr> },
    /// `...`
    Vararg,
    /// (expr) — truncates multiple returns to one
    Paren(Box<Expr>),
    /// `expr?` — propagates `Err`/`None` from the current function.
    Try { value: Box<Expr> },
    /// `await expr` / `expr.await` — drives a task until it settles.
    Await { value: Box<Expr> },
}

#[derive(Clone, Debug)]
pub struct TableEntry {
    pub key: Option<Expr>,          // None for positional
    pub value: Expr,
    pub span: Span,
}

/// A field of a static `struct` declaration.
#[derive(Clone, Debug)]
pub struct StructField {
    pub name: String,
    /// type annotation string, e.g. `u32` or `*mut u32`.
    pub ty: String,
    pub span: Span,
}

/// One `name: value` entry of a struct literal.
#[derive(Clone, Debug)]
pub struct StructLitField {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FuncLit {
    pub params: Vec<String>,
    pub has_vararg: bool,
    pub is_async: bool,
    pub body: Block,
    pub span_body: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Concat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOp {
    And,
    Or,
}

#[derive(Clone, Copy, Debug)]
pub enum UnOp {
    Neg,
    Not,
    Len,
    /// raw pointer dereference - `*ptr`
    Deref,
}
