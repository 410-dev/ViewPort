//! Untyped syntax tree produced by the parser. Names and parameters are still raw text.

use serde::Serialize;

/// Byte range in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn join(self, other: Span) -> Span {
        Span::new(self.start.min(other.start), self.end.max(other.end))
    }

    /// Converts to UTF-16 code unit offsets, which is what JavaScript string indices use.
    pub fn to_utf16(self, src: &str) -> Span {
        let conv = |b: usize| src[..b.min(src.len())].encode_utf16().count();
        Span::new(conv(self.start), conv(self.end))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Param {
    pub text: String,
    /// Quoted parameters are always values, never operators.
    pub quoted: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Call {
    /// Name with inner whitespace collapsed to single spaces.
    pub name: String,
    pub name_span: Span,
    pub params: Vec<Param>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExprKind {
    And { items: Vec<Expr> },
    Or { items: Vec<Expr> },
    Not { inner: Box<Expr> },
    Call(Call),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}
