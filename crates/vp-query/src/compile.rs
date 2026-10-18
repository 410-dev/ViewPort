//! Compiles a typed [`Query`] into SQLite `WHERE` / `ORDER BY` fragments with numbered parameters.
//!
//! Every predicate compiles to an expression that is never NULL, so negative operators
//! (`Ne`, `ContainsNone`, `NotExists`) and `NOT` are exact negations: items with no value
//! for a field match `점수(5:다름)`.

use serde::Serialize;

use crate::analyze::{Filter, Pred, Query};
use crate::ast::Span;
use crate::error::{ErrorKind, QueryError};
use crate::fields::{ExtStorage, FieldDef, FieldRegistry, FieldSource, FieldType};
use crate::lang::{Op, SortDir};
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct CompileContext {
    pub now_ms: i64,
    pub tz_offset_minutes: i32,
    /// Collation used for text sorting, e.g. a registered natural-order collation.
    pub text_collation: String,
    pub schema: SchemaMap,
}

impl Default for CompileContext {
    fn default() -> Self {
        Self { now_ms: 0, tz_offset_minutes: 0, text_collation: "NOCASE".into(), schema: SchemaMap::default() }
    }
}

/// Names of the extension-value table and columns, and the media id expression.
#[derive(Debug, Clone)]
pub struct SchemaMap {
    pub media_id: String,
    pub ext_table: String,
    pub ext_media_id: String,
    pub ext_field_id: String,
    pub ext_int: String,
    pub ext_real: String,
    pub ext_text: String,
}

impl Default for SchemaMap {
    fn default() -> Self {
        Self {
            media_id: "m.id".into(),
            ext_table: "ext_value".into(),
            ext_media_id: "media_id".into(),
            ext_field_id: "field_id".into(),
            ext_int: "int_val".into(),
            ext_real: "real_val".into(),
            ext_text: "text_val".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum SqlValue {
    Int(i64),
    Real(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// Boolean expression; `1` when the query has no filter.
    pub where_sql: String,
    /// Comma-separated sort terms, always ending with the media id as a tiebreaker.
    pub order_sql: String,
    /// Bound as `?1`, `?2`, … in both fragments.
    pub params: Vec<SqlValue>,
}

pub fn compile(query: &Query, reg: &FieldRegistry, ctx: &CompileContext) -> Result<Compiled, QueryError> {
    let mut c = Compiler { reg, ctx, params: Vec::new() };
    let where_sql = match &query.filter {
        Some(f) => c.filter(f)?,
        None => "1".into(),
    };
    // Successive stable sorts: the last one written is the most significant key.
    let mut terms = Vec::new();
    for sort in query.sorts.iter().rev() {
        let def = c.field(&sort.field)?;
        let key = c.sort_key(def, sort.dir);
        let dir = match sort.dir {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        };
        let collate = if def.ty.is_textual() { format!(" COLLATE {}", ctx.text_collation) } else { String::new() };
        // Items without a value always go last.
        terms.push(format!("({key}) IS NULL, {key}{collate} {dir}"));
    }
    terms.push(format!("{} ASC", ctx.schema.media_id));
    Ok(Compiled { where_sql, order_sql: terms.join(", "), params: c.params })
}

struct Compiler<'a> {
    reg: &'a FieldRegistry,
    ctx: &'a CompileContext,
    params: Vec<SqlValue>,
}

impl<'a> Compiler<'a> {
    fn bind(&mut self, v: SqlValue) -> String {
        self.params.push(v);
        format!("?{}", self.params.len())
    }

    fn field(&self, key: &str) -> Result<&'a FieldDef, QueryError> {
        self.reg
            .by_key(key)
            .ok_or_else(|| QueryError::new(ErrorKind::UnknownField { name: key.into() }, Span::default()))
    }

    fn filter(&mut self, f: &Filter) -> Result<String, QueryError> {
        Ok(match f {
            Filter::And { items } => self.join(items, " AND ")?,
            Filter::Or { items } => self.join(items, " OR ")?,
            Filter::Not { inner } => format!("NOT ({})", self.filter(inner)?),
            Filter::Pred(p) => self.pred(p)?,
        })
    }

    fn join(&mut self, items: &[Filter], sep: &str) -> Result<String, QueryError> {
        let parts = items.iter().map(|f| self.filter(f).map(|s| format!("({s})"))).collect::<Result<Vec<_>, _>>()?;
        Ok(parts.join(sep))
    }

    fn pred(&mut self, p: &Pred) -> Result<String, QueryError> {
        let def = self.field(&p.field)?;
        if let Some(positive) = p.op.negates() {
            return Ok(format!("NOT ({})", self.positive(def, positive, &p.values)));
        }
        Ok(self.positive(def, p.op, &p.values))
    }

    /// Never-NULL SQL for a positive operator.
    fn positive(&mut self, def: &FieldDef, op: Op, values: &[Value]) -> String {
        match op {
            // Each value must be present: one existence test per value.
            Op::ContainsAll => values
                .iter()
                .map(|v| format!("({})", self.wrap(def, Op::ContainsAny, std::slice::from_ref(v))))
                .collect::<Vec<_>>()
                .join(" AND "),
            _ => self.wrap(def, op, values),
        }
    }

    fn wrap(&mut self, def: &FieldDef, op: Op, values: &[Value]) -> String {
        match &def.source {
            FieldSource::Column { expr } => {
                let cond = self.cond(def.ty, expr, op, values);
                format!("COALESCE({cond}, 0)")
            }
            FieldSource::Ext { field_id } => {
                let ctx = self.ctx;
                let s = &ctx.schema;
                let col = format!("ev.{}", self.ext_col(def.ty));
                let head = format!(
                    "EXISTS (SELECT 1 FROM {} ev WHERE ev.{} = {} AND ev.{} = {}",
                    s.ext_table,
                    s.ext_media_id,
                    s.media_id,
                    s.ext_field_id,
                    self.bind(SqlValue::Int(*field_id))
                );
                let cond = self.cond(def.ty, &col, op, values);
                format!("{head} AND {cond})")
            }
        }
    }

    fn ext_col(&self, ty: FieldType) -> &'a str {
        let s = &self.ctx.schema;
        match ty.ext_storage() {
            ExtStorage::Int => &s.ext_int,
            ExtStorage::Real => &s.ext_real,
            ExtStorage::Text => &s.ext_text,
        }
    }

    /// Condition on a single value expression. May be NULL; callers wrap it.
    fn cond(&mut self, ty: FieldType, expr: &str, op: Op, values: &[Value]) -> String {
        if ty == FieldType::Date {
            return self.date_cond(expr, op, values);
        }
        let textual = ty.is_textual();
        match op {
            Op::Exists if textual => format!("({expr} IS NOT NULL AND {expr} <> '')"),
            Op::Exists => format!("{expr} IS NOT NULL"),
            Op::Eq if textual => format!("{expr} = {} COLLATE NOCASE", self.value(&values[0])),
            Op::Eq => format!("{expr} = {}", self.value(&values[0])),
            Op::Gt | Op::Ge | Op::Lt | Op::Le => {
                let sym = match op {
                    Op::Gt => ">",
                    Op::Ge => ">=",
                    Op::Lt => "<",
                    _ => "<=",
                };
                format!("{expr} {sym} {}", self.value(&values[0]))
            }
            Op::Between => {
                let (a, b) = (self.value(&values[0]), self.value(&values[1]));
                format!("{expr} BETWEEN {a} AND {b}")
            }
            // Text fields match substrings; other types match exact values.
            Op::ContainsAny if ty == FieldType::Text => self.likes(expr, values, |t| format!("%{t}%")),
            Op::ContainsAny if textual => {
                let list = self.list(values);
                format!("{expr} COLLATE NOCASE IN ({list})")
            }
            Op::ContainsAny => format!("{expr} IN ({})", self.list(values)),
            Op::StartsWith => self.likes(expr, values, |t| format!("{t}%")),
            Op::EndsWith => self.likes(expr, values, |t| format!("%{t}")),
            // `png`, `.png` and `PNG` all mean "name ends with .png" (LIKE is case-insensitive for ASCII).
            Op::HasExt => {
                let exts: Vec<Value> = values
                    .iter()
                    .map(|v| match v {
                        Value::Text(t) => Value::Text(t.trim().trim_start_matches('.').to_string()),
                        other => other.clone(),
                    })
                    .collect();
                self.likes(expr, &exts, |t| format!("%.{t}"))
            }
            Op::Glob => {
                let Value::Text(t) = &values[0] else { unreachable!("analyzer only produces text for text fields") };
                let pat = self.bind(SqlValue::Text(glob_to_like(t)));
                format!("{expr} LIKE {pat} ESCAPE '\\'")
            }
            Op::Ne
            | Op::ContainsAll
            | Op::ContainsNone
            | Op::NotExists
            | Op::NotStartsWith
            | Op::NotEndsWith
            | Op::NotHasExt => {
                unreachable!("handled by positive()")
            }
        }
    }

    fn date_cond(&mut self, expr: &str, op: Op, values: &[Value]) -> String {
        let ranges: Vec<(i64, i64)> = values
            .iter()
            .map(|v| match v {
                Value::Date(d) => d.range(self.ctx.now_ms, self.ctx.tz_offset_minutes),
                other => unreachable!("date field with value {other:?}"),
            })
            .collect();
        let mut b = |ms: i64| self.bind(SqlValue::Int(ms));
        match op {
            Op::Exists => format!("{expr} IS NOT NULL"),
            // `이후`/`이전` both include the given day.
            Op::Ge => format!("{expr} >= {}", b(ranges[0].0)),
            Op::Gt => format!("{expr} >= {}", b(ranges[0].1)),
            Op::Le => format!("{expr} < {}", b(ranges[0].1)),
            Op::Lt => format!("{expr} < {}", b(ranges[0].0)),
            Op::Between => format!("({expr} >= {} AND {expr} < {})", b(ranges[0].0), b(ranges[1].1)),
            Op::Eq | Op::ContainsAny => {
                let parts: Vec<String> =
                    ranges.iter().map(|(s, e)| format!("({expr} >= {} AND {expr} < {})", b(*s), b(*e))).collect();
                format!("({})", parts.join(" OR "))
            }
            _ => unreachable!("operator {op:?} not valid for dates"),
        }
    }

    fn likes(&mut self, expr: &str, values: &[Value], pattern: impl Fn(&str) -> String) -> String {
        let parts: Vec<String> = values
            .iter()
            .map(|v| {
                let Value::Text(t) = v else { unreachable!("LIKE on non-text value") };
                let p = self.bind(SqlValue::Text(pattern(&escape_like(t))));
                format!("{expr} LIKE {p} ESCAPE '\\'")
            })
            .collect();
        format!("({})", parts.join(" OR "))
    }

    fn list(&mut self, values: &[Value]) -> String {
        values.iter().map(|v| self.value(v)).collect::<Vec<_>>().join(", ")
    }

    fn value(&mut self, v: &Value) -> String {
        let sql = match v {
            Value::Int(i) => SqlValue::Int(*i),
            Value::Real(r) => SqlValue::Real(*r),
            Value::Text(t) => SqlValue::Text(t.clone()),
            Value::Size(b) => SqlValue::Int(*b as i64),
            Value::Duration(ms) => SqlValue::Int(*ms),
            Value::Kind(k) => SqlValue::Text(k.as_str().into()),
            Value::Date(_) => unreachable!("dates go through date_cond"),
        };
        self.bind(sql)
    }

    fn sort_key(&mut self, def: &FieldDef, dir: SortDir) -> String {
        match &def.source {
            FieldSource::Column { expr } => expr.clone(),
            FieldSource::Ext { field_id } => {
                let ctx = self.ctx;
                let s = &ctx.schema;
                // Multi-valued fields sort by their smallest value ascending, largest descending.
                let agg = if dir == SortDir::Asc { "MIN" } else { "MAX" };
                format!(
                    "(SELECT {agg}(ev.{col}) FROM {} ev WHERE ev.{} = {} AND ev.{} = {})",
                    s.ext_table,
                    s.ext_media_id,
                    s.media_id,
                    s.ext_field_id,
                    self.bind(SqlValue::Int(*field_id)),
                    col = self.ext_col(def.ty),
                )
            }
        }
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `*` → any run, `?` → one char; everything else literal.
fn glob_to_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '*' => out.push('%'),
            '?' => out.push('_'),
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::{Sort, analyze};

    fn compile_src(src: &str) -> Compiled {
        let reg = crate::analyze::tests::registry();
        let q = analyze(&crate::parse(src).unwrap(), &reg).unwrap();
        compile(&q, &reg, &CompileContext::default()).unwrap()
    }

    #[test]
    fn last_sort_is_most_significant() {
        let c = compile_src("정렬(생성 날짜) 그리고 정렬(이름) 그리고 내림차순 정렬(동일 부모 폴더)");
        assert_eq!(c.where_sql, "1");
        let folder = c.order_sql.find("m.parent_path").unwrap();
        let name = c.order_sql.find("m.name").unwrap();
        let created = c.order_sql.find("m.created_ms").unwrap();
        assert!(folder < name && name < created, "{}", c.order_sql);
        assert!(c.order_sql.ends_with("m.id ASC"));
        assert!(c.order_sql.contains("m.parent_path COLLATE NOCASE DESC"));
    }

    #[test]
    fn extension_and_negated_text_ops() {
        let c = compile_src("이름(.PNG:jpg:의 확장자를 가지고 있음)");
        assert_eq!(c.params, [SqlValue::Text("%.PNG".into()), SqlValue::Text("%.jpg".into())]);
        let c = compile_src("이름(tmp:로 시작하지 않음)");
        assert!(c.where_sql.starts_with("NOT (COALESCE"), "{}", c.where_sql);
        assert_eq!(c.params, [SqlValue::Text("tmp%".into())]);
    }

    #[test]
    fn negation_wraps_positive() {
        let c = compile_src("점수(3:다름)");
        assert!(c.where_sql.starts_with("NOT (EXISTS"), "{}", c.where_sql);
        assert_eq!(c.params, [SqlValue::Int(2), SqlValue::Int(3)]);
    }

    #[test]
    fn like_escaping() {
        let c = compile_src("이름(100%_done)");
        assert_eq!(c.params, [SqlValue::Text("%100\\%\\_done%".into())]);
        let c = compile_src("이름(패턴:IMG_*.jpg)");
        assert_eq!(c.params, [SqlValue::Text("IMG\\_%.jpg".into())]);
    }

    #[test]
    fn unknown_saved_field() {
        let reg = crate::analyze::tests::registry();
        let q = Query { filter: None, sorts: vec![Sort { field: "x.deleted".into(), dir: SortDir::Asc }] };
        assert!(compile(&q, &reg, &CompileContext::default()).is_err());
    }
}
