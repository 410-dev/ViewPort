//! Source mirrors (`vp_db::mirror`): a source kept copied into a folder of another source,
//! e.g. a phone's photo folder into a shard on a USB drive that is only sometimes connected.
//!
//! A sync runs by hand, and by itself as each mirror's triggers say (`MirrorTriggers`):
//! when the bundle opens, when the target (or source) comes back after being away, when a
//! scan finds the source changed, and every so often while both stay. It needs both connected
//! (and the target writable): a source that is away is never read as "everything deleted".
//! The source is scanned first, then:
//! - files gone from the source are deleted from the target if the mirror's copy there is
//!   unchanged (`propagate_deletes`);
//! - new files are copied; changed ones replace the mirror's copy if that copy is unchanged,
//!   otherwise go in beside it under a new name;
//! - a file already at a new file's place that the mirror did not put there is left alone:
//!   the same file is adopted, a different one gets the new file beside it;
//! - property changes (values, favorites, statistics, ...) of source items reach their
//!   copies.
//! Only what the mirror copied is tracked, so files others put into the target are never
//! touched.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};
use vp_db::SourceRow;
use vp_db::mirror::{MirrorItem, MirrorRow, MirrorTriggers};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::shards::{Conflicts, CopyJob, OnConflict};
use crate::tasks::{self, Unit};
use crate::workspace::{Workspace, now_ms};

fn workspace(app: &AppHandle) -> CmdResult<Arc<Workspace>> {
    app.state::<AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new("internal", e))?
}

#[tauri::command]
pub async fn mirror_list(app: AppHandle) -> CmdResult<Vec<MirrorRow>> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().mirrors()?)).await
}

/// Creates (`id` None) or changes a mirror, then syncs it when it is on.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn mirror_save(
    app: AppHandle,
    id: Option<i64>,
    source_id: i64,
    target_id: i64,
    target_folder: String,
    enabled: bool,
    propagate_deletes: bool,
    triggers: Option<MirrorTriggers>,
) -> CmdResult<i64> {
    let ws = workspace(&app)?;
    let ws2 = ws.clone();
    let is_new = id.is_none();
    let has_triggers = triggers.is_some();
    let id = blocking(move || {
        if source_id == target_id {
            return Err(CmdError::new("invalid", "a source cannot mirror into itself"));
        }
        let folder = vp_shard::normalize_path(&target_folder).ok_or_else(|| CmdError::new("invalid", "invalid folder"))?;
        let db = ws2.write_db.lock().unwrap();
        for s in [source_id, target_id] {
            db.source(s)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        }
        let saved = db.save_mirror(id, source_id, target_id, &folder, enabled, propagate_deletes, now_ms())?;
        if let Some(t) = &triggers {
            db.set_mirror_triggers(saved, t)?;
        }
        Ok(saved)
    })
    .await?;
    // A new mirror, or one switched on or pointed elsewhere, catches up at once; changing
    // only when it syncs does not start one.
    if enabled && (is_new || !has_triggers) {
        spawn_sync(&app, ws, id);
    }
    Ok(id)
}

#[tauri::command]
pub async fn mirror_delete(app: AppHandle, id: i64) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.write_db.lock().unwrap().delete_mirror(id)?)).await
}

/// Syncs a mirror now (in the background, task `mirror`).
#[tauri::command]
pub fn mirror_sync(app: AppHandle, id: i64) -> CmdResult<()> {
    spawn_sync(&app, workspace(&app)?, id);
    Ok(())
}

/// Mirrors syncing now.
static RUNNING: Mutex<Option<HashSet<i64>>> = Mutex::new(None);

fn spawn_sync(app: &AppHandle, ws: Arc<Workspace>, id: i64) {
    {
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if !running.get_or_insert_with(HashSet::new).insert(id) {
            return;
        }
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let result = ws.read_db.lock().unwrap().mirror(id).map_err(CmdError::from).and_then(|m| m.ok_or_else(|| CmdError::new("not_found", "mirror")));
        let result = result.and_then(|m| sync(&app, &ws, &m));
        let (at, text) = match &result {
            Ok(summary) => (Some(now_ms()), summary.clone()),
            Err(e) => (None, format!("not synced: {}", e.message)),
        };
        crate::applog::info("mirror", format!("mirror {id}: {text}"));
        let _ = ws.write_db.lock().unwrap().set_mirror_result(id, at, &text);
        if let Some(r) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            r.remove(&id);
        }
    });
}

/// Syncs the mirrors of a source that a scan just found changed (`on_scan`). A mirror's
/// own scan of its source, during its sync, starts nothing: that mirror is running.
pub fn scanned(app: &AppHandle, ws: &Arc<Workspace>, source_id: i64) {
    let Ok(mirrors) = ws.read_db.lock().unwrap().mirrors() else { return };
    for m in mirrors.iter().filter(|m| m.enabled && m.source_id == source_id && m.triggers.on_scan) {
        spawn_sync(app, ws.clone(), m.id);
    }
}

/// Whether a mirror syncs at this look: `first` is the first look since the bundle opened,
/// `was` whether both ends were connected at the last look, `since` how long ago it last
/// synced (or the watch began).
fn wants_sync(t: &MirrorTriggers, default_mins: u64, first: bool, was: bool, since: std::time::Duration) -> bool {
    if first {
        return t.on_open;
    }
    if !was && t.on_connect {
        return true;
    }
    let mins = t.interval_mins.unwrap_or(default_mins);
    mins > 0 && since.as_secs() >= mins * 60
}

/// Watches the bundle's mirrors until it closes and syncs each as its triggers say: when
/// the bundle opens, when its source and target are both there again after one was away,
/// and every so often while they stay.
pub fn start(app: &AppHandle, ws: Arc<Workspace>) {
    let app = app.clone();
    std::thread::spawn(move || {
        // Mirror id -> (was reachable at the last look, when it last synced or the watch began).
        let mut seen: HashMap<i64, (bool, std::time::Instant)> = HashMap::new();
        let mut first = true;
        loop {
            let settings = ws.settings.read().unwrap().mirror.clone();
            let mirrors = ws.read_db.lock().unwrap().mirrors().unwrap_or_default();
            if !mirrors.is_empty() {
                let offline: HashSet<i64> = ws.offline_sources().into_iter().chain(ws.locked_sources()).collect();
                for m in mirrors.iter().filter(|m| m.enabled) {
                    let reachable = !offline.contains(&m.source_id) && !offline.contains(&m.target_id);
                    // A mirror added since the bundle opened synced when it was saved.
                    let (was, last) = seen.get(&m.id).copied().unwrap_or((first || reachable, std::time::Instant::now()));
                    if reachable && wants_sync(&m.triggers, settings.resync_mins, first, was, last.elapsed()) {
                        spawn_sync(&app, ws.clone(), m.id);
                        seen.insert(m.id, (true, std::time::Instant::now()));
                    } else {
                        seen.insert(m.id, (reachable, last));
                    }
                }
            }
            first = false;
            for _ in 0..settings.check_secs.max(1) * 10 {
                if ws.closed.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    });
}

fn source(ws: &Workspace, id: i64) -> CmdResult<SourceRow> {
    ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))
}

/// The copy at `path` is still what the mirror wrote.
fn untouched(e: &vp_driver::Entry, item: &MirrorItem) -> bool {
    e.size == item.target_size && e.modified_ms == item.target_modified_ms
}

/// `path` with " (2)", " (3)", ... before its extension: the first one neither in `out` nor
/// already planned.
fn free_name(out: &dyn vp_driver::MediaDriver, path: &str, planned: &HashSet<String>) -> String {
    let (dir, name) = path.rsplit_once('/').map(|(d, n)| (Some(d), n)).unwrap_or((None, path));
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| {
            let name = format!("{stem} ({n}){ext}");
            dir.map(|d| format!("{d}/{name}")).unwrap_or(name)
        })
        .find(|p| !planned.contains(p) && out.stat(p).is_err())
        .expect("some name is free")
}

/// One sync; returns a short summary.
pub fn sync(app: &AppHandle, ws: &Arc<Workspace>, m: &MirrorRow) -> CmdResult<String> {
    let registry = app.state::<AppState>().drivers.clone();
    sync_with(ws, &registry, m, &|s| crate::commands::scan_now(app, ws.clone(), s.clone()).map(|_| ()))
}

/// Folders (relative to `src`, ending in `/`) of `src` and `tgt` when they are shards
/// kept inside `src`.
fn own_shards(ws: &Workspace, registry: &vp_driver::DriverRegistry, src: &SourceRow, tgt: &SourceRow) -> Vec<String> {
    let Some(root) = ws.driver(registry, src).ok().and_then(|d| d.local_path("")) else { return Vec::new() };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    [src, tgt]
        .into_iter()
        .filter(|s| s.driver == crate::shards::DRIVER)
        .filter_map(|s| ws.shard_root(s).ok())
        .filter_map(|dir| {
            let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
            let rel = dir.strip_prefix(&root).ok()?;
            let rel: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
            (!rel.is_empty()).then(|| format!("{}/", rel.join("/")))
        })
        .collect()
}

/// [`sync`], scanning sources with `scan`.
fn sync_with(
    ws: &Arc<Workspace>,
    registry: &vp_driver::DriverRegistry,
    m: &MirrorRow,
    scan: &dyn Fn(&SourceRow) -> Result<(), String>,
) -> CmdResult<String> {
    if !ws.autosave() {
        return Err(CmdError::new("read_only", "the bundle is open without autosave"));
    }
    let (src, tgt) = (source(ws, m.source_id)?, source(ws, m.target_id)?);
    let away: HashSet<i64> = ws.offline_sources().into_iter().chain(ws.locked_sources()).collect();
    if away.contains(&src.id) || away.contains(&tgt.id) {
        return Err(CmdError::new("offline", "the source or the target is not connected"));
    }
    let shard = if tgt.driver == crate::shards::DRIVER { Some(crate::shards::writable(ws, &tgt)?) } else { None };
    let out = ws.driver(registry, &tgt)?;
    if shard.is_none() && !out.capabilities().write {
        return Err(CmdError::new("unsupported", "files cannot be written to the target"));
    }
    // What is in the source now (a source away was ruled out above).
    scan(&src).map_err(|e| CmdError::new("scan", e))?;

    let cancel = Arc::new(AtomicBool::new(false));
    let task = tasks::begin_with_cancel("mirror", format!("{} → {}", src.name, tgt.name), Unit::Bytes, None, Some(cancel.clone()));
    // The source's and the target's own shard folders, when they lie inside the source,
    // are never copied (the target would copy into itself). Other shards are just files.
    let mut present = ws.read_db.lock().unwrap().present_files(src.id)?;
    let skip = own_shards(ws, registry, &src, &tgt);
    present.retain(|rel, _| !skip.iter().any(|d| rel.starts_with(d.as_str())));
    let tracked = ws.read_db.lock().unwrap().mirror_items(m.id)?;

    // Deletions: only copies the mirror made and nobody changed since.
    let mut deleted = 0;
    for (rel, item) in tracked.iter().filter(|(rel, _)| !present.contains_key(*rel)) {
        if m.propagate_deletes
            && let Ok(e) = out.stat(&item.target_path)
            && untouched(&e, item)
        {
            let gone = match &shard {
                Some(s) => s.delete(&item.target_path).map(|_| ()).map_err(CmdError::from),
                None => out.delete(&item.target_path).map_err(CmdError::from),
            };
            if gone.is_ok() {
                deleted += 1;
            }
        }
        ws.write_db.lock().unwrap().delete_mirror_item(m.id, rel)?;
    }

    // Copies, with their targets settled here (renames included).
    let mut jobs = Vec::new();
    let mut planned: HashSet<String> = HashSet::new();
    let mut adopted = Vec::new();
    let mut paths: Vec<&String> = present.keys().collect();
    paths.sort();
    for rel in paths {
        if cancel.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed) {
            return Err(CmdError::new("cancelled", "cancelled"));
        }
        let f = &present[rel];
        let place = crate::shards::join_rel(&m.target_folder, rel);
        let target = match tracked.get(rel) {
            Some(item) => {
                let unchanged = f.size == item.size && f.modified_ms == item.modified_ms;
                match out.stat(&item.target_path) {
                    // Up to date.
                    Ok(_) if unchanged => continue,
                    // Replace the mirror's own copy; one someone changed stays.
                    Ok(e) if untouched(&e, item) => item.target_path.clone(),
                    Ok(_) => free_name(out.as_ref(), &item.target_path, &planned),
                    // Gone from the target: put it back.
                    Err(_) => item.target_path.clone(),
                }
            }
            None => match out.stat(&place) {
                Err(_) => place,
                Ok(e) => {
                    let from = ws.driver(registry, &src)?;
                    if e.size == f.size && crate::shards::same_content(out.as_ref(), &place, from.as_ref(), rel).unwrap_or(false) {
                        adopted.push((rel.clone(), place, e));
                        continue;
                    }
                    free_name(out.as_ref(), &place, &planned)
                }
            },
        };
        planned.insert(target.clone());
        jobs.push(CopyJob {
            id: f.id,
            source_id: src.id,
            rel: rel.clone(),
            target,
            size: f.size,
            created_ms: f.created_ms,
            modified_ms: f.modified_ms,
        });
    }
    let rels: HashMap<i64, String> = jobs.iter().map(|j| (j.id, j.rel.clone())).collect();
    let wanted = jobs.len();
    let report = if jobs.is_empty() {
        crate::shards::CopyReport { failed: 0, skipped: 0, copied: Vec::new() }
    } else {
        crate::shards::copy_files(ws, registry, &tgt, jobs, Conflicts::Fixed(OnConflict::Overwrite), &task, &cancel, || {
            let _ = scan(&tgt);
        })?
    };
    if report.copied.is_empty() && deleted > 0 {
        let _ = scan(&tgt);
    }

    // Track what is there now. Copies carried their properties (`copy_files`).
    {
        let db = ws.write_db.lock().unwrap();
        for (id, target) in &report.copied {
            let (Some(rel), Ok(e)) = (rels.get(id), out.stat(target)) else { continue };
            let f = &present[rel];
            db.put_mirror_item(m.id, &MirrorItem {
                rel_path: rel.clone(),
                target_path: target.clone(),
                size: f.size,
                modified_ms: f.modified_ms,
                target_size: e.size,
                target_modified_ms: e.modified_ms,
                meta_rev: db.meta_rev(*id)?,
            })?;
        }
        for (rel, target, e) in &adopted {
            let f = &present[rel];
            // Taken over as if the mirror had copied it; its properties follow below.
            db.put_mirror_item(m.id, &MirrorItem {
                rel_path: rel.clone(),
                target_path: target.clone(),
                size: f.size,
                modified_ms: f.modified_ms,
                target_size: e.size,
                target_modified_ms: e.modified_ms,
                meta_rev: -1,
            })?;
        }
        // Copies count as added when their originals were.
        let mut added: Vec<(i64, String)> = report.copied.clone();
        added.extend(adopted.iter().map(|(rel, target, _)| (present[rel].id, target.clone())));
        db.copy_added_to(tgt.id, &added)?;
    }

    // Property changes of items already mirrored.
    let mut pairs = Vec::new();
    let mut revs = Vec::new();
    {
        let db = ws.read_db.lock().unwrap();
        for (rel, item) in db.mirror_items(m.id)? {
            let Some(f) = present.get(&rel) else { continue };
            let rev = db.meta_rev(f.id)?;
            if rev > item.meta_rev {
                pairs.push((f.id, item.target_path.clone()));
                revs.push((rel, rev));
            }
        }
    }
    if !pairs.is_empty() {
        let db = ws.write_db.lock().unwrap();
        db.copy_metadata_to(tgt.id, &pairs)?;
        for (rel, rev) in revs.iter() {
            db.set_mirror_item_meta(m.id, rel, (*rev).max(0))?;
        }
    }
    drop(task);
    if shard.is_some() {
        crate::shards::maybe_compact(ws, &tgt);
    }
    let summary = format!(
        "{} copied, {} deleted, {} adopted, {} properties updated{}",
        report.copied.len(),
        deleted,
        adopted.len(),
        pairs.len(),
        if report.failed > 0 || report.copied.len() < wanted { format!(", {} failed", wanted - report.copied.len()) } else { String::new() }
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_db::{ExtValue, NewSource};
    use vp_driver::DriverRegistry;
    use vp_shard::{CreateOptions, Shard};

    fn scan(ws: &Workspace, registry: &DriverRegistry, s: &SourceRow) -> Result<(), String> {
        let drv = ws.driver(registry, s).map_err(|e| e.message)?;
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: s.id, include_nonmedia: true, now_ms: now_ms(), cancel: &cancel, added_times: None }
            .run(&mut |_| {})
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[test]
    fn mirror_follows_the_source_and_leaves_other_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Arc::new(Workspace::create(&root, "Lib").unwrap());
        let registry = DriverRegistry::with_builtin();
        let phone = dir.path().join("phone");
        std::fs::create_dir_all(phone.join("sub")).unwrap();
        std::fs::write(phone.join("a.jpg"), b"aaa").unwrap();
        std::fs::write(phone.join("b.jpg"), b"bbb").unwrap();
        std::fs::write(phone.join("sub/c.jpg"), b"ccc").unwrap();
        // Another shard kept in the source folder is copied like any file.
        std::fs::create_dir_all(phone.join("Box.vpshard")).unwrap();
        std::fs::write(phone.join("Box.vpshard/000001.chunk"), b"chunk").unwrap();
        let add = |name: &str, driver: &str, root: &str, config: serde_json::Value| {
            ws.write_db.lock().unwrap().add_source(&NewSource { name: name.into(), driver: driver.into(), config, root: root.into() }).unwrap()
        };
        let src_id = add("phone", "local", &phone.display().to_string(), serde_json::json!({}));
        Shard::create(&root.join("shards/usb.vpshard"), &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();
        let usb_id = add("usb", crate::shards::DRIVER, "shards/usb.vpshard", serde_json::json!({ "in_bundle": true }));
        let src = source(&ws, src_id).unwrap();
        let usb = source(&ws, usb_id).unwrap();
        let do_scan = |s: &SourceRow| scan(&ws, &registry, s);
        let id = ws.write_db.lock().unwrap().save_mirror(None, src_id, usb_id, "phone", true, true, 1).unwrap();
        let m = || ws.read_db.lock().unwrap().mirror(id).unwrap().unwrap();
        let shard = ws.shard(&usb).unwrap();
        let read = |p: &str| shard.open_file(p).ok().map(|r| r.read_all().unwrap());

        // First sync copies everything.
        let s1 = sync_with(&ws, &registry, &m(), &do_scan).unwrap();
        assert!(s1.starts_with("4 copied"), "{s1}");
        assert_eq!(read("phone/sub/c.jpg").unwrap(), b"ccc");
        assert_eq!(read("phone/Box.vpshard/000001.chunk").unwrap(), b"chunk");

        // Meanwhile another device puts files into the target.
        shard.put("phone/ext.jpg", b"theirs", None, None).unwrap();
        shard.put("phone/x.jpg", b"their x", None, None).unwrap();
        shard.put("phone/d.jpg", b"ddd", None, None).unwrap();
        // And the source changes: one gone, one edited, two new (one already there), a
        // property changes.
        std::fs::remove_file(phone.join("a.jpg")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(phone.join("b.jpg"), b"bbb, edited").unwrap();
        std::fs::write(phone.join("x.jpg"), b"my x").unwrap();
        std::fs::write(phone.join("d.jpg"), b"ddd").unwrap();
        let c = ws.read_db.lock().unwrap().known_files(src_id).unwrap()["sub/c.jpg"].id;
        ws.write_db.lock().unwrap().set_ext_values(c, "x.rating", &[ExtValue::Int(5)], "user").unwrap();

        let s2 = sync_with(&ws, &registry, &m(), &do_scan).unwrap();
        assert!(read("phone/a.jpg").is_none(), "deleted with the source: {s2}");
        assert_eq!(read("phone/b.jpg").unwrap(), b"bbb, edited");
        assert_eq!(read("phone/ext.jpg").unwrap(), b"theirs", "not the mirror's");
        assert_eq!(read("phone/x.jpg").unwrap(), b"their x", "not the mirror's");
        assert_eq!(read("phone/x (2).jpg").unwrap(), b"my x");
        assert!(read("phone/d (2).jpg").is_none(), "the same file is adopted");
        let copy = ws.read_db.lock().unwrap().known_files(usb_id).unwrap()["phone/sub/c.jpg"].id;
        let props = ws.read_db.lock().unwrap().item_props(copy).unwrap();
        assert!(props.values.iter().any(|v| v.field == "x.rating" && v.int == Some(5)), "properties follow");
        let added = |s: i64, p: &str| ws.read_db.lock().unwrap().known_files(s).unwrap()[p].added_ms;
        assert_eq!(added(usb_id, "phone/sub/c.jpg"), added(src_id, "sub/c.jpg"), "added when the original was");

        // A copy someone changed is not deleted with its source.
        shard.put("phone/sub/c.jpg", b"edited on the usb", None, None).unwrap();
        std::fs::remove_file(phone.join("sub/c.jpg")).unwrap();
        sync_with(&ws, &registry, &m(), &do_scan).unwrap();
        assert_eq!(read("phone/sub/c.jpg").unwrap(), b"edited on the usb");

        // A source that is away is not "everything deleted".
        std::fs::rename(&phone, dir.path().join("phone-away")).unwrap();
        assert_eq!(sync_with(&ws, &registry, &m(), &do_scan).unwrap_err().code, "offline");
        assert_eq!(read("phone/b.jpg").unwrap(), b"bbb, edited");
        let _ = src;
        drop(shard);
        ws.close_shard(usb_id).unwrap();
    }

    #[test]
    fn triggers_decide_when_a_mirror_syncs() {
        use std::time::Duration;
        let t = MirrorTriggers::default();
        let min = Duration::from_secs(60);
        assert!(wants_sync(&t, 30, true, false, Duration::ZERO), "on open");
        assert!(wants_sync(&t, 30, false, false, Duration::ZERO), "back after being away");
        assert!(!wants_sync(&t, 30, false, true, 29 * min));
        assert!(wants_sync(&t, 30, false, true, 30 * min), "the library's interval");
        let quiet = MirrorTriggers { on_open: false, on_connect: false, on_scan: true, interval_mins: Some(0) };
        assert!(!wants_sync(&quiet, 30, true, false, Duration::ZERO));
        assert!(!wants_sync(&quiet, 30, false, false, Duration::ZERO));
        assert!(!wants_sync(&quiet, 30, false, true, 1000 * min), "never repeats");
        let hourly = MirrorTriggers { interval_mins: Some(60), ..quiet };
        assert!(!wants_sync(&hourly, 30, false, true, 59 * min));
        assert!(wants_sync(&hourly, 30, false, true, 60 * min));
    }

    #[test]
    fn mirror_skips_the_target_shard_kept_in_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Arc::new(Workspace::create(&dir.path().join("Lib.vpbun"), "Lib").unwrap());
        let registry = DriverRegistry::with_builtin();
        let photos = dir.path().join("photos");
        std::fs::create_dir_all(&photos).unwrap();
        std::fs::write(photos.join("a.jpg"), b"aaa").unwrap();
        let inner = photos.join("backup.vpshard");
        Shard::create(&inner, &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();
        let add = |name: &str, driver: &str, root: &str| {
            let config = serde_json::json!({});
            ws.write_db.lock().unwrap().add_source(&NewSource { name: name.into(), driver: driver.into(), config, root: root.into() }).unwrap()
        };
        let src_id = add("photos", "local", &photos.display().to_string());
        let tgt_id = add("backup", crate::shards::DRIVER, &inner.display().to_string());
        let id = ws.write_db.lock().unwrap().save_mirror(None, src_id, tgt_id, "photos", true, true, 1).unwrap();
        let m = ws.read_db.lock().unwrap().mirror(id).unwrap().unwrap();
        let s = sync_with(&ws, &registry, &m, &|s| scan(&ws, &registry, s)).unwrap();
        assert!(s.starts_with("1 copied"), "{s}");
        let tgt = source(&ws, tgt_id).unwrap();
        let shard = ws.shard(&tgt).unwrap();
        assert!(shard.open_file("photos/a.jpg").is_ok());
        drop(shard);
        ws.close_shard(tgt_id).unwrap();
    }
}
