//! Item properties kept in a shard (a shard source with `props_in_shard`): its items'
//! values, favorites, statistics, flags and loop segments go with the shard to other
//! libraries and devices, encrypted with it when it is encrypted.
//!
//! The library database stays the working copy (searches run on it). Changes reach the
//! shard shortly after they are made (`meta_rev` stamps from database triggers, written by
//! a background pass every `shard.props_flush_secs` and when the bundle closes), and what a
//! shard holds is taken in after every scan of it and when it is unlocked, so a shard used
//! by another library brings its changes along.
//!
//! Faces and people go along too (`vp_db::face_sync`): a library takes in what the shard
//! holds before writing its own, keeping the newer of two changes, so a shard analysed on
//! one device and edited on another brings both sides together.
//!
//! Storage: ordinary files under the shard's reserved folder,
//! `.viewport/props/<xx>.json` and `.viewport/faces/<xx>.json`, one per first two hex
//! digits of the item's id. The id stays with a file through renames and moves, also by
//! other apps; a replaced file starts over. People are in `.viewport/people.json`, face
//! pictures in `.viewport/crops/<face uid>.jpg` and, when the library chose so
//! (`embeddings_in_shard`), identity vectors in `.viewport/embed/<face uid>.bin`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use vp_db::SourceRow;
use vp_db::face_sync::{ItemFaces, PeopleDoc};
use vp_db::props::{ItemProps, PropField};
use vp_shard::Shard;

use crate::error::CmdResult;
use crate::workspace::{Workspace, now_ms};

const DIR: &str = ".viewport/props";
const FACES_DIR: &str = ".viewport/faces";
const PEOPLE: &str = ".viewport/people.json";
const CROPS_DIR: &str = ".viewport/crops";
const EMBED_DIR: &str = ".viewport/embed";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Bucket {
    #[serde(default)]
    version: u32,
    /// Definitions of the fields its items use.
    #[serde(default)]
    fields: BTreeMap<String, PropField>,
    /// By item id (hex).
    #[serde(default)]
    items: BTreeMap<String, ItemProps>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct FaceBucket {
    #[serde(default)]
    version: u32,
    /// By item id (hex).
    #[serde(default)]
    items: BTreeMap<String, ItemFaces>,
}

fn bucket_path(id: &str) -> String {
    format!("{DIR}/{}.json", &id[..2.min(id.len())])
}

fn face_bucket_path(id: &str) -> String {
    format!("{FACES_DIR}/{}.json", &id[..2.min(id.len())])
}

fn crop_path(uid: &str) -> String {
    format!("{CROPS_DIR}/{uid}.jpg")
}

fn embed_path(uid: &str) -> String {
    format!("{EMBED_DIR}/{uid}.bin")
}

fn read_file(shard: &Shard, path: &str) -> Option<Vec<u8>> {
    shard.open_file(path).ok().and_then(|r| r.read_all().ok())
}

fn read_json<T: serde::de::DeserializeOwned + Default>(shard: &Shard, path: &str) -> T {
    read_file(shard, path).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Whether `shard` holds item properties.
pub fn has_props(shard: &Shard) -> bool {
    shard.files_under(DIR).is_ok_and(|f| !f.is_empty())
}

/// Writes the properties of `source`'s items that changed since the last pass. Returns
/// how many items were written; nothing when the shard is not open for writing.
pub fn flush(ws: &Workspace, source: &SourceRow) -> CmdResult<usize> {
    if !crate::shards::props_in_shard(source) {
        return Ok(0);
    }
    let Ok(shard) = ws.shard(source) else { return Ok(0) };
    if !shard.is_writable() {
        return Ok(0);
    }
    let (changed, upto, people_changed) = {
        let db = ws.read_db.lock().unwrap();
        let synced = db.meta_synced(source.id)?;
        (db.meta_changed(source.id, synced)?, db.meta_counter()?, db.people_rev()? > synced)
    };
    if changed.is_empty() && !people_changed {
        return Ok(0);
    }
    sync_people(ws, &shard, source)?;
    let ids: HashSet<String> = shard.files_under("").unwrap_or_default().into_iter().filter_map(|(p, _)| shard.file_id(&p)).collect();
    // Item id in the shard -> library item.
    let changed: Vec<(String, i64)> = changed.iter().filter_map(|c| shard.file_id(&c.rel_path).map(|id| (id, c.media_id))).collect();
    flush_faces(ws, &shard, source, &changed, &ids)?;
    // Item id -> properties, grouped by bucket.
    let mut buckets: HashMap<String, Vec<(String, ItemProps)>> = HashMap::new();
    {
        let db = ws.read_db.lock().unwrap();
        for (id, media) in &changed {
            buckets.entry(bucket_path(id)).or_default().push((id.clone(), db.item_props(*media)?));
        }
    }
    let mut written = 0;
    for (path, items) in buckets {
        let mut b = read_bucket(&shard, &path);
        b.version = 1;
        for (id, props) in items {
            if props.is_empty() {
                b.items.remove(&id);
            } else {
                b.items.insert(id, props);
            }
            written += 1;
        }
        // Drop what belongs to files no longer there, and keep only used field definitions.
        b.items.retain(|id, _| ids.contains(id));
        let keys: std::collections::BTreeSet<String> = b.items.values().flat_map(|p| p.values.iter().map(|v| v.field.clone())).collect();
        let mut fields = ws.read_db.lock().unwrap().prop_fields(keys.iter().cloned())?;
        b.fields = keys.into_iter().filter_map(|k| fields.remove(&k).or_else(|| b.fields.remove(&k)).map(|f| (k, f))).collect();
        let now = Some(now_ms());
        shard.put(&path, &serde_json::to_vec(&b)?, now, now)?;
    }
    ws.write_db.lock().unwrap().set_meta_synced(source.id, upto)?;
    crate::applog::info("props", format!("wrote properties of {written} items to shard '{}'", source.name));
    Ok(written)
}

fn read_bucket(shard: &Shard, path: &str) -> Bucket {
    read_json(shard, path)
}

/// Takes in the people the shard holds, then writes this library's (now the newer of
/// both). Returns whether anything here changed.
fn sync_people(ws: &Workspace, shard: &Shard, source: &SourceRow) -> CmdResult<bool> {
    let theirs: PeopleDoc = read_json(shard, PEOPLE);
    let changed = ws.write_db.lock().unwrap().merge_people_doc(&theirs)?;
    if shard.is_writable() {
        let ours = ws.read_db.lock().unwrap().people_doc(source.id)?;
        if ours != theirs {
            let now = Some(now_ms());
            shard.put(PEOPLE, &serde_json::to_vec(&ours)?, now, now)?;
        }
    }
    Ok(changed)
}

/// Writes the faces of `changed` items (item id in the shard, library item), after
/// taking in what the shard holds for them.
fn flush_faces(ws: &Workspace, shard: &Shard, source: &SourceRow, changed: &[(String, i64)], ids: &HashSet<String>) -> CmdResult<()> {
    let embeddings = crate::shards::embeddings_in_shard(source);
    let iou = ws.settings.read().unwrap().analysis.same_face_iou;
    let doc: PeopleDoc = read_json(shard, PEOPLE);
    let mut buckets: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    for (id, media) in changed {
        buckets.entry(face_bucket_path(id)).or_default().push((id.clone(), *media));
    }
    for (path, items) in buckets {
        let mut b: FaceBucket = read_json(shard, &path);
        b.version = 1;
        for (id, media) in items {
            let before = b.items.remove(&id);
            if let Some(theirs) = &before {
                let added = ws.write_db.lock().unwrap().merge_item_faces(media, theirs, &doc, iou)?;
                fill_faces(ws, shard, &added)?;
            }
            let now = ws.read_db.lock().unwrap().item_faces(media)?;
            write_face_files(ws, shard, &now, before.as_ref(), embeddings)?;
            if !now.is_empty() {
                b.items.insert(id, now);
            }
        }
        let gone: Vec<String> = b.items.keys().filter(|id| !ids.contains(*id)).cloned().collect();
        for id in gone {
            if let Some(f) = b.items.remove(&id) {
                f.faces.iter().for_each(|f| remove_face_files(shard, &f.uid));
            }
        }
        let now = Some(now_ms());
        shard.put(&path, &serde_json::to_vec(&b)?, now, now)?;
    }
    Ok(())
}

/// Writes the pictures (and vectors, if kept) of an item's faces the shard lacks, and
/// removes those of faces that are gone.
fn write_face_files(ws: &Workspace, shard: &Shard, now: &ItemFaces, before: Option<&ItemFaces>, embeddings: bool) -> CmdResult<()> {
    for f in &now.faces {
        let crop = crop_path(&f.uid);
        if !shard.exists(&crop)
            && let Some(jpeg) = crate::ml::face_crop_by_uid(ws, &f.uid)?
        {
            shard.put(&crop, &jpeg, None, None)?;
        }
        let emb = embed_path(&f.uid);
        if embeddings
            && !shard.exists(&emb)
            && let Some(v) = ws.read_db.lock().unwrap().face_embedding(&f.uid)?
        {
            shard.put(&emb, &v, None, None)?;
        }
    }
    if let Some(before) = before {
        for f in before.faces.iter().filter(|f| !now.faces.iter().any(|n| n.uid == f.uid)) {
            remove_face_files(shard, &f.uid);
        }
    }
    Ok(())
}

fn remove_face_files(shard: &Shard, uid: &str) {
    for p in [crop_path(uid), embed_path(uid)] {
        if shard.exists(&p) {
            let _ = shard.delete(&p);
        }
    }
}

/// Gives faces just taken in their picture (into this library's packs) and vector.
fn fill_faces(ws: &Workspace, shard: &Shard, uids: &[String]) -> CmdResult<()> {
    for uid in uids {
        let crop = read_file(shard, &crop_path(uid)).and_then(|jpeg| crate::ml::keep_crop_jpeg(ws, &jpeg));
        let embedding = read_file(shard, &embed_path(uid));
        if crop.is_some() || embedding.is_some() {
            ws.write_db.lock().unwrap().set_face_extras(uid, embedding.as_deref(), crop)?;
        }
    }
    Ok(())
}

/// Takes in the people and faces the shard holds for `source`'s items. Returns how many
/// items (and people) changed.
fn import_faces(ws: &Workspace, shard: &Shard, source: &SourceRow) -> CmdResult<usize> {
    let doc: PeopleDoc = read_json(shard, PEOPLE);
    let mut changed = usize::from(ws.write_db.lock().unwrap().merge_people_doc(&doc)?);
    let mut by_id: HashMap<String, ItemFaces> = HashMap::new();
    for (path, _) in shard.files_under(FACES_DIR).unwrap_or_default() {
        let b: FaceBucket = read_json(shard, &path);
        by_id.extend(b.items);
    }
    if by_id.is_empty() {
        return Ok(changed);
    }
    let iou = ws.settings.read().unwrap().analysis.same_face_iou;
    let files = ws.read_db.lock().unwrap().known_files(source.id)?;
    for (rel, f) in files {
        if ws.closed.load(Ordering::Relaxed) {
            break;
        }
        let Some(theirs) = shard.file_id(&rel).and_then(|id| by_id.remove(&id)) else { continue };
        if ws.read_db.lock().unwrap().item_faces(f.id)? == theirs {
            continue;
        }
        let added = ws.write_db.lock().unwrap().merge_item_faces(f.id, &theirs, &doc, iou)?;
        fill_faces(ws, shard, &added)?;
        changed += 1;
    }
    Ok(changed)
}

/// Takes in the properties `source`'s shard holds (after writing out pending local
/// changes). Only items whose properties differ are touched. Returns how many changed.
pub fn import(ws: &Workspace, source: &SourceRow) -> CmdResult<usize> {
    if !crate::shards::props_in_shard(source) {
        return Ok(0);
    }
    let Ok(shard) = ws.shard(source) else { return Ok(0) };
    flush(ws, source)?;
    let mut changed = import_faces(ws, &shard, source)?;
    if changed > 0 {
        ws.write_db.lock().unwrap().sync_people_values()?;
    }
    let mut fields: HashMap<String, PropField> = HashMap::new();
    let mut by_id: HashMap<String, ItemProps> = HashMap::new();
    for (path, _) in shard.files_under(DIR).unwrap_or_default() {
        let b = read_bucket(&shard, &path);
        fields.extend(b.fields);
        by_id.extend(b.items);
    }
    let files = if by_id.is_empty() { Default::default() } else { ws.read_db.lock().unwrap().known_files(source.id)? };
    for (rel, f) in files {
        let Some(props) = shard.file_id(&rel).and_then(|id| by_id.remove(&id)) else { continue };
        let db = ws.write_db.lock().unwrap();
        if db.item_props(f.id)? != props {
            db.apply_item_props(f.id, &props, &fields)?;
            changed += 1;
        }
    }
    // What was just taken in need not be written back.
    let db = ws.write_db.lock().unwrap();
    let upto = db.meta_counter()?;
    db.set_meta_synced(source.id, upto)?;
    if changed > 0 {
        crate::applog::info("props", format!("took in properties of {changed} items from shard '{}'", source.name));
    }
    Ok(changed)
}

/// Writes (on) or removes (off) the identity vectors of `source`'s faces in its shard.
/// Returns how many files were written or removed.
pub fn write_embeddings(ws: &Workspace, source: &SourceRow, on: bool, task: &crate::tasks::Task) -> CmdResult<usize> {
    let Ok(shard) = ws.shard(source) else { return Ok(0) };
    if !shard.is_writable() {
        return Ok(0);
    }
    let mut n = 0;
    if on {
        let uids = ws.read_db.lock().unwrap().source_face_uids(source.id)?;
        task.set(0, Some(uids.len() as u64));
        for uid in uids {
            if ws.closed.load(Ordering::Relaxed) {
                break;
            }
            let p = embed_path(&uid);
            if !shard.exists(&p)
                && let Some(v) = ws.read_db.lock().unwrap().face_embedding(&uid)?
            {
                shard.put(&p, &v, None, None)?;
                n += 1;
            }
            task.add(1);
        }
    } else {
        let files = shard.files_under(EMBED_DIR).unwrap_or_default();
        task.set(0, Some(files.len() as u64));
        for (p, _) in files {
            shard.delete(&p)?;
            n += 1;
            task.add(1);
        }
    }
    Ok(n)
}

/// Writes pending properties of every shard that keeps them.
pub fn flush_all(ws: &Workspace) {
    let sources = ws.read_db.lock().unwrap().sources().unwrap_or_default();
    for s in sources.iter().filter(|s| crate::shards::props_in_shard(s)) {
        if let Err(e) = flush(ws, s) {
            crate::applog::warn("props", format!("could not write properties to shard '{}': {}", s.name, e.message));
        }
    }
}

/// Runs [`flush_all`] every `shard.props_flush_secs` until the bundle closes.
pub fn start(ws: Arc<Workspace>) {
    std::thread::spawn(move || {
        loop {
            let secs = ws.settings.read().unwrap().shard.props_flush_secs.max(1);
            for _ in 0..secs * 10 {
                if ws.closed.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            flush_all(&ws);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_db::{ExtValue, NewSource};
    use vp_driver::DriverRegistry;
    use vp_shard::CreateOptions;

    fn scan(ws: &Workspace, registry: &DriverRegistry, s: &SourceRow) {
        let drv = ws.driver(registry, s).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: s.id, include_nonmedia: true, now_ms: 1, cancel: &cancel, added_times: None }
            .run(&mut |_| {})
            .unwrap();
    }

    fn attach(dir: &std::path::Path, usb: &std::path::Path, name: &str, cfg: serde_json::Value) -> (Workspace, SourceRow) {
        let ws = Workspace::create(&dir.join(format!("{name}.vpbun")), name).unwrap();
        let id = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "usb".into(), driver: crate::shards::DRIVER.into(), config: cfg, root: usb.display().to_string() })
            .unwrap();
        let s = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
        (ws, s)
    }

    /// Properties written by one library reach another that attaches the same shard,
    /// through a rename; the app's own files stay out of the library.
    #[test]
    fn properties_travel_with_the_shard() {
        let dir = tempfile::tempdir().unwrap();
        let registry = DriverRegistry::with_builtin();
        let usb = dir.path().join("usb.vpshard");
        Shard::create(&usb, &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();
        let cfg = serde_json::json!({ "props_in_shard": true });

        let (one, s1) = attach(dir.path(), &usb, "one", cfg.clone());
        one.shard(&s1).unwrap().put("a.jpg", b"aaa", None, None).unwrap();
        scan(&one, &registry, &s1);
        let a = one.read_db.lock().unwrap().known_files(s1.id).unwrap()["a.jpg"].id;
        one.write_db.lock().unwrap().set_ext_values(a, "x.rating", &[ExtValue::Int(4)], "user").unwrap();
        one.write_db.lock().unwrap().set_ext_values(a, "x.favorite", &[ExtValue::Int(1)], "user").ok();
        assert_eq!(flush(&one, &s1).unwrap(), 1);
        assert_eq!(flush(&one, &s1).unwrap(), 0, "nothing new");
        // Moved by the first library: the properties stay with the file.
        one.shard(&s1).unwrap().rename("a.jpg", "trip/a.jpg").unwrap();
        one.close_shard(s1.id).unwrap();
        drop(one);

        let (two, s2) = attach(dir.path(), &usb, "two", cfg);
        scan(&two, &registry, &s2);
        let files = two.read_db.lock().unwrap().known_files(s2.id).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["trip/a.jpg"], "the props folder is not listed");
        assert_eq!(import(&two, &s2).unwrap(), 1);
        let props = two.read_db.lock().unwrap().item_props(files["trip/a.jpg"].id).unwrap();
        assert!(props.values.iter().any(|v| v.field == "x.rating" && v.int == Some(4)));
        // Taken in, not written back; taking in again changes nothing.
        assert_eq!(flush(&two, &s2).unwrap(), 0);
        assert_eq!(import(&two, &s2).unwrap(), 0);
        two.close_shard(s2.id).unwrap();
    }

    /// The PC analyses a shard and names a person; the phone sees the faces (with their
    /// pictures) and renames him; back on the PC the new name is there. Vectors go into
    /// the shard only when the PC keeps them there, and can be removed again.
    #[test]
    fn faces_and_people_travel_with_the_shard() {
        use vp_db::faces::NewFace;
        let dir = tempfile::tempdir().unwrap();
        let registry = DriverRegistry::with_builtin();
        let usb = dir.path().join("usb.vpshard");
        Shard::create(&usb, &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();

        let (pc, s1) = attach(dir.path(), &usb, "pc", serde_json::json!({ "props_in_shard": true, "embeddings_in_shard": true }));
        pc.shard(&s1).unwrap().put("a.jpg", b"aaa", None, None).unwrap();
        scan(&pc, &registry, &s1);
        let a = pc.read_db.lock().unwrap().known_files(s1.id).unwrap()["a.jpg"].id;
        let crop = crate::ml::keep_crop_jpeg(&pc, b"jpeg!").unwrap();
        {
            let db = pc.write_db.lock().unwrap();
            let emb = db.ml_model_id("sface@1", "face_embedder").unwrap();
            let f = NewFace { model_id: emb, kind: "real".into(), bbox: [0.1, 0.1, 0.2, 0.2], score: 0.9, embedding: Some(vec![7; 8]), crop: Some(crop) };
            db.record_analysis(a, &[emb], true, &[f], None, 1).unwrap();
            let face = db.media_faces(a).unwrap()[0].id;
            db.apply_grouping("real", &[], &[(face, 0)], 1).unwrap();
            let person = db.people("real", false).unwrap()[0].id;
            db.rename_person(person, "김철수").unwrap();
        }
        assert_eq!(flush(&pc, &s1).unwrap(), 1);
        let shard = pc.shard(&s1).unwrap();
        assert_eq!(shard.files_under(CROPS_DIR).unwrap().len(), 1);
        assert_eq!(shard.files_under(EMBED_DIR).unwrap().len(), 1);
        drop(shard);
        pc.close_shard(s1.id).unwrap();

        let (phone, s2) = attach(dir.path(), &usb, "phone", serde_json::json!({ "props_in_shard": true }));
        scan(&phone, &registry, &s2);
        assert!(import(&phone, &s2).unwrap() > 0);
        let people = phone.read_db.lock().unwrap().people("real", false).unwrap();
        assert_eq!((people.len(), people[0].name.as_str(), people[0].faces), (1, "김철수", 1));
        let face = phone.read_db.lock().unwrap().person_faces(people[0].id, 0, 1).unwrap()[0].id;
        assert_eq!(crate::ml::face_crop(&phone, face).unwrap().as_deref(), Some(&b"jpeg!"[..]), "the picture came along");
        std::thread::sleep(std::time::Duration::from_millis(5));
        phone.write_db.lock().unwrap().rename_person(people[0].id, "철수").unwrap();
        flush(&phone, &s2).unwrap();
        phone.close_shard(s2.id).unwrap();

        assert!(import(&pc, &s1).unwrap() > 0);
        let people = pc.read_db.lock().unwrap().people("real", false).unwrap();
        assert_eq!(people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["철수"]);
        let task = crate::tasks::begin("test", "", crate::tasks::Unit::Items, None);
        assert_eq!(write_embeddings(&pc, &s1, false, &task).unwrap(), 1);
        assert!(pc.shard(&s1).unwrap().files_under(EMBED_DIR).unwrap().is_empty());
        pc.close_shard(s1.id).unwrap();
    }
}
