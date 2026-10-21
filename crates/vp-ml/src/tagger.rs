//! Taggers (SmilingWolf WD taggers): one score per label of `selected_tags.csv`
//! (`tag_id,name,category,count`; category 0 = general, 4 = character, 9 = rating).

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TagCategory {
    General,
    Character,
    Rating,
    Other,
}

impl TagCategory {
    fn from_code(code: &str) -> Self {
        match code.trim() {
            "0" => Self::General,
            "4" => Self::Character,
            "9" => Self::Rating,
            _ => Self::Other,
        }
    }

    pub fn code(self) -> i64 {
        match self {
            Self::General => 0,
            Self::Character => 4,
            Self::Rating => 9,
            Self::Other => -1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub name: String,
    pub category: TagCategory,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagScore {
    /// Underscores read as spaces (`long_hair` → `long hair`), as people type tags.
    pub name: String,
    pub category: TagCategory,
    pub score: f32,
}

/// Splits one CSV line, honouring double quotes.
fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Labels from the tagger's CSV, in output order. The header names the columns.
pub fn parse_labels(csv: &str) -> Result<Vec<Label>, crate::Error> {
    let mut lines = csv.lines().filter(|l| !l.trim().is_empty());
    let header = split_csv(lines.next().ok_or_else(|| crate::Error::Pack("empty label file".into()))?);
    let col = |n: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(n));
    let name = col("name").ok_or_else(|| crate::Error::Pack("label file has no 'name' column".into()))?;
    let category = col("category");
    Ok(lines
        .map(|l| {
            let f = split_csv(l);
            Label {
                name: f.get(name).map(|s| s.trim().replace('_', " ")).unwrap_or_default(),
                category: category.and_then(|c| f.get(c)).map(|c| TagCategory::from_code(c)).unwrap_or(TagCategory::General),
            }
        })
        .collect())
}

/// Scores at or above `keep`, best first. Ratings are skipped (they are not tags).
pub fn decode(labels: &[Label], scores: &[f32], keep: f32) -> Vec<TagScore> {
    let mut out: Vec<TagScore> = labels
        .iter()
        .zip(scores)
        .filter(|(l, s)| **s >= keep && l.category != TagCategory::Rating && !l.name.is_empty())
        .map(|(l, s)| TagScore { name: l.name.clone(), category: l.category, score: *s })
        .collect();
    out.sort_by(|a, b| b.score.total_cmp(&a.score));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_labels_and_decodes_scores() {
        let csv = "tag_id,name,category,count\n9999,general,9,1\n1,long_hair,0,10\n2,\"hatsune_miku\",4,5\n3,\"a,b\",0,1\n";
        let labels = parse_labels(csv).unwrap();
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[1], Label { name: "long hair".into(), category: TagCategory::General });
        assert_eq!(labels[3].name, "a,b");
        let tags = decode(&labels, &[0.99, 0.4, 0.9, 0.1], 0.35);
        assert_eq!(tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["hatsune miku", "long hair"]);
        assert_eq!(tags[0].category, TagCategory::Character);
        assert!(parse_labels("id,tag\n1,x").is_err());
    }
}
