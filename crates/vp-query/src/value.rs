//! Typed parameter values: parsing from query text and rendering back.

use serde::{Deserialize, Serialize};
use vp_core::MediaKind;

use crate::fields::FieldType;
use crate::lang::{self, normalize};

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", content = "v", rename_all = "snake_case")]
pub enum Value {
    Int(i64),
    Real(f64),
    Text(String),
    Date(DateValue),
    /// Bytes.
    Size(u64),
    /// Milliseconds.
    Duration(i64),
    Kind(MediaKind),
}

/// A date as written by the user. Kept in civil form so saved queries do not depend
/// on the time zone they were written in; resolved to epoch ms at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum DateValue {
    Civil { year: i32, month: u8, day: u8, hour: Option<u8>, minute: Option<u8>, second: Option<u8> },
    /// Whole day relative to today (`0` = today, `-1` = yesterday).
    RelativeDays { days: i32 },
}

impl DateValue {
    /// Half-open `[start, end)` epoch-ms range covered by this value.
    pub fn range(&self, now_ms: i64, tz_offset_minutes: i32) -> (i64, i64) {
        let off = tz_offset_minutes as i64 * 60_000;
        match *self {
            DateValue::Civil { year, month, day, hour, minute, second } => {
                let days = days_from_civil(year, month as u32, day as u32);
                let local = days * DAY_MS
                    + hour.unwrap_or(0) as i64 * 3_600_000
                    + minute.unwrap_or(0) as i64 * 60_000
                    + second.unwrap_or(0) as i64 * 1000;
                let len = if second.is_some() {
                    1000
                } else if minute.is_some() {
                    60_000
                } else {
                    DAY_MS
                };
                (local - off, local - off + len)
            }
            DateValue::RelativeDays { days } => {
                let today = (now_ms + off).div_euclid(DAY_MS);
                let start = (today + days as i64) * DAY_MS - off;
                (start, start + DAY_MS)
            }
        }
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        _ => 0,
    }
}

/// Human-readable name of what a type expects, used in error messages.
pub fn type_label(ty: FieldType) -> &'static str {
    match ty {
        FieldType::Text | FieldType::Enum => "text",
        FieldType::Integer | FieldType::RangedInt { .. } => "integer",
        FieldType::Real => "number",
        FieldType::Date => "date",
        FieldType::Size => "size",
        FieldType::Duration => "duration",
        FieldType::Kind => "media type",
    }
}

pub fn parse_value(ty: FieldType, text: &str) -> Option<Value> {
    match ty {
        FieldType::Text | FieldType::Enum => Some(Value::Text(text.to_string())),
        FieldType::Integer | FieldType::RangedInt { .. } => parse_int(text).map(Value::Int),
        FieldType::Real => parse_real(text).map(Value::Real),
        FieldType::Date => parse_date(text).map(Value::Date),
        FieldType::Size => parse_size(text).map(Value::Size),
        FieldType::Duration => parse_duration(text).map(Value::Duration),
        FieldType::Kind => lang::kind(text).map(Value::Kind),
    }
}

fn strip_digit_separators(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '_' | ',') && !c.is_whitespace()).collect()
}

fn parse_int(s: &str) -> Option<i64> {
    strip_digit_separators(s).parse().ok()
}

fn parse_real(s: &str) -> Option<f64> {
    strip_digit_separators(s).parse::<f64>().ok().filter(|v| v.is_finite())
}

fn parse_date(s: &str) -> Option<DateValue> {
    let n = normalize(s);
    if lang::TODAY.matches(&n) {
        return Some(DateValue::RelativeDays { days: 0 });
    }
    if lang::YESTERDAY.matches(&n) {
        return Some(DateValue::RelativeDays { days: -1 });
    }
    if let Some(days) = parse_relative_days(&n) {
        return Some(DateValue::RelativeDays { days });
    }

    let s = s.trim();
    let (date, time) = match s.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t.trim())),
        None => (s, None),
    };
    let mut parts = date.split(['-', '/', '.']);
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let (mut hour, mut minute, mut second) = (None, None, None);
    if let Some(t) = time {
        let mut tp = t.split(':');
        let h: u8 = tp.next()?.parse().ok()?;
        let mi: u8 = tp.next()?.parse().ok()?;
        let sec: Option<u8> = match tp.next() {
            Some(x) => Some(x.parse().ok()?),
            None => None,
        };
        if tp.next().is_some() || h > 23 || mi > 59 || sec.is_some_and(|x| x > 59) {
            return None;
        }
        (hour, minute, second) = (Some(h), Some(mi), sec);
    }
    Some(DateValue::Civil { year, month: month as u8, day: day as u8, hour, minute, second })
}

/// `7일 전`, `7 days ago`, `-7d`, `+3d`, `3일 후`.
fn parse_relative_days(n: &str) -> Option<i32> {
    let (num, sign) = if let Some(x) = n.strip_suffix("일전") {
        (x, -1)
    } else if let Some(x) = n.strip_suffix("일후") {
        (x, 1)
    } else if let Some(x) = n.strip_suffix("daysago").or_else(|| n.strip_suffix("dayago")) {
        (x, -1)
    } else {
        let x = n.strip_suffix('d')?;
        if let Some(x) = x.strip_prefix('-') {
            (x, -1)
        } else {
            (x.strip_prefix('+')?, 1)
        }
    };
    let v: i32 = num.parse().ok()?;
    (v >= 0).then_some(v * sign)
}

const SIZE_UNITS: [(&str, u64); 5] =
    [("tb", 1 << 40), ("gb", 1 << 30), ("mb", 1 << 20), ("kb", 1 << 10), ("b", 1)];

/// `1024`, `100MB`, `1.5 GiB`, `20k`. All units are binary (1 KB = 1024 B).
fn parse_size(s: &str) -> Option<u64> {
    let n = normalize(s).replace("ib", "b");
    let split = n.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',' || c == '_')).unwrap_or(n.len());
    let (num, unit) = n.split_at(split);
    let num = parse_real(num)?;
    let mult = match unit {
        "" | "b" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        u => SIZE_UNITS.iter().find(|(name, _)| *name == u)?.1,
    };
    let bytes = num * mult as f64;
    (bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes.round() as u64)
}

/// `90` (seconds), `90s`, `1m30s`, `1.5h`, `1시간 30분`, `500ms`, `01:30`, `1:02:03.5`.
fn parse_duration(s: &str) -> Option<i64> {
    let n = normalize(s);
    if n.contains(':') {
        let parts: Vec<&str> = n.split(':').collect();
        if parts.len() > 3 {
            return None;
        }
        let mut secs = 0.0;
        for (i, p) in parts.iter().enumerate() {
            let v: f64 = p.parse().ok()?;
            if v < 0.0 || (i > 0 && v >= 60.0) || (i + 1 < parts.len() && v.fract() != 0.0) {
                return None;
            }
            secs = secs * 60.0 + v;
        }
        return Some((secs * 1000.0).round() as i64);
    }
    if let Some(v) = parse_real(&n) {
        return (v >= 0.0).then(|| (v * 1000.0).round() as i64);
    }
    let mut total = 0.0;
    let mut rest = n.as_str();
    while !rest.is_empty() {
        let num_len = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        if num_len == 0 {
            return None;
        }
        let v: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let unit_len = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let mult = match &rest[..unit_len] {
            "h" | "시간" => 3_600_000.0,
            "m" | "min" | "분" => 60_000.0,
            "s" | "sec" | "초" => 1000.0,
            "ms" => 1.0,
            _ => return None,
        };
        rest = &rest[unit_len..];
        total += v * mult;
    }
    Some(total.round() as i64)
}

/// Renders a value in a form [`parse_value`] reads back to the same value.
pub fn render_value(v: &Value, locale: &str) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Real(r) => r.to_string(),
        Value::Text(t) => t.clone(),
        Value::Kind(k) => lang::kind_aliases(*k).display(locale).to_string(),
        Value::Size(b) => SIZE_UNITS
            .iter()
            .find(|(_, m)| *b != 0 && b % m == 0)
            .map(|(u, m)| if *m == 1 { b.to_string() } else { format!("{}{}", b / m, u.to_uppercase()) })
            .unwrap_or_else(|| b.to_string()),
        Value::Duration(ms) => {
            let (h, m, s, rem) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
            let mut out = String::new();
            for (n, u) in [(h, "h"), (m, "m"), (s, "s"), (rem, "ms")] {
                if n != 0 {
                    out.push_str(&format!("{n}{u}"));
                }
            }
            if out.is_empty() { "0s".into() } else { out }
        }
        Value::Date(DateValue::Civil { year, month, day, hour, minute, second }) => {
            let mut out = format!("{year:04}-{month:02}-{day:02}");
            if let (Some(h), Some(mi)) = (hour, minute) {
                out.push_str(&format!(" {h:02}:{mi:02}"));
                if let Some(s) = second {
                    out.push_str(&format!(":{s:02}"));
                }
            }
            out
        }
        Value::Date(DateValue::RelativeDays { days }) => match (*days, locale) {
            (0, _) => lang::TODAY.display(locale).into(),
            (-1, _) => lang::YESTERDAY.display(locale).into(),
            (d, "ko") if d < 0 => format!("{}일 전", -d),
            (d, "ko") => format!("{d}일 후"),
            (d, _) if d < 0 => format!("{} days ago", -d),
            (d, _) => format!("+{d}d"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
    }

    #[test]
    fn date_ranges() {
        let d = parse_date("2026-10-18").unwrap();
        let (s, e) = d.range(0, 9 * 60); // KST
        assert_eq!(s, days_from_civil(2026, 10, 18) * DAY_MS - 9 * 3_600_000);
        assert_eq!(e - s, DAY_MS);
        let d = parse_date("2026/10/18 13:05").unwrap();
        let (s, e) = d.range(0, 0);
        assert_eq!(e - s, 60_000);
        assert_eq!(s, days_from_civil(2026, 10, 18) * DAY_MS + 13 * 3_600_000 + 5 * 60_000);
        assert!(parse_date("2026-02-30").is_none());
        assert!(parse_date("2026-13-01").is_none());
    }

    #[test]
    fn relative_dates() {
        assert_eq!(parse_date("오늘"), Some(DateValue::RelativeDays { days: 0 }));
        assert_eq!(parse_date("7일 전"), Some(DateValue::RelativeDays { days: -7 }));
        assert_eq!(parse_date("7 days ago"), Some(DateValue::RelativeDays { days: -7 }));
        assert_eq!(parse_date("-3d"), Some(DateValue::RelativeDays { days: -3 }));
        let now = days_from_civil(2026, 10, 18) * DAY_MS + 3_600_000; // 01:00 UTC = 10:00 KST
        let (s, _) = DateValue::RelativeDays { days: -1 }.range(now, 9 * 60);
        assert_eq!(s, days_from_civil(2026, 10, 17) * DAY_MS - 9 * 3_600_000);
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("100MB"), Some(100 << 20));
        assert_eq!(parse_size("1.5 GiB"), Some(3 << 29));
        assert_eq!(parse_size("20k"), Some(20 << 10));
        assert_eq!(parse_size("10 parsecs"), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90"), Some(90_000));
        assert_eq!(parse_duration("1m30s"), Some(90_000));
        assert_eq!(parse_duration("1시간 30분"), Some(5_400_000));
        assert_eq!(parse_duration("1:02:03.5"), Some(3_723_500));
        assert_eq!(parse_duration("500ms"), Some(500));
        assert_eq!(parse_duration("1:60"), None);
        assert_eq!(parse_duration("abc"), None);
    }

    #[test]
    fn render_round_trips() {
        let cases = [
            (FieldType::Size, "100MB"),
            (FieldType::Size, "1000"),
            (FieldType::Duration, "1h2m3s500ms"),
            (FieldType::Date, "2026-10-18 13:05:09"),
            (FieldType::Date, "3일 전"),
            (FieldType::Kind, "영상"),
            (FieldType::Real, "2.5"),
        ];
        for locale in ["ko", "en"] {
            for (ty, text) in cases {
                let v = parse_value(ty, text).unwrap();
                assert_eq!(parse_value(ty, &render_value(&v, locale)), Some(v), "{text} in {locale}");
            }
        }
    }
}
