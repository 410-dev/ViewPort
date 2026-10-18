//! ViewPort library database (SQLite).

pub mod collation;
pub mod schema;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, ToSql, params, types::ToSqlOutput};
use vp_core::MediaKind;
use vp_query::{CompileContext, FieldDef, FieldRegistry, FieldType, Query, QueryError, SqlValue};

pub use schema::SCHEMA_VERSION;

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
                missing = 0
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
        self.search_in(query, reg, ctx, None)
    }

    /// Like [`Db::search`], optionally limited to one source.
    pub fn search_in(
        &self,
        query: &Query,
        reg: &FieldRegistry,
        ctx: &CompileContext,
        source_id: Option<i64>,
    ) -> Result<Vec<i64>> {
        let mut c = vp_query::compile(query, reg, ctx)?;
        let source_filter = match source_id {
            Some(id) => {
                c.params.push(SqlValue::Int(id));
                format!(" AND m.source_id = ?{}", c.params.len())
            }
            None => String::new(),
        };
        let sql = format!(
            "SELECT m.id FROM media m
             JOIN source src ON src.id = m.source_id
             LEFT JOIN media_stats s ON s.media_id = m.id
             WHERE src.enabled AND NOT m.missing{source_filter} AND ({})
             ORDER BY {}",
            c.where_sql, c.order_sql
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let binds: Vec<Bind> = c.params.iter().map(Bind).collect();
        let refs: Vec<&dyn ToSql> = binds.iter().map(|b| b as &dyn ToSql).collect();
        let ids = stmt.query_map(refs.as_slice(), |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    /// Runs `f` inside one transaction; rolls back if it fails.
    pub fn batch<T>(&self, f: impl FnOnce(&Db) -> Result<T>) -> Result<T> {
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

    /// Removes a source and everything recorded about its media.
    pub fn remove_source(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM source WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn known_files(&self, source_id: i64) -> Result<std::collections::HashMap<String, KnownFile>> {
        let mut stmt = self.conn.prepare("SELECT rel_path, id, size, modified_ms FROM media WHERE source_id = ?1")?;
        let rows = stmt.query_map([source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                KnownFile { id: r.get(1)?, size: r.get::<_, i64>(2)? as u64, modified_ms: r.get(3)? },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mark_seen(&self, media_id: i64, gen_id: i64) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE media SET seen_gen = ?2, missing = 0 WHERE id = ?1")?
            .execute(params![media_id, gen_id])?;
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
                "SELECT m.id, m.name, m.kind, m.width, m.height, m.duration_ms, t.status
                 FROM media m LEFT JOIN thumbnail t ON t.media_id = m.id WHERE m.id IN ({marks})"
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
                    kind: if kind == "video" { MediaKind::Video } else { MediaKind::Image },
                    size: r.get::<_, i64>(3)? as u64,
                })
            })
            .optional()?)
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
    pub fn missing_thumbnails(&self, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id FROM media m JOIN source src ON src.id = m.source_id
             LEFT JOIN thumbnail t ON t.media_id = m.id
             WHERE t.media_id IS NULL AND NOT m.missing AND src.enabled
             ORDER BY m.id LIMIT ?1",
        )?;
        let ids = stmt.query_map([limit as i64], |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
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
