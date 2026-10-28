//! ViewPort library database (SQLite).

pub mod collation;
pub mod dedup;
pub mod face_sync;
pub mod faces;
pub mod mirror;
pub mod props;
pub mod schema;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, ToSql, params, types::ToSqlOutput};
use vp_core::MediaKind;
use vp_query::{CompileContext, FieldDef, FieldRegistry, FieldType, Query, QueryError, SqlValue};

pub use schema::SCHEMA_VERSION;

/// Re-exported so callers using [`Db::conn`] can handle its errors.
pub mod rusqlite_error {
    pub use rusqlite::Error;
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] schema::MigrationError),
    #[error(transparent)]
    Query(#[from] QueryError),
    #[error(transparent)]
    Registry(#[from] vp_query::fields::RegistryError),
    #[error("invalid stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown extension field '{0}'")]
    UnknownField(String),
    #[error("invalid value for field '{0}'")]
    InvalidValue(String),
}

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, Clone)]
pub struct NewSource {
    pub name: String,
    pub driver: String,
    pub config: serde_json::Value,
    pub root: String,
}

#[derive(Debug, Clone)]
pub struct NewMedia {
    pub source_id: i64,
    /// '/'-separated path relative to the source root.
    pub rel_path: String,
    pub kind: MediaKind,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub added_ms: i64,
    pub duration_ms: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Scan generation that saw this file (see [`Db::finish_scan`]).
    pub seen_gen: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SourceRow {
    pub id: i64,
    pub name: String,
    pub driver: String,
    pub config: serde_json::Value,
    pub root: String,
    pub enabled: bool,
}

/// What a rescan needs to know to skip unchanged files.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnownFile {
    pub id: i64,
    pub size: u64,
    pub modified_ms: Option<i64>,
    /// Width and height are recorded (images scanned by older versions may lack them).
    pub has_size: bool,
    pub added_ms: i64,
}

/// An extension field as shown in the UI.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct FieldInfo {
    pub key: String,
    pub names: Vec<(String, String)>,
    pub aliases: Vec<String>,
    pub ty: FieldType,
    pub builtin: bool,
}

/// Everything the info panel shows for one item.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MediaDetail {
    pub id: i64,
    pub source_id: i64,
    pub source_name: String,
    pub rel_path: String,
    pub name: String,
    pub kind: String,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub added_ms: i64,
    pub duration_ms: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub view_count: i64,
    pub last_viewed_ms: Option<i64>,
    /// Extension values by field key.
    pub values: std::collections::BTreeMap<String, Vec<ExtValue>>,
}

/// A labelled A-B loop inside a video.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoopSegment {
    pub id: i64,
    pub media_id: i64,
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Jump to and loop this segment as soon as the video opens.
    pub autoplay: bool,
}

/// Grid tile data.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MediaBrief {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    /// `None` until a thumbnail has been attempted.
    pub thumb: Option<String>,
    /// Videos: whether it plays natively (`None` = not probed yet).
    pub compatible: Option<bool>,
    /// A converted copy exists.
    pub has_variant: bool,
    /// The user chose not to be warned about playback for this file.
    pub ignore_warning: bool,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    /// Folder relative to the source root ("" = root).
    pub folder: String,
    pub ext: String,
    pub source_id: i64,
    /// The built-in favorite (`FAVORITE_FIELD`) is on.
    pub favorite: bool,
    /// When the item was first found by a scan.
    pub added_ms: i64,
}

/// Key of the built-in on/off favorite field.
pub const FAVORITE_FIELD: &str = "x.favorite";

/// Flag set by "add this file to the ignore list" on the playback banner.
pub const FLAG_IGNORE_PLAYBACK_WARNING: &str = "ignore_playback_warning";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ProbeRow {
    pub ok: bool,
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub compatible: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaLocation {
    pub source_id: i64,
    pub rel_path: String,
    pub kind: MediaKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbStatus {
    Ok,
    Failed,
    Unsupported,
}

impl ThumbStatus {
    fn as_str(self) -> &'static str {
        match self {
            ThumbStatus::Ok => "ok",
            ThumbStatus::Failed => "failed",
            ThumbStatus::Unsupported => "unsupported",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "ok" => ThumbStatus::Ok,
            "unsupported" => ThumbStatus::Unsupported,
            _ => ThumbStatus::Failed,
        }
    }
}

/// `ThumbRow::pack` of a thumbnail kept by the source itself (a shard stores its items'
/// thumbnails next to them) rather than in the bundle's pack files.
pub const THUMB_IN_SOURCE: u32 = u32::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct ThumbRow {
    pub status: ThumbStatus,
    pub pack: u32,
    pub offset: u64,
    pub len: u32,
    pub width: u32,
    pub height: u32,
    pub mime: String,
}

impl ThumbRow {
    pub fn without_data(status: ThumbStatus) -> Self {
        Self { status, pack: 0, offset: 0, len: 0, width: 0, height: 0, mime: String::new() }
    }
}

/// A value for an extension field. Enum fields may hold several per item.
/// Serialized as a plain JSON number or string.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ExtValue {
    Int(i64),
    Real(f64),
    Text(String),
}

/// Rows of a folder and every folder below it (`?1` source, `?2` folder path).
const FOLDER_AND_BELOW: &str = "(path = ?2 OR substr(path, 1, length(?2) + 1) = ?2 || '/')";

/// A change to an extension field of a folder (`Db::edit_folder_values`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", content = "value", rename_all = "snake_case")]
pub enum FolderEdit {
    /// Replaces the values (a single value, or none to clear).
    Set(Vec<ExtValue>),
    /// Adds one value of a multi-valued (enum) field.
    Add(ExtValue),
    /// Removes one value of a multi-valued (enum) field.
    Remove(ExtValue),
}

struct Bind<'a>(&'a SqlValue);

impl ToSql for Bind<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self.0 {
            SqlValue::Int(i) => ToSqlOutput::from(*i),
            SqlValue::Real(r) => ToSqlOutput::from(*r),
            SqlValue::Text(t) => ToSqlOutput::from(t.as_str()),
        })
    }
}

/// How the section keys of a search are to be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// The order has no meaningful groups.
    #[default]
    None,
    /// Local days since 1970-01-01 (`""` = no date).
    Day,
    /// Text shown as is (folder path, first letter, extension...).
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SectionMark {
    /// Position in the result where the section starts.
    pub index: u32,
    pub key: String,
}

/// Where the result's most significant sort key changes value.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sections {
    pub kind: SectionKind,
    pub marks: Vec<SectionMark>,
}

/// The SQL expression giving each row's section key for `query`'s most significant sort.
fn section_expr(query: &Query, reg: &FieldRegistry, ctx: &CompileContext, scope: &Scope) -> (SectionKind, String) {
    let none = (SectionKind::None, "NULL".to_string());
    let mut sorts = query.sorts.iter().rev().map(|s| s.field.as_str());
    let Some(mut key) = sorts.next() else { return none };
    // Within one source the source sort is a no-op: group by what comes next.
    if key == "source" && scope.source_id.is_some() {
        match sorts.next() {
            Some(k) => key = k,
            None => return none,
        }
    }
    let folder = |with_source: bool| {
        let path = "CASE WHEN m.parent_path = '' THEN '/' ELSE m.parent_path END";
        if with_source { format!("src.name || ' › ' || {path}") } else { path.to_string() }
    };
    match key {
        "folder" => (SectionKind::Text, folder(scope.source_id.is_none())),
        // Sources sorted, then their folders: one section per folder.
        "source" if sorts.next() == Some("folder") => (SectionKind::Text, folder(true)),
        "source" => (SectionKind::Text, "src.name".into()),
        "name" => (SectionKind::Text, "upper(substr(m.name, 1, 1))".into()),
        "ext" => (SectionKind::Text, "upper(m.ext)".into()),
        "kind" => (SectionKind::Text, "m.kind".into()),
        _ => match reg.by_key(key) {
            Some(FieldDef { ty: FieldType::Date, source: vp_query::FieldSource::Column { expr }, .. }) => {
                let offset = ctx.tz_offset_minutes as i64 * 60_000;
                (SectionKind::Day, format!("(({expr}) + {offset}) / 86400000"))
            }
            _ => none,
        },
    }
}

/// Where a search looks.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub source_id: Option<i64>,
    /// A folder of `source_id`, relative to its root (`""` = root).
    pub folder: Option<String>,
    /// Include subfolders of `folder`.
    pub recursive: bool,
    /// Sources left out (e.g. ones whose storage is not connected).
    pub exclude_sources: Vec<i64>,
    /// Folders `(source, folder)` left out unless the search is limited to that source
    /// (replica mirrors' copies, `Db::replica_folders`). `""` is the whole source.
    pub exclude_folders: Vec<(i64, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct DbOptions {
    /// Open a database whose schema is newer than this build, without migrating it.
    pub allow_newer_schema: bool,
}

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with(path, &DbOptions::default())
    }

    pub fn open_with(path: &Path, opts: &DbOptions) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn, opts)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, &DbOptions::default())
    }

    /// An in-memory database loaded from `bytes` (a serialized database file), or empty.
    /// Used for encrypted bundles so the decrypted database never touches the disk.
    pub fn open_memory_from(bytes: Option<&[u8]>, opts: &DbOptions) -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        if let Some(bytes) = bytes.filter(|b| !b.is_empty()) {
            // A file that was in WAL mode says so in header bytes 18-19; an in-memory
            // database cannot use WAL, so mark it as a rollback-journal database.
            let mut bytes = bytes.to_vec();
            if bytes.len() >= 20 && bytes.starts_with(b"SQLite format 3\0") {
                bytes[18] = 1;
                bytes[19] = 1;
            }
            conn.deserialize_read_exact(rusqlite::MAIN_DB, bytes.as_slice(), bytes.len(), false)?;
        }
        Self::init(conn, opts)
    }

    /// The whole database as the bytes of a database file.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        Ok(self.conn.serialize(rusqlite::MAIN_DB)?.to_vec())
    }

    /// Changes made through this connection since it opened; used to tell whether an
    /// in-memory database needs saving.
    pub fn total_changes(&self) -> u64 {
        self.conn.total_changes()
    }

    fn init(mut conn: Connection, opts: &DbOptions) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.create_collation(collation::NATURAL, collation::natural_cmp)?;
        schema::migrate(&mut conn, opts.allow_newer_schema)?;
        Ok(Self { conn })
    }

    /// Writes a consistent copy of the database to `dest` (works while in use).
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        let _ = std::fs::remove_file(dest);
        self.conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Base fields plus every extension field stored in this library.
    pub fn field_registry(&self) -> Result<FieldRegistry> {
        let mut reg = FieldRegistry::with_base_fields();
        let mut stmt = self.conn.prepare("SELECT id, key, names, aliases, ty FROM ext_field ORDER BY sort_index, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, String>(4)?))
        })?;
        for row in rows {
            let (id, key, names, aliases, ty) = row?;
            let ty: FieldType = serde_json::from_str(&ty)?;
            let mut def = FieldDef::ext(&key, ty, id);
            def.names = serde_json::from_str(&names)?;
            def.aliases = serde_json::from_str(&aliases)?;
            reg.add(def)?;
        }
        Ok(reg)
    }

    /// Creates a user-defined extension field and returns its key.
    /// Fails if a name collides with an existing field or a query keyword.
    pub fn create_ext_field(&mut self, names: &[(String, String)], aliases: &[String], ty: FieldType) -> Result<String> {
        // Validate against the current registry before touching the database.
        let mut reg = self.field_registry()?;
        let tx = self.conn.transaction()?;
        let next: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ext_field", [], |r| r.get(0))?;
        let key = format!("u.{next}");
        let mut def = FieldDef::ext(&key, ty, next);
        def.names = names.to_vec();
        def.aliases = aliases.to_vec();
        reg.add(def)?;
        tx.execute(
            "INSERT INTO ext_field (id, key, names, aliases, ty, sort_index) VALUES (?1, ?2, ?3, ?4, ?5, ?1)",
            params![next, key, serde_json::to_string(names)?, serde_json::to_string(aliases)?, serde_json::to_string(&ty)?],
        )?;
        tx.commit()?;
        Ok(key)
    }

    pub fn add_source(&self, s: &NewSource) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO source (name, driver, config, root, sort_index)
             VALUES (?1, ?2, ?3, ?4, (SELECT COALESCE(MAX(sort_index), -1) + 1 FROM source))",
            params![s.name, s.driver, s.config.to_string(), s.root],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Inserts or updates the item at `(source_id, rel_path)` and returns its id.
    pub fn upsert_media(&self, m: &NewMedia) -> Result<i64> {
        let (parent, name) = match m.rel_path.rsplit_once('/') {
            Some((p, n)) => (p, n),
            None => ("", m.rel_path.as_str()),
        };
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        let id = self.conn.query_row(
            "INSERT INTO media (source_id, rel_path, parent_path, name, ext, kind, size,
                                created_ms, modified_ms, added_ms, duration_ms, width, height, seen_gen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT (source_id, rel_path) DO UPDATE SET
                kind = excluded.kind, size = excluded.size, created_ms = excluded.created_ms,
                modified_ms = excluded.modified_ms, duration_ms = excluded.duration_ms,
                width = excluded.width, height = excluded.height, seen_gen = excluded.seen_gen,
                added_ms = MIN(added_ms, excluded.added_ms), missing = 0
             RETURNING id",
            params![
                m.source_id,
                m.rel_path,
                parent,
                name,
                ext,
                m.kind.as_str(),
                m.size as i64,
                m.created_ms,
                m.modified_ms,
                m.added_ms,
                m.duration_ms,
                m.width,
                m.height,
                m.seen_gen
            ],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    fn ext_field(&self, key: &str) -> Result<(i64, FieldType)> {
        self.conn
            .query_row("SELECT id, ty FROM ext_field WHERE key = ?1", [key], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?
            .map(|(id, ty)| Ok((id, serde_json::from_str(&ty)?)))
            .unwrap_or_else(|| Err(DbError::UnknownField(key.into())))
    }

    /// Replaces the values of an extension field on one item.
    pub fn set_ext_values(&mut self, media_id: i64, field_key: &str, values: &[ExtValue], origin: &str) -> Result<()> {
        let (field_id, _ty) = self.ext_field(field_key)?;
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM ext_value WHERE media_id = ?1 AND field_id = ?2", params![media_id, field_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO ext_value (media_id, field_id, int_val, real_val, text_val, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for v in values {
                let (i, r, t) = match v {
                    ExtValue::Int(i) => (Some(*i), None, None),
                    ExtValue::Real(r) => (None, Some(*r), None),
                    ExtValue::Text(t) => (None, None, Some(t.as_str())),
                };
                stmt.execute(params![media_id, field_id, i, r, t, origin])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Runs a query over enabled sources and returns matching media ids in result order.
    pub fn search(&self, query: &Query, reg: &FieldRegistry, ctx: &CompileContext) -> Result<Vec<i64>> {
        self.search_in(query, reg, ctx, &Scope::default())
    }

    /// Like [`Db::search`], limited to a source and optionally one of its folders.
    pub fn search_in(&self, query: &Query, reg: &FieldRegistry, ctx: &CompileContext, scope: &Scope) -> Result<Vec<i64>> {
        Ok(self.search_sections(query, reg, ctx, scope)?.0)
    }

    /// Like [`Db::search_in`], also marking where the most significant sort key changes
    /// (folder, day, first letter...), for section lines on the scrollbar.
    pub fn search_sections(
        &self,
        query: &Query,
        reg: &FieldRegistry,
        ctx: &CompileContext,
        scope: &Scope,
    ) -> Result<(Vec<i64>, Sections)> {
        let (kind, expr) = section_expr(query, reg, ctx, scope);
        let mut c = vp_query::compile(query, reg, ctx)?;
        let mut source_filter = String::new();
        if let Some(id) = scope.source_id {
            c.params.push(SqlValue::Int(id));
            source_filter.push_str(&format!(" AND m.source_id = ?{}", c.params.len()));
            if let Some(folder) = &scope.folder {
                c.params.push(SqlValue::Text(folder.trim_matches('/').to_string()));
                let n = c.params.len();
                if scope.recursive {
                    source_filter.push_str(&format!(
                        " AND (?{n} = '' OR m.parent_path = ?{n} OR substr(m.parent_path, 1, length(?{n}) + 1) = ?{n} || '/')"
                    ));
                } else {
                    source_filter.push_str(&format!(" AND m.parent_path = ?{n}"));
                }
            }
        }
        if !scope.exclude_sources.is_empty() {
            let ids: Vec<String> = scope.exclude_sources.iter().map(|id| id.to_string()).collect();
            source_filter.push_str(&format!(" AND m.source_id NOT IN ({})", ids.join(",")));
        }
        for (source, folder) in scope.exclude_folders.iter().filter(|(s, _)| scope.source_id != Some(*s)) {
            c.params.push(SqlValue::Int(*source));
            let s = c.params.len();
            c.params.push(SqlValue::Text(folder.trim_matches('/').to_string()));
            let n = c.params.len();
            source_filter.push_str(&format!(
                " AND NOT (m.source_id = ?{s} AND (?{n} = '' OR m.parent_path = ?{n} OR substr(m.parent_path, 1, length(?{n}) + 1) = ?{n} || '/'))"
            ));
        }
        let sql = format!(
            "SELECT m.id, {expr} FROM media m
             JOIN source src ON src.id = m.source_id
             LEFT JOIN media_stats s ON s.media_id = m.id
             WHERE src.enabled AND NOT m.missing{source_filter} AND ({})
             ORDER BY {}",
            c.where_sql, c.order_sql
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let binds: Vec<Bind> = c.params.iter().map(Bind).collect();
        let refs: Vec<&dyn ToSql> = binds.iter().map(|b| b as &dyn ToSql).collect();
        let mut ids = Vec::new();
        let mut sections = Sections { kind, marks: Vec::new() };
        let mut last: Option<String> = None;
        let mut rows = stmt.query(refs.as_slice())?;
        while let Some(r) = rows.next()? {
            ids.push(r.get::<_, i64>(0)?);
            if kind == SectionKind::None {
                continue;
            }
            let key = match r.get_ref(1)? {
                rusqlite::types::ValueRef::Null => String::new(),
                rusqlite::types::ValueRef::Integer(i) => i.to_string(),
                rusqlite::types::ValueRef::Real(f) => (f as i64).to_string(),
                v => String::from_utf8_lossy(v.as_bytes().unwrap_or_default()).into_owned(),
            };
            if last.as_deref() != Some(key.as_str()) {
                sections.marks.push(SectionMark { index: ids.len() as u32 - 1, key: key.clone() });
                last = Some(key);
            }
        }
        Ok((ids, sections))
    }

    /// Runs `f` inside one transaction; rolls back if it fails.
    pub fn batch<T>(&self, f: impl FnOnce(&Db) -> Result<T>) -> Result<T> {
        // Inside another batch: part of its transaction.
        if !self.conn.is_autocommit() {
            return f(self);
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    pub fn sources(&self) -> Result<Vec<SourceRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, driver, config, root, enabled FROM source ORDER BY sort_index, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?, r.get(4)?, r.get(5)?))
        })?;
        rows.map(|row| {
            let (id, name, driver, config, root, enabled) = row?;
            Ok(SourceRow { id, name, driver, config: serde_json::from_str(&config)?, root, enabled })
        })
        .collect()
    }

    pub fn source(&self, id: i64) -> Result<Option<SourceRow>> {
        Ok(self.sources()?.into_iter().find(|s| s.id == id))
    }

    pub fn update_source(&self, id: i64, name: &str, root: &str, config: &serde_json::Value, enabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE source SET name = ?2, root = ?3, config = ?4, enabled = ?5 WHERE id = ?1",
            params![id, name, root, config.to_string(), enabled],
        )?;
        Ok(())
    }

    /// Direct subfolders of `parent` in a source, with how many items each contains
    /// (including deeper folders), in natural name order.
    pub fn child_folders(&self, source_id: i64, parent: &str) -> Result<Vec<(String, u64)>> {
        let parent = parent.trim_matches('/');
        let mut stmt = self.conn.prepare_cached(
            "SELECT parent_path, COUNT(*) FROM media
             WHERE source_id = ?1 AND NOT missing
               AND (?2 = '' OR substr(parent_path, 1, length(?2) + 1) = ?2 || '/')
             GROUP BY parent_path",
        )?;
        let rows = stmt.query_map(params![source_id, parent], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut counts: std::collections::BTreeMap<String, u64> = Default::default();
        for row in rows {
            let (path, n) = row?;
            let rest = if parent.is_empty() { path.as_str() } else { &path[parent.len() + 1..] };
            if rest.is_empty() {
                continue;
            }
            let child = rest.split('/').next().unwrap_or(rest).to_string();
            *counts.entry(child).or_default() += n as u64;
        }
        let mut out: Vec<(String, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| collation::natural_cmp(&a.0, &b.0));
        Ok(out)
    }

    /// Removes a source and everything recorded about its media.
    pub fn remove_source(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM source WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn known_files(&self, source_id: i64) -> Result<std::collections::HashMap<String, KnownFile>> {
        let mut stmt = self.conn.prepare("SELECT rel_path, id, size, modified_ms, width IS NOT NULL, added_ms FROM media WHERE source_id = ?1")?;
        let rows = stmt.query_map([source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                KnownFile {
                    id: r.get(1)?,
                    size: r.get::<_, i64>(2)? as u64,
                    modified_ms: r.get(3)?,
                    has_size: r.get(4)?,
                    added_ms: r.get(5)?,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Gives copies (in source `to`, by path) the added time of the items they were copied
    /// from, when that is earlier: a mirror's copy arrived when its original did.
    pub fn copy_added_to(&self, to: i64, pairs: &[(i64, String)]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE media SET added_ms = MIN(added_ms, (SELECT o.added_ms FROM media o WHERE o.id = ?1))
             WHERE source_id = ?2 AND rel_path = ?3",
        )?;
        for (from, rel) in pairs {
            stmt.execute(params![from, to, rel])?;
        }
        Ok(())
    }

    /// Moves an item's added time back to `added_ms` (the file system knew an earlier one).
    pub fn set_added(&self, media_id: i64, added_ms: i64) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE media SET added_ms = ?2 WHERE id = ?1 AND added_ms > ?2")?
            .execute(params![media_id, added_ms])?;
        Ok(())
    }

    /// Records the pixel size of an image found unchanged but scanned without one.
    pub fn set_dimensions(&self, media_id: i64, width: u32, height: u32) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE media SET width = ?2, height = ?3 WHERE id = ?1")?
            .execute(params![media_id, width, height])?;
        Ok(())
    }

    pub fn mark_seen(&self, media_id: i64, gen_id: i64) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE media SET seen_gen = ?2, missing = 0 WHERE id = ?1")?
            .execute(params![media_id, gen_id])?;
        Ok(())
    }

    /// Marks items whose file was found gone outside a scan; the next scan clears it if it is back.
    pub fn mark_missing(&self, ids: &[i64]) -> Result<()> {
        let mut stmt = self.conn.prepare_cached("UPDATE media SET missing = 1 WHERE id = ?1")?;
        for id in ids {
            stmt.execute([id])?;
        }
        Ok(())
    }

    /// Marks files of `source_id` not seen in scan `gen_id` as missing; returns how many.
    /// Missing items keep their tags and stats in case the file comes back.
    pub fn finish_scan(&self, source_id: i64, gen_id: i64) -> Result<usize> {
        self.conn
            .execute("UPDATE media SET missing = (seen_gen <> ?2) WHERE source_id = ?1", params![source_id, gen_id])?;
        let n: i64 =
            self.conn.query_row("SELECT COUNT(*) FROM media WHERE source_id = ?1 AND missing", [source_id], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// Tile data for `ids`, in the same order. Unknown ids are skipped.
    pub fn media_brief(&self, ids: &[i64]) -> Result<Vec<MediaBrief>> {
        let mut by_id = std::collections::HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT m.id, m.name, m.kind, m.width, m.height, m.duration_ms, t.status,
                        p.compatible, v.media_id IS NOT NULL,
                        EXISTS (SELECT 1 FROM media_flag f WHERE f.media_id = m.id AND f.flag = '{FLAG_IGNORE_PLAYBACK_WARNING}'),
                        m.size, m.created_ms, m.modified_ms, m.parent_path, m.ext, m.source_id,
                        EXISTS (SELECT 1 FROM ext_value e JOIN ext_field xf ON xf.id = e.field_id
                                WHERE e.media_id = m.id AND xf.key = '{FAVORITE_FIELD}' AND e.int_val = 1),
                        m.added_ms
                 FROM media m LEFT JOIN thumbnail t ON t.media_id = m.id
                 LEFT JOIN media_probe p ON p.media_id = m.id AND p.ok
                 LEFT JOIN media_variant v ON v.media_id = m.id
                 WHERE m.id IN ({marks})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok(MediaBrief {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    kind: r.get(2)?,
                    width: r.get(3)?,
                    height: r.get(4)?,
                    duration_ms: r.get(5)?,
                    thumb: r.get(6)?,
                    compatible: r.get(7)?,
                    has_variant: r.get(8)?,
                    ignore_warning: r.get(9)?,
                    size: r.get::<_, i64>(10)? as u64,
                    created_ms: r.get(11)?,
                    modified_ms: r.get(12)?,
                    folder: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
                    ext: r.get::<_, Option<String>>(14)?.unwrap_or_default(),
                    source_id: r.get(15)?,
                    favorite: r.get(16)?,
                    added_ms: r.get(17)?,
                })
            })?;
            for row in rows {
                let b = row?;
                by_id.insert(b.id, b);
            }
        }
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    pub fn media_location(&self, id: i64) -> Result<Option<MediaLocation>> {
        Ok(self
            .conn
            .prepare_cached("SELECT source_id, rel_path, kind, size FROM media WHERE id = ?1")?
            .query_row([id], |r| {
                let kind: String = r.get(2)?;
                Ok(MediaLocation {
                    source_id: r.get(0)?,
                    rel_path: r.get(1)?,
                    kind: MediaKind::parse(&kind).unwrap_or(MediaKind::NonMedia),
                    size: r.get::<_, i64>(3)? as u64,
                })
            })
            .optional()?)
    }

    /// Where items are and their pixel size (when the scan read it), for the viewer.
    pub fn media_view_locations(&self, ids: &[i64]) -> Result<Vec<(i64, MediaLocation, Option<(u32, u32)>)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, source_id, rel_path, kind, size, width, height FROM media
             WHERE id IN (SELECT value FROM json_each(?1))",
        )?;
        let ids = serde_json::to_string(ids).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        let rows = stmt.query_map([ids], |r| {
            let kind: String = r.get(3)?;
            let loc = MediaLocation {
                source_id: r.get(1)?,
                rel_path: r.get(2)?,
                kind: MediaKind::parse(&kind).unwrap_or(MediaKind::NonMedia),
                size: r.get::<_, i64>(4)? as u64,
            };
            Ok((r.get(0)?, loc, r.get::<_, Option<u32>>(5)?.zip(r.get::<_, Option<u32>>(6)?)))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Pixel size of an item, when the scan read it.
    pub fn media_dimensions(&self, id: i64) -> Result<Option<(u32, u32)>> {
        Ok(self
            .conn
            .prepare_cached("SELECT width, height FROM media WHERE id = ?1")?
            .query_row([id], |r| Ok(r.get::<_, Option<u32>>(0)?.zip(r.get::<_, Option<u32>>(1)?)))
            .optional()?
            .flatten())
    }

    pub fn thumbnail(&self, media_id: i64) -> Result<Option<ThumbRow>> {
        Ok(self
            .conn
            .prepare_cached("SELECT status, pack, offset, len, width, height, mime FROM thumbnail WHERE media_id = ?1")?
            .query_row([media_id], |r| {
                Ok(ThumbRow {
                    status: ThumbStatus::parse(&r.get::<_, String>(0)?),
                    pack: r.get::<_, Option<u32>>(1)?.unwrap_or(0),
                    offset: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    len: r.get::<_, Option<u32>>(3)?.unwrap_or(0),
                    width: r.get::<_, Option<u32>>(4)?.unwrap_or(0),
                    height: r.get::<_, Option<u32>>(5)?.unwrap_or(0),
                    mime: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                })
            })
            .optional()?)
    }

    pub fn set_thumbnail(&self, media_id: i64, t: &ThumbRow) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO thumbnail (media_id, status, pack, offset, len, width, height, mime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?
            .execute(params![media_id, t.status.as_str(), t.pack, t.offset as i64, t.len, t.width, t.height, t.mime])?;
        Ok(())
    }

    /// Present items on enabled sources that have no thumbnail attempt yet.
    /// Items of sources in `skip` (e.g. locked shards) are left out.
    pub fn missing_thumbnails(&self, limit: usize, skip: &[i64]) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id FROM media m JOIN source src ON src.id = m.source_id
             LEFT JOIN thumbnail t ON t.media_id = m.id
             WHERE t.media_id IS NULL AND NOT m.missing AND src.enabled
               AND m.source_id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY m.id LIMIT ?1",
        )?;
        let skip = serde_json::to_string(skip)?;
        let ids = stmt.query_map(params![limit as i64, skip], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Number of items [`Db::missing_thumbnails`] would eventually return.
    pub fn count_missing_thumbnails(&self, skip: &[i64]) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM media m JOIN source src ON src.id = m.source_id
             LEFT JOIN thumbnail t ON t.media_id = m.id
             WHERE t.media_id IS NULL AND NOT m.missing AND src.enabled
               AND m.source_id NOT IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(skip)?],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Records one view of `media_id` lasting `view_ms`.
    ///
    /// Popularity is a frecency score that halves every `half_life_ms` without views. It is
    /// stored as `log2(score) + time / half_life`, which keeps the ordering correct over time
    /// without rewriting every row: higher stored value = more popular right now.
    pub fn record_view(&self, media_id: i64, now_ms: i64, view_ms: i64, half_life_ms: f64) -> Result<()> {
        let hl = half_life_ms.max(1.0);
        let old: Option<f64> = self
            .conn
            .query_row("SELECT score FROM media_stats WHERE media_id = ?1 AND view_count > 0", [media_id], |r| r.get(0))
            .optional()?;
        let now_term = now_ms as f64 / hl;
        let current = old.map(|k| (k - now_term).exp2()).unwrap_or(0.0);
        let key = (current + 1.0).log2() + now_term;
        self.conn.execute(
            "INSERT INTO media_stats (media_id, view_count, total_view_ms, last_viewed_ms, score)
             VALUES (?1, 1, ?2, ?3, ?4)
             ON CONFLICT (media_id) DO UPDATE SET
                view_count = view_count + 1,
                total_view_ms = total_view_ms + excluded.total_view_ms,
                last_viewed_ms = excluded.last_viewed_ms,
                score = excluded.score",
            params![media_id, view_ms, now_ms, key],
        )?;
        Ok(())
    }

    /// Most popular present items as `(id, size)`, best first.
    pub fn most_popular(&self, limit: usize) -> Result<Vec<(i64, u64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.id, m.size FROM media_stats s JOIN media m ON m.id = s.media_id
             JOIN source src ON src.id = m.source_id
             WHERE s.view_count > 0 AND NOT m.missing AND src.enabled
             ORDER BY s.score DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn ext_fields(&self) -> Result<Vec<FieldInfo>> {
        let mut stmt =
            self.conn.prepare("SELECT key, names, aliases, ty, builtin FROM ext_field ORDER BY sort_index, id")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (key, names, aliases, ty, builtin) = row?;
            Ok(FieldInfo {
                key,
                names: serde_json::from_str(&names)?,
                aliases: serde_json::from_str(&aliases)?,
                ty: serde_json::from_str(&ty)?,
                builtin,
            })
        })
        .collect()
    }

    /// Deletes a user-defined field and all its values. Built-in fields cannot be deleted.
    pub fn delete_ext_field(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM ext_field WHERE key = ?1 AND builtin = 0", [key])?;
        Ok(())
    }

    /// Renames a field. Fails if a new name collides with another field or a keyword.
    pub fn rename_ext_field(&self, key: &str, names: &[(String, String)], aliases: &[String]) -> Result<()> {
        let (id, ty) = self.ext_field(key)?;
        let mut reg = FieldRegistry::with_base_fields();
        for f in self.ext_fields()?.into_iter().filter(|f| f.key != key) {
            let mut def = FieldDef::ext(&f.key, f.ty, 0);
            def.names = f.names;
            def.aliases = f.aliases;
            reg.add(def)?;
        }
        let mut def = FieldDef::ext(key, ty, id);
        def.names = names.to_vec();
        def.aliases = aliases.to_vec();
        reg.add(def)?;
        self.conn.execute(
            "UPDATE ext_field SET names = ?2, aliases = ?3 WHERE id = ?1",
            params![id, serde_json::to_string(names)?, serde_json::to_string(aliases)?],
        )?;
        Ok(())
    }

    pub fn media_detail(&self, id: i64) -> Result<Option<MediaDetail>> {
        let detail = self
            .conn
            .query_row(
                "SELECT m.id, m.source_id, src.name, m.rel_path, m.name, m.kind, m.size, m.created_ms,
                        m.modified_ms, m.added_ms, m.duration_ms, m.width, m.height,
                        COALESCE(s.view_count, 0), s.last_viewed_ms
                 FROM media m JOIN source src ON src.id = m.source_id
                 LEFT JOIN media_stats s ON s.media_id = m.id WHERE m.id = ?1",
                [id],
                |r| {
                    Ok(MediaDetail {
                        id: r.get(0)?,
                        source_id: r.get(1)?,
                        source_name: r.get(2)?,
                        rel_path: r.get(3)?,
                        name: r.get(4)?,
                        kind: r.get(5)?,
                        size: r.get::<_, i64>(6)? as u64,
                        created_ms: r.get(7)?,
                        modified_ms: r.get(8)?,
                        added_ms: r.get(9)?,
                        duration_ms: r.get(10)?,
                        width: r.get(11)?,
                        height: r.get(12)?,
                        view_count: r.get(13)?,
                        last_viewed_ms: r.get(14)?,
                        values: Default::default(),
                    })
                },
            )
            .optional()?;
        let Some(mut detail) = detail else { return Ok(None) };
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.key, v.int_val, v.real_val, v.text_val FROM ext_value v
             JOIN ext_field f ON f.id = v.field_id WHERE v.media_id = ?1 ORDER BY v.id",
        )?;
        let rows = stmt.query_map([id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<String>>(3)?))
        })?;
        for row in rows {
            let (key, i, f, t) = row?;
            let v = match (i, f, t) {
                (Some(i), _, _) => ExtValue::Int(i),
                (_, Some(f), _) => ExtValue::Real(f),
                (_, _, Some(t)) => ExtValue::Text(t),
                _ => continue,
            };
            detail.values.entry(key).or_default().push(v);
        }
        Ok(Some(detail))
    }

    /// Replaces a field's values on every item in `ids` (one transaction).
    pub fn set_values_many(&self, ids: &[i64], key: &str, values: &[ExtValue], origin: &str) -> Result<()> {
        let (field_id, ty) = self.ext_field(key)?;
        let values = coerce_values(ty, values, key)?;
        self.batch(|db| {
            for &id in ids {
                db.conn.execute("DELETE FROM ext_value WHERE media_id = ?1 AND field_id = ?2", params![id, field_id])?;
                for v in &values {
                    db.insert_value(id, field_id, v, origin)?;
                }
            }
            Ok(())
        })
    }

    /// Adds one value of a multi-valued (enum) field to every item that lacks it.
    pub fn add_value_many(&self, ids: &[i64], key: &str, value: &ExtValue, origin: &str) -> Result<()> {
        let (field_id, ty) = self.ext_field(key)?;
        let value = coerce_values(ty, std::slice::from_ref(value), key)?.remove(0);
        let ExtValue::Text(text) = &value else { return Err(DbError::InvalidValue(key.into())) };
        self.batch(|db| {
            for &id in ids {
                let exists: bool = db.conn.query_row(
                    "SELECT EXISTS (SELECT 1 FROM ext_value WHERE media_id = ?1 AND field_id = ?2
                                    AND text_val = ?3 COLLATE NOCASE)",
                    params![id, field_id, text],
                    |r| r.get(0),
                )?;
                if !exists {
                    db.insert_value(id, field_id, &value, origin)?;
                }
            }
            db.conn.execute(
                "INSERT OR IGNORE INTO ext_enum_option (field_id, value) VALUES (?1, ?2)",
                params![field_id, text],
            )?;
            Ok(())
        })
    }

    pub fn remove_value_many(&self, ids: &[i64], key: &str, value: &ExtValue) -> Result<()> {
        let (field_id, _) = self.ext_field(key)?;
        let ExtValue::Text(text) = value else { return Err(DbError::InvalidValue(key.into())) };
        self.batch(|db| {
            for &id in ids {
                db.conn.execute(
                    "DELETE FROM ext_value WHERE media_id = ?1 AND field_id = ?2 AND text_val = ?3 COLLATE NOCASE",
                    params![id, field_id, text],
                )?;
            }
            Ok(())
        })
    }

    /// Values already used for a text or enum field, most used first, for suggestions.
    pub fn suggest_values(&self, key: &str, prefix: &str, limit: usize) -> Result<Vec<String>> {
        let (field_id, _) = self.ext_field(key)?;
        let pattern = format!("{}%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = self.conn.prepare_cached(
            "SELECT value FROM (
                SELECT text_val AS value, COUNT(*) AS n FROM ext_value
                WHERE field_id = ?1 AND text_val LIKE ?2 ESCAPE '\\' GROUP BY text_val COLLATE NOCASE
                UNION ALL
                SELECT value, 0 FROM ext_enum_option WHERE field_id = ?1 AND value LIKE ?2 ESCAPE '\\'
             ) GROUP BY value COLLATE NOCASE ORDER BY MAX(n) DESC, value LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![field_id, pattern, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn insert_value(&self, media_id: i64, field_id: i64, v: &ExtValue, origin: &str) -> Result<()> {
        let (i, r, t) = match v {
            ExtValue::Int(i) => (Some(*i), None, None),
            ExtValue::Real(r) => (None, Some(*r), None),
            ExtValue::Text(t) => (None, None, Some(t.as_str())),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO ext_value (media_id, field_id, int_val, real_val, text_val, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?
            .execute(params![media_id, field_id, i, r, t, origin])?;
        Ok(())
    }

    /// Saved queries as `(id, label, query)`, in display order.
    pub fn saved_queries(&self) -> Result<Vec<(i64, String, Query)>> {
        let mut stmt = self.conn.prepare("SELECT id, label, query FROM saved_query ORDER BY sort_index, id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, String>(2)?)))?;
        rows.map(|row| {
            let (id, label, json) = row?;
            Ok((id, label, serde_json::from_str(&json)?))
        })
        .collect()
    }

    /// Creates (`id = None`) or updates a saved query; returns its id.
    pub fn save_query(&self, id: Option<i64>, label: &str, query: &Query, now_ms: i64) -> Result<i64> {
        let json = serde_json::to_string(query)?;
        match id {
            Some(id) => {
                self.conn.execute(
                    "UPDATE saved_query SET label = ?2, query = ?3, updated_ms = ?4 WHERE id = ?1",
                    params![id, label, json, now_ms],
                )?;
                Ok(id)
            }
            None => {
                self.conn.execute(
                    "INSERT INTO saved_query (label, query, sort_index, created_ms, updated_ms)
                     VALUES (?1, ?2, (SELECT COALESCE(MAX(sort_index), -1) + 1 FROM saved_query), ?3, ?3)",
                    params![label, json, now_ms],
                )?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    pub fn delete_saved_query(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM saved_query WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Videos that have not been probed yet.
    /// Items of sources in `skip` (e.g. locked shards) are left out.
    pub fn unprobed_videos(&self, limit: usize, skip: &[i64]) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id FROM media m JOIN source src ON src.id = m.source_id
             LEFT JOIN media_probe p ON p.media_id = m.id
             WHERE m.kind = 'video' AND p.media_id IS NULL AND NOT m.missing AND src.enabled
               AND m.source_id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY m.id LIMIT ?1",
        )?;
        let skip = serde_json::to_string(skip)?;
        let ids = stmt.query_map(params![limit as i64, skip], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Number of videos [`Db::unprobed_videos`] would eventually return.
    pub fn count_unprobed_videos(&self, skip: &[i64]) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM media m JOIN source src ON src.id = m.source_id
             LEFT JOIN media_probe p ON p.media_id = m.id
             WHERE m.kind = 'video' AND p.media_id IS NULL AND NOT m.missing AND src.enabled
               AND m.source_id NOT IN (SELECT value FROM json_each(?1))",
            [serde_json::to_string(skip)?],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Stores a probe result; on success also fills the item's duration and dimensions.
    #[allow(clippy::too_many_arguments)]
    pub fn set_probe(
        &self,
        media_id: i64,
        ok: bool,
        container: Option<&str>,
        video_codec: Option<&str>,
        audio_codec: Option<&str>,
        compatible: bool,
        meta: Option<(Option<i64>, Option<u32>, Option<u32>)>,
        now_ms: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO media_probe (media_id, ok, container, video_codec, audio_codec, compatible, probed_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![media_id, ok, container, video_codec, audio_codec, compatible, now_ms],
        )?;
        if let Some((duration, w, h)) = meta {
            self.conn.execute(
                "UPDATE media SET duration_ms = COALESCE(?2, duration_ms), width = COALESCE(?3, width),
                                  height = COALESCE(?4, height) WHERE id = ?1",
                params![media_id, duration, w, h],
            )?;
        }
        Ok(())
    }

    pub fn probe_info(&self, media_id: i64) -> Result<Option<ProbeRow>> {
        Ok(self
            .conn
            .prepare_cached("SELECT ok, container, video_codec, audio_codec, compatible FROM media_probe WHERE media_id = ?1")?
            .query_row([media_id], |r| {
                Ok(ProbeRow {
                    ok: r.get(0)?,
                    container: r.get(1)?,
                    video_codec: r.get(2)?,
                    audio_codec: r.get(3)?,
                    compatible: r.get(4)?,
                })
            })
            .optional()?)
    }

    pub fn variant(&self, media_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .prepare_cached("SELECT rel_path FROM media_variant WHERE media_id = ?1")?
            .query_row([media_id], |r| r.get(0))
            .optional()?)
    }

    pub fn set_variant(&self, media_id: i64, rel_path: &str, size: u64, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO media_variant (media_id, rel_path, size, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![media_id, rel_path, size as i64, now_ms],
        )?;
        Ok(())
    }

    /// Probed videos that will not play natively, have no converted copy and are not ignored.
    pub fn incompatible_videos(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT m.id FROM media m JOIN media_probe p ON p.media_id = m.id AND p.ok AND NOT p.compatible
             JOIN source src ON src.id = m.source_id
             LEFT JOIN media_variant v ON v.media_id = m.id
             WHERE v.media_id IS NULL AND NOT m.missing AND src.enabled
               AND NOT EXISTS (SELECT 1 FROM media_flag f WHERE f.media_id = m.id AND f.flag = '{FLAG_IGNORE_PLAYBACK_WARNING}')
             ORDER BY m.id"
        ))?;
        let ids = stmt.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    pub fn set_flag(&self, media_id: i64, flag: &str, on: bool) -> Result<()> {
        if on {
            self.conn.execute("INSERT OR IGNORE INTO media_flag (media_id, flag) VALUES (?1, ?2)", params![media_id, flag])?;
        } else {
            self.conn.execute("DELETE FROM media_flag WHERE media_id = ?1 AND flag = ?2", params![media_id, flag])?;
        }
        Ok(())
    }

    /// Forgets failed or unsupported video thumbnails so they are retried (e.g. once FFmpeg is found).
    pub fn reset_video_thumbnails(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM thumbnail WHERE status <> 'ok' AND media_id IN (SELECT id FROM media WHERE kind = 'video')",
            [],
        )?)
    }

    /// Forgets images whose thumbnail was recorded as unsupported (HEIF/HEIC before an
    /// image decoder was available), so they are tried again.
    pub fn reset_unsupported_image_thumbnails(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM thumbnail WHERE status = 'unsupported' AND media_id IN (SELECT id FROM media WHERE kind = 'image')",
            [],
        )?)
    }

    /// Forgets every thumbnail that could not be made for images with one of `exts`
    /// (lowercase), failed ones too: a new decoder for them (libheif) may read what the
    /// previous ones could not.
    pub fn reset_image_thumbnails_with_ext(&self, exts: &[&str]) -> Result<usize> {
        let mut n = 0;
        for ext in exts {
            n += self.conn.execute(
                "DELETE FROM thumbnail WHERE status <> 'ok' AND media_id IN (SELECT id FROM media WHERE kind = 'image' AND ext = ?1)",
                [ext.to_ascii_lowercase()],
            )?;
        }
        Ok(n)
    }

    pub fn loop_segments(&self, media_id: i64) -> Result<Vec<LoopSegment>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, media_id, label, start_ms, end_ms, autoplay FROM loop_segment
             WHERE media_id = ?1 ORDER BY sort_index, start_ms, id",
        )?;
        let rows = stmt.query_map([media_id], |r| {
            Ok(LoopSegment {
                id: r.get(0)?,
                media_id: r.get(1)?,
                label: r.get(2)?,
                start_ms: r.get(3)?,
                end_ms: r.get(4)?,
                autoplay: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Creates (`seg.id == 0`) or updates a segment and returns its id. Only one segment per
    /// video can auto-play; setting it clears the others.
    pub fn save_loop_segment(&self, seg: &LoopSegment) -> Result<i64> {
        if seg.end_ms <= seg.start_ms || seg.start_ms < 0 {
            return Err(DbError::InvalidValue("loop segment".into()));
        }
        self.batch(|db| {
            if seg.autoplay {
                db.conn.execute("UPDATE loop_segment SET autoplay = 0 WHERE media_id = ?1", [seg.media_id])?;
            }
            let id = if seg.id == 0 {
                db.conn.execute(
                    "INSERT INTO loop_segment (media_id, label, start_ms, end_ms, autoplay) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![seg.media_id, seg.label, seg.start_ms, seg.end_ms, seg.autoplay],
                )?;
                db.conn.last_insert_rowid()
            } else {
                db.conn.execute(
                    "UPDATE loop_segment SET label = ?2, start_ms = ?3, end_ms = ?4, autoplay = ?5 WHERE id = ?1",
                    params![seg.id, seg.label, seg.start_ms, seg.end_ms, seg.autoplay],
                )?;
                seg.id
            };
            Ok(id)
        })
    }

    pub fn delete_loop_segment(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM loop_segment WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Copies what is recorded about the items of source `from` (field values, statistics,
    /// flags, loop segments, probe results, sizes) to the items of source `to` found at
    /// `prefix + rel_path`, replacing what those had. Used after files were copied into
    /// another source. Returns how many items matched.
    pub fn copy_media_metadata(&self, from: i64, to: i64, prefix: &str) -> Result<usize> {
        self.batch(|db| {
            db.conn.execute_batch(
                "DROP TABLE IF EXISTS temp.media_map;
                 CREATE TEMP TABLE media_map (src INTEGER PRIMARY KEY, dst INTEGER NOT NULL);",
            )?;
            let n = db.conn.execute(
                "INSERT INTO temp.media_map (src, dst)
                 SELECT a.id, b.id FROM media a JOIN media b ON b.source_id = ?2 AND b.rel_path = ?3 || a.rel_path
                 WHERE a.source_id = ?1",
                params![from, to, prefix],
            )?;
            db.copy_mapped_metadata()?;
            Ok(n)
        })
    }

    /// Like [`Db::copy_media_metadata`] for single items: each `(src, rel_path)` pair copies
    /// item `src`'s values to the item at `rel_path` in source `to`. Returns how many matched.
    pub fn copy_metadata_to(&self, to: i64, pairs: &[(i64, String)]) -> Result<usize> {
        self.batch(|db| {
            db.conn.execute_batch(
                "DROP TABLE IF EXISTS temp.media_map;
                 CREATE TEMP TABLE media_map (src INTEGER PRIMARY KEY, dst INTEGER NOT NULL);",
            )?;
            let mut n = 0;
            {
                let mut stmt = db.conn.prepare(
                    "INSERT OR IGNORE INTO temp.media_map (src, dst) SELECT ?1, id FROM media WHERE source_id = ?2 AND rel_path = ?3",
                )?;
                for (src, rel) in pairs {
                    n += stmt.execute(params![src, to, rel])?;
                }
            }
            db.copy_mapped_metadata()?;
            Ok(n)
        })
    }

    /// Copies values from `temp.media_map`'s `src` items to its `dst` items, then drops the map.
    fn copy_mapped_metadata(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM ext_value WHERE media_id IN (SELECT dst FROM temp.media_map);
             INSERT INTO ext_value (media_id, field_id, int_val, real_val, text_val, origin)
                SELECT m.dst, e.field_id, e.int_val, e.real_val, e.text_val, e.origin
                FROM ext_value e JOIN temp.media_map m ON m.src = e.media_id;
             INSERT OR REPLACE INTO media_stats (media_id, view_count, total_view_ms, last_viewed_ms, score)
                SELECT m.dst, s.view_count, s.total_view_ms, s.last_viewed_ms, s.score
                FROM media_stats s JOIN temp.media_map m ON m.src = s.media_id;
             INSERT OR IGNORE INTO media_flag (media_id, flag)
                SELECT m.dst, f.flag FROM media_flag f JOIN temp.media_map m ON m.src = f.media_id;
             DELETE FROM loop_segment WHERE media_id IN (SELECT dst FROM temp.media_map);
             INSERT INTO loop_segment (media_id, label, start_ms, end_ms, sort_index, autoplay)
                SELECT m.dst, l.label, l.start_ms, l.end_ms, l.sort_index, l.autoplay
                FROM loop_segment l JOIN temp.media_map m ON m.src = l.media_id;
             INSERT OR REPLACE INTO media_probe (media_id, ok, container, video_codec, audio_codec, compatible, probed_ms)
                SELECT m.dst, p.ok, p.container, p.video_codec, p.audio_codec, p.compatible, p.probed_ms
                FROM media_probe p JOIN temp.media_map m ON m.src = p.media_id;
             UPDATE media SET
                duration_ms = COALESCE(duration_ms, (SELECT a.duration_ms FROM media a JOIN temp.media_map m ON m.src = a.id WHERE m.dst = media.id)),
                width = COALESCE(width, (SELECT a.width FROM media a JOIN temp.media_map m ON m.src = a.id WHERE m.dst = media.id)),
                height = COALESCE(height, (SELECT a.height FROM media a JOIN temp.media_map m ON m.src = a.id WHERE m.dst = media.id))
             WHERE id IN (SELECT dst FROM temp.media_map);
             DROP TABLE temp.media_map;",
        )?;
        Ok(())
    }

    /// Moves item `id` to `rel_path` in source `to`, keeping its id and everything recorded
    /// about it (used when its file moved into another source). An item already at the
    /// destination is replaced.
    pub fn move_media(&self, id: i64, to: i64, rel_path: &str) -> Result<()> {
        let (parent, name) = rel_path.rsplit_once('/').unwrap_or(("", rel_path));
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default();
        self.conn.execute("DELETE FROM media WHERE source_id = ?1 AND rel_path = ?2 AND id <> ?3", params![to, rel_path, id])?;
        self.conn.execute(
            "UPDATE media SET source_id = ?2, rel_path = ?3, parent_path = ?4, name = ?5, ext = ?6, missing = 0 WHERE id = ?1",
            params![id, to, rel_path, parent, name, ext],
        )?;
        Ok(())
    }

    /// Ids of every item inside `folder` (any depth) of a source, missing ones included.
    pub fn folder_media(&self, source_id: i64, folder: &str) -> Result<Vec<i64>> {
        let folder = folder.trim_matches('/');
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM media WHERE source_id = ?1 AND substr(rel_path, 1, length(?2) + 1) = ?2 || '/' ORDER BY id",
        )?;
        let ids = stmt.query_map(params![source_id, folder], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Up to `limit` images or videos inside `folder` (any depth) to stand for it, files
    /// directly in the folder first, then by path. Items whose thumbnail failed are skipped.
    pub fn folder_samples(&self, source_id: i64, folder: &str, limit: u32) -> Result<Vec<i64>> {
        let folder = folder.trim_matches('/');
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT m.id FROM media m LEFT JOIN thumbnail t ON t.media_id = m.id
             WHERE m.source_id = ?1 AND NOT m.missing AND m.kind IN ('image', 'video')
               AND substr(m.rel_path, 1, length(?2) + 1) = ?2 || '/'
               AND (t.status IS NULL OR t.status = 'ok')
             ORDER BY m.parent_path <> ?2, m.rel_path COLLATE {} LIMIT ?3",
            collation::NATURAL
        ))?;
        let ids = stmt.query_map(params![source_id, folder, limit], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(ids)
    }

    /// Records that folder `from` of a source is now at `to` (the files were moved as a
    /// whole): every item inside keeps its id. Stale rows already under `to` are dropped.
    pub fn move_folder(&mut self, source_id: i64, from: &str, to: &str) -> Result<usize> {
        let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM media WHERE source_id = ?1 AND substr(rel_path, 1, length(?2) + 1) = ?2 || '/'",
            params![source_id, to],
        )?;
        let n = tx.execute(
            "UPDATE media SET
                rel_path = ?3 || substr(rel_path, length(?2) + 1),
                parent_path = ?3 || substr(parent_path, length(?2) + 1)
             WHERE source_id = ?1 AND substr(rel_path, 1, length(?2) + 1) = ?2 || '/'",
            params![source_id, from, to],
        )?;
        tx.commit()?;
        self.move_folder_values(source_id, from, to)?;
        Ok(n)
    }

    /// Extension field values of a folder, by field key.
    pub fn folder_values(&self, source_id: i64, path: &str) -> Result<std::collections::BTreeMap<String, Vec<ExtValue>>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.key, v.int_val, v.real_val, v.text_val FROM folder_value v
             JOIN ext_field f ON f.id = v.field_id WHERE v.source_id = ?1 AND v.path = ?2 ORDER BY v.id",
        )?;
        let rows = stmt.query_map(params![source_id, path.trim_matches('/')], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<String>>(3)?))
        })?;
        let mut out = std::collections::BTreeMap::<String, Vec<ExtValue>>::new();
        for row in rows {
            let (key, i, f, t) = row?;
            let v = match (i, f, t) {
                (Some(i), _, _) => ExtValue::Int(i),
                (_, Some(f), _) => ExtValue::Real(f),
                (_, _, Some(t)) => ExtValue::Text(t),
                _ => continue,
            };
            out.entry(key).or_default().push(v);
        }
        Ok(out)
    }

    /// Changes a field of a folder. With `recursive` the change is also applied, one by
    /// one, to every item and subfolder inside: values of multi-valued (enum) fields are
    /// merged in, single values replace what was there. Later edits inside simply
    /// overwrite. Returns how many items inside were changed.
    pub fn edit_folder_values(&self, source_id: i64, path: &str, key: &str, edit: &FolderEdit, recursive: bool) -> Result<usize> {
        let path = path.trim_matches('/');
        let (field_id, ty) = self.ext_field(key)?;
        let multi = matches!(ty, FieldType::Enum);
        let edit = match edit {
            FolderEdit::Set(v) => FolderEdit::Set(coerce_values(ty, v, key)?),
            FolderEdit::Add(_) | FolderEdit::Remove(_) if !multi => return Err(DbError::InvalidValue(key.into())),
            FolderEdit::Add(v) => FolderEdit::Add(coerce_values(ty, std::slice::from_ref(v), key)?.remove(0)),
            FolderEdit::Remove(v) => FolderEdit::Remove(v.clone()),
        };
        self.batch(|db| {
            // `replace`: the folder takes exactly the values set; otherwise they are merged in.
            let folder = |p: &str, replace: bool| -> Result<()> {
                match &edit {
                    FolderEdit::Set(values) => {
                        if replace {
                            db.conn.execute(
                                "DELETE FROM folder_value WHERE source_id = ?1 AND path = ?2 AND field_id = ?3",
                                params![source_id, p, field_id],
                            )?;
                        }
                        for v in values {
                            if replace || !db.folder_has(source_id, p, field_id, v)? {
                                db.insert_folder_value(source_id, p, field_id, v)?;
                            }
                        }
                    }
                    FolderEdit::Add(v) => {
                        if !db.folder_has(source_id, p, field_id, v)? {
                            db.insert_folder_value(source_id, p, field_id, v)?;
                        }
                    }
                    FolderEdit::Remove(v) => {
                        if let ExtValue::Text(t) = v {
                            db.conn.execute(
                                "DELETE FROM folder_value WHERE source_id = ?1 AND path = ?2 AND field_id = ?3
                                   AND text_val = ?4 COLLATE NOCASE",
                                params![source_id, p, field_id, t],
                            )?;
                        }
                    }
                }
                Ok(())
            };
            folder(path, true)?;
            if let FolderEdit::Add(ExtValue::Text(t)) = &edit {
                db.conn.execute("INSERT OR IGNORE INTO ext_enum_option (field_id, value) VALUES (?1, ?2)", params![field_id, t])?;
            }
            if !recursive {
                return Ok(0);
            }
            for sub in db.subfolders(source_id, path)? {
                folder(&sub, !multi)?;
            }
            let ids = db.folder_media(source_id, path)?;
            for &id in &ids {
                match &edit {
                    FolderEdit::Set(values) if !multi => {
                        db.conn.execute("DELETE FROM ext_value WHERE media_id = ?1 AND field_id = ?2", params![id, field_id])?;
                        for v in values {
                            db.insert_value(id, field_id, v, "user")?;
                        }
                    }
                    FolderEdit::Set(values) => {
                        for v in values {
                            db.add_value_if_missing(id, field_id, v)?;
                        }
                    }
                    FolderEdit::Add(v) => db.add_value_if_missing(id, field_id, v)?,
                    FolderEdit::Remove(v) => {
                        if let ExtValue::Text(t) = v {
                            db.conn.execute(
                                "DELETE FROM ext_value WHERE media_id = ?1 AND field_id = ?2 AND text_val = ?3 COLLATE NOCASE",
                                params![id, field_id, t],
                            )?;
                        }
                    }
                }
            }
            Ok(ids.len())
        })
    }

    /// Every folder inside `path` (any depth) that holds items or has values of its own.
    fn subfolders(&self, source_id: i64, path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT parent_path FROM media WHERE source_id = ?1 AND substr(parent_path, 1, length(?2) + 1) = ?2 || '/'
             UNION SELECT path FROM folder_value WHERE source_id = ?1 AND substr(path, 1, length(?2) + 1) = ?2 || '/'",
        )?;
        let mut out = std::collections::BTreeSet::new();
        for p in stmt.query_map(params![source_id, path], |r| r.get::<_, String>(0))? {
            // Folders between `path` and an item's own folder count too.
            let p = p?;
            let mut cur = p.as_str();
            while cur.len() > path.len() {
                out.insert(cur.to_string());
                cur = cur.rsplit_once('/').map(|(a, _)| a).unwrap_or("");
            }
        }
        Ok(out.into_iter().collect())
    }

    fn folder_has(&self, source_id: i64, path: &str, field_id: i64, v: &ExtValue) -> Result<bool> {
        let ExtValue::Text(t) = v else { return Ok(false) };
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM folder_value WHERE source_id = ?1 AND path = ?2 AND field_id = ?3
                            AND text_val = ?4 COLLATE NOCASE)",
            params![source_id, path, field_id, t],
            |r| r.get(0),
        )?)
    }

    fn add_value_if_missing(&self, id: i64, field_id: i64, v: &ExtValue) -> Result<()> {
        let ExtValue::Text(t) = v else { return self.insert_value(id, field_id, v, "user") };
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM ext_value WHERE media_id = ?1 AND field_id = ?2 AND text_val = ?3 COLLATE NOCASE)",
            params![id, field_id, t],
            |r| r.get(0),
        )?;
        if !exists {
            self.insert_value(id, field_id, v, "user")?;
        }
        Ok(())
    }

    fn insert_folder_value(&self, source_id: i64, path: &str, field_id: i64, v: &ExtValue) -> Result<()> {
        let (i, r, t) = match v {
            ExtValue::Int(i) => (Some(*i), None, None),
            ExtValue::Real(r) => (None, Some(*r), None),
            ExtValue::Text(t) => (None, None, Some(t.as_str())),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO folder_value (source_id, path, field_id, int_val, real_val, text_val, origin)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'user')",
            )?
            .execute(params![source_id, path, field_id, i, r, t])?;
        Ok(())
    }

    /// Folder values follow a folder moved from `from` to `to` (its subfolders' too);
    /// stale values already at `to` are dropped.
    pub fn move_folder_values(&self, source_id: i64, from: &str, to: &str) -> Result<()> {
        let (from, to) = (from.trim_matches('/'), to.trim_matches('/'));
        self.conn.execute(&format!("DELETE FROM folder_value WHERE source_id = ?1 AND {FOLDER_AND_BELOW}"), params![source_id, to])?;
        self.conn.execute(
            &format!("UPDATE folder_value SET path = ?3 || substr(path, length(?2) + 1) WHERE source_id = ?1 AND {FOLDER_AND_BELOW}"),
            params![source_id, from, to],
        )?;
        Ok(())
    }

    /// Forgets the values of a folder and of every folder inside it.
    pub fn delete_folder_values(&self, source_id: i64, path: &str) -> Result<()> {
        self.conn.execute(
            &format!("DELETE FROM folder_value WHERE source_id = ?1 AND {FOLDER_AND_BELOW}"),
            params![source_id, path.trim_matches('/')],
        )?;
        Ok(())
    }

    /// Forgets an item and everything recorded about it (tags, stats, probe, variant).
    pub fn delete_media(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM media WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Forgets the converted copy of an item.
    pub fn remove_variant(&self, media_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM media_variant WHERE media_id = ?1", [media_id])?;
        Ok(())
    }

    /// Context for compiling queries against this database.
    pub fn compile_context(now_ms: i64, tz_offset_minutes: i32) -> CompileContext {
        CompileContext {
            now_ms,
            tz_offset_minutes,
            text_collation: collation::NATURAL.into(),
            ..CompileContext::default()
        }
    }
}

#[cfg(test)]
mod tests;

/// Converts values to the storage form of `ty`, rejecting ones that do not fit
/// (e.g. text for an integer field, or a rating outside its range).
fn coerce_values(ty: FieldType, values: &[ExtValue], key: &str) -> Result<Vec<ExtValue>> {
    let bad = || DbError::InvalidValue(key.into());
    let single = !matches!(ty, FieldType::Enum);
    if single && values.len() > 1 {
        return Err(bad());
    }
    // On/off fields store only "on" (1); "off" clears the value.
    if ty == FieldType::Toggle {
        return values
            .iter()
            .filter_map(|v| match v {
                ExtValue::Int(1) => Some(Ok(ExtValue::Int(1))),
                ExtValue::Int(0) => None,
                _ => Some(Err(bad())),
            })
            .collect();
    }
    values
        .iter()
        .map(|v| match (ty, v) {
            (FieldType::Text | FieldType::Enum, ExtValue::Text(t)) => {
                let t = t.trim();
                if t.is_empty() { Err(bad()) } else { Ok(ExtValue::Text(t.to_string())) }
            }
            (FieldType::Real, ExtValue::Int(i)) => Ok(ExtValue::Real(*i as f64)),
            (FieldType::Real, ExtValue::Real(r)) if r.is_finite() => Ok(ExtValue::Real(*r)),
            (FieldType::RangedInt { min, max }, ExtValue::Int(i)) if (min..=max).contains(i) => Ok(ExtValue::Int(*i)),
            (FieldType::Integer | FieldType::Date | FieldType::Size | FieldType::Duration, ExtValue::Int(i)) => {
                Ok(ExtValue::Int(*i))
            }
            _ => Err(bad()),
        })
        .collect()
}
