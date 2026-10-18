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
#[derive(Debug, Clone, PartialEq)]
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

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.create_collation(collation::NATURAL, collation::natural_cmp)?;
        schema::migrate(&mut conn)?;
        Ok(Self { conn })
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
