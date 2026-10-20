//! Abstract syntax tree for Lusa.

use crate::error::Span;

pub type Block = Vec<St>;

#[derive(Clone, Debug)]
pub struct St {
    pub kind: StKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StKind {
    /// `local` declarations
    Local { targets: Vec<String>, values: Vec<Expr> },
    /// `name = value` and `t.a = value` assignments
    Assign { targets: Vec<Expr>, values: Vec<Expr> },
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
    FuncSt { target: Expr, func: FuncLit },
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
    Func(FuncLit),
    /// ordinary binary operators
    Bin { op: BinOp, l: Box<Expr>, r: Box<Expr> },
    /// `a and b` / `a or b` (kept distinct for short-circuit eval)
    Log { op: LogOp, l: Box<Expr>, r: Box<Expr> },
    Un { op: UnOp, e: Box<Expr> },
    /// `borrow expr` / `clone expr` / `borrow mut expr`
    Borrow { e: Box<Expr>, is_mut: bool },
    Clone { e: Box<Expr> },
    /// `...`
    Vararg,
    /// (expr) — truncates multiple returns to one
    Paren(Box<Expr>),
}

#[derive(Clone, Debug)]
pub struct TableEntry {
    pub key: Option<Expr>,          // None for positional
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FuncLit {
    pub params: Vec<String>,
    pub has_vararg: bool,
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
}
