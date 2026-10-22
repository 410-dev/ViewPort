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
//! Storage: ordinary files under the shard's reserved folder,
//! `.viewport/props/<xx>.json`, one per first two hex digits of the item's id. The id stays
//! with a file through renames and moves, also by other apps; a replaced file starts over.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use vp_db::SourceRow;
use vp_db::props::{ItemProps, PropField};
use vp_shard::Shard;

use crate::error::CmdResult;
use crate::workspace::{Workspace, now_ms};

const DIR: &str = ".viewport/props";

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

fn bucket_path(id: &str) -> String {
    format!("{DIR}/{}.json", &id[..2.min(id.len())])
}

fn read_bucket(shard: &Shard, path: &str) -> Bucket {
    shard
        .open_file(path)
        .ok()
        .and_then(|r| r.read_all().ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
    let (changed, upto) = {
        let db = ws.read_db.lock().unwrap();
        let synced = db.meta_synced(source.id)?;
        (db.meta_changed(source.id, synced)?, db.meta_counter()?)
    };
    if changed.is_empty() {
        return Ok(0);
    }
    // Item id -> properties, grouped by bucket.
    let mut buckets: HashMap<String, Vec<(String, ItemProps)>> = HashMap::new();
    {
        let db = ws.read_db.lock().unwrap();
        for c in &changed {
            let Some(id) = shard.file_id(&c.rel_path) else { continue };
            buckets.entry(bucket_path(&id)).or_default().push((id, db.item_props(c.media_id)?));
        }
    }
    let mut written = 0;
    let ids: std::collections::HashSet<String> =
        shard.files_under("").unwrap_or_default().into_iter().filter_map(|(p, _)| shard.file_id(&p)).collect();
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

/// Takes in the properties `source`'s shard holds (after writing out pending local
/// changes). Only items whose properties differ are touched. Returns how many changed.
pub fn import(ws: &Workspace, source: &SourceRow) -> CmdResult<usize> {
    if !crate::shards::props_in_shard(source) {
        return Ok(0);
    }
    let Ok(shard) = ws.shard(source) else { return Ok(0) };
    flush(ws, source)?;
    let mut fields: HashMap<String, PropField> = HashMap::new();
    let mut by_id: HashMap<String, ItemProps> = HashMap::new();
    for (path, _) in shard.files_under(DIR).unwrap_or_default() {
        let b = read_bucket(&shard, &path);
        fields.extend(b.fields);
        by_id.extend(b.items);
    }
    if by_id.is_empty() {
        return Ok(0);
    }
    let files = ws.read_db.lock().unwrap().known_files(source.id)?;
    let mut changed = 0;
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
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: s.id, include_nonmedia: true, now_ms: 1, cancel: &cancel }
            .run(&mut |_| {})
            .unwrap();
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
        let attach = |name: &str| {
            let ws = Workspace::create(&dir.path().join(format!("{name}.vpbun")), name).unwrap();
            let id = ws
                .write_db
                .lock()
                .unwrap()
                .add_source(&NewSource { name: "usb".into(), driver: crate::shards::DRIVER.into(), config: cfg.clone(), root: usb.display().to_string() })
                .unwrap();
            let s = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
            (ws, s)
        };

        let (one, s1) = attach("one");
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

        let (two, s2) = attach("two");
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
}
