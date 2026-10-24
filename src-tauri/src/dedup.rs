//! Duplicate finder: items with the same contents, within each source or across several.
//!
//! Comparing is staged so little is read: items are grouped by size (from the library),
//! then by a hash of both ends of each file, and only what still matches is read whole
//! (SHA-256). Shard items cost nothing: a shard knows every file's SHA-256 already. Hashes
//! of other items are cached in the library (`media_hash`) while their size and time stay
//! the same. Files are hashed in parallel on every CPU, with fewer reads at once from
//! network sources.
//!
//! What to do with duplicates is the user's choice per item: delete it, or, within a
//! shard, let it share another copy's stored data (`Shard::link`, copy-on-write) so the
//! space is freed while every file stays. Reports can be saved in the library; they are
//! never written to a shard.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tauri::{AppHandle, Manager};
use vp_db::dedup::{DupCandidate, DupEntry, DupReportRow, HashRow};
use vp_driver::{DriverRegistry, MediaDriver};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::tasks::{self, Task, Unit};
use crate::workspace::{BUNDLE_DRIVER, Workspace, now_ms};

type Hash = [u8; 32];

fn workspace(app: &AppHandle) -> CmdResult<Arc<Workspace>> {
    app.state::<AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new("internal", e))?
}

fn hex(h: &[u8]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash32(v: &[u8]) -> Option<Hash> {
    v.try_into().ok()
}

#[derive(Debug, Clone, Serialize)]
pub struct DupItem {
    pub media_id: i64,
    pub source_id: i64,
    pub rel_path: String,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
    pub added_ms: i64,
    /// Id of the stored data of a shard item: items with the same one already share it.
    pub body: Option<String>,
    /// From a saved report: what was done to the item (`deleted`, `linked`), or `gone`
    /// when it is no longer in the library or no longer the same file.
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DupGroup {
    pub sha256: String,
    pub size: u64,
    pub items: Vec<DupItem>,
}

#[derive(Debug, Serialize)]
pub struct DupScan {
    pub groups: Vec<DupGroup>,
    /// Items compared (those sharing a size with another).
    pub checked: u64,
    /// Bytes read to hash.
    pub read_bytes: u64,
    /// Files that could not be read, as "path: reason".
    pub failed: Vec<String>,
    pub elapsed_ms: u64,
}

struct Src {
    driver: Arc<dyn MediaDriver>,
    /// Shard files by path: size, SHA-256 and stored-data id.
    shard: HashMap<String, vp_shard::FileContent>,
    network: bool,
}

struct Entry {
    c: DupCandidate,
    partial: Option<Hash>,
    full: Option<Hash>,
    body: Option<String>,
    /// Hashes computed now, to cache (not for shard items, whose shard knows them).
    fresh: bool,
    failed: bool,
}

/// Runs `f` on each job index with `workers` threads; network jobs use their own,
/// smaller pool at the same time.
fn run_pool(jobs: &[usize], network: &dyn Fn(usize) -> bool, workers: usize, net_workers: usize, f: &(dyn Fn(usize) + Sync)) {
    let (net, local): (Vec<usize>, Vec<usize>) = jobs.iter().partition(|&&j| network(j));
    std::thread::scope(|s| {
        for (list, n) in [(&local, workers), (&net, net_workers)] {
            if list.is_empty() {
                continue;
            }
            let next = Arc::new(AtomicUsize::new(0));
            for _ in 0..n.clamp(1, list.len()) {
                let next = next.clone();
                s.spawn(move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&j) = list.get(i) else { break };
                        f(j);
                    }
                });
            }
        }
    });
}

fn interrupted() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
}

/// Hash of the size and both ends of a file (`n` bytes each).
fn partial_hash(driver: &dyn MediaDriver, rel: &str, size: u64, n: u64) -> std::io::Result<Hash> {
    let r = driver.open(rel).map_err(std::io::Error::other)?;
    let mut h = Sha256::new();
    h.update(size.to_le_bytes());
    let mut buf = vec![0u8; n.min(size) as usize];
    for off in [0, size.saturating_sub(n)] {
        let got = r.read_full_at(off, &mut buf)?;
        if got < buf.len() {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        h.update(&buf);
    }
    Ok(digest(h))
}

fn digest(h: Sha256) -> Hash {
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// SHA-256 of a whole file; `on_read` gets each piece's length.
fn full_hash(driver: &dyn MediaDriver, rel: &str, size: u64, piece: usize, stop: &dyn Fn() -> bool, on_read: &dyn Fn(u64)) -> std::io::Result<Hash> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; piece.max(64 * 1024)];
    let mut done = 0u64;
    // Local files are read sequentially so the OS reads ahead.
    if let Some(path) = driver.local_path(rel) {
        let mut f = std::fs::File::open(path)?;
        loop {
            if stop() {
                return Err(interrupted());
            }
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            done += n as u64;
            on_read(n as u64);
        }
    } else {
        let r = driver.open(rel).map_err(std::io::Error::other)?;
        loop {
            if stop() {
                return Err(interrupted());
            }
            let n = r.read_full_at(done, &mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            done += n as u64;
            on_read(n as u64);
        }
    }
    if done != size {
        return Err(std::io::Error::other("the file changed size; rescan the source"));
    }
    Ok(digest(h))
}

/// Finds duplicates among the items of `sources`: within each source, or across them.
#[tauri::command]
pub async fn dup_scan(app: AppHandle, sources: Vec<i64>, across: bool) -> CmdResult<DupScan> {
    let ws = workspace(&app)?;
    let registry = app.state::<AppState>().drivers.clone();
    blocking(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("dedup_scan", "", Unit::Bytes, Some(0), Some(cancel.clone()));
        let result = scan(&ws, &registry, &sources, across, &task, &cancel);
        match &result {
            Ok(_) => task.finish(),
            Err(e) if e.code == "cancelled" => task.cancelled(),
            Err(e) => task.fail(e.message.clone()),
        }
        result
    })
    .await
}

fn scan(ws: &Workspace, registry: &DriverRegistry, sources: &[i64], across: bool, task: &Task, cancel: &AtomicBool) -> CmdResult<DupScan> {
    let started = Instant::now();
    let settings = ws.settings.read().unwrap().dedup.clone();
    let stop = || cancel.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed);
    let workers = match settings.workers {
        0 => std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4),
        n => n as usize,
    };
    let net_workers = settings.network_workers.max(1) as usize;
    let pb = settings.partial_bytes.max(4096);
    let mut failed = Vec::new();

    let cands = ws.read_db.lock().unwrap().dup_candidates(sources, across)?;
    let mut srcs: HashMap<i64, Src> = HashMap::new();
    for id in cands.iter().map(|c| c.source_id).collect::<HashSet<_>>() {
        let Some(row) = ws.read_db.lock().unwrap().source(id)? else { continue };
        let opened = (|| -> CmdResult<Src> {
            let driver = ws.driver(registry, &row)?;
            let shard = if row.driver == crate::shards::DRIVER {
                ws.shard(&row)?.contents().into_iter().map(|c| (c.path.clone(), c)).collect()
            } else {
                HashMap::new()
            };
            let network = row.driver != BUNDLE_DRIVER && row.driver != crate::shards::DRIVER && driver.local_path("").is_none();
            Ok(Src { driver, shard, network })
        })();
        match opened {
            Ok(s) => {
                srcs.insert(id, s);
            }
            Err(e) => failed.push(format!("{}: {}", row.name, e.message)),
        }
    }
    let cached = {
        let ids: Vec<i64> = cands.iter().filter(|c| srcs.get(&c.source_id).is_some_and(|s| s.shard.is_empty())).map(|c| c.media_id).collect();
        ws.read_db.lock().unwrap().cached_hashes(&ids)?
    };
    let mut entries: Vec<Entry> = Vec::with_capacity(cands.len());
    for c in cands {
        let Some(src) = srcs.get(&c.source_id) else { continue };
        let mut e = Entry { c, partial: None, full: None, body: None, fresh: false, failed: false };
        if let Some(f) = src.shard.get(&e.c.rel_path).filter(|f| f.size == e.c.size) {
            e.full = f.sha256.map(|d| d.0);
            e.body = Some(f.body.to_string());
        } else if let Some(h) = cached.get(&e.c.media_id).filter(|h| h.size == e.c.size && h.modified_ms == e.c.modified_ms) {
            e.full = h.sha256.as_deref().and_then(hash32);
            if h.partial_len == Some(pb) {
                e.partial = h.partial.as_deref().and_then(hash32);
            }
        }
        entries.push(e);
    }
    // Groups of the same size (in the same source unless across).
    let scope = |e: &Entry| (if across { 0 } else { e.c.source_id }, e.c.size);
    let mut by_size: HashMap<(i64, u64), Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        by_size.entry(scope(e)).or_default().push(i);
    }
    let by_size: Vec<Vec<usize>> = by_size.into_values().filter(|g| g.len() > 1).collect();
    let checked = by_size.iter().map(|g| g.len() as u64).sum();

    let is_net: Vec<bool> = entries.iter().map(|e| srcs[&e.c.source_id].network).collect();
    let network = |i: usize| is_net[i];
    let read_bytes = std::sync::atomic::AtomicU64::new(0);
    let on_read = |n: u64| {
        read_bytes.fetch_add(n, Ordering::Relaxed);
        task.add(n);
    };

    // Stage 1: both ends of large files, where some item's whole hash is unknown.
    let mut jobs: Vec<usize> = Vec::new();
    for g in &by_size {
        let size = entries[g[0]].c.size;
        if size > 2 * pb && g.iter().any(|&i| entries[i].full.is_none()) {
            jobs.extend(g.iter().copied().filter(|&i| entries[i].partial.is_none()));
        }
    }
    task.add_total(jobs.iter().map(|&i| 2 * pb.min(entries[i].c.size)).sum());
    let results: Vec<std::sync::Mutex<Option<std::io::Result<Hash>>>> = (0..entries.len()).map(|_| Default::default()).collect();
    run_pool(&jobs, &network, workers, net_workers, &|i| {
        if stop() {
            return;
        }
        let e = &entries[i];
        let src = &srcs[&e.c.source_id];
        let r = partial_hash(src.driver.as_ref(), &e.c.rel_path, e.c.size, pb);
        on_read(2 * pb.min(e.c.size));
        *results[i].lock().unwrap() = Some(r);
    });
    if stop() {
        return Err(CmdError::new("cancelled", "cancelled"));
    }
    for &i in &jobs {
        match results[i].lock().unwrap().take() {
            Some(Ok(h)) => {
                entries[i].partial = Some(h);
                entries[i].fresh = true;
            }
            Some(Err(err)) => {
                entries[i].failed = true;
                failed.push(format!("{}: {err}", entries[i].c.rel_path));
            }
            None => entries[i].failed = true,
        }
    }

    // Stage 2: whole files whose ends match another's (small files directly).
    let mut jobs: Vec<usize> = Vec::new();
    for g in &by_size {
        let size = entries[g[0]].c.size;
        let mut sub: HashMap<Option<Hash>, Vec<usize>> = HashMap::new();
        for &i in g.iter().filter(|&&i| !entries[i].failed) {
            let key = if size > 2 * pb && entries[i].full.is_none() { entries[i].partial } else { None };
            sub.entry(key).or_default().push(i);
        }
        // Items with a known whole hash meet the others in the `None` subgroup only if
        // the file is small; for large files compare their ends too.
        if size > 2 * pb {
            let known: Vec<usize> = sub.remove(&None).unwrap_or_default();
            for i in known {
                match entries[i].partial {
                    Some(p) if sub.contains_key(&Some(p)) => sub.get_mut(&Some(p)).unwrap().push(i),
                    _ => {}
                }
            }
        }
        for list in sub.values().filter(|l| l.len() > 1) {
            jobs.extend(list.iter().copied().filter(|&i| entries[i].full.is_none()));
        }
    }
    // Largest first, so one big file does not finish last on its own.
    jobs.sort_by_key(|&i| std::cmp::Reverse(entries[i].c.size));
    task.add_total(jobs.iter().map(|&i| entries[i].c.size).sum());
    let piece = settings.read_bytes.clamp(64 * 1024, 64 << 20) as usize;
    run_pool(&jobs, &network, workers, net_workers, &|i| {
        if stop() {
            return;
        }
        let e = &entries[i];
        let src = &srcs[&e.c.source_id];
        task.detail(crate::workspace::file_name(&e.c.rel_path));
        let r = full_hash(src.driver.as_ref(), &e.c.rel_path, e.c.size, piece, &stop, &on_read);
        *results[i].lock().unwrap() = Some(r);
    });
    if stop() {
        return Err(CmdError::new("cancelled", "cancelled"));
    }
    for &i in &jobs {
        match results[i].lock().unwrap().take() {
            Some(Ok(h)) => {
                entries[i].full = Some(h);
                entries[i].fresh = true;
            }
            Some(Err(err)) => {
                entries[i].failed = true;
                failed.push(format!("{}: {err}", entries[i].c.rel_path));
            }
            None => entries[i].failed = true,
        }
    }

    let fresh: Vec<HashRow> = entries
        .iter()
        .filter(|e| e.fresh && e.body.is_none())
        .map(|e| HashRow {
            media_id: e.c.media_id,
            size: e.c.size,
            modified_ms: e.c.modified_ms,
            partial_len: e.partial.map(|_| pb),
            partial: e.partial.map(|h| h.to_vec()),
            sha256: e.full.map(|h| h.to_vec()),
        })
        .collect();
    if !fresh.is_empty() {
        ws.write_db.lock().unwrap().store_hashes(&fresh)?;
    }

    let mut groups: HashMap<(i64, u64, Hash), Vec<usize>> = HashMap::new();
    for (i, e) in entries.iter().enumerate() {
        if let (Some(full), false) = (e.full, e.failed) {
            let (s, size) = scope(e);
            groups.entry((s, size, full)).or_default().push(i);
        }
    }
    let mut groups: Vec<DupGroup> = groups
        .into_iter()
        .filter(|(_, g)| g.len() > 1)
        .map(|((_, size, full), g)| DupGroup { sha256: hex(&full), size, items: g.into_iter().map(|i| item(&entries[i].c, entries[i].body.clone())).collect() })
        .collect();
    // Most space to win first.
    groups.sort_by(|a, b| (b.size * (b.items.len() as u64 - 1)).cmp(&(a.size * (a.items.len() as u64 - 1))).then(a.sha256.cmp(&b.sha256)));
    Ok(DupScan { groups, checked, read_bytes: read_bytes.into_inner(), failed, elapsed_ms: started.elapsed().as_millis() as u64 })
}

fn item(c: &DupCandidate, body: Option<String>) -> DupItem {
    DupItem {
        media_id: c.media_id,
        source_id: c.source_id,
        rel_path: c.rel_path.clone(),
        size: c.size,
        created_ms: c.created_ms,
        modified_ms: c.modified_ms,
        added_ms: c.added_ms,
        body,
        action: String::new(),
    }
}

// ---------------------------------------------------------------- acting on duplicates

/// One group's choices: items to delete, and shard items to let share another copy's data.
#[derive(Debug, Deserialize)]
pub struct ApplyGroup {
    pub items: Vec<i64>,
    #[serde(default)]
    pub delete: Vec<i64>,
    #[serde(default)]
    pub link: Vec<i64>,
}

#[derive(Debug, Default, Serialize)]
pub struct ApplyReport {
    pub deleted: usize,
    pub linked: usize,
    /// Bytes no longer stored (deleted files and shard copies now shared).
    pub freed: u64,
    pub failed: Vec<String>,
}

#[tauri::command]
pub async fn dup_apply(app: AppHandle, groups: Vec<ApplyGroup>) -> CmdResult<ApplyReport> {
    let ws = workspace(&app)?;
    let registry = app.state::<AppState>().drivers.clone();
    blocking(move || {
        let total: usize = groups.iter().map(|g| g.delete.len() + g.link.len()).sum();
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("dedup_apply", "", Unit::Items, Some(total as u64), Some(cancel.clone()));
        let report = apply(&ws, &registry, &groups, &task, &cancel);
        match &report {
            Ok(r) if !r.failed.is_empty() => task.fail(format!("{} files failed", r.failed.len())),
            Ok(_) => task.finish(),
            Err(e) if e.code == "cancelled" => task.cancelled(),
            Err(e) => task.fail(e.message.clone()),
        }
        report
    })
    .await
}

fn apply(ws: &Workspace, registry: &DriverRegistry, groups: &[ApplyGroup], task: &Task, cancel: &AtomicBool) -> CmdResult<ApplyReport> {
    let mut report = ApplyReport::default();
    let stop = || cancel.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed);
    let mut touched: HashSet<i64> = HashSet::new();
    let mut linked: Vec<(i64, String)> = Vec::new();
    // Shares first: a copy chosen to keep is never one being deleted.
    for g in groups {
        if g.link.iter().any(|id| g.delete.contains(id)) {
            report.failed.push(format!("#{}: both shared and deleted", g.link[0]));
            continue;
        }
        let db_items = ws.read_db.lock().unwrap().dup_items(&g.items)?;
        let loc: HashMap<i64, &DupCandidate> = db_items.iter().map(|c| (c.media_id, c)).collect();
        let shard_sources: HashSet<i64> = g.link.iter().filter_map(|id| loc.get(id)).map(|c| c.source_id).collect();
        for sid in shard_sources {
            if stop() {
                return Err(CmdError::new("cancelled", "cancelled"));
            }
            let Some(shard) = ws.shard_for(sid)? else {
                report.failed.push(format!("#{sid}: not a shard"));
                continue;
            };
            let in_source = |id: &i64| loc.get(id).is_some_and(|c| c.source_id == sid);
            let mut targets: Vec<i64> = g.link.iter().copied().filter(in_source).collect();
            // The copy the others share: one being kept as it is, else the first of them.
            let base = match g.items.iter().copied().find(|id| in_source(id) && !g.delete.contains(id) && !g.link.contains(id)) {
                Some(b) => b,
                None => {
                    let b = targets.remove(0);
                    task.add(1);
                    b
                }
            };
            for t in targets {
                let (from, to) = (&loc[&base].rel_path, &loc[&t].rel_path);
                task.detail(crate::workspace::file_name(to));
                match shard.link(from, to) {
                    Ok(n) => {
                        report.linked += 1;
                        report.freed += n;
                        touched.insert(sid);
                        linked.push((sid, to.clone()));
                    }
                    Err(e) => report.failed.push(format!("{to}: {e}")),
                }
                task.add(1);
            }
        }
    }
    let mut deleted: Vec<(i64, String)> = Vec::new();
    for g in groups {
        for &id in &g.delete {
            if stop() {
                break;
            }
            let Some(c) = ws.read_db.lock().unwrap().dup_items(&[id])?.pop() else {
                task.add(1);
                continue;
            };
            match crate::commands::delete_one(ws, registry, id, task) {
                Ok(()) => {
                    report.deleted += 1;
                    report.freed += c.size;
                    touched.insert(c.source_id);
                    deleted.push((c.source_id, c.rel_path));
                }
                Err(e) => report.failed.push(e.message),
            }
            task.add(1);
        }
    }
    {
        let db = ws.write_db.lock().unwrap();
        db.mark_dup_entries(&linked, "linked")?;
        db.mark_dup_entries(&deleted, "deleted")?;
    }
    if ws.settings.read().unwrap().dedup.compact_after {
        for sid in touched {
            let Ok(Some(shard)) = ws.shard_for(sid) else { continue };
            if !shard.is_writable() || stop() {
                continue;
            }
            let name = ws.read_db.lock().unwrap().source(sid)?.map(|s| s.name).unwrap_or_default();
            let t = tasks::begin("shard_compact", name, Unit::Bytes, None);
            match shard.compact(true, 0.0, cancel, &mut |done, total| t.set(done, Some(total))) {
                Ok(_) => t.finish(),
                Err(e) => t.fail(e.to_string()),
            }
        }
    }
    if stop() {
        return Err(CmdError::new("cancelled", "cancelled"));
    }
    Ok(report)
}

// ---------------------------------------------------------------- saved reports

#[tauri::command]
pub async fn dup_save_report(app: AppHandle, label: String, scope: serde_json::Value, entries: Vec<DupEntry>) -> CmdResult<i64> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.write_db.lock().unwrap().save_dup_report(label.trim(), &scope, &entries, now_ms())?)).await
}

#[tauri::command]
pub async fn dup_reports(app: AppHandle) -> CmdResult<Vec<DupReportRow>> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().dup_reports()?)).await
}

#[derive(Debug, Serialize)]
pub struct SavedReport {
    pub report: DupReportRow,
    pub groups: Vec<DupGroup>,
}

/// A saved report with its items as they are now: ones gone from the library or changed
/// since are marked `gone` (unless the report says what was done to them).
#[tauri::command]
pub async fn dup_report(app: AppHandle, id: i64) -> CmdResult<Option<SavedReport>> {
    let ws = workspace(&app)?;
    blocking(move || {
        let Some((report, entries)) = ws.read_db.lock().unwrap().dup_report(id)? else { return Ok(None) };
        let ids: Vec<i64> = entries.iter().filter_map(|e| e.media_id).collect();
        let now: HashMap<i64, DupCandidate> =
            ws.read_db.lock().unwrap().dup_items(&ids)?.into_iter().map(|c| (c.media_id, c)).collect();
        let mut shards: HashMap<i64, HashMap<String, String>> = HashMap::new();
        let mut groups: Vec<DupGroup> = Vec::new();
        let mut last = None;
        for e in entries {
            if last != Some(e.grp) {
                last = Some(e.grp);
                groups.push(DupGroup { sha256: e.sha256.clone(), size: e.size, items: Vec::new() });
            }
            let cur = e.media_id.and_then(|id| now.get(&id)).filter(|c| c.size == e.size && c.rel_path == e.rel_path);
            let it = match cur {
                Some(c) => {
                    let bodies = shards.entry(c.source_id).or_insert_with(|| match ws.shard_for(c.source_id) {
                        Ok(Some(s)) => s.contents().into_iter().map(|f| (f.path, f.body.to_string())).collect(),
                        _ => HashMap::new(),
                    });
                    let mut it = item(c, bodies.get(&c.rel_path).cloned());
                    it.action = e.action;
                    it
                }
                None => DupItem {
                    media_id: e.media_id.unwrap_or(0),
                    source_id: e.source_id,
                    rel_path: e.rel_path,
                    size: e.size,
                    created_ms: None,
                    modified_ms: None,
                    added_ms: 0,
                    body: None,
                    action: if e.action.is_empty() { "gone".into() } else { e.action },
                },
            };
            groups.last_mut().unwrap().items.push(it);
        }
        Ok(Some(SavedReport { report, groups }))
    })
    .await
}

#[tauri::command]
pub async fn dup_rename_report(app: AppHandle, id: i64, label: String) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.write_db.lock().unwrap().rename_dup_report(id, label.trim())?)).await
}

#[tauri::command]
pub async fn dup_delete_report(app: AppHandle, id: i64) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.write_db.lock().unwrap().delete_dup_report(id)?)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_db::NewSource;
    use vp_shard::{CreateOptions, Shard};

    fn rescan(ws: &Workspace, registry: &DriverRegistry, id: i64) {
        let s = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
        let drv = ws.driver(registry, &s).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: id, include_nonmedia: true, now_ms: now_ms(), cancel: &cancel, added_times: None }
            .run(&mut |_| {})
            .unwrap();
    }

    #[test]
    fn finds_duplicates_and_shares_or_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        ws.settings.write().unwrap().dedup.partial_bytes = 4096;
        let registry = DriverRegistry::with_builtin();
        let big = |seed: u8| (0..20_000u32).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)).collect::<Vec<u8>>();
        // Same size and same ends, different middle: only a whole read tells them apart.
        let mut twin = big(1);
        twin[10_000] ^= 0xff;
        let folder = dir.path().join("pics");
        std::fs::create_dir_all(folder.join("sub")).unwrap();
        std::fs::write(folder.join("a.jpg"), big(1)).unwrap();
        std::fs::write(folder.join("sub/a copy.jpg"), big(1)).unwrap();
        std::fs::write(folder.join("twin.jpg"), &twin).unwrap();
        std::fs::write(folder.join("small1.png"), b"tiny").unwrap();
        std::fs::write(folder.join("small2.png"), b"tiny").unwrap();
        std::fs::write(folder.join("other.png"), b"tinx").unwrap();
        let add = |name: &str, driver: &str, root: &str, config: serde_json::Value| {
            ws.write_db.lock().unwrap().add_source(&NewSource { name: name.into(), driver: driver.into(), config, root: root.into() }).unwrap()
        };
        let local = add("pics", "local", &folder.display().to_string(), serde_json::json!({}));
        Shard::create(&root.join("shards/box.vpshard"), &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();
        let boxed = add("box", crate::shards::DRIVER, "shards/box.vpshard", serde_json::json!({ "in_bundle": true }));
        let shard = ws.shard_for(boxed).unwrap().unwrap();
        for p in ["x/1.jpg", "x/2.jpg", "y/3.jpg"] {
            shard.put(p, &big(1), None, None).unwrap();
        }
        shard.put("tiny.png", b"tiny", None, None).unwrap();
        rescan(&ws, &registry, local);
        rescan(&ws, &registry, boxed);
        let cancel = AtomicBool::new(false);
        let run = |sources: &[i64], across: bool| {
            let task = tasks::begin("dedup_scan", "", Unit::Bytes, Some(0));
            scan(&ws, &registry, sources, across, &task, &cancel).unwrap()
        };
        let paths = |g: &DupGroup| {
            let mut v: Vec<String> = g.items.iter().map(|i| i.rel_path.clone()).collect();
            v.sort();
            v
        };

        // Within each source: the big file twice locally, three times in the shard; the tiny ones.
        let r = run(&[local, boxed], false);
        let mut found: Vec<Vec<String>> = r.groups.iter().map(paths).collect();
        found.sort();
        assert_eq!(found, [vec!["a.jpg", "sub/a copy.jpg"], vec!["small1.png", "small2.png"], vec!["x/1.jpg", "x/2.jpg", "y/3.jpg"]]);
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert!(r.groups[0].items.iter().all(|i| i.source_id == boxed && i.body.is_some()), "largest waste first");
        // Cached now: a second scan reads nothing.
        assert_eq!(run(&[local, boxed], false).read_bytes, 0);

        // Across sources the shard's copies join the folder's.
        let r = run(&[local, boxed], true);
        let big_group = r.groups.iter().find(|g| g.items.len() == 5).expect("five copies of the big file");
        assert!(r.groups.iter().any(|g| paths(g) == ["small1.png", "small2.png", "tiny.png"]));

        // Share two shard copies with the third, delete one local copy.
        let id_of = |p: &str| big_group.items.iter().find(|i| i.rel_path == p).unwrap().media_id;
        let before = shard.stats();
        let task = tasks::begin("dedup_apply", "", Unit::Items, None);
        let rep = apply(
            &ws,
            &registry,
            &[ApplyGroup {
                items: big_group.items.iter().map(|i| i.media_id).collect(),
                delete: vec![id_of("sub/a copy.jpg")],
                link: vec![id_of("x/2.jpg"), id_of("y/3.jpg")],
            }],
            &task,
            &cancel,
        )
        .unwrap();
        assert_eq!((rep.deleted, rep.linked), (1, 2), "{:?}", rep.failed);
        assert!(!folder.join("sub/a copy.jpg").exists());
        let after = shard.stats();
        assert_eq!(after.linked, 2);
        assert!(after.disk_bytes + 2 * 20_000 <= before.disk_bytes, "compacted: {before:?} -> {after:?}");
        assert_eq!(shard.open_file("y/3.jpg").unwrap().read_all().unwrap(), big(1));

        // Shared copies show the same stored data next time; a saved report remembers what was done.
        let r = run(&[boxed], false);
        let bodies: HashSet<_> = r.groups[0].items.iter().map(|i| i.body.clone()).collect();
        assert_eq!(bodies.len(), 1);
        drop(shard);
        ws.close_shard(boxed).unwrap();
    }
}
