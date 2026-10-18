//! Field registry: what condition names mean and where their values live.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::lang::{Op, normalize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldType {
    Text,
    Integer,
    Real,
    /// Epoch milliseconds.
    Date,
    /// Bytes.
    Size,
    /// Milliseconds.
    Duration,
    /// Image / video.
    Kind,
    /// Multi-valued set of labels (tags).
    Enum,
    /// Integer constrained to `min..=max` (ratings).
    RangedInt { min: i64, max: i64 },
}

impl FieldType {
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            FieldType::Integer
                | FieldType::Real
                | FieldType::Date
                | FieldType::Size
                | FieldType::Duration
                | FieldType::RangedInt { .. }
        )
    }

    pub fn is_textual(self) -> bool {
        matches!(self, FieldType::Text | FieldType::Enum)
    }

    pub fn allows(self, op: Op) -> bool {
        use Op::*;
        match op {
            Eq | Ne | ContainsAny | ContainsNone | Exists | NotExists => true,
            Gt | Ge | Lt | Le | Between => self.is_numeric(),
            ContainsAll => self.is_textual(),
            StartsWith | NotStartsWith | EndsWith | NotEndsWith | HasExt | NotHasExt | Glob => {
                self == FieldType::Text
            }
        }
    }

    /// Operator used when a condition gives only values, e.g. `태그(풍경)`.
    pub fn default_op(self) -> Op {
        if self.is_numeric() { Op::Eq } else { Op::ContainsAny }
    }

    /// Which `ext_value` column stores this type.
    pub fn ext_storage(self) -> ExtStorage {
        match self {
            FieldType::Real => ExtStorage::Real,
            FieldType::Text | FieldType::Enum | FieldType::Kind => ExtStorage::Text,
            _ => ExtStorage::Int,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtStorage {
    Int,
    Real,
    Text,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldSource {
    /// A SQL expression over the main query's tables (`m` = media, `s` = stats, `src` = source).
    Column { expr: String },
    /// A row set in the extension value table.
    Ext { field_id: i64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDef {
    /// Stable, locale-independent identifier used in saved queries (`name`, `x.genre`).
    pub key: String,
    /// Display names by locale; the first entry is the fallback.
    pub names: Vec<(String, String)>,
    /// Extra accepted spellings.
    pub aliases: Vec<String>,
    pub ty: FieldType,
    pub source: FieldSource,
}

impl FieldDef {
    pub fn column(key: &str, ty: FieldType, expr: &str) -> Self {
        Self::new(key, ty, FieldSource::Column { expr: expr.into() })
    }

    pub fn ext(key: &str, ty: FieldType, field_id: i64) -> Self {
        Self::new(key, ty, FieldSource::Ext { field_id })
    }

    fn new(key: &str, ty: FieldType, source: FieldSource) -> Self {
        Self { key: key.into(), names: Vec::new(), aliases: Vec::new(), ty, source }
    }

    pub fn name(mut self, locale: &str, name: &str) -> Self {
        self.names.push((locale.into(), name.into()));
        self
    }

    pub fn alias(mut self, alias: &str) -> Self {
        self.aliases.push(alias.into());
        self
    }

    pub fn display_name(&self, locale: &str) -> &str {
        self.names
            .iter()
            .find(|(l, _)| l == locale)
            .or(self.names.first())
            .map(|(_, n)| n.as_str())
            .unwrap_or(&self.key)
    }

    fn spellings(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|(_, n)| n.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("field key '{0}' already registered")]
    DuplicateKey(String),
    #[error("name '{name}' is already used by field '{existing}'")]
    DuplicateName { name: String, existing: String },
    #[error("name '{0}' is reserved by the query language")]
    ReservedName(String),
}

#[derive(Debug, Clone, Default)]
pub struct FieldRegistry {
    fields: Vec<FieldDef>,
    by_name: HashMap<String, usize>,
    by_key: HashMap<String, usize>,
}

impl FieldRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry with every base (file-derived) field.
    pub fn with_base_fields() -> Self {
        let mut r = Self::new();
        for def in base_fields() {
            r.add(def).expect("base fields are consistent");
        }
        r
    }

    pub fn add(&mut self, def: FieldDef) -> Result<(), RegistryError> {
        if self.by_key.contains_key(&def.key) {
            return Err(RegistryError::DuplicateKey(def.key));
        }
        let mut names = Vec::new();
        for s in def.spellings() {
            let n = normalize(s);
            if crate::lang::keyword(s).is_some() || crate::lang::sort_call(s).is_some() {
                return Err(RegistryError::ReservedName(s.into()));
            }
            if let Some(&i) = self.by_name.get(&n) {
                return Err(RegistryError::DuplicateName { name: s.into(), existing: self.fields[i].key.clone() });
            }
            names.push(n);
        }
        let idx = self.fields.len();
        for n in names {
            self.by_name.insert(n, idx);
        }
        self.by_key.insert(def.key.clone(), idx);
        self.fields.push(def);
        Ok(())
    }

    pub fn lookup(&self, name: &str) -> Option<&FieldDef> {
        self.by_name.get(&normalize(name)).map(|&i| &self.fields[i])
    }

    pub fn by_key(&self, key: &str) -> Option<&FieldDef> {
        self.by_key.get(key).map(|&i| &self.fields[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &FieldDef> {
        self.fields.iter()
    }
}

/// Base conditions, derived from the file itself or from app statistics.
/// Column expressions assume the main query aliases documented on [`FieldSource::Column`].
pub fn base_fields() -> Vec<FieldDef> {
    use FieldType::*;
    vec![
        FieldDef::column("name", Text, "m.name").name("ko", "이름").name("en", "name").alias("파일 이름").alias("파일명").alias("filename"),
        FieldDef::column("ext", Text, "m.ext").name("ko", "확장자").name("en", "extension").alias("ext"),
        FieldDef::column("path", Text, "m.rel_path").name("ko", "경로").name("en", "path"),
        FieldDef::column("folder", Text, "m.parent_path")
            .name("ko", "동일 부모 폴더")
            .name("en", "folder")
            .alias("폴더")
            .alias("부모 폴더")
            .alias("parent folder")
            .alias("same folder"),
        FieldDef::column("source", Text, "src.name").name("ko", "소스").name("en", "source").alias("위치"),
        FieldDef::column("kind", Kind, "m.kind").name("ko", "유형").name("en", "type").alias("종류").alias("kind"),
        FieldDef::column("size", Size, "m.size").name("ko", "크기").name("en", "size").alias("용량"),
        FieldDef::column("created", Date, "m.created_ms").name("ko", "생성 날짜").name("en", "created").alias("생성일"),
        FieldDef::column("modified", Date, "m.modified_ms")
            .name("ko", "변경 날짜")
            .name("en", "modified")
            .alias("수정 날짜")
            .alias("수정일"),
        FieldDef::column("added", Date, "m.added_ms").name("ko", "추가 날짜").name("en", "added").alias("추가일"),
        FieldDef::column("duration", Duration, "m.duration_ms")
            .name("ko", "길이")
            .name("en", "duration")
            .alias("재생 시간")
            .alias("length"),
        FieldDef::column("width", Integer, "m.width").name("ko", "너비").name("en", "width").alias("가로"),
        FieldDef::column("height", Integer, "m.height").name("ko", "높이").name("en", "height").alias("세로"),
        FieldDef::column("views", Integer, "COALESCE(s.view_count, 0)")
            .name("ko", "조회수")
            .name("en", "views")
            .alias("재생 횟수"),
        FieldDef::column("last_viewed", Date, "s.last_viewed_ms").name("ko", "마지막 조회").name("en", "last viewed"),
    ]
}
