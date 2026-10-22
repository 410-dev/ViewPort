//! Image analysis results: which items each model has seen, faces with identity vectors,
//! people (named or not) they are grouped into, and tagger scores. Names and tags reach
//! searches as values of built-in fields with an `auto:*` origin, rewritten from these
//! tables (`sync_*`), so values the user entered are never touched.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::{Db, Result};

/// Built-in fields automatic values go to (see schema v7).
pub const PEOPLE_FIELD: &str = "x.people";
pub const CHARACTERS_FIELD: &str = "x.characters";
pub const AUTO_TAGS_FIELD: &str = "x.auto_tags";
pub const ORIGIN_FACES: &str = "auto:faces";
pub const ORIGIN_TAGGER: &str = "auto:tagger";

/// Where a face picture is stored in the thumbnail packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropRef {
    pub pack: u32,
    pub offset: u64,
    pub len: u32,
}

/// A tagger result: tag, category (0 general, 4 character), score.
pub type TagRow = (String, i64, f32);

#[derive(Debug, Clone)]
pub struct NewFace {
    pub model_id: i64,
    pub kind: String,
    pub bbox: [f32; 4],
    pub score: f32,
    pub embedding: Option<Vec<u8>>,
    pub crop: Option<CropRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonRow {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// Items showing this person.
    pub items: u64,
    pub faces: u64,
    /// The best-scored face, for the card.
    pub cover: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FaceRow {
    pub id: i64,
    pub media_id: i64,
    pub bbox: [f32; 4],
    pub score: f32,
    pub person_id: Option<i64>,
    pub confirmed: bool,
}

/// A face as grouping sees it.
#[derive(Debug, Clone)]
pub struct GroupFace {
    pub id: i64,
    pub person_id: Option<i64>,
    /// Its person has a name (grouping keeps named people and only regroups the rest).
    pub named: bool,
    pub confirmed: bool,
    pub embedding: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct AnalysisCounts {
    /// Images that could be analysed (present, in enabled and connected sources).
    pub images: u64,
    /// Of those, analysed by every chosen model.
    pub done: u64,
    pub faces: u64,
    pub people: u64,
    pub unnamed: u64,
    /// Items a model could not analyse (unreadable image, model error).
    pub failed: u64,
}

impl std::ops::AddAssign for AnalysisCounts {
    /// Sums the item counts of separate groups of sources (faces and people are totals).
    fn add_assign(&mut self, o: Self) {
        self.images += o.images;
        self.done += o.done;
        self.failed += o.failed;
        (self.faces, self.people, self.unnamed) = (o.faces, o.people, o.unnamed);
    }
}

/// `json_each` input for a list of ids.
fn json_ids(ids: &[i64]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())
}

impl Db {
    /// The id of model `key` (`id@version`), registered on first use.
    pub fn ml_model_id(&self, key: &str, kind: &str) -> Result<i64> {
        self.conn.execute("INSERT OR IGNORE INTO ml_model (key, kind) VALUES (?1, ?2)", params![key, kind])?;
        Ok(self.conn.query_row("SELECT id FROM ml_model WHERE key = ?1", [key], |r| r.get(0))?)
    }

    /// Images not yet analysed by every one of `models`, after `after_id`, in id order.
    /// Sources in `exclude` (disconnected) are left out.
    pub fn media_to_analyze(&self, models: &[i64], exclude: &[i64], after_id: i64, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT m.id FROM media m JOIN source src ON src.id = m.source_id
             WHERE m.kind = 'image' AND NOT m.missing AND src.enabled AND m.id > ?3
               AND m.source_id NOT IN (SELECT value FROM json_each(?2))
               AND EXISTS (SELECT 1 FROM json_each(?1) j
                           WHERE NOT EXISTS (SELECT 1 FROM ml_done d WHERE d.media_id = m.id AND d.model_id = j.value))
             ORDER BY m.id LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![json_ids(models), json_ids(exclude), after_id, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn analysis_counts(&self, models: &[i64], exclude: &[i64]) -> Result<AnalysisCounts> {
        let (images, done, failed): (i64, i64, i64) = self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(NOT EXISTS (SELECT 1 FROM json_each(?1) j
                         WHERE NOT EXISTS (SELECT 1 FROM ml_done d WHERE d.media_id = m.id AND d.model_id = j.value))), 0),
                    COALESCE(SUM(EXISTS (SELECT 1 FROM ml_done d WHERE d.media_id = m.id AND NOT d.ok
                                         AND d.model_id IN (SELECT value FROM json_each(?1)))), 0)
             FROM media m JOIN source src ON src.id = m.source_id
             WHERE m.kind = 'image' AND NOT m.missing AND src.enabled
               AND m.source_id NOT IN (SELECT value FROM json_each(?2))",
            params![json_ids(models), json_ids(exclude)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let faces: i64 = self.conn.query_row("SELECT COUNT(*) FROM face", [], |r| r.get(0))?;
        let (people, unnamed): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(name <> ''), 0), COALESCE(SUM(name = ''), 0) FROM person WHERE NOT hidden",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(AnalysisCounts {
            images: images as u64,
            done: done as u64,
            faces: faces as u64,
            people: people as u64,
            unnamed: unnamed as u64,
            failed: failed as u64,
        })
    }

    /// Forgets failed analyses, so the next job tries those items again. Returns how many.
    pub fn retry_failed(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM ml_done WHERE NOT ok", [])?)
    }

    /// Stores one item's results: it was analysed by `models` (`ok` = the image was read),
    /// its faces from these models are replaced, and so are its tags from `tagger`.
    pub fn record_analysis(&self, media_id: i64, models: &[i64], ok: bool, faces: &[NewFace], tags: Option<(i64, &[TagRow])>, now_ms: i64) -> Result<()> {
        self.batch(|db| {
            for &m in models {
                db.conn.execute("DELETE FROM face WHERE media_id = ?1 AND model_id = ?2", params![media_id, m])?;
                db.conn.execute(
                    "INSERT OR REPLACE INTO ml_done (media_id, model_id, ok, done_ms) VALUES (?1, ?2, ?3, ?4)",
                    params![media_id, m, ok, now_ms],
                )?;
            }
            let mut ins = db.conn.prepare_cached(
                "INSERT INTO face (media_id, model_id, kind, x, y, w, h, score, embedding, crop_pack, crop_off, crop_len)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            for f in faces {
                let [x, y, w, h] = f.bbox;
                ins.execute(params![
                    media_id,
                    f.model_id,
                    f.kind,
                    x,
                    y,
                    w,
                    h,
                    f.score,
                    f.embedding,
                    f.crop.map(|c| c.pack),
                    f.crop.map(|c| c.offset as i64),
                    f.crop.map(|c| c.len),
                ])?;
            }
            if let Some((model, tags)) = tags {
                db.conn.execute("DELETE FROM tag_score WHERE media_id = ?1 AND model_id = ?2", params![media_id, model])?;
                let mut ins = db.conn.prepare_cached(
                    "INSERT OR REPLACE INTO tag_score (media_id, model_id, tag, category, score) VALUES (?1, ?2, ?3, ?4, ?5)",
                )?;
                for (tag, cat, score) in tags {
                    ins.execute(params![media_id, model, tag, cat, score])?;
                }
            }
            Ok(())
        })
    }

    /// Models that have analysed item `media`.
    pub fn models_done(&self, media: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached("SELECT model_id FROM ml_done WHERE media_id = ?1")?;
        let rows = stmt.query_map([media], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The model most faces of `kind` with vectors come from (the one grouping uses).
    pub fn main_face_model(&self, kind: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT model_id FROM face WHERE kind = ?1 AND embedding IS NOT NULL GROUP BY model_id ORDER BY COUNT(*) DESC LIMIT 1",
                [kind],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Faces of `kind` made by embedder `model` that have a vector (hidden people's included).
    pub fn faces_for_grouping(&self, kind: &str, model: i64) -> Result<Vec<GroupFace>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.person_id, COALESCE(p.name, '') <> '', f.confirmed, f.embedding
             FROM face f LEFT JOIN person p ON p.id = f.person_id
             WHERE f.kind = ?1 AND f.model_id = ?2 AND f.embedding IS NOT NULL
             ORDER BY f.id",
        )?;
        let rows = stmt.query_map(params![kind, model], |r| {
            Ok(GroupFace { id: r.get(0)?, person_id: r.get(1)?, named: r.get(2)?, confirmed: r.get(3)?, embedding: r.get(4)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `(face, person)` pairs the user said do not belong together.
    pub fn face_rejects(&self) -> Result<std::collections::HashSet<(i64, i64)>> {
        let mut stmt = self.conn.prepare("SELECT face_id, person_id FROM face_reject")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces the unnamed groups of `kind`: `assign` puts faces in named people (by id) or
    /// in new unnamed groups (by group number); faces not listed are left as they are
    /// unless they were in an old unnamed group.
    pub fn apply_grouping(&self, kind: &str, to_people: &[(i64, i64)], to_groups: &[(i64, usize)], now_ms: i64) -> Result<()> {
        self.batch(|db| {
            db.conn.execute(
                "UPDATE face SET person_id = NULL WHERE person_id IN (SELECT id FROM person WHERE kind = ?1 AND name = '')",
                [kind],
            )?;
            db.conn.execute("DELETE FROM person WHERE kind = ?1 AND name = ''", [kind])?;
            let mut set = db.conn.prepare_cached("UPDATE face SET person_id = ?2 WHERE id = ?1 AND NOT confirmed")?;
            for (face, person) in to_people {
                set.execute(params![face, person])?;
            }
            let mut groups: std::collections::HashMap<usize, i64> = Default::default();
            for (face, g) in to_groups {
                let person = match groups.get(g) {
                    Some(p) => *p,
                    None => {
                        db.conn.execute("INSERT INTO person (kind, name, created_ms) VALUES (?1, '', ?2)", params![kind, now_ms])?;
                        let id = db.conn.last_insert_rowid();
                        groups.insert(*g, id);
                        id
                    }
                };
                set.execute(params![face, person])?;
            }
            Ok(())
        })
    }

    /// People of `kind`, most items first; unnamed groups after named people.
    pub fn people(&self, kind: &str, include_hidden: bool) -> Result<Vec<PersonRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.kind, p.name, COUNT(DISTINCT f.media_id), COUNT(f.id),
                    (SELECT f2.id FROM face f2 WHERE f2.person_id = p.id ORDER BY f2.confirmed DESC, f2.score DESC LIMIT 1)
             FROM person p LEFT JOIN face f ON f.person_id = p.id
             WHERE p.kind = ?1 AND (?2 OR NOT p.hidden)
             GROUP BY p.id
             HAVING COUNT(f.id) > 0 OR p.name <> ''
             ORDER BY p.name = '', COUNT(DISTINCT f.media_id) DESC, p.id",
        )?;
        let rows = stmt.query_map(params![kind, include_hidden], |r| {
            Ok(PersonRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                name: r.get(2)?,
                items: r.get::<_, i64>(3)? as u64,
                faces: r.get::<_, i64>(4)? as u64,
                cover: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn person_faces(&self, person: i64, offset: u64, limit: u64) -> Result<Vec<FaceRow>> {
        self.face_rows("WHERE f.person_id = ?1 ORDER BY f.confirmed DESC, f.score DESC, f.id LIMIT ?2 OFFSET ?3", params![person, limit as i64, offset as i64])
    }

    pub fn media_faces(&self, media: i64) -> Result<Vec<FaceRow>> {
        self.face_rows("WHERE f.media_id = ?1 ORDER BY f.x", params![media])
    }

    fn face_rows(&self, tail: &str, p: impl rusqlite::Params) -> Result<Vec<FaceRow>> {
        let mut stmt = self.conn.prepare(&format!("SELECT f.id, f.media_id, f.x, f.y, f.w, f.h, f.score, f.person_id, f.confirmed FROM face f {tail}"))?;
        let rows = stmt.query_map(p, |r| {
            Ok(FaceRow {
                id: r.get(0)?,
                media_id: r.get(1)?,
                bbox: [r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?],
                score: r.get(6)?,
                person_id: r.get(7)?,
                confirmed: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn face_crop(&self, face: i64) -> Result<Option<CropRef>> {
        Ok(self
            .conn
            .query_row("SELECT crop_pack, crop_off, crop_len FROM face WHERE id = ?1 AND crop_pack IS NOT NULL", [face], |r| {
                Ok(CropRef { pack: r.get(0)?, offset: r.get::<_, i64>(1)? as u64, len: r.get(2)? })
            })
            .optional()?)
    }

    fn person_kind(&self, id: i64) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT kind FROM person WHERE id = ?1", [id], |r| r.get(0)).optional()?)
    }

    /// Names a person. A person of the same kind already called that takes this one's faces
    /// (naming two groups alike merges them). Returns the id that carries the name.
    pub fn rename_person(&self, id: i64, name: &str) -> Result<i64> {
        let name = name.trim();
        let Some(kind) = self.person_kind(id)? else { return Ok(id) };
        let same: Option<i64> = if name.is_empty() {
            None
        } else {
            self.conn
                .query_row(
                    "SELECT id FROM person WHERE kind = ?1 AND name = ?2 COLLATE NOCASE AND id <> ?3",
                    params![kind, name, id],
                    |r| r.get(0),
                )
                .optional()?
        };
        match same {
            Some(other) => {
                self.merge_people(other, &[id])?;
                Ok(other)
            }
            None => {
                self.conn.execute("UPDATE person SET name = ?2 WHERE id = ?1", params![id, name])?;
                Ok(id)
            }
        }
    }

    /// Moves the faces of `from` into `into` and drops `from`.
    pub fn merge_people(&self, into: i64, from: &[i64]) -> Result<()> {
        self.batch(|db| {
            for &f in from.iter().filter(|&&f| f != into) {
                db.conn.execute("UPDATE face SET person_id = ?1 WHERE person_id = ?2", params![into, f])?;
                db.conn.execute(
                    "INSERT OR IGNORE INTO face_reject (face_id, person_id) SELECT face_id, ?1 FROM face_reject WHERE person_id = ?2",
                    params![into, f],
                )?;
                db.conn.execute("DELETE FROM person WHERE id = ?1", [f])?;
            }
            Ok(())
        })
    }

    pub fn create_person(&self, kind: &str, name: &str, now_ms: i64) -> Result<i64> {
        self.conn.execute("INSERT INTO person (kind, name, created_ms) VALUES (?1, ?2, ?3)", params![kind, name.trim(), now_ms])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_person_hidden(&self, id: i64, hidden: bool) -> Result<()> {
        self.conn.execute("UPDATE person SET hidden = ?2 WHERE id = ?1", params![id, hidden])?;
        Ok(())
    }

    /// The user put faces in a person (`Some`) or took them out of theirs ("not this
    /// person": remembered, so grouping does not put them back).
    pub fn assign_faces(&self, faces: &[i64], person: Option<i64>) -> Result<()> {
        self.batch(|db| {
            for &f in faces {
                match person {
                    Some(p) => {
                        db.conn.execute("DELETE FROM face_reject WHERE face_id = ?1 AND person_id = ?2", params![f, p])?;
                        db.conn.execute("UPDATE face SET person_id = ?2, confirmed = 1 WHERE id = ?1", params![f, p])?;
                    }
                    None => {
                        db.conn.execute(
                            "INSERT OR IGNORE INTO face_reject (face_id, person_id) SELECT id, person_id FROM face WHERE id = ?1 AND person_id IS NOT NULL",
                            [f],
                        )?;
                        db.conn.execute("UPDATE face SET person_id = NULL, confirmed = 0 WHERE id = ?1", [f])?;
                    }
                }
            }
            Ok(())
        })
    }

    fn field_id(&self, key: &str) -> Result<Option<i64>> {
        Ok(self.conn.query_row("SELECT id FROM ext_field WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    /// Rewrites the `origin` values of field `key` from `select` (rows of `media_id, text`).
    /// A value the item already has from elsewhere (the user) is not added twice.
    fn sync_values(&self, key: &str, origin: &str, select: &str, p: impl rusqlite::Params) -> Result<()> {
        let Some(field) = self.field_id(key)? else { return Ok(()) };
        self.batch(|db| {
            db.conn.execute("DELETE FROM ext_value WHERE field_id = ?1 AND origin = ?2", params![field, origin])?;
            db.conn.execute_batch("DROP TABLE IF EXISTS temp.auto_vals; CREATE TEMP TABLE auto_vals (media_id INTEGER, val TEXT);")?;
            db.conn.execute(&format!("INSERT INTO temp.auto_vals (media_id, val) {select}"), p)?;
            db.conn.execute(
                "INSERT INTO ext_value (media_id, field_id, text_val, origin)
                 SELECT DISTINCT a.media_id, ?1, a.val, ?2 FROM temp.auto_vals a
                 WHERE a.val <> '' AND NOT EXISTS (SELECT 1 FROM ext_value e
                                                   WHERE e.media_id = a.media_id AND e.field_id = ?1 AND e.text_val = a.val COLLATE NOCASE)",
                params![field, origin],
            )?;
            db.conn.execute(
                "INSERT OR IGNORE INTO ext_enum_option (field_id, value) SELECT DISTINCT ?1, val FROM temp.auto_vals WHERE val <> ''",
                [field],
            )?;
            db.conn.execute_batch("DROP TABLE temp.auto_vals;")?;
            Ok(())
        })
    }

    /// Names of people (photos → `x.people`, characters → `x.characters`) on their items.
    pub fn sync_people_values(&self) -> Result<()> {
        for (kind, key) in [("real", PEOPLE_FIELD), ("anime", CHARACTERS_FIELD)] {
            self.sync_values(
                key,
                ORIGIN_FACES,
                "SELECT DISTINCT f.media_id, p.name FROM face f JOIN person p ON p.id = f.person_id WHERE p.kind = ?1 AND p.name <> ''",
                [kind],
            )?;
        }
        Ok(())
    }

    /// Tagger results of `model` as values: general tags at `tags` or above to
    /// `x.auto_tags`, characters at `characters` or above to `x.characters`.
    pub fn sync_tag_values(&self, model: i64, tags: f32, characters: f32) -> Result<()> {
        self.sync_values(AUTO_TAGS_FIELD, ORIGIN_TAGGER, "SELECT media_id, tag FROM tag_score WHERE model_id = ?1 AND category = 0 AND score >= ?2", params![model, tags])?;
        self.sync_values(CHARACTERS_FIELD, ORIGIN_TAGGER, "SELECT media_id, tag FROM tag_score WHERE model_id = ?1 AND category = 4 AND score >= ?2", params![model, characters])
    }

    /// Forgets every result of a model (its vectors are useless to another one).
    pub fn forget_model(&self, key: &str) -> Result<()> {
        self.conn.execute("DELETE FROM ml_model WHERE key = ?1", [key])?;
        Ok(())
    }
}
