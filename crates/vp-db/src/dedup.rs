//! Duplicate finder bookkeeping: the items to compare, their cached content hashes, and
//! the duplicate reports the user saved. Reports live only in the library (never in a
//! shard).

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{Db, Result};

/// An item that may have a duplicate (another item of the same size in scope).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DupCandidate {
    pub media_id: i64,
    pub source_id: i64,
    pub rel_path: String,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub added_ms: i64,
}

/// Hashes of an item as of `size` and `modified_ms`.
#[derive(Debug, Clone, PartialEq)]
pub struct HashRow {
    pub media_id: i64,
    pub size: u64,
    pub modified_ms: Option<i64>,
    pub partial_len: Option<u64>,
    pub partial: Option<Vec<u8>>,
    pub sha256: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DupReportRow {
    pub id: i64,
    pub created_ms: i64,
    pub label: String,
    pub scope: serde_json::Value,
    pub groups: u64,
    pub files: u64,
    pub wasted: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DupEntry {
    pub grp: u32,
    /// `None` once the item is gone from the library.
    pub media_id: Option<i64>,
    pub source_id: i64,
    pub rel_path: String,
    pub size: u64,
    /// Hex.
    pub sha256: String,
    /// `""`, `"deleted"` or `"linked"`.
    pub action: String,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

impl Db {
    /// Present, non-empty items of `sources` whose size another item shares: within
    /// their own source, or across all of them with `across`. Ordered by size.
    pub fn dup_candidates(&self, sources: &[i64], across: bool) -> Result<Vec<DupCandidate>> {
        if sources.is_empty() {
            return Ok(Vec::new());
        }
        let list = sources.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");
        let partition = if across { "size" } else { "source_id, size" };
        let sql = format!(
            "SELECT id, source_id, rel_path, size, created_ms, modified_ms, added_ms FROM (
                 SELECT m.*, COUNT(*) OVER (PARTITION BY {partition}) AS n FROM media m
                 WHERE m.source_id IN ({list}) AND m.missing = 0 AND m.size > 0)
             WHERE n > 1 ORDER BY size, source_id, rel_path"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(DupCandidate {
                media_id: r.get(0)?,
                source_id: r.get(1)?,
                rel_path: r.get(2)?,
                size: r.get::<_, i64>(3)? as u64,
                created_ms: r.get(4)?,
                modified_ms: r.get(5)?,
                added_ms: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The items of `ids` still in the library (present or not), in no particular order.
    pub fn dup_items(&self, ids: &[i64]) -> Result<Vec<DupCandidate>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, source_id, rel_path, size, created_ms, modified_ms, added_ms FROM media WHERE id = ?1 AND missing = 0",
        )?;
        let mut out = Vec::new();
        for &id in ids {
            let row = stmt
                .query_row([id], |r| {
                    Ok(DupCandidate {
                        media_id: r.get(0)?,
                        source_id: r.get(1)?,
                        rel_path: r.get(2)?,
                        size: r.get::<_, i64>(3)? as u64,
                        created_ms: r.get(4)?,
                        modified_ms: r.get(5)?,
                        added_ms: r.get(6)?,
                    })
                })
                .optional()?;
            out.extend(row);
        }
        Ok(out)
    }

    /// Cached hashes of `ids` (stale ones included; callers compare size and time).
    pub fn cached_hashes(&self, ids: &[i64]) -> Result<HashMap<i64, HashRow>> {
        let mut out = HashMap::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT size, modified_ms, partial_len, partial, sha256 FROM media_hash WHERE media_id = ?1")?;
        for &id in ids {
            let row = stmt
                .query_row([id], |r| {
                    Ok(HashRow {
                        media_id: id,
                        size: r.get::<_, i64>(0)? as u64,
                        modified_ms: r.get(1)?,
                        partial_len: r.get::<_, Option<i64>>(2)?.map(|n| n as u64),
                        partial: r.get(3)?,
                        sha256: r.get(4)?,
                    })
                })
                .optional()?;
            if let Some(row) = row {
                out.insert(id, row);
            }
        }
        Ok(out)
    }

    pub fn store_hashes(&self, rows: &[HashRow]) -> Result<()> {
        self.batch(|db| {
            let mut stmt = db.conn.prepare_cached(
                "INSERT OR REPLACE INTO media_hash (media_id, size, modified_ms, partial_len, partial, sha256)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6 WHERE EXISTS (SELECT 1 FROM media WHERE id = ?1)",
            )?;
            for h in rows {
                stmt.execute(params![h.media_id, h.size as i64, h.modified_ms, h.partial_len.map(|n| n as i64), h.partial, h.sha256])?;
            }
            Ok(())
        })
    }

    /// Saves a duplicate report; returns its id.
    pub fn save_dup_report(&self, label: &str, scope: &serde_json::Value, entries: &[DupEntry], now_ms: i64) -> Result<i64> {
        let mut sizes: HashMap<u32, (u64, u64)> = HashMap::new();
        for e in entries {
            let g = sizes.entry(e.grp).or_insert((e.size, 0));
            g.1 += 1;
        }
        let wasted: u64 = sizes.values().map(|(size, n)| size * n.saturating_sub(1)).sum();
        self.batch(|db| {
            db.conn.execute(
                "INSERT INTO dup_report (created_ms, label, scope, groups, files, wasted) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![now_ms, label, serde_json::to_string(scope)?, sizes.len() as i64, entries.len() as i64, wasted as i64],
            )?;
            let id = db.conn.last_insert_rowid();
            let mut stmt = db.conn.prepare_cached(
                "INSERT INTO dup_entry (report_id, grp, media_id, source_id, rel_path, size, sha256, action)
                 VALUES (?1, ?2, (SELECT id FROM media WHERE id = ?3), ?4, ?5, ?6, ?7, ?8)",
            )?;
            for e in entries {
                stmt.execute(params![id, e.grp, e.media_id, e.source_id, e.rel_path, e.size as i64, unhex(&e.sha256), e.action])?;
            }
            Ok(id)
        })
    }

    pub fn dup_reports(&self) -> Result<Vec<DupReportRow>> {
        let mut stmt =
            self.conn.prepare("SELECT id, created_ms, label, scope, groups, files, wasted FROM dup_report ORDER BY created_ms DESC, id DESC")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?, r.get::<_, i64>(4)?, r.get::<_, i64>(5)?, r.get::<_, i64>(6)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, created_ms, label, scope, groups, files, wasted) = row?;
            out.push(DupReportRow {
                id,
                created_ms,
                label,
                scope: serde_json::from_str(&scope)?,
                groups: groups as u64,
                files: files as u64,
                wasted: wasted as u64,
            });
        }
        Ok(out)
    }

    pub fn dup_report(&self, id: i64) -> Result<Option<(DupReportRow, Vec<DupEntry>)>> {
        let Some(row) = self.dup_reports()?.into_iter().find(|r| r.id == id) else { return Ok(None) };
        let mut stmt = self.conn.prepare(
            "SELECT grp, media_id, source_id, rel_path, size, sha256, action FROM dup_entry WHERE report_id = ?1 ORDER BY grp, rowid",
        )?;
        let entries = stmt
            .query_map([id], |r| {
                Ok(DupEntry {
                    grp: r.get(0)?,
                    media_id: r.get(1)?,
                    source_id: r.get(2)?,
                    rel_path: r.get(3)?,
                    size: r.get::<_, i64>(4)? as u64,
                    sha256: hex(&r.get::<_, Vec<u8>>(5)?),
                    action: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some((row, entries)))
    }

    pub fn rename_dup_report(&self, id: i64, label: &str) -> Result<()> {
        self.conn.execute("UPDATE dup_report SET label = ?2 WHERE id = ?1", params![id, label])?;
        Ok(())
    }

    pub fn delete_dup_report(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM dup_report WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Records what was done to files in every saved report that lists them.
    pub fn mark_dup_entries(&self, files: &[(i64, String)], action: &str) -> Result<()> {
        self.batch(|db| {
            let mut stmt = db.conn.prepare_cached("UPDATE dup_entry SET action = ?3 WHERE source_id = ?1 AND rel_path = ?2")?;
            for (source_id, rel_path) in files {
                stmt.execute(params![source_id, rel_path, action])?;
            }
            Ok(())
        })
    }
}
