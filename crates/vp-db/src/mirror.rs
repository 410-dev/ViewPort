//! Source mirrors: a source kept copied into a folder of another source. Only what the
//! mirror itself copied is tracked (`mirror_item`), so files others put into the target
//! are never replaced or deleted.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, Result};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MirrorRow {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub target_folder: String,
    pub enabled: bool,
    /// Files gone from the source are deleted from the target (when the mirror's copy is
    /// unchanged).
    pub propagate_deletes: bool,
    pub created_ms: i64,
    pub last_sync_ms: Option<i64>,
    /// Summary of the last sync (or why it did not run).
    pub last_result: Option<String>,
    pub triggers: MirrorTriggers,
}

/// When a mirror syncs by itself (it can always be synced by hand).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MirrorTriggers {
    /// When the bundle opens.
    pub on_open: bool,
    /// When the source and the target are both connected again after one was away.
    pub on_connect: bool,
    /// When a scan of the source finds files added, changed or gone.
    pub on_scan: bool,
    /// Again after this many minutes while both stay connected: `None` follows the
    /// library setting (`mirror.resync_mins`), 0 never.
    pub interval_mins: Option<u64>,
}

impl Default for MirrorTriggers {
    fn default() -> Self {
        Self { on_open: true, on_connect: true, on_scan: false, interval_mins: None }
    }
}

/// A file the mirror copied.
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorItem {
    pub rel_path: String,
    pub target_path: String,
    /// The source file when copied.
    pub size: u64,
    pub modified_ms: Option<i64>,
    /// The copy as written.
    pub target_size: u64,
    pub target_modified_ms: Option<i64>,
    /// Revision of the source item's properties last carried to the copy.
    pub meta_rev: i64,
}

/// A source file as the library knows it (present ones only).
#[derive(Debug, Clone, PartialEq)]
pub struct PresentFile {
    pub id: i64,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
}

impl Db {
    pub fn mirrors(&self) -> Result<Vec<MirrorRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, target_id, target_folder, enabled, propagate_deletes, created_ms, last_sync_ms, last_result,
                    on_open, on_connect, on_scan, interval_mins
             FROM mirror ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MirrorRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                target_id: r.get(2)?,
                target_folder: r.get(3)?,
                enabled: r.get(4)?,
                propagate_deletes: r.get(5)?,
                created_ms: r.get(6)?,
                last_sync_ms: r.get(7)?,
                last_result: r.get(8)?,
                triggers: MirrorTriggers {
                    on_open: r.get(9)?,
                    on_connect: r.get(10)?,
                    on_scan: r.get(11)?,
                    interval_mins: r.get::<_, Option<i64>>(12)?.map(|m| m.max(0) as u64),
                },
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn mirror(&self, id: i64) -> Result<Option<MirrorRow>> {
        Ok(self.mirrors()?.into_iter().find(|m| m.id == id))
    }

    /// Creates (`id` None) or updates a mirror; returns its id. Changing the target starts
    /// over: what was copied to the old target is no longer tracked.
    pub fn save_mirror(&self, id: Option<i64>, source_id: i64, target_id: i64, target_folder: &str, enabled: bool, propagate_deletes: bool, now_ms: i64) -> Result<i64> {
        match id {
            None => {
                self.conn.execute(
                    "INSERT INTO mirror (source_id, target_id, target_folder, enabled, propagate_deletes, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![source_id, target_id, target_folder, enabled, propagate_deletes, now_ms],
                )?;
                Ok(self.conn.last_insert_rowid())
            }
            Some(id) => {
                let old: Option<(i64, String)> = self
                    .conn
                    .query_row("SELECT target_id, target_folder FROM mirror WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .optional()?;
                if old.is_some_and(|(t, f)| t != target_id || f != target_folder) {
                    self.conn.execute("DELETE FROM mirror_item WHERE mirror_id = ?1", [id])?;
                }
                self.conn.execute(
                    "UPDATE mirror SET source_id = ?2, target_id = ?3, target_folder = ?4, enabled = ?5, propagate_deletes = ?6 WHERE id = ?1",
                    params![id, source_id, target_id, target_folder, enabled, propagate_deletes],
                )?;
                Ok(id)
            }
        }
    }

    pub fn set_mirror_triggers(&self, id: i64, t: &MirrorTriggers) -> Result<()> {
        self.conn.execute(
            "UPDATE mirror SET on_open = ?2, on_connect = ?3, on_scan = ?4, interval_mins = ?5 WHERE id = ?1",
            params![id, t.on_open, t.on_connect, t.on_scan, t.interval_mins.map(|m| m as i64)],
        )?;
        Ok(())
    }

    pub fn delete_mirror(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM mirror WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn set_mirror_result(&self, id: i64, at_ms: Option<i64>, result: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE mirror SET last_sync_ms = COALESCE(?2, last_sync_ms), last_result = ?3 WHERE id = ?1",
            params![id, at_ms, result],
        )?;
        Ok(())
    }

    pub fn mirror_items(&self, mirror_id: i64) -> Result<HashMap<String, MirrorItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT rel_path, target_path, size, modified_ms, target_size, target_modified_ms, meta_rev FROM mirror_item WHERE mirror_id = ?1",
        )?;
        let rows = stmt.query_map([mirror_id], |r| {
            Ok(MirrorItem {
                rel_path: r.get(0)?,
                target_path: r.get(1)?,
                size: r.get::<_, i64>(2)? as u64,
                modified_ms: r.get(3)?,
                target_size: r.get::<_, i64>(4)? as u64,
                target_modified_ms: r.get(5)?,
                meta_rev: r.get(6)?,
            })
        })?;
        rows.map(|r| r.map(|i| (i.rel_path.clone(), i)).map_err(Into::into)).collect()
    }

    pub fn put_mirror_item(&self, mirror_id: i64, i: &MirrorItem) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO mirror_item (mirror_id, rel_path, target_path, size, modified_ms, target_size, target_modified_ms, meta_rev)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![mirror_id, i.rel_path, i.target_path, i.size as i64, i.modified_ms, i.target_size as i64, i.target_modified_ms, i.meta_rev],
        )?;
        Ok(())
    }

    pub fn set_mirror_item_meta(&self, mirror_id: i64, rel_path: &str, meta_rev: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE mirror_item SET meta_rev = ?3 WHERE mirror_id = ?1 AND rel_path = ?2",
            params![mirror_id, rel_path, meta_rev],
        )?;
        Ok(())
    }

    pub fn delete_mirror_item(&self, mirror_id: i64, rel_path: &str) -> Result<()> {
        self.conn.execute("DELETE FROM mirror_item WHERE mirror_id = ?1 AND rel_path = ?2", params![mirror_id, rel_path])?;
        Ok(())
    }

    /// Files of `source_id` that are there (not missing), by path.
    pub fn present_files(&self, source_id: i64) -> Result<HashMap<String, PresentFile>> {
        let mut stmt =
            self.conn.prepare("SELECT rel_path, id, size, created_ms, modified_ms FROM media WHERE source_id = ?1 AND missing = 0")?;
        let rows = stmt.query_map([source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                PresentFile { id: r.get(1)?, size: r.get::<_, i64>(2)? as u64, created_ms: r.get(3)?, modified_ms: r.get(4)? },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
