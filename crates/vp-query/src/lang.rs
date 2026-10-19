//! Locale alias tables for keywords, operators and enum values.
//!
//! Matching is done on [`normalize`]d text (lowercase, whitespace removed), so
//! `생성 날짜`, `생성날짜` and `Sort Asc` / `sortasc` all match.

use serde::{Deserialize, Serialize};
use vp_core::MediaKind;

/// Normalizes text for alias comparison.
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// A set of spellings for one concept. The first alias of each locale is its display form.
#[derive(Debug, Clone, Copy)]
pub struct Aliases {
    pub ko: &'static [&'static str],
    pub en: &'static [&'static str],
}

impl Aliases {
    pub fn all(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.ko.iter().chain(self.en.iter()).copied()
    }

    pub fn matches(&self, normalized: &str) -> bool {
        self.all().any(|a| normalize(a) == normalized)
    }

    pub fn display(&self, locale: &str) -> &'static str {
        let list = match locale {
            "ko" => self.ko,
            _ => self.en,
        };
        list.first().or(self.en.first()).copied().unwrap_or("")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    And,
    Or,
    Not,
}

pub const AND: Aliases = Aliases { ko: &["그리고", "및"], en: &["AND", "&&"] };
pub const OR: Aliases = Aliases { ko: &["혹은", "또는"], en: &["OR", "||"] };
pub const NOT: Aliases = Aliases { ko: &["아님"], en: &["NOT", "!"] };

pub fn keyword(word: &str) -> Option<Keyword> {
    let n = normalize(word);
    if AND.matches(&n) {
        Some(Keyword::And)
    } else if OR.matches(&n) {
        Some(Keyword::Or)
    } else if NOT.matches(&n) {
        Some(Keyword::Not)
    } else {
        None
    }
}

pub fn keyword_aliases(k: Keyword) -> Aliases {
    match k {
        Keyword::And => AND,
        Keyword::Or => OR,
        Keyword::Not => NOT,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortDir {
    Asc,
    Desc,
}

pub const SORT_ASC: Aliases =
    Aliases { ko: &["오름차순 정렬", "오름차순", "정렬"], en: &["sort asc", "asc", "sort", "order by"] };
pub const SORT_DESC: Aliases = Aliases { ko: &["내림차순 정렬", "내림차순"], en: &["sort desc", "desc"] };

pub fn sort_call(name: &str) -> Option<SortDir> {
    let n = normalize(name);
    if SORT_ASC.matches(&n) {
        Some(SortDir::Asc)
    } else if SORT_DESC.matches(&n) {
        Some(SortDir::Desc)
    } else {
        None
    }
}

pub fn sort_aliases(dir: SortDir) -> Aliases {
    match dir {
        SortDir::Asc => SORT_ASC,
        SortDir::Desc => SORT_DESC,
    }
}

/// Comparison operators. Negative operators are exact negations of a positive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    ContainsAny,
    ContainsAll,
    ContainsNone,
    StartsWith,
    NotStartsWith,
    EndsWith,
    NotEndsWith,
    /// File extension test on a text field (`이름(png:의 확장자를 가지고 있음)`).
    HasExt,
    NotHasExt,
    Glob,
    Exists,
    NotExists,
}

impl Op {
    pub const ALL: [Op; 19] = [
        Op::Eq,
        Op::Ne,
        Op::Gt,
        Op::Ge,
        Op::Lt,
        Op::Le,
        Op::Between,
        Op::ContainsAny,
        Op::ContainsAll,
        Op::ContainsNone,
        Op::StartsWith,
        Op::NotStartsWith,
        Op::EndsWith,
        Op::NotEndsWith,
        Op::HasExt,
        Op::NotHasExt,
        Op::Glob,
        Op::Exists,
        Op::NotExists,
    ];

    /// The positive operator this one negates, if it is a negative operator.
    pub fn negates(self) -> Option<Op> {
        match self {
            Op::Ne => Some(Op::Eq),
            Op::ContainsNone => Some(Op::ContainsAny),
            Op::NotExists => Some(Op::Exists),
            Op::NotStartsWith => Some(Op::StartsWith),
            Op::NotEndsWith => Some(Op::EndsWith),
            Op::NotHasExt => Some(Op::HasExt),
            _ => None,
        }
    }

    /// Whether the operator is written after its values (`점수(5:이상)`) rather than before
    /// (`장르(다음을 포함:느와르)`). Only affects rendering; parsing accepts either position.
    pub fn is_postfix(self) -> bool {
        matches!(
            self,
            Op::Eq
                | Op::Ne
                | Op::Gt
                | Op::Ge
                | Op::Lt
                | Op::Le
                | Op::Between
                | Op::StartsWith
                | Op::NotStartsWith
                | Op::EndsWith
                | Op::NotEndsWith
                | Op::HasExt
                | Op::NotHasExt
        )
    }

    pub fn aliases(self) -> Aliases {
        match self {
            Op::Eq => Aliases { ko: &["같음", "일치"], en: &["eq", "=", "is"] },
            Op::Ne => Aliases { ko: &["다름", "같지 않음"], en: &["ne", "!=", "is not"] },
            Op::Gt => Aliases { ko: &["초과"], en: &["gt", ">"] },
            Op::Ge => Aliases { ko: &["이상", "이후"], en: &["gte", ">=", "after"] },
            Op::Lt => Aliases { ko: &["미만"], en: &["lt", "<"] },
            Op::Le => Aliases { ko: &["이하", "이전"], en: &["lte", "<=", "before"] },
            Op::Between => Aliases { ko: &["사이", "범위"], en: &["between", "range"] },
            Op::ContainsAny => Aliases {
                ko: &["다음을 포함", "포함", "하나라도 포함", "가 포함됨", "이 포함됨"],
                en: &["contains", "any", "in"],
            },
            Op::ContainsAll => Aliases { ko: &["모두 포함"], en: &["contains all", "all"] },
            Op::ContainsNone => Aliases {
                ko: &["다음을 제외", "제외", "포함하지 않음", "를 포함하지 않음", "을 포함하지 않음"],
                en: &["excludes", "none", "not in"],
            },
            Op::StartsWith => Aliases { ko: &["로 시작함", "으로 시작함", "시작", "로 시작"], en: &["starts with", "prefix"] },
            Op::NotStartsWith => Aliases {
                ko: &["로 시작하지 않음", "으로 시작하지 않음"],
                en: &["not starts with", "no prefix"],
            },
            Op::EndsWith => Aliases { ko: &["로 끝남", "으로 끝남", "끝"], en: &["ends with", "suffix"] },
            Op::NotEndsWith => {
                Aliases { ko: &["로 끝나지 않음", "으로 끝나지 않음"], en: &["not ends with", "no suffix"] }
            }
            Op::HasExt => Aliases { ko: &["의 확장자를 가지고 있음", "확장자 일치"], en: &["has extension", "ext is"] },
            Op::NotHasExt => Aliases {
                ko: &["의 확장자를 가지고 있지 않음", "확장자 불일치"],
                en: &["lacks extension", "ext is not"],
            },
            Op::Glob => Aliases { ko: &["패턴"], en: &["glob", "pattern"] },
            Op::Exists => Aliases { ko: &["있음", "값 있음"], en: &["exists", "has"] },
            Op::NotExists => Aliases { ko: &["없음", "값 없음"], en: &["missing", "empty"] },
        }
    }
}

pub fn op(word: &str) -> Option<Op> {
    let n = normalize(word);
    Op::ALL.into_iter().find(|o| o.aliases().matches(&n))
}

pub fn kind_aliases(kind: MediaKind) -> Aliases {
    match kind {
        MediaKind::Image => Aliases { ko: &["이미지", "사진", "그림"], en: &["image", "photo", "picture"] },
        MediaKind::Video => Aliases { ko: &["비디오", "영상", "동영상"], en: &["video", "movie", "clip"] },
        MediaKind::NonMedia => Aliases { ko: &["기타 파일", "기타", "미디어 아님"], en: &["nonmedia", "other", "file"] },
    }
}

pub fn kind(word: &str) -> Option<MediaKind> {
    let n = normalize(word);
    MediaKind::ALL.into_iter().find(|k| kind_aliases(*k).matches(&n))
}

/// Values of on/off (toggle) fields: `좋아요(예)`, `favorite(no)`.
pub const YES: Aliases = Aliases { ko: &["예", "네", "켜짐", "참"], en: &["yes", "true", "on"] };
pub const NO: Aliases = Aliases { ko: &["아니오", "아니요", "꺼짐", "거짓"], en: &["no", "false", "off"] };

pub fn toggle(word: &str) -> Option<bool> {
    let n = normalize(word);
    if YES.matches(&n) || n == "1" {
        Some(true)
    } else if NO.matches(&n) || n == "0" {
        Some(false)
    } else {
        None
    }
}

/// Relative day words: `오늘` = 0, `어제` = -1.
pub const TODAY: Aliases = Aliases { ko: &["오늘"], en: &["today"] };
pub const YESTERDAY: Aliases = Aliases { ko: &["어제"], en: &["yesterday"] };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_unambiguous() {
        // An operator alias must not double as a keyword, sort call or another operator.
        let mut seen = std::collections::HashMap::new();
        let mut check = |a: &str, what: String| {
            if let Some(prev) = seen.insert(normalize(a), what.clone()) {
                panic!("alias '{a}' used by both {prev} and {what}");
            }
        };
        for o in Op::ALL {
            o.aliases().all().for_each(|a| check(a, format!("{o:?}")));
        }
        for (k, n) in [(AND, "and"), (OR, "or"), (NOT, "not"), (SORT_ASC, "asc"), (SORT_DESC, "desc")] {
            k.all().for_each(|a| check(a, n.to_string()));
        }
    }
}
