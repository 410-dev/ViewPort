//! Faces and people as portable documents, for keeping them with the files elsewhere (a
//! shard), and merging what another library or device did.
//!
//! Faces and people are known by `uid`s that are the same everywhere. The user's changes
//! carry the time they were made, and merging keeps the newer one: a face's placement
//! (`set_ms`: person, confirmed, "not this person"), a person's name (`name_ms`) and
//! hidden state (`hidden_ms`). A merged person leaves a forwarding record (an alias), so
//! a library that still knows the old person merges it too. Automatic grouping never sets
//! these times; between two automatic placements a named person beats an unnamed group.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::faces::{CropRef, now};
use crate::{Db, Result};

/// An item's faces and the models that analysed it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ItemFaces {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faces: Vec<PortFace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub done: Vec<PortDone>,
}

impl ItemFaces {
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty() && self.done.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortFace {
    pub uid: String,
    /// The model that made it (`ml_model.key`) and that model's kind.
    pub model: String,
    pub model_kind: String,
    /// `real` or `anime`.
    pub kind: String,
    pub bbox: [f32; 4],
    pub score: f32,
    /// The person's uid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub set_ms: i64,
    /// People the user said this face is not (uids).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejects: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortDone {
    pub model: String,
    pub model_kind: String,
    pub ok: bool,
    pub done_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortPerson {
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub name_ms: i64,
    #[serde(default)]
    pub hidden: bool,
    #[serde(default)]
    pub hidden_ms: i64,
    #[serde(default)]
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortAlias {
    pub into: String,
    pub ms: i64,
}

/// People (by uid) and forwarding records of merged people.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PeopleDoc {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub people: BTreeMap<String, PortPerson>,
    #[serde(default)]
    pub aliases: BTreeMap<String, PortAlias>,
}

/// A local face, for merging.
struct LocalFace {
    id: i64,
    uid: String,
    model: String,
    bbox: [f32; 4],
    person: Option<i64>,
    confirmed: bool,
    set_ms: i64,
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let (ax2, ay2, bx2, by2) = (a[0] + a[2], a[1] + a[3], b[0] + b[2], b[1] + b[3]);
    let w = (ax2.min(bx2) - a[0].max(b[0])).max(0.0);
    let h = (ay2.min(by2) - a[1].max(b[1])).max(0.0);
    let inter = w * h;
    let union = a[2] * a[3] + b[2] * b[3] - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

impl Db {
    /// The faces of an item and the models that analysed it.
    pub fn item_faces(&self, media_id: i64) -> Result<ItemFaces> {
        let mut out = ItemFaces::default();
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id, f.uid, m.key, m.kind, f.kind, f.x, f.y, f.w, f.h, f.score, p.uid, f.confirmed, f.set_ms
             FROM face f JOIN ml_model m ON m.id = f.model_id LEFT JOIN person p ON p.id = f.person_id
             WHERE f.media_id = ?1 ORDER BY f.id",
        )?;
        let rows = stmt.query_map([media_id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                PortFace {
                    uid: r.get(1)?,
                    model: r.get(2)?,
                    model_kind: r.get(3)?,
                    kind: r.get(4)?,
                    bbox: [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
                    score: r.get(9)?,
                    person: r.get(10)?,
                    confirmed: r.get(11)?,
                    set_ms: r.get(12)?,
                    rejects: Vec::new(),
                },
            ))
        })?;
        let faces: Vec<(i64, PortFace)> = rows.collect::<rusqlite::Result<_>>()?;
        let mut rej = self.conn.prepare_cached("SELECT p.uid FROM face_reject r JOIN person p ON p.id = r.person_id WHERE r.face_id = ?1 ORDER BY p.uid")?;
        for (id, mut f) in faces {
            f.rejects = rej.query_map([id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            out.faces.push(f);
        }
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.key, m.kind, d.ok, d.done_ms FROM ml_done d JOIN ml_model m ON m.id = d.model_id WHERE d.media_id = ?1 ORDER BY m.key",
        )?;
        let rows = stmt.query_map([media_id], |r| Ok(PortDone { model: r.get(0)?, model_kind: r.get(1)?, ok: r.get(2)?, done_ms: r.get(3)? }))?;
        out.done = rows.collect::<rusqlite::Result<_>>()?;
        Ok(out)
    }

    /// The people faces of `source_id`'s items are in or kept out of, and every
    /// forwarding record.
    pub fn people_doc(&self, source_id: i64) -> Result<PeopleDoc> {
        let mut doc = PeopleDoc { version: 1, ..Default::default() };
        let mut stmt = self.conn.prepare(
            "SELECT p.uid, p.kind, p.name, p.name_ms, p.hidden, p.hidden_ms, p.created_ms FROM person p
             WHERE p.id IN (SELECT f.person_id FROM face f JOIN media m ON m.id = f.media_id WHERE m.source_id = ?1
                            UNION SELECT r.person_id FROM face_reject r JOIN face f ON f.id = r.face_id
                                  JOIN media m ON m.id = f.media_id WHERE m.source_id = ?1)",
        )?;
        let rows = stmt.query_map([source_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                PortPerson { kind: r.get(1)?, name: r.get(2)?, name_ms: r.get(3)?, hidden: r.get(4)?, hidden_ms: r.get(5)?, created_ms: r.get(6)? },
            ))
        })?;
        doc.people = rows.collect::<rusqlite::Result<_>>()?;
        let mut stmt = self.conn.prepare("SELECT uid, into_uid, ms FROM person_alias")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, PortAlias { into: r.get(1)?, ms: r.get(2)? })))?;
        doc.aliases = rows.collect::<rusqlite::Result<_>>()?;
        Ok(doc)
    }

    /// Where person `uid` went: itself, or the person it was merged into.
    fn resolve_uid(&self, uid: &str) -> Result<String> {
        let mut uid = uid.to_string();
        for _ in 0..64 {
            let next: Option<String> = self.conn.query_row("SELECT into_uid FROM person_alias WHERE uid = ?1", [&uid], |r| r.get(0)).optional()?;
            match next {
                Some(n) if n != uid => uid = n,
                _ => break,
            }
        }
        Ok(uid)
    }

    fn person_by_uid(&self, uid: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT id FROM person WHERE uid = ?1", [uid], |r| r.get(0)).optional()?)
    }

    /// The person `uid` (after forwarding), created from `doc` (or as an unnamed group of
    /// `kind`) when this library does not know it.
    fn ensure_person(&self, uid: &str, kind: &str, doc: &PeopleDoc) -> Result<i64> {
        let uid = self.resolve_uid(uid)?;
        if let Some(id) = self.person_by_uid(&uid)? {
            return Ok(id);
        }
        let p = doc.people.get(&uid);
        self.conn.execute(
            "INSERT INTO person (uid, kind, name, name_ms, hidden, hidden_ms, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                uid,
                p.map(|p| p.kind.as_str()).unwrap_or(kind),
                p.map(|p| p.name.as_str()).unwrap_or(""),
                p.map(|p| p.name_ms).unwrap_or(0),
                p.is_some_and(|p| p.hidden),
                p.map(|p| p.hidden_ms).unwrap_or(0),
                p.map(|p| p.created_ms).unwrap_or_else(now),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Takes in another library's people: merges it made, and names and hidden states it
    /// changed after this one. Returns whether anything changed.
    pub fn merge_people_doc(&self, doc: &PeopleDoc) -> Result<bool> {
        self.batch(|db| {
            let mut changed = false;
            for (uid, a) in &doc.aliases {
                if uid == &a.into {
                    continue;
                }
                let known: Option<String> = db.conn.query_row("SELECT into_uid FROM person_alias WHERE uid = ?1", [uid], |r| r.get(0)).optional()?;
                if known.is_none() {
                    db.conn.execute("INSERT INTO person_alias (uid, into_uid, ms) VALUES (?1, ?2, ?3)", params![uid, a.into, a.ms])?;
                }
                if let Some(from) = db.person_by_uid(uid)? {
                    let kind: String = db.conn.query_row("SELECT kind FROM person WHERE id = ?1", [from], |r| r.get(0))?;
                    let target = db.resolve_uid(uid)?;
                    if target != *uid {
                        let into = db.ensure_person(&target, &kind, doc)?;
                        db.merge_people(into, &[from])?;
                        changed = true;
                    }
                }
            }
            for (uid, p) in &doc.people {
                let uid = db.resolve_uid(uid)?;
                let Some(id) = db.person_by_uid(&uid)? else {
                    // Named people show up even before any face of theirs is here.
                    if !p.name.is_empty() {
                        db.ensure_person(&uid, &p.kind, doc)?;
                        changed = true;
                    }
                    continue;
                };
                let (name_ms, hidden_ms): (i64, i64) = db.conn.query_row("SELECT name_ms, hidden_ms FROM person WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
                if p.hidden_ms > hidden_ms {
                    db.conn.execute("UPDATE person SET hidden = ?2, hidden_ms = ?3 WHERE id = ?1", params![id, p.hidden, p.hidden_ms])?;
                    changed = true;
                }
                if p.name_ms > name_ms {
                    let name = p.name.trim();
                    let same: Option<i64> = if name.is_empty() {
                        None
                    } else {
                        db.conn
                            .query_row(
                                "SELECT id FROM person WHERE kind = ?1 AND name = ?2 COLLATE NOCASE AND id <> ?3",
                                params![p.kind, name, id],
                                |r| r.get(0),
                            )
                            .optional()?
                    };
                    match same {
                        Some(other) => db.merge_people(other, &[id])?,
                        None => {
                            db.conn.execute("UPDATE person SET name = ?2, name_ms = ?3 WHERE id = ?1", params![id, name, p.name_ms])?;
                        }
                    }
                    changed = true;
                }
            }
            Ok(changed)
        })
    }

    fn local_faces(&self, media_id: i64) -> Result<Vec<LocalFace>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id, f.uid, m.key, f.x, f.y, f.w, f.h, f.person_id, f.confirmed, f.set_ms
             FROM face f JOIN ml_model m ON m.id = f.model_id WHERE f.media_id = ?1",
        )?;
        let rows = stmt.query_map([media_id], |r| {
            Ok(LocalFace {
                id: r.get(0)?,
                uid: r.get(1)?,
                model: r.get(2)?,
                bbox: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
                person: r.get(7)?,
                confirmed: r.get(8)?,
                set_ms: r.get(9)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn is_named(&self, person: Option<i64>) -> Result<bool> {
        let Some(p) = person else { return Ok(false) };
        Ok(self.conn.query_row("SELECT name <> '' FROM person WHERE id = ?1", [p], |r| r.get(0)).optional()?.unwrap_or(false))
    }

    /// Takes in another library's faces of an item (`doc` holds the people they refer to,
    /// already merged with [`Db::merge_people_doc`]). A face this library found too (same
    /// uid, or the same model's face overlapping by `same_iou` or more) keeps one uid and the
    /// newer placement; faces only the other library found are added. Returns the uids of
    /// added faces (they still lack a picture and vector).
    pub fn merge_item_faces(&self, media_id: i64, item: &ItemFaces, doc: &PeopleDoc, same_iou: f32) -> Result<Vec<String>> {
        self.batch(|db| {
            for d in &item.done {
                let model = db.ml_model_id(&d.model, &d.model_kind)?;
                db.conn.execute(
                    "INSERT OR IGNORE INTO ml_done (media_id, model_id, ok, done_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![media_id, model, d.ok, d.done_ms],
                )?;
            }
            let mut local = db.local_faces(media_id)?;
            let theirs: std::collections::HashSet<&str> = item.faces.iter().map(|f| f.uid.as_str()).collect();
            let mut added = Vec::new();
            for sf in &item.faces {
                let at = local.iter().position(|l| l.uid == sf.uid).or_else(|| {
                    local
                        .iter()
                        .enumerate()
                        .filter(|(_, l)| l.model == sf.model && !theirs.contains(l.uid.as_str()))
                        .map(|(i, l)| (i, iou(&l.bbox, &sf.bbox)))
                        .filter(|(_, o)| *o >= same_iou)
                        .max_by(|a, b| a.1.total_cmp(&b.1))
                        .map(|(i, _)| i)
                });
                // Created only where it is put to use.
                let person = |db: &Db| -> Result<Option<i64>> {
                    sf.person.as_deref().map(|uid| db.ensure_person(uid, &sf.kind, doc)).transpose()
                };
                match at {
                    Some(i) => {
                        let lf = local.remove(i);
                        if lf.uid != sf.uid {
                            db.conn.execute("UPDATE face SET uid = ?2 WHERE id = ?1", params![lf.id, sf.uid])?;
                        }
                        if sf.set_ms > lf.set_ms {
                            db.conn.execute(
                                "UPDATE face SET person_id = ?2, confirmed = ?3, set_ms = ?4 WHERE id = ?1",
                                params![lf.id, person(db)?, sf.confirmed, sf.set_ms],
                            )?;
                            db.set_rejects(lf.id, &sf.rejects, &sf.kind, doc)?;
                        } else if sf.set_ms == lf.set_ms && !lf.confirmed && !sf.confirmed && db.uid_named(sf.person.as_deref(), doc)? && !db.is_named(lf.person)? {
                            let p = person(db)?;
                            if p != lf.person {
                                db.conn.execute("UPDATE face SET person_id = ?2 WHERE id = ?1", params![lf.id, p])?;
                            }
                        }
                    }
                    None => {
                        let model = db.ml_model_id(&sf.model, &sf.model_kind)?;
                        let [x, y, w, h] = sf.bbox;
                        db.conn.execute(
                            "INSERT INTO face (uid, media_id, model_id, kind, x, y, w, h, score, person_id, confirmed, set_ms)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                            params![sf.uid, media_id, model, sf.kind, x, y, w, h, sf.score, person(db)?, sf.confirmed, sf.set_ms],
                        )?;
                        let id = db.conn.last_insert_rowid();
                        db.set_rejects(id, &sf.rejects, &sf.kind, doc)?;
                        added.push(sf.uid.clone());
                    }
                }
            }
            Ok(added)
        })
    }

    /// Whether person `uid` has a name, here or in `doc`.
    fn uid_named(&self, uid: Option<&str>, doc: &PeopleDoc) -> Result<bool> {
        let Some(uid) = uid else { return Ok(false) };
        let uid = self.resolve_uid(uid)?;
        Ok(self.is_named(self.person_by_uid(&uid)?)? || doc.people.get(&uid).is_some_and(|p| !p.name.is_empty()))
    }

    fn set_rejects(&self, face: i64, rejects: &[String], kind: &str, doc: &PeopleDoc) -> Result<()> {
        self.conn.execute("DELETE FROM face_reject WHERE face_id = ?1", [face])?;
        for uid in rejects {
            let p = self.ensure_person(uid, kind, doc)?;
            self.conn.execute("INSERT OR IGNORE INTO face_reject (face_id, person_id) VALUES (?1, ?2)", params![face, p])?;
        }
        Ok(())
    }

    /// A face's identity vector, by uid.
    pub fn face_embedding(&self, uid: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.conn.query_row("SELECT embedding FROM face WHERE uid = ?1", [uid], |r| r.get(0)).optional()?.flatten())
    }

    /// Where a face's picture is stored, by uid.
    pub fn face_crop_by_uid(&self, uid: &str) -> Result<Option<CropRef>> {
        Ok(self
            .conn
            .query_row("SELECT crop_pack, crop_off, crop_len FROM face WHERE uid = ?1 AND crop_pack IS NOT NULL", [uid], |r| {
                Ok(CropRef { pack: r.get(0)?, offset: r.get::<_, i64>(1)? as u64, len: r.get(2)? })
            })
            .optional()?)
    }

    /// Fills in a face's vector and picture (those it lacks).
    pub fn set_face_extras(&self, uid: &str, embedding: Option<&[u8]>, crop: Option<CropRef>) -> Result<()> {
        if let Some(e) = embedding {
            self.conn.execute("UPDATE face SET embedding = ?2 WHERE uid = ?1 AND embedding IS NULL", params![uid, e])?;
        }
        if let Some(c) = crop {
            self.conn.execute(
                "UPDATE face SET crop_pack = ?2, crop_off = ?3, crop_len = ?4 WHERE uid = ?1 AND crop_pack IS NULL",
                params![uid, c.pack, c.offset as i64, c.len],
            )?;
        }
        Ok(())
    }

    /// Faces on `source_id`'s items, and the bytes their vectors take.
    pub fn source_face_stats(&self, source_id: i64) -> Result<(u64, u64)> {
        let (n, bytes): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(f.embedding)), 0) FROM face f JOIN media m ON m.id = f.media_id WHERE m.source_id = ?1",
            [source_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((n as u64, bytes as u64))
    }

    /// Uids of the faces on `source_id`'s items that have a vector.
    pub fn source_face_uids(&self, source_id: i64) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.uid FROM face f JOIN media m ON m.id = f.media_id WHERE m.source_id = ?1 AND f.embedding IS NOT NULL ORDER BY f.id",
        )?;
        let rows = stmt.query_map([source_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The average size of a face's vector here, if any face has one.
    pub fn average_embedding_bytes(&self) -> Result<Option<u64>> {
        let avg: Option<f64> = self.conn.query_row("SELECT AVG(length(embedding)) FROM face WHERE embedding IS NOT NULL", [], |r| r.get(0))?;
        Ok(avg.map(|a| a.round() as u64))
    }

    /// How far people changes go (compared with [`Db::meta_synced`]).
    pub fn people_rev(&self) -> Result<i64> {
        Ok(self.conn.query_row("SELECT rev FROM people_rev WHERE id = 1", [], |r| r.get(0))?)
    }
}
