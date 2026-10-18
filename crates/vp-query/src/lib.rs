//! ViewPort advanced search language.
//!
//! Pipeline: [`parse`] text → [`ast::Expr`] → [`analyze`] against a [`FieldRegistry`] →
//! locale-neutral [`Query`] (also the saved-query format) → [`compile`] to SQL, or
//! [`render`] back to text in any locale.

pub mod analyze;
pub mod ast;
pub mod compile;
pub mod error;
pub mod fields;
pub mod lang;
pub mod parser;
pub mod render;
pub mod value;

pub use analyze::{Filter, Pred, Query, Sort, analyze};
pub use compile::{CompileContext, Compiled, SchemaMap, SqlValue, compile};
pub use error::{ErrorKind, QueryError};
pub use fields::{FieldDef, FieldRegistry, FieldSource, FieldType};
pub use lang::{Op, SortDir};
pub use parser::parse;
pub use render::render;
pub use value::Value;

/// Parses and analyzes `src` in one step.
pub fn parse_query(src: &str, reg: &FieldRegistry) -> Result<Query, QueryError> {
    analyze(&parse(src)?, reg)
}
