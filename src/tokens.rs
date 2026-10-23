//! Lexical tokens for Lusa.

use crate::error::Span;

#[derive(Clone, Debug, PartialEq)]
pub enum StrSeg {
    /// literal text (escapes already resolved)
    Lit(String),
    /// interpolated expression tokens between `\{` and `}` (already re-lexed)
    Expr(Vec<Token>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // literals
    Name(String),
    /// Floating-point literal (or a numeric token that must be treated as f64).
    Num(f64),
    /// Exact pure-integer literal.  The digits are kept verbatim so the C11
    /// backend can emit an exact `i128` instead of round-tripping through f64.
    Int(String),
    Str(Vec<StrSeg>),
    Nil,
    True,
    False,
    // keywords
    Local,
    Function,
    End,
    If,
    Then,
    Elseif,
    Else,
    For,
    In,
    Do,
    While,
    Repeat,
    Until,
    Unsafe,
    Struct,
    Return,
    Break,
    And,
    Or,
    Not,
    Borrow,
    Clone,
    Move,
    // operators
    Plus,
    Minus,
    Star,
    Slash,
    FloorDiv,
    Percent,
    Caret,
    DotDot,
    Dot,
    DotDotDot,
    Len, // `#`
    /// `*` at the start of a line - begins a pointer dereference instead of
    /// continuing a multiplication.
    StarLine,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Assign,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn desc(&self) -> String {
        match &self.kind {
            TokenKind::Name(n) => format!("name `{}`", n),
            TokenKind::Num(n) => format!("number `{}`", crate::value::format_number(*n)),
            TokenKind::Int(s) => format!("integer `{s}`"),
            TokenKind::Str(_) => "string literal".into(),
            TokenKind::Nil => "nil".into(),
            TokenKind::True => "true".into(),
            TokenKind::False => "false".into(),
            TokenKind::Local => "`local`".into(),
            TokenKind::Function => "`function`".into(),
            TokenKind::End => "`end`".into(),
            TokenKind::If => "`if`".into(),
            TokenKind::Then => "`then`".into(),
            TokenKind::Elseif => "`elseif`".into(),
            TokenKind::Else => "`else`".into(),
            TokenKind::For => "`for`".into(),
            TokenKind::In => "`in`".into(),
            TokenKind::Do => "`do`".into(),
            TokenKind::While => "`while`".into(),
            TokenKind::Repeat => "`repeat`".into(),
            TokenKind::Until => "`until`".into(),
            TokenKind::Unsafe => "`unsafe`".into(),
            TokenKind::Struct => "`struct`".into(),
            TokenKind::Return => "`return`".into(),
            TokenKind::Break => "`break`".into(),
            TokenKind::And => "`and`".into(),
            TokenKind::Or => "`or`".into(),
            TokenKind::Not => "`not`".into(),
            TokenKind::Borrow => "`borrow`".into(),
            TokenKind::Clone => "`clone`".into(),
            TokenKind::Move => "`move`".into(),
            TokenKind::Plus => "`+`".into(),
            TokenKind::Minus => "`-`".into(),
            TokenKind::Star => "`*`".into(),
            TokenKind::Slash => "`/`".into(),
            TokenKind::FloorDiv => "`//`".into(),
            TokenKind::Percent => "`%`".into(),
            TokenKind::Caret => "`^`".into(),
            TokenKind::DotDot => "`..`".into(),
            TokenKind::Dot => "`.`".into(),
            TokenKind::DotDotDot => "`...`".into(),
            TokenKind::Len => "`#`".into(),
            TokenKind::StarLine => "`*`".into(),
            TokenKind::Eq => "`==`".into(),
            TokenKind::Ne => "`~=`".into(),
            TokenKind::Lt => "`<`".into(),
            TokenKind::Gt => "`>`".into(),
            TokenKind::Le => "`<=`".into(),
            TokenKind::Ge => "`>=`".into(),
            TokenKind::Assign => "`=`".into(),
            TokenKind::LParen => "`(`".into(),
            TokenKind::RParen => "`)`".into(),
            TokenKind::LBrace => "`{`".into(),
            TokenKind::RBrace => "`}`".into(),
            TokenKind::LBracket => "`[`".into(),
            TokenKind::RBracket => "`]`".into(),
            TokenKind::Comma => "`,`".into(),
            TokenKind::Semi => "`;`".into(),
            TokenKind::Colon => "`:`".into(),
            TokenKind::Eof => "end of input".into(),
        }
    }
}
