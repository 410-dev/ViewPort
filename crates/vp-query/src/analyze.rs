//! Resolves the untyped syntax tree against a [`FieldRegistry`] into a typed, locale-neutral [`Query`].
//!
//! [`Query`] is also the saved-query format: it serializes to JSON and can be rendered
//! back to text in any locale with [`crate::render`].

use serde::{Deserialize, Serialize};

use crate::ast::{Call, Expr, ExprKind, Span};
use crate::error::{ErrorKind, QueryError};
use crate::fields::{FieldDef, FieldRegistry};
use crate::lang::{self, Op, SortDir};
use crate::value::{Value, parse_value, type_label};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Query {
    pub filter: Option<Filter>,
    /// Sorts in the order written. Applied as successive stable sorts, so the
    /// **last** entry is the most significant key.
    pub sorts: Vec<Sort>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Filter {
    And { items: Vec<Filter> },
    Or { items: Vec<Filter> },
    Not { inner: Box<Filter> },
    Pred(Pred),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pred {
    /// [`FieldDef::key`].
    pub field: String,
    pub op: Op,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sort {
    pub field: String,
    pub dir: SortDir,
}

type Result<T> = std::result::Result<T, QueryError>;

pub fn analyze(expr: &Expr, reg: &FieldRegistry) -> Result<Query> {
    let mut sorts = Vec::new();
    let filter = match &expr.kind {
        ExprKind::And { items } => lower_and(items, expr.span, reg, Some(&mut sorts), true)?,
        _ => lower_and(std::slice::from_ref(expr), expr.span, reg, Some(&mut sorts), true)?,
    };
    Ok(Query { filter, sorts })
}

/// Sort calls joined with AND apply to the whole query wherever they appear, so
/// `a(1) 혹은 b(2) 그리고 정렬(x)` sorts the whole result even though AND binds tighter.
/// They are rejected under NOT, directly under OR, and in an AND chain made only of sorts
/// (other than the top level), where they would leave an empty condition behind.
fn lower_and(
    items: &[Expr],
    span: Span,
    reg: &FieldRegistry,
    mut sorts: Option<&mut Vec<Sort>>,
    top: bool,
) -> Result<Option<Filter>> {
    let mut filters = Vec::new();
    for item in items {
        match &item.kind {
            ExprKind::Call(call) if lang::sort_call(&call.name).is_some() => match sorts.as_deref_mut() {
                Some(sorts) => sorts.push(lower_sort(call, reg)?),
                None => return Err(QueryError::new(ErrorKind::SortNotTopLevel, call.span)),
            },
            _ => filters.push(lower(item, reg, sorts.as_deref_mut())?),
        }
    }
    Ok(match filters.len() {
        0 if top => None,
        0 => return Err(QueryError::new(ErrorKind::SortNotTopLevel, span)),
        1 => filters.pop(),
        _ => Some(Filter::And { items: filters }),
    })
}

fn lower(expr: &Expr, reg: &FieldRegistry, mut sorts: Option<&mut Vec<Sort>>) -> Result<Filter> {
    Ok(match &expr.kind {
        ExprKind::And { items } => lower_and(items, expr.span, reg, sorts, false)?.expect("non-top AND is never empty"),
        ExprKind::Or { items } => Filter::Or {
            items: items.iter().map(|e| lower(e, reg, sorts.as_deref_mut())).collect::<Result<_>>()?,
        },
        ExprKind::Not { inner } => Filter::Not { inner: Box::new(lower(inner, reg, None)?) },
        ExprKind::Call(call) => {
            if lang::sort_call(&call.name).is_some() {
                return Err(QueryError::new(ErrorKind::SortNotTopLevel, call.span));
            }
            Filter::Pred(lower_pred(call, reg)?)
        }
    })
}

fn lookup<'r>(reg: &'r FieldRegistry, name: &str, span: Span) -> Result<&'r FieldDef> {
    reg.lookup(name).ok_or_else(|| QueryError::new(ErrorKind::UnknownField { name: name.into() }, span))
}

fn lower_sort(call: &Call, reg: &FieldRegistry) -> Result<Sort> {
    let dir = lang::sort_call(&call.name).expect("checked by caller");
    let [param] = call.params.as_slice() else {
        return Err(QueryError::new(ErrorKind::SortArity, call.span));
    };
    let def = lookup(reg, &param.text, param.span)?;
    Ok(Sort { field: def.key.clone(), dir })
}

fn lower_pred(call: &Call, reg: &FieldRegistry) -> Result<Pred> {
    let def = lookup(reg, &call.name, call.name_span)?;
    let ty = def.ty;

    // The operator may appear at any position; a parameter counts as the operator only
    // if it is an operator alias valid for this field type, so `이름(이상)` still means
    // "name contains 이상". Quoting forces a parameter to be a value.
    let mut op: Option<(Op, Span)> = None;
    let mut raw_values = Vec::new();
    for p in &call.params {
        match (!p.quoted).then(|| lang::op(&p.text)).flatten().filter(|o| ty.allows(*o)) {
            Some(o) => {
                if op.is_some() {
                    return Err(QueryError::new(ErrorKind::MultipleOperators, p.span));
                }
                op = Some((o, p.span));
            }
            None => raw_values.push(p),
        }
    }

    // An operator alias that exists but is invalid for this type is reported only when
    // nothing else could explain it (a lone parameter on a non-text field).
    if op.is_none() && !ty.is_textual()
        && let Some(p) = call.params.iter().find(|p| !p.quoted && lang::op(&p.text).is_some()) {
            return Err(QueryError::new(
                ErrorKind::OperatorNotAllowed { op: p.text.clone(), field: call.name.clone() },
                p.span,
            ));
        }

    let (op, op_span) = op.unwrap_or((ty.default_op(), call.span));
    let n = raw_values.len();
    let arity_ok = match op {
        Op::Exists | Op::NotExists => n == 0,
        Op::Between => n == 2,
        Op::ContainsAny | Op::ContainsAll | Op::ContainsNone | Op::HasExt | Op::NotHasExt => n >= 1,
        _ => n == 1,
    };
    if !arity_ok {
        let expected = match op {
            Op::Exists | Op::NotExists => "0",
            Op::Between => "2",
            Op::ContainsAny | Op::ContainsAll | Op::ContainsNone | Op::HasExt | Op::NotHasExt => "1+",
            _ => "1",
        };
        return Err(QueryError::new(
            ErrorKind::WrongArity { op: op.aliases().display("en").into(), expected: expected.into(), got: n },
            op_span.join(call.span),
        ));
    }

    let values = raw_values
        .into_iter()
        .map(|p| {
            parse_value(ty, &p.text).ok_or_else(|| {
                QueryError::new(
                    ErrorKind::InvalidValue { expected: type_label(ty).into(), text: p.text.clone() },
                    p.span,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Pred { field: def.key.clone(), op, values })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::fields::FieldType;
    use crate::value::DateValue;

    pub(crate) fn registry() -> FieldRegistry {
        let mut r = FieldRegistry::with_base_fields();
        r.add(FieldDef::ext("x.tags", FieldType::Enum, 1).name("ko", "태그").name("en", "tags")).unwrap();
        r.add(FieldDef::ext("x.rating", FieldType::RangedInt { min: 0, max: 5 }, 2).name("ko", "점수").name("en", "rating"))
            .unwrap();
        r.add(FieldDef::ext("x.genre", FieldType::Text, 3).name("ko", "장르").name("en", "genre")).unwrap();
        r.add(FieldDef::ext("x.favorite", FieldType::Toggle, 4).name("ko", "좋아요").name("en", "favorite")).unwrap();
        r
    }

    fn q(src: &str) -> Result<Query> {
        analyze(&crate::parse(src)?, &registry())
    }

    fn pred(f: &Filter) -> &Pred {
        match f {
            Filter::Pred(p) => p,
            other => panic!("expected pred, got {other:?}"),
        }
    }

    #[test]
    fn spec_example() {
        let query = q("생성 날짜(2026-10-18:이후) 그리고 (변경 날짜(2026-09-18:이후) 혹은 점수(5:이상)) \
                       그리고 장르(다음을 포함:느와르:단편:코미디) 그리고 유형(다음을 포함:이미지:비디오) \
                       그리고 오름차순 정렬(생성 날짜) 그리고 오름차순 정렬(이름) 그리고 오름차순 정렬(동일 부모 폴더)")
        .unwrap();
        let Some(Filter::And { items }) = &query.filter else { panic!() };
        assert_eq!(items.len(), 4);
        let created = pred(&items[0]);
        assert_eq!((created.field.as_str(), created.op), ("created", Op::Ge));
        assert!(matches!(created.values[0], Value::Date(DateValue::Civil { year: 2026, month: 10, day: 18, .. })));
        let genre = pred(&items[2]);
        assert_eq!(genre.op, Op::ContainsAny);
        assert_eq!(genre.values.len(), 3);
        assert_eq!(
            query.sorts.iter().map(|s| s.field.as_str()).collect::<Vec<_>>(),
            ["created", "name", "folder"]
        );
    }

    #[test]
    fn operator_position_and_defaults() {
        let a = q("점수(5:이상)").unwrap();
        let b = q("점수(이상:5)").unwrap();
        assert_eq!(a, b);
        assert_eq!(pred(q("태그(풍경)").unwrap().filter.as_ref().unwrap()).op, Op::ContainsAny);
        assert_eq!(pred(q("점수(3)").unwrap().filter.as_ref().unwrap()).op, Op::Eq);
        // Not an operator for text fields, so it is a value.
        let p = q("이름(이상)").unwrap();
        assert_eq!(pred(p.filter.as_ref().unwrap()).values, [Value::Text("이상".into())]);
        // Quoted text is never an operator.
        let p = q("장르(\"포함\")").unwrap();
        assert_eq!(pred(p.filter.as_ref().unwrap()).op, Op::ContainsAny);
    }

    #[test]
    fn english_aliases() {
        let ko = q("크기(100MB:이상) 그리고 유형(영상)").unwrap();
        let en = q("size(>=:100mb) AND type(video)").unwrap();
        assert_eq!(ko, en);
    }

    #[test]
    fn errors() {
        let e = |s: &str| q(s).unwrap_err().kind;
        assert_eq!(e("모름(1)"), ErrorKind::UnknownField { name: "모름".into() });
        assert!(matches!(e("점수(1:2)"), ErrorKind::WrongArity { .. }));
        assert!(matches!(e("점수(1:사이)"), ErrorKind::WrongArity { .. }));
        assert!(matches!(e("점수(이상:초과:1)"), ErrorKind::MultipleOperators));
        assert!(matches!(e("점수(abc)"), ErrorKind::InvalidValue { .. }));
        assert!(matches!(e("점수(모두 포함:1)"), ErrorKind::OperatorNotAllowed { .. }));
        assert_eq!(e("이름(a) 혹은 정렬(이름)"), ErrorKind::SortNotTopLevel);
        assert_eq!(e("아님 (이름(a) 그리고 정렬(이름))"), ErrorKind::SortNotTopLevel);
        assert_eq!(e("이름(a) 혹은 (정렬(이름) 그리고 정렬(크기))"), ErrorKind::SortNotTopLevel);
        assert_eq!(e("정렬(이름:크기)"), ErrorKind::SortArity);
    }

    #[test]
    fn sorts_apply_to_whole_query() {
        let query = q("이름(a) 혹은 이름(b) 그리고 내림차순 정렬(점수)").unwrap();
        assert!(matches!(query.filter, Some(Filter::Or { .. })));
        assert_eq!(query.sorts, [Sort { field: "x.rating".into(), dir: SortDir::Desc }]);
        assert_eq!(q("정렬(이름)").unwrap().filter, None);
    }

    #[test]
    fn saved_form_is_json() {
        let query = q("태그(바다:산) 그리고 내림차순 정렬(점수)").unwrap();
        let json = serde_json::to_string(&query).unwrap();
        assert_eq!(serde_json::from_str::<Query>(&json).unwrap(), query);
    }
}
