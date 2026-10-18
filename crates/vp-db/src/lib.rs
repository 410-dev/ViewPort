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
                                created_ms, modified_ms, added_ms, duration_ms, width, height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (source_id, rel_path) DO UPDATE SET
                kind = excluded.kind, size = excluded.size, created_ms = excluded.created_ms,
                modified_ms = excluded.modified_ms, duration_ms = excluded.duration_ms,
                width = excluded.width, height = excluded.height, missing = 0
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
                m.height
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
        let c = vp_query::compile(query, reg, ctx)?;
        let sql = format!(
            "SELECT m.id FROM media m
             JOIN source src ON src.id = m.source_id
             LEFT JOIN media_stats s ON s.media_id = m.id
             WHERE src.enabled AND NOT m.missing AND ({})
             ORDER BY {}",
            c.where_sql, c.order_sql
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let binds: Vec<Bind> = c.params.iter().map(Bind).collect();
        let refs: Vec<&dyn ToSql> = binds.iter().map(|b| b as &dyn ToSql).collect();
        let ids = stmt.query_map(refs.as_slice(), |r| r.get(0))?.collect::<rusqlite::Result<Vec<i64>>>()?;
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
