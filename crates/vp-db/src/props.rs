//! An item's properties (extension values, statistics, flags, loop segments) as one
//! portable document, for keeping them with the file elsewhere (a shard). Field keys are
//! those of the library; a document also carries the definitions of the fields it uses,
//! so another library can create the ones it lacks.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, Result};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemProps {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<PropValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stats: Option<PropStats>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loops: Vec<PropLoop>,
}

impl ItemProps {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.stats.is_none() && self.flags.is_empty() && self.loops.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropValue {
    pub field: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub int: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropStats {
    pub view_count: i64,
    pub total_view_ms: i64,
    pub last_viewed_ms: Option<i64>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropLoop {
    pub label: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub sort_index: i64,
    #[serde(default)]
    pub autoplay: bool,
}

/// An extension field's definition, as stored in `ext_field`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropField {
    pub names: serde_json::Value,
    #[serde(default)]
    pub aliases: serde_json::Value,
    pub ty: serde_json::Value,
}

/// An item that changed, for whoever keeps properties elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Changed {
    pub media_id: i64,
    pub rel_path: String,
    pub rev: i64,
}

impl Db {
    pub fn item_props(&self, media_id: i64) -> Result<ItemProps> {
        let mut p = ItemProps::default();
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT f.key, v.int_val, v.real_val, v.text_val, v.origin FROM ext_value v
                 JOIN ext_field f ON f.id = v.field_id WHERE v.media_id = ?1 ORDER BY v.id",
            )?;
            let rows = stmt.query_map([media_id], |r| {
                Ok(PropValue { field: r.get(0)?, int: r.get(1)?, real: r.get(2)?, text: r.get(3)?, origin: r.get(4)? })
            })?;
            p.values = rows.collect::<rusqlite::Result<_>>()?;
        }
        p.stats = self
            .conn
            .query_row(
                "SELECT view_count, total_view_ms, last_viewed_ms, score FROM media_stats WHERE media_id = ?1",
                [media_id],
                |r| Ok(PropStats { view_count: r.get(0)?, total_view_ms: r.get(1)?, last_viewed_ms: r.get(2)?, score: r.get(3)? }),
            )
            .optional()?;
        {
            let mut stmt = self.conn.prepare_cached("SELECT flag FROM media_flag WHERE media_id = ?1 ORDER BY flag")?;
            p.flags = stmt.query_map([media_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        }
        {
            let mut stmt = self.conn.prepare_cached(
                "SELECT label, start_ms, end_ms, sort_index, autoplay FROM loop_segment WHERE media_id = ?1 ORDER BY sort_index, id",
            )?;
            let rows = stmt.query_map([media_id], |r| {
                Ok(PropLoop { label: r.get(0)?, start_ms: r.get(1)?, end_ms: r.get(2)?, sort_index: r.get(3)?, autoplay: r.get(4)? })
            })?;
            p.loops = rows.collect::<rusqlite::Result<_>>()?;
        }
        Ok(p)
    }

    /// Definitions of the fields `keys`, by key.
    pub fn prop_fields(&self, keys: impl IntoIterator<Item = String>) -> Result<HashMap<String, PropField>> {
        let mut out = HashMap::new();
        let mut stmt = self.conn.prepare_cached("SELECT names, aliases, ty FROM ext_field WHERE key = ?1")?;
        for key in keys {
            if out.contains_key(&key) {
                continue;
            }
            let f = stmt
                .query_row([&key], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))
                .optional()?;
            if let Some((names, aliases, ty)) = f {
                out.insert(key, PropField { names: serde_json::from_str(&names)?, aliases: serde_json::from_str(&aliases)?, ty: serde_json::from_str(&ty)? });
            }
        }
        Ok(out)
    }

    /// The id of field `key` here, creating it from `def` when missing. A field another
    /// library made under a key this one uses for something else (or named like one of
    /// this library's fields) maps to the field here with the same name.
    fn prop_field_id(&self, key: &str, def: Option<&PropField>, made: &mut HashMap<String, Option<i64>>) -> Result<Option<i64>> {
        if let Some(id) = made.get(key) {
            return Ok(*id);
        }
        let mut id: Option<i64> = self.conn.query_row("SELECT id FROM ext_field WHERE key = ?1", [key], |r| r.get(0)).optional()?;
        if let (Some(found), Some(def)) = (id, def) {
            // Same key, different field (user fields are numbered per library).
            let ty: String = self.conn.query_row("SELECT ty FROM ext_field WHERE id = ?1", [found], |r| r.get(0))?;
            if serde_json::from_str::<serde_json::Value>(&ty)? != def.ty {
                id = None;
            }
        }
        if id.is_none()
            && let Some(def) = def
        {
            let names: Vec<(String, String)> = serde_json::from_value(def.names.clone()).unwrap_or_default();
            // A field here with one of its names.
            let mut stmt = self.conn.prepare_cached("SELECT id, names FROM ext_field")?;
            let existing: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            id = existing.into_iter().find_map(|(fid, n)| {
                let n: Vec<(String, String)> = serde_json::from_str(&n).unwrap_or_default();
                let same = |a: &str, b: &str| a.trim().to_lowercase() == b.trim().to_lowercase();
                n.iter().any(|(_, a)| names.iter().any(|(_, b)| same(a, b))).then_some(fid)
            });
            if id.is_none() && !key.starts_with("x.") {
                let next: i64 = self.conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM ext_field", [], |r| r.get(0))?;
                let free_key: bool = self.conn.query_row("SELECT NOT EXISTS (SELECT 1 FROM ext_field WHERE key = ?1)", [key], |r| r.get(0))?;
                let new_key = if free_key { key.to_string() } else { format!("u.{next}") };
                self.conn.execute(
                    "INSERT INTO ext_field (id, key, names, aliases, ty, sort_index) VALUES (?1, ?2, ?3, ?4, ?5, ?1)",
                    params![next, new_key, def.names.to_string(), def.aliases.to_string(), def.ty.to_string()],
                )?;
                id = Some(next);
            }
        }
        made.insert(key.to_string(), id);
        Ok(id)
    }

    /// Replaces an item's properties with `p` (values of fields that cannot be placed are
    /// left out). `fields` defines the fields `p` uses.
    pub fn apply_item_props(&self, media_id: i64, p: &ItemProps, fields: &HashMap<String, PropField>) -> Result<()> {
        self.batch(|db| {
            let mut made = HashMap::new();
            db.conn.execute("DELETE FROM ext_value WHERE media_id = ?1", [media_id])?;
            for v in &p.values {
                let Some(field) = db.prop_field_id(&v.field, fields.get(&v.field), &mut made)? else { continue };
                db.conn.execute(
                    "INSERT INTO ext_value (media_id, field_id, int_val, real_val, text_val, origin) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![media_id, field, v.int, v.real, v.text, v.origin],
                )?;
            }
            match &p.stats {
                Some(s) => {
                    db.conn.execute(
                        "INSERT OR REPLACE INTO media_stats (media_id, view_count, total_view_ms, last_viewed_ms, score) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![media_id, s.view_count, s.total_view_ms, s.last_viewed_ms, s.score],
                    )?;
                }
                None => {
                    db.conn.execute("DELETE FROM media_stats WHERE media_id = ?1", [media_id])?;
                }
            }
            db.conn.execute("DELETE FROM media_flag WHERE media_id = ?1", [media_id])?;
            for f in &p.flags {
                db.conn.execute("INSERT OR IGNORE INTO media_flag (media_id, flag) VALUES (?1, ?2)", params![media_id, f])?;
            }
            db.conn.execute("DELETE FROM loop_segment WHERE media_id = ?1", [media_id])?;
            for l in &p.loops {
                db.conn.execute(
                    "INSERT INTO loop_segment (media_id, label, start_ms, end_ms, sort_index, autoplay) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![media_id, l.label, l.start_ms, l.end_ms, l.sort_index, l.autoplay],
                )?;
            }
            Ok(())
        })
    }

    /// The latest property revision.
    pub fn meta_counter(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT rev FROM meta_counter WHERE id = 1", [], |r| r.get(0))?)
    }

    /// Items of `source_id` whose properties changed after revision `after`, oldest first.
    pub fn meta_changed(&self, source_id: i64, after: i64) -> Result<Vec<Changed>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id, m.rel_path, r.rev FROM meta_rev r JOIN media m ON m.id = r.media_id
             WHERE m.source_id = ?1 AND r.rev > ?2 ORDER BY r.rev",
        )?;
        let rows = stmt.query_map(params![source_id, after], |r| Ok(Changed { media_id: r.get(0)?, rel_path: r.get(1)?, rev: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The revision of an item's properties (0: never changed).
    pub fn meta_rev(&self, media_id: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT rev FROM meta_rev WHERE media_id = ?1", [media_id], |r| r.get(0)).optional()?.unwrap_or(0))
    }

    /// How far the properties of `source_id` have been written out.
    pub fn meta_synced(&self, source_id: i64) -> Result<i64> {
        Ok(self.conn.query_row("SELECT rev FROM meta_sync WHERE source_id = ?1", [source_id], |r| r.get(0)).optional()?.unwrap_or(0))
    }

    pub fn set_meta_synced(&self, source_id: i64, rev: i64) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO meta_sync (source_id, rev) VALUES (?1, ?2)", params![source_id, rev])?;
        Ok(())
    }

    /// Marks every item of `source_id` as changed, so all their properties are written out
    /// (properties start being kept elsewhere).
    pub fn touch_meta(&self, source_id: i64) -> Result<usize> {
        self.batch(|db| {
            db.conn.execute("UPDATE meta_counter SET rev = rev + 1 WHERE id = 1", [])?;
            Ok(db.conn.execute(
                "INSERT OR REPLACE INTO meta_rev (media_id, rev) SELECT id, (SELECT rev FROM meta_counter) FROM media WHERE source_id = ?1",
                [source_id],
            )?)
        })
    }
}
