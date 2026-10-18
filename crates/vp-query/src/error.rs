use serde::Serialize;

use crate::ast::Span;

/// A parse, analysis or compile error. `kind` is serialized with a stable `code`
/// so the UI can show a localized message and underline `span`.
#[derive(Debug, Clone, PartialEq, Serialize, thiserror::Error)]
#[error("{kind} (at {}..{})", span.start, span.end)]
pub struct QueryError {
    pub kind: ErrorKind,
    pub span: Span,
}

impl QueryError {
    pub fn new(kind: ErrorKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, thiserror::Error)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ErrorKind {
    #[error("expected a condition")]
    ExpectedCondition,
    #[error("unclosed parenthesis")]
    UnclosedParen,
    #[error("expected '(' after '{name}'")]
    ExpectedOpenParen { name: String },
    #[error("unexpected character '{ch}'")]
    UnexpectedChar { ch: char },
    #[error("empty parameter")]
    EmptyParam,
    #[error("unterminated quoted string")]
    UnterminatedString,
    #[error("expected AND or OR between conditions")]
    ExpectedConnective,
    #[error("unknown condition '{name}'")]
    UnknownField { name: String },
    #[error("operator '{op}' cannot be used with '{field}'")]
    OperatorNotAllowed { op: String, field: String },
    #[error("only one operator may be given")]
    MultipleOperators,
    #[error("'{op}' takes {expected} value(s), got {got}")]
    WrongArity { op: String, expected: String, got: usize },
    #[error("'{text}' is not a valid {expected}")]
    InvalidValue { expected: String, text: String },
    #[error("sorting must be joined with AND, not placed inside OR or NOT")]
    SortNotTopLevel,
    #[error("sort takes exactly one condition name")]
    SortArity,
}
