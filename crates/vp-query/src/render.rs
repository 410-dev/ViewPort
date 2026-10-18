//! Renders a [`Query`] back to query text in a given locale.

use crate::analyze::{Filter, Pred, Query};
use crate::fields::FieldRegistry;
use crate::lang::{self, Keyword};
use crate::value::render_value;

/// Renders `query` so that parsing the result yields the same `Query`.
/// Fields missing from the registry are rendered by key.
pub fn render(query: &Query, reg: &FieldRegistry, locale: &str) -> String {
    let and = format!(" {} ", lang::keyword_aliases(Keyword::And).display(locale));
    let mut parts = Vec::new();
    match &query.filter {
        Some(Filter::And { items }) => parts.extend(items.iter().map(|f| filter(f, reg, locale, Ctx::And))),
        Some(f) => parts.push(filter(f, reg, locale, if query.sorts.is_empty() { Ctx::Top } else { Ctx::And })),
        None => {}
    }
    for s in &query.sorts {
        let name = field_name(reg, &s.field, locale);
        parts.push(format!("{}({})", lang::sort_aliases(s.dir).display(locale), quote(&name)));
    }
    parts.join(&and)
}

#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Top,
    And,
    Or,
    Not,
}

fn filter(f: &Filter, reg: &FieldRegistry, locale: &str, ctx: Ctx) -> String {
    let kw = |k| format!(" {} ", lang::keyword_aliases(k).display(locale));
    match f {
        Filter::And { items } => {
            let s = items.iter().map(|i| filter(i, reg, locale, Ctx::And)).collect::<Vec<_>>().join(&kw(Keyword::And));
            if matches!(ctx, Ctx::Top | Ctx::Or) { s } else { format!("({s})") }
        }
        Filter::Or { items } => {
            let s = items.iter().map(|i| filter(i, reg, locale, Ctx::Or)).collect::<Vec<_>>().join(&kw(Keyword::Or));
            if ctx == Ctx::Top { s } else { format!("({s})") }
        }
        Filter::Not { inner } => {
            format!("{} {}", lang::keyword_aliases(Keyword::Not).display(locale), filter(inner, reg, locale, Ctx::Not))
        }
        Filter::Pred(p) => pred(p, reg, locale),
    }
}

fn pred(p: &Pred, reg: &FieldRegistry, locale: &str) -> String {
    let mut params: Vec<String> = p.values.iter().map(|v| quote(&render_value(v, locale))).collect();
    let op = p.op.aliases().display(locale).to_string();
    if p.op.is_postfix() {
        params.push(op);
    } else {
        params.insert(0, op);
    }
    format!("{}({})", field_name(reg, &p.field, locale), params.join(":"))
}

fn field_name(reg: &FieldRegistry, key: &str, locale: &str) -> String {
    reg.by_key(key).map(|d| d.display_name(locale).to_string()).unwrap_or_else(|| key.to_string())
}

fn quote(text: &str) -> String {
    let needs = text.is_empty()
        || text.trim() != text
        || text.contains(['(', ')', ':', '"', '\\'])
        || lang::op(text).is_some();
    if !needs {
        return text.to_string();
    }
    let mut out = String::from("\"");
    for c in text.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze;

    fn round_trip(src: &str) {
        let reg = crate::analyze::tests::registry();
        let q = analyze(&crate::parse(src).unwrap(), &reg).unwrap();
        for locale in ["ko", "en"] {
            let text = render(&q, &reg, locale);
            let back = analyze(&crate::parse(&text).unwrap_or_else(|e| panic!("{text}: {e}")), &reg)
                .unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(back, q, "{locale}: {text}");
        }
    }

    #[test]
    fn round_trips() {
        round_trip(
            "생성 날짜(2026-10-18:이후) 그리고 (변경 날짜(2026-09-18:이후) 혹은 점수(5:이상)) \
             그리고 장르(다음을 포함:느와르:단편:코미디) 그리고 유형(다음을 포함:이미지:비디오) \
             그리고 오름차순 정렬(생성 날짜) 그리고 오름차순 정렬(이름)",
        );
        round_trip("이름(a) 혹은 이름(b) 그리고 내림차순 정렬(점수)");
        round_trip("NOT (태그(바다) OR 태그(산)) AND 이름(\"a:b\") AND 이름(\"이상\")");
        round_trip("(이름(a) 그리고 이름(b)) 혹은 아님 크기(1GB:초과)");
        round_trip("생성 날짜(\"2026-10-18 13:00\":이전) 그리고 길이(1m:30s:사이)");
    }

    #[test]
    fn renders_in_locale() {
        let reg = crate::analyze::tests::registry();
        let q = analyze(&crate::parse("점수(5:이상) 그리고 정렬(이름)").unwrap(), &reg).unwrap();
        assert_eq!(render(&q, &reg, "ko"), "점수(5:이상) 그리고 오름차순 정렬(이름)");
        assert_eq!(render(&q, &reg, "en"), "rating(5:gte) AND sort asc(name)");
    }
}
