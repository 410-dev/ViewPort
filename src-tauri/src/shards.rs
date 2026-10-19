//! Shard sources (`.vpshard`, see `vp-shard`): opening them with their keys, creating and
//! attaching them, and the jobs that move files in and out of them.
//!
//! A shard source's `root` is the shard folder, relative to the bundle when the shard lives
//! in the bundle's `shards/` folder (`config.in_bundle`). An encrypted shard needs its
//! master key: entered once per session (`unlock_shard`), or remembered in the source
//! config, which only an encrypted (locked) bundle may hold. Thumbnails and converted
//! copies of a shard's items are kept inside the shard, so an encrypted shard stays
//! encrypted even in a plain bundle (only names, tags and statistics are in the bundle).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use vp_db::{NewSource, SourceRow, ThumbRow, ThumbStatus};
use vp_driver::MediaReader;
use vp_shard::{CreateOptions, Shard, ShardInfo, Stream};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::tasks::{self, Task, Unit};
use crate::workspace::{BUNDLE_DRIVER, Workspace, now_ms};

pub const DRIVER: &str = vp_driver::shard::KIND;
/// `media_variant.rel_path` of a converted copy kept in the item's shard.
pub const VARIANT_IN_SHARD: &str = "shard:converted";
const PIECE: usize = 1024 * 1024;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ShardConfig {
    #[serde(default)]
    in_bundle: bool,
    /// Master key (hex), only ever stored in an encrypted bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
}

fn config_of(source: &SourceRow) -> ShardConfig {
    serde_json::from_value(source.config.clone()).unwrap_or_default()
}

fn locked_error(source_id: i64, info: &ShardInfo) -> CmdError {
    let requires_keyfile = info.encryption.as_ref().is_some_and(|e| e.kdf.requires_keyfile);
    CmdError::new("shard_locked", "the shard is encrypted and needs its password")
        .with_detail(serde_json::json!({ "source_id": source_id, "requires_keyfile": requires_keyfile }))
}

fn read_keyfile(keyfile: Option<String>) -> CmdResult<Option<Vec<u8>>> {
    Ok(keyfile.filter(|k| !k.is_empty()).map(std::fs::read).transpose()?)
}

fn ms(t: std::io::Result<SystemTime>) -> Option<i64> {
    t.ok()?.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

fn cancelled() -> CmdError {
    CmdError::new("cancelled", "cancelled")
}

impl Workspace {
    pub fn shard_root(&self, source: &SourceRow) -> CmdResult<PathBuf> {
        if config_of(source).in_bundle {
            let mut p = self.bundle.root.clone();
            p.extend(vp_driver::checked_components(&source.root)?);
            Ok(p)
        } else {
            Ok(PathBuf::from(&source.root))
        }
    }

    fn shard_open_options(&self) -> vp_shard::OpenOptions {
        let s = self.settings.read().unwrap().shard.clone();
        vp_shard::OpenOptions {
            writable: self.autosave(),
            snapshot_min_records: s.snapshot_min_records,
            snapshot_ratio: s.snapshot_ratio as f64,
        }
    }

    /// The open shard of a shard source, opened on first use. Fails with `shard_locked`
    /// when it is encrypted and its key was not given.
    pub fn shard(&self, source: &SourceRow) -> CmdResult<Arc<Shard>> {
        // Held while opening: a second open would find the write lock taken.
        let mut open = self.shards.lock().unwrap();
        if let Some(s) = open.get(&source.id) {
            return Ok(s.clone());
        }
        let root = self.shard_root(source)?;
        let info = Shard::read_info(&root)?;
        let key = match &info.encryption {
            None => None,
            Some(_) => {
                let session = self.shard_keys.lock().unwrap().get(&source.id).cloned();
                let remembered = config_of(source).key.and_then(|h| vp_crypto::BundleKey::from_hex(&h).ok());
                Some(session.or(remembered).ok_or_else(|| locked_error(source.id, &info))?)
            }
        };
        let shard = Arc::new(Shard::open(&root, key, self.shard_open_options())?);
        open.insert(source.id, shard.clone());
        Ok(shard)
    }

    /// The shard of a source, or `None` if it is not a shard source.
    pub fn shard_for(&self, source_id: i64) -> CmdResult<Option<Arc<Shard>>> {
        if let Some(s) = self.shards.lock().unwrap().get(&source_id) {
            return Ok(Some(s.clone()));
        }
        let Some(source) = self.read_db.lock().unwrap().source(source_id)? else { return Ok(None) };
        if source.driver != DRIVER {
            return Ok(None);
        }
        Ok(Some(self.shard(&source)?))
    }

    /// Shard sources that cannot be read now (locked, or their folder is missing).
    pub fn locked_sources(&self) -> Vec<i64> {
        let sources = self.read_db.lock().unwrap().sources().unwrap_or_default();
        sources.iter().filter(|s| s.driver == DRIVER && self.shard(s).is_err()).map(|s| s.id).collect()
    }

    /// Closes the shard of a source and waits until nothing else holds it, so its folder
    /// can be moved or deleted.
    pub fn close_shard(&self, source_id: i64) -> CmdResult<()> {
        let Some(shard) = self.forget_driver(source_id) else { return Ok(()) };
        for _ in 0..200 {
            if Arc::strong_count(&shard) == 1 {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(CmdError::new("busy", "the shard is still in use"))
    }

    /// Remembers (or forgets) a shard's master key in the source config. Only an encrypted
    /// bundle keeps it; in a plain bundle it is always forgotten.
    fn remember_key(&self, source_id: i64, key: Option<&vp_crypto::BundleKey>) -> CmdResult<()> {
        let source = self.read_db.lock().unwrap().source(source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let mut cfg = config_of(&source);
        cfg.key = key.filter(|_| self.key.is_some()).map(|k| k.to_hex().to_string());
        self.write_db.lock().unwrap().update_source(source_id, &source.name, &source.root, &serde_json::to_value(cfg)?, source.enabled)?;
        Ok(())
    }

    /// `(root, in_bundle)` to store for a shard folder: relative inside the bundle.
    pub(crate) fn shard_location(&self, folder: &Path) -> (String, bool) {
        let (driver, root) = self.source_root_for(folder);
        (root, driver == BUNDLE_DRIVER)
    }
}

fn workspace(app: &AppHandle) -> CmdResult<Arc<Workspace>> {
    app.state::<AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new("internal", e))?
}

fn source(ws: &Workspace, id: i64) -> CmdResult<SourceRow> {
    let s = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    if s.driver != DRIVER {
        return Err(CmdError::new("invalid", "not a shard source"));
    }
    Ok(s)
}

fn writable(ws: &Workspace, s: &SourceRow) -> CmdResult<Arc<Shard>> {
    let shard = ws.shard(s)?;
    if !shard.is_writable() {
        let why = if shard.is_busy() { "another program is writing to the shard" } else { "the shard is open read-only" };
        return Err(CmdError::new("read_only", why));
    }
    Ok(shard)
}

fn end_task<T>(task: &Task, result: &CmdResult<T>) {
    match result {
        Ok(_) => task.finish(),
        Err(e) if e.code == "cancelled" => task.cancelled(),
        Err(e) => task.fail(e.message.clone()),
    }
}

fn shard_result<T>(r: vp_shard::Result<T>) -> CmdResult<T> {
    r.map_err(|e| if matches!(e, vp_shard::ShardError::Cancelled) { cancelled() } else { e.into() })
}

// ---------------------------------------------------------------- files

/// A file or folder name usable on every platform (FAT32 and Windows are the strictest).
pub(crate) fn safe_name(name: &str, fallback: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let s = s.trim().trim_end_matches('.').trim();
    if s.is_empty() { fallback.into() } else { s.into() }
}

fn unique_dir(parent: &Path, stem: &str) -> PathBuf {
    let ext = vp_shard::EXTENSION;
    let mut p = parent.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = parent.join(format!("{stem} ({n}).{ext}"));
        n += 1;
    }
    p
}

fn folder_stem(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    name.strip_suffix(&format!(".{}", vp_shard::EXTENSION)).unwrap_or(&name).to_string()
}

/// Every file below `root`: absolute path, `/`-separated relative path, size.
fn walk_files(root: &Path) -> std::io::Result<Vec<(PathBuf, String, u64)>> {
    let mut out = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, rel)) = dirs.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            let ty = e.file_type()?;
            if ty.is_dir() {
                dirs.push((e.path(), child_rel));
            } else if ty.is_file() {
                out.push((e.path(), child_rel, e.metadata()?.len()));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

/// Removes `dir` and the folders below it that are empty.
fn remove_empty_dirs(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_dirs(&e.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Copies a shard folder (without its lock file), reporting bytes.
fn copy_dir(src: &Path, dst: &Path, task: &Task, cancel: &AtomicBool) -> CmdResult<()> {
    let files = walk_files(src)?;
    task.set(0, Some(files.iter().map(|f| f.2).sum()));
    std::fs::create_dir_all(dst)?;
    for s in Stream::ALL {
        std::fs::create_dir_all(dst.join(s.dir()))?;
    }
    let mut buf = vec![0u8; PIECE];
    for (abs, rel, _) in files {
        if rel == vp_shard::LOCK_FILE {
            continue;
        }
        let target = rel.split('/').fold(dst.to_path_buf(), |p, c| p.join(c));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut from = std::fs::File::open(&abs)?;
        let mut to = std::fs::File::create(&target)?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let n = std::io::Read::read(&mut from, &mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut to, &buf[..n])?;
            task.add(n as u64);
        }
        to.sync_all()?;
    }
    Ok(())
}

/// Streams `reader` into `w`, reporting bytes, and completes the item.
fn fill(mut w: vp_shard::ItemWriter<'_>, reader: &dyn MediaReader, task: &Task, cancel: &AtomicBool) -> CmdResult<()> {
    let len = reader.len();
    let mut buf = vec![0u8; PIECE.min(len.max(1) as usize)];
    let mut off = 0u64;
    while off < len {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let n = reader.read_full_at(off, &mut buf)?;
        if n == 0 {
            break; // the file got shorter
        }
        w.write_all(&buf[..n])?;
        off += n as u64;
        task.add(n as u64);
    }
    Ok(w.finish()?)
}

fn put_file(
    shard: &Shard,
    path: &str,
    reader: &dyn MediaReader,
    created: Option<i64>,
    modified: Option<i64>,
    task: &Task,
    cancel: &AtomicBool,
) -> CmdResult<()> {
    fill(shard.writer(path, Some(reader.len()), created, modified)?, reader, task, cancel)
}

fn thumb_attrs(t: &vp_thumb::Thumb) -> serde_json::Map<String, serde_json::Value> {
    let mut a = serde_json::Map::new();
    a.insert("mime".into(), t.mime.into());
    a.insert("width".into(), t.width.into());
    a.insert("height".into(), t.height.into());
    a
}

fn join_rel(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.into(),
        (_, true) => a.into(),
        _ => format!("{a}/{b}"),
    }
}

// ---------------------------------------------------------------- status

#[derive(Serialize)]
pub struct ShardStatus {
    pub source_id: i64,
    /// The shard folder.
    pub path: String,
    pub in_bundle: bool,
    /// The folder holds a shard.
    pub found: bool,
    pub encrypted: bool,
    pub requires_keyfile: bool,
    pub unlocked: bool,
    /// The key is remembered by the (locked) bundle.
    pub remembered: bool,
    pub writable: bool,
    /// Another program is writing to it, so it is open read-only here.
    pub busy: bool,
    pub chunk_bytes: u64,
    pub stats: Option<vp_shard::ShardStats>,
}

#[tauri::command]
pub async fn shard_status(app: AppHandle) -> CmdResult<Vec<ShardStatus>> {
    let ws = workspace(&app)?;
    blocking(move || {
        let sources = ws.read_db.lock().unwrap().sources()?;
        let mut out = Vec::new();
        for s in sources.into_iter().filter(|s| s.driver == DRIVER) {
            let cfg = config_of(&s);
            let path = ws.shard_root(&s)?;
            let info = Shard::read_info(&path).ok();
            let open = info.as_ref().and_then(|_| ws.shard(&s).ok());
            out.push(ShardStatus {
                source_id: s.id,
                path: path.display().to_string(),
                in_bundle: cfg.in_bundle,
                found: info.is_some(),
                encrypted: info.as_ref().is_some_and(|i| i.encryption.is_some()),
                requires_keyfile: info.as_ref().and_then(|i| i.encryption.as_ref()).is_some_and(|e| e.kdf.requires_keyfile),
                unlocked: open.is_some(),
                remembered: cfg.key.is_some(),
                writable: open.as_ref().is_some_and(|o| o.is_writable()),
                busy: open.as_ref().is_some_and(|o| o.is_busy()),
                chunk_bytes: info.as_ref().map(|i| i.chunk_bytes).unwrap_or(0),
                stats: open.map(|o| o.stats()),
            });
        }
        Ok(out)
    })
    .await
}

#[derive(Serialize)]
pub struct ShardPeek {
    pub name: String,
    pub encrypted: bool,
    pub requires_keyfile: bool,
    pub shard_version: u32,
}

/// What `info.json` of a shard folder says, before attaching it.
#[tauri::command]
pub async fn inspect_shard(path: String) -> CmdResult<ShardPeek> {
    blocking(move || {
        let p = PathBuf::from(&path);
        let info = Shard::read_info(&p)?;
        Ok(ShardPeek {
            name: if info.name.is_empty() { folder_stem(&p) } else { info.name.clone() },
            encrypted: info.encryption.is_some(),
            requires_keyfile: info.encryption.as_ref().is_some_and(|e| e.kdf.requires_keyfile),
            shard_version: info.shard_version,
        })
    })
    .await
}

// ---------------------------------------------------------------- create, attach, keys

/// Creates an empty shard and adds it as a source. Without `location` it goes into the
/// bundle's `shards/` folder; otherwise into that folder. A password encrypts it.
#[tauri::command]
pub async fn create_shard(
    app: AppHandle,
    name: String,
    location: Option<String>,
    password: Option<String>,
    keyfile: Option<String>,
    remember: bool,
) -> CmdResult<SourceRow> {
    let ws = workspace(&app)?;
    blocking(move || {
        if !ws.autosave() {
            return Err(CmdError::new("read_only", "the bundle is open without autosave"));
        }
        let name = safe_name(&name, "shard");
        let parent = match location.filter(|l| !l.trim().is_empty()) {
            Some(l) => PathBuf::from(l),
            None => ws.bundle.path(vp_bundle::SHARDS_DIR),
        };
        std::fs::create_dir_all(&parent)?;
        let folder = unique_dir(&parent, &name);
        let s = ws.settings.read().unwrap().shard.clone();
        let opts = CreateOptions {
            name: name.clone(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            created_ms: now_ms(),
            chunk_bytes: s.chunk_bytes.clamp(vp_shard::MIN_CHUNK_BYTES, vp_shard::MAX_CHUNK_BYTES),
            segment_bytes: s.segment_bytes.clamp(vp_shard::MIN_SEGMENT_BYTES, vp_shard::MAX_SEGMENT_BYTES),
            password: password.filter(|p| !p.is_empty()),
            keyfile: read_keyfile(keyfile)?,
            ..Default::default()
        };
        let key = Shard::create(&folder, &opts)?;
        let (root, in_bundle) = ws.shard_location(&folder);
        let cfg = ShardConfig { in_bundle, key: key.as_ref().filter(|_| remember && ws.key.is_some()).map(|k| k.to_hex().to_string()) };
        let id = ws.write_db.lock().unwrap().add_source(&NewSource { name, driver: DRIVER.into(), config: serde_json::to_value(cfg)?, root })?;
        if let Some(k) = key {
            ws.shard_keys.lock().unwrap().insert(id, k);
        }
        ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))
    })
    .await
}

/// Adds an existing shard folder as a source: used where it is (`link`), or copied or
/// moved into the bundle first. A password, if given, unlocks it for this session.
#[tauri::command]
pub async fn attach_shard(
    app: AppHandle,
    path: String,
    mode: String,
    password: Option<String>,
    keyfile: Option<String>,
    remember: bool,
) -> CmdResult<SourceRow> {
    let ws = workspace(&app)?;
    let ws2 = ws.clone();
    let source = blocking(move || {
        let ws = ws2;
        let src = PathBuf::from(&path);
        let info = Shard::read_info(&src)?;
        for s in ws.read_db.lock().unwrap().sources()?.iter().filter(|s| s.driver == DRIVER) {
            if ws.shard_root(s).ok().and_then(|r| Shard::read_info(&r).ok()).is_some_and(|i| i.id == info.id) {
                return Err(CmdError::new("already_attached", format!("this shard is already the source '{}'", s.name)));
            }
        }
        let key = match (&info.encryption, password.filter(|p| !p.is_empty())) {
            (Some(_), Some(pw)) => Some(Shard::unlock(&info, &pw, read_keyfile(keyfile)?.as_deref())?),
            _ => None,
        };
        let (mut root, mut in_bundle) = ws.shard_location(&src);
        if !in_bundle && (mode == "copy" || mode == "move") {
            let dest = unique_dir(&ws.bundle.path(vp_bundle::SHARDS_DIR), &folder_stem(&src));
            let cancel = Arc::new(AtomicBool::new(false));
            let task = tasks::begin_with_cancel("shard_copy", folder_stem(&src), Unit::Bytes, None, Some(cancel.clone()));
            let copied = copy_dir(&src, &dest, &task, &cancel);
            end_task(&task, &copied);
            if let Err(e) = copied {
                let _ = std::fs::remove_dir_all(&dest);
                return Err(e);
            }
            if mode == "move" {
                std::fs::remove_dir_all(&src)?;
            }
            (root, in_bundle) = ws.shard_location(&dest);
        }
        let name = if info.name.is_empty() { folder_stem(&src) } else { info.name.clone() };
        let cfg = ShardConfig { in_bundle, key: key.as_ref().filter(|_| remember && ws.key.is_some()).map(|k| k.to_hex().to_string()) };
        let id = ws.write_db.lock().unwrap().add_source(&NewSource { name, driver: DRIVER.into(), config: serde_json::to_value(cfg)?, root })?;
        if let Some(k) = key {
            ws.shard_keys.lock().unwrap().insert(id, k);
        }
        ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))
    })
    .await?;
    if ws.shard(&source).is_ok() {
        crate::commands::start_scan(&app, ws, source.clone());
    }
    Ok(source)
}

/// Unlocks an encrypted shard for this session; `remember` keeps its key in a locked bundle.
#[tauri::command]
pub async fn unlock_shard(app: AppHandle, source_id: i64, password: String, keyfile: Option<String>, remember: bool) -> CmdResult<()> {
    let ws = workspace(&app)?;
    let ws2 = ws.clone();
    blocking(move || {
        let s = source(&ws2, source_id)?;
        let info = Shard::read_info(&ws2.shard_root(&s)?)?;
        let key = Shard::unlock(&info, &password, read_keyfile(keyfile)?.as_deref())?;
        ws2.close_shard(source_id)?;
        ws2.remember_key(source_id, remember.then_some(&key))?;
        ws2.shard_keys.lock().unwrap().insert(source_id, key);
        Ok(())
    })
    .await?;
    if ws.autosave() {
        crate::thumbs::spawn_backfill(app.clone(), ws.clone());
        crate::video::spawn_probe_backfill(&app, ws);
    }
    Ok(())
}

/// Locks an unlocked shard again: forgets its key (also a remembered one) and drops what
/// was read from it out of memory.
#[tauri::command]
pub async fn lock_shard(app: AppHandle, source_id: i64) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || {
        source(&ws, source_id)?;
        ws.shard_keys.lock().unwrap().remove(&source_id);
        ws.remember_key(source_id, None)?;
        ws.close_shard(source_id)?;
        ws.cache.clear();
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn shard_change_password(app: AppHandle, source_id: i64, password: String, keyfile: Option<String>) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || {
        let shard = writable(&ws, &source(&ws, source_id)?)?;
        Ok(shard.change_password(&password, read_keyfile(keyfile)?.as_deref(), vp_crypto::DEFAULT_MEMORY_KIB, vp_crypto::DEFAULT_ITERATIONS)?)
    })
    .await
}

// ---------------------------------------------------------------- moving files in and out

/// Copies or moves files and folders from the file system into a shard folder (`dest`),
/// in the background. Moved originals are deleted once read back intact.
#[tauri::command]
pub async fn shard_import(app: AppHandle, source_id: i64, paths: Vec<String>, dest: String, mode: String) -> CmdResult<()> {
    let ws = workspace(&app)?;
    let s = source(&ws, source_id)?;
    let ws2 = ws.clone();
    let s2 = s.clone();
    blocking(move || writable(&ws2, &s2).map(|_| ())).await?;
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_import", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let result = import_files(&ws, &s, &paths, &dest, mode == "move", &task, &cancel);
        report(&task, result);
        let _ = crate::commands::scan_now(&app, ws.clone(), s.clone());
        maybe_compact(&ws, &s);
    });
    Ok(())
}

/// Ends a job's task: failures of single files are counted, not fatal.
fn report(task: &Task, result: CmdResult<usize>) {
    match result {
        Ok(0) => task.finish(),
        Ok(n) => task.fail(format!("{n} files failed")),
        Err(e) => end_task::<()>(task, &Err(e)),
    }
}

fn import_files(ws: &Workspace, s: &SourceRow, paths: &[PathBuf], dest: &str, mv: bool, task: &Task, cancel: &AtomicBool) -> CmdResult<usize> {
    let shard = writable(ws, s)?;
    let dest = vp_shard::normalize_path(dest).ok_or_else(|| CmdError::new("invalid", "invalid folder"))?;
    let mut list = Vec::new();
    let mut dirs = Vec::new();
    for p in paths {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if p.is_dir() {
            for (abs, rel, size) in walk_files(p)? {
                list.push((abs, join_rel(&join_rel(&dest, &name), &rel), size));
            }
            dirs.push(p.clone());
        } else {
            list.push((p.clone(), join_rel(&dest, &name), std::fs::metadata(p)?.len()));
        }
    }
    task.set(0, Some(list.iter().map(|f| f.2).sum()));
    let verify = ws.settings.read().unwrap().shard.verify_moves;
    let mut failed = 0;
    for (abs, rel, _) in list {
        if ws.closed.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        task.detail(&rel);
        let meta = std::fs::metadata(&abs)?;
        let reader = vp_driver::local::open_file(&abs)?;
        match put_file(&shard, &rel, reader.as_ref(), ms(meta.created()), ms(meta.modified()), task, cancel) {
            Err(e) if e.code == "cancelled" => return Err(e),
            Err(_) => {
                failed += 1;
                continue;
            }
            Ok(()) => {}
        }
        drop(reader);
        if mv && ((verify && !shard.verify_file(&rel)?) || std::fs::remove_file(&abs).is_err()) {
            failed += 1;
        }
    }
    if mv {
        for d in dirs {
            remove_empty_dirs(&d);
        }
    }
    Ok(failed)
}

/// Copies or moves every file of another source into a shard folder (`dest`), in the
/// background. What is recorded about the items (tags, ratings, statistics, thumbnails,
/// converted copies) follows them: moved items keep their ids, copies get the same values.
#[tauri::command]
pub async fn shard_from_source(app: AppHandle, from: i64, to: i64, dest: String, mode: String) -> CmdResult<()> {
    let ws = workspace(&app)?;
    let target = source(&ws, to)?;
    let origin = ws.read_db.lock().unwrap().source(from)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    if from == to {
        return Err(CmdError::new("invalid", "a source cannot be copied into itself"));
    }
    let ws2 = ws.clone();
    let t2 = target.clone();
    blocking(move || writable(&ws2, &t2).map(|_| ())).await?;
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_import", &target.name, Unit::Bytes, None, Some(cancel.clone()));
        let result = source_to_shard(&app, &ws, &origin, &target, &dest, mode == "move", &task, &cancel);
        report(&task, result);
        maybe_compact(&ws, &target);
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn source_to_shard(
    app: &AppHandle,
    ws: &Arc<Workspace>,
    from: &SourceRow,
    to: &SourceRow,
    dest: &str,
    mv: bool,
    task: &Task,
    cancel: &AtomicBool,
) -> CmdResult<usize> {
    let shard = writable(ws, to)?;
    let registry = app.state::<AppState>().drivers.clone();
    let driver = ws.driver(&registry, from)?;
    if mv && !driver.capabilities().write {
        return Err(CmdError::new("unsupported", "files cannot be deleted from this source"));
    }
    let dest = vp_shard::normalize_path(dest).ok_or_else(|| CmdError::new("invalid", "invalid folder"))?;
    let prefix = if dest.is_empty() { String::new() } else { format!("{dest}/") };
    let mut files = Vec::new();
    let mut dirs = vec![String::new()];
    let mut i = 0;
    while i < dirs.len() {
        for e in driver.list(&dirs[i])? {
            if e.is_dir { dirs.push(e.path) } else { files.push(e) }
        }
        i += 1;
    }
    let known = ws.read_db.lock().unwrap().known_files(from.id)?;
    task.set(0, Some(files.iter().map(|f| f.size).sum()));
    let verify = ws.settings.read().unwrap().shard.verify_moves;
    let mut failed = 0;
    let mut copied_variants = Vec::new();
    for e in files {
        if ws.closed.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let new_rel = format!("{prefix}{}", e.path);
        task.detail(&new_rel);
        let reader = driver.open(&e.path)?;
        match put_file(&shard, &new_rel, reader.as_ref(), e.created_ms, e.modified_ms, task, cancel) {
            Err(e) if e.code == "cancelled" => return Err(e),
            Err(_) => {
                failed += 1;
                continue;
            }
            Ok(()) => {}
        }
        drop(reader);
        // Thumbnail and converted copy go along, so nothing is made twice.
        let item = known.get(&e.path).map(|k| k.id);
        let mut thumb = None;
        let mut variant = None;
        if let Some(id) = item {
            if let Some(t) = ws.stored_thumb(id, &e.path, from.id)? {
                shard.put_blob(Stream::Thumbs, &new_rel, &t.data, thumb_attrs(&t))?;
                thumb = Some(ThumbRow {
                    status: ThumbStatus::Ok,
                    pack: vp_db::THUMB_IN_SOURCE,
                    offset: 0,
                    len: t.data.len() as u32,
                    width: t.width,
                    height: t.height,
                    mime: t.mime.into(),
                });
            }
            if let Some(v) = ws.variant_reader(id)? {
                let mut attrs = serde_json::Map::new();
                attrs.insert("mime".into(), "video/mp4".into());
                let len = v.len();
                fill(shard.blob_writer(Stream::Converted, &new_rel, Some(len), attrs)?, v.as_ref(), task, cancel)?;
                variant = Some(len);
            }
        }
        if !mv {
            if let Some(len) = variant {
                copied_variants.push((new_rel, len));
            }
            continue;
        }
        if (verify && !shard.verify_file(&new_rel)?) || driver.delete(&e.path).is_err() {
            failed += 1;
            continue;
        }
        if let Some(id) = item {
            let db = ws.write_db.lock().unwrap();
            let old_variant = db.variant(id)?;
            db.move_media(id, to.id, &new_rel)?;
            if let Some(row) = &thumb {
                db.set_thumbnail(id, row)?;
            }
            if let Some(len) = variant {
                db.set_variant(id, VARIANT_IN_SHARD, len, now_ms())?;
                if let Some(old) = old_variant.filter(|v| v != VARIANT_IN_SHARD) {
                    let _ = std::fs::remove_file(ws.bundle.root.join(old));
                }
            }
        }
    }
    if mv {
        // Folders left empty in a local source go too, deepest first.
        for d in dirs.iter().rev() {
            if let Some(p) = driver.local_path(d).filter(|_| !d.is_empty()) {
                let _ = std::fs::remove_dir(p);
            }
        }
    }
    let _ = crate::commands::scan_now(app, ws.clone(), to.clone());
    if mv {
        let _ = crate::commands::scan_now(app, ws.clone(), from.clone());
    } else {
        ws.write_db.lock().unwrap().copy_media_metadata(from.id, to.id, &prefix)?;
        let now_known = ws.read_db.lock().unwrap().known_files(to.id)?;
        let db = ws.write_db.lock().unwrap();
        for (rel, len) in copied_variants {
            if let Some(k) = now_known.get(&rel) {
                db.set_variant(k.id, VARIANT_IN_SHARD, len, now_ms())?;
            }
        }
    }
    Ok(failed)
}

/// Writes files of a shard (`paths`, empty = all) into a folder, in the background.
/// A chosen folder or file lands in `dest` by its own name.
#[tauri::command]
pub async fn shard_export(app: AppHandle, source_id: i64, paths: Vec<String>, dest: String) -> CmdResult<()> {
    let ws = workspace(&app)?;
    let s = source(&ws, source_id)?;
    let ws2 = ws.clone();
    let s2 = s.clone();
    let shard = blocking(move || ws2.shard(&s2)).await?;
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_export", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let result = export_files(&ws, &shard, &paths, Path::new(&dest), &task, &cancel);
        report(&task, result);
    });
    Ok(())
}

fn export_files(ws: &Workspace, shard: &Shard, paths: &[String], dest: &Path, task: &Task, cancel: &AtomicBool) -> CmdResult<usize> {
    let selected = if paths.is_empty() { vec![String::new()] } else { paths.to_vec() };
    let mut list = Vec::new();
    for p in selected {
        let p = vp_shard::normalize_path(&p).ok_or_else(|| CmdError::new("invalid", "invalid path"))?;
        let base = p.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();
        for (path, size) in shard.files_under(&p)? {
            let rel = if base.is_empty() { path.clone() } else { path[base.len() + 1..].to_string() };
            list.push((path, rel, size));
        }
    }
    task.set(0, Some(list.iter().map(|f| f.2).sum()));
    let mut failed = 0;
    let mut buf = vec![0u8; PIECE];
    for (path, rel, _) in list {
        if ws.closed.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        task.detail(&rel);
        let target = rel.split('/').fold(dest.to_path_buf(), |p, c| p.join(safe_name(c, "_")));
        let one = (|| -> CmdResult<()> {
            let r = shard.open_file(&path)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let f = std::fs::File::create(&target)?;
            let mut off = 0u64;
            while off < r.len() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(cancelled());
                }
                let n = r.read_full_at(off, &mut buf)?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut &f, &buf[..n])?;
                off += n as u64;
                task.add(n as u64);
            }
            if let Some(m) = r.item().modified_ms.and_then(|m| u64::try_from(m).ok()) {
                let _ = f.set_modified(UNIX_EPOCH + std::time::Duration::from_millis(m));
            }
            Ok(())
        })();
        match one {
            Err(e) if e.code == "cancelled" => {
                let _ = std::fs::remove_file(&target);
                return Err(e);
            }
            Err(_) => failed += 1,
            Ok(()) => {}
        }
    }
    Ok(failed)
}

// ---------------------------------------------------------------- maintenance

#[tauri::command]
pub async fn shard_verify(app: AppHandle, source_id: i64) -> CmdResult<vp_shard::VerifyReport> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let shard = ws.shard(&s)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_verify", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let result = shard_result(shard.verify(&cancel, &mut |done, total| task.set(done, Some(total))));
        end_task(&task, &result);
        result
    })
    .await
}

/// Rewrites a shard without its garbage (deleted and replaced data).
#[tauri::command]
pub async fn shard_compact(app: AppHandle, source_id: i64) -> CmdResult<vp_shard::CompactReport> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let shard = writable(&ws, &s)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_compact", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let result = shard_result(shard.compact(true, 0.0, &cancel, &mut |done, total| task.set(done, Some(total))));
        end_task(&task, &result);
        result
    })
    .await
}

/// Compacts in the background when enough of the shard is garbage (setting).
pub fn maybe_compact(ws: &Workspace, s: &SourceRow) {
    let settings = ws.settings.read().unwrap().shard.clone();
    let Ok(shard) = ws.shard(s) else { return };
    if !settings.auto_compact || !shard.is_writable() || shard.garbage_ratio() < settings.compact_garbage_ratio as f64 {
        return;
    }
    let task = tasks::begin("shard_compact", &s.name, Unit::Bytes, None);
    let ratio = settings.compact_garbage_ratio as f64;
    let result = shard_result(shard.compact(false, ratio, &ws.closed, &mut |done, total| task.set(done, Some(total))));
    end_task(&task, &result);
}

/// Adds (`password`) or removes (`None`) encryption by rewriting the shard into a new
/// one and swapping it in. Items, thumbnails and converted copies are kept.
#[tauri::command]
pub async fn shard_set_encryption(
    app: AppHandle,
    source_id: i64,
    password: Option<String>,
    keyfile: Option<String>,
    remember: bool,
) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let shard = writable(&ws, &s)?;
        let info = shard.info();
        let root = shard.root().to_path_buf();
        let folder = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let tmp = root.with_file_name(format!("{folder}.new"));
        let old = root.with_file_name(format!("{folder}.old"));
        let _ = std::fs::remove_dir_all(&tmp);
        let opts = CreateOptions {
            name: info.name.clone(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            created_ms: info.created_ms,
            chunk_bytes: info.chunk_bytes,
            segment_bytes: info.segment_bytes,
            password: password.filter(|p| !p.is_empty()),
            keyfile: read_keyfile(keyfile)?,
            ..Default::default()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_repack", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let repacked = shard_result(shard.repack(&tmp, &opts, ws.shard_open_options(), &cancel, &mut |d, t| task.set(d, Some(t))));
        end_task(&task, &repacked);
        let new = match repacked {
            Ok(n) => n,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                return Err(e);
            }
        };
        let key = new.key().cloned();
        drop(new);
        drop(shard);
        ws.close_shard(source_id)?;
        std::fs::rename(&root, &old)?;
        if let Err(e) = std::fs::rename(&tmp, &root) {
            let _ = std::fs::rename(&old, &root);
            return Err(e.into());
        }
        let _ = std::fs::remove_dir_all(&old);
        match &key {
            Some(k) => {
                ws.shard_keys.lock().unwrap().insert(source_id, k.clone());
            }
            None => {
                ws.shard_keys.lock().unwrap().remove(&source_id);
            }
        }
        ws.remember_key(source_id, key.as_ref().filter(|_| remember))?;
        Ok(())
    })
    .await
}

/// Moves or copies a shard into the bundle (`target` = `None`) or into another folder,
/// and points the source at the new place.
#[tauri::command]
pub async fn shard_relocate(app: AppHandle, source_id: i64, target: Option<String>, mode: String) -> CmdResult<SourceRow> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let old_root = ws.shard_root(&s)?;
        let parent = match target.filter(|t| !t.trim().is_empty()) {
            Some(t) => PathBuf::from(t),
            None => ws.bundle.path(vp_bundle::SHARDS_DIR),
        };
        std::fs::create_dir_all(&parent)?;
        let dest = unique_dir(&parent, &folder_stem(&old_root));
        ws.close_shard(source_id)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("shard_copy", &s.name, Unit::Bytes, None, Some(cancel.clone()));
        let copied = copy_dir(&old_root, &dest, &task, &cancel);
        end_task(&task, &copied);
        if let Err(e) = copied {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e);
        }
        if mode == "move" {
            std::fs::remove_dir_all(&old_root)?;
        }
        let (root, in_bundle) = ws.shard_location(&dest);
        let mut cfg = config_of(&s);
        cfg.in_bundle = in_bundle;
        ws.write_db.lock().unwrap().update_source(source_id, &s.name, &root, &serde_json::to_value(cfg)?, s.enabled)?;
        ws.read_db.lock().unwrap().source(source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_everywhere() {
        assert_eq!(safe_name(" a:b/c? ", "x"), "a_b_c_");
        assert_eq!(safe_name("...", "x"), "x");
        assert_eq!(join_rel("", "a"), "a");
        assert_eq!(join_rel("d", "a"), "d/a");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("s.vpshard")).unwrap();
        assert_eq!(unique_dir(dir.path(), "s").file_name().unwrap(), "s (2).vpshard");
        assert_eq!(folder_stem(Path::new("/x/My.vpshard")), "My");
    }
}
