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
    /// Item properties (values, favorites, statistics, ...) are kept in the shard itself
    /// (`shard_props.rs`), so they go wherever the shard goes; otherwise only in the library.
    #[serde(default)]
    props_in_shard: bool,
    /// Faces' identity vectors are kept in the shard too (with `props_in_shard`), so
    /// another library can go on grouping without analysing again.
    #[serde(default)]
    embeddings_in_shard: bool,
}

fn config_of(source: &SourceRow) -> ShardConfig {
    serde_json::from_value(source.config.clone()).unwrap_or_default()
}

/// Whether `source` is a shard that keeps its items' properties itself.
pub(crate) fn props_in_shard(source: &SourceRow) -> bool {
    source.driver == DRIVER && config_of(source).props_in_shard
}

/// Whether `source` is a shard that keeps its faces' identity vectors itself.
pub(crate) fn embeddings_in_shard(source: &SourceRow) -> bool {
    props_in_shard(source) && config_of(source).embeddings_in_shard
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

pub(crate) fn writable(ws: &Workspace, s: &SourceRow) -> CmdResult<Arc<Shard>> {
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

pub(crate) fn join_rel(a: &str, b: &str) -> String {
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
    /// Item properties are kept in the shard.
    pub props_in_shard: bool,
    /// Faces' identity vectors are kept in the shard too.
    pub embeddings_in_shard: bool,
    /// Faces found on its items, and the bytes their vectors take.
    pub faces: u64,
    pub embedding_bytes: u64,
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
            let (faces, embedding_bytes) = ws.read_db.lock().unwrap().source_face_stats(s.id)?;
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
                props_in_shard: cfg.props_in_shard,
                embeddings_in_shard: cfg.embeddings_in_shard,
                faces,
                embedding_bytes,
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
    /// Item properties are kept in it (another library chose so); unknown while encrypted.
    pub has_props: Option<bool>,
}

/// What `info.json` of a shard folder says, before attaching it.
#[tauri::command]
pub async fn inspect_shard(path: String) -> CmdResult<ShardPeek> {
    blocking(move || {
        let p = PathBuf::from(&path);
        let info = Shard::read_info(&p)?;
        let has_props = match info.encryption {
            Some(_) => None,
            None => Shard::open(&p, None, vp_shard::OpenOptions { writable: false, ..Default::default() })
                .ok()
                .map(|s| crate::shard_props::has_props(&s)),
        };
        Ok(ShardPeek {
            name: if info.name.is_empty() { folder_stem(&p) } else { info.name.clone() },
            encrypted: info.encryption.is_some(),
            requires_keyfile: info.encryption.as_ref().is_some_and(|e| e.kdf.requires_keyfile),
            shard_version: info.shard_version,
            has_props,
        })
    })
    .await
}

/// Keeps item properties in the shard (from now on, all of them written there first) or
/// only in the library.
#[tauri::command]
pub async fn shard_set_props(app: AppHandle, source_id: i64, in_shard: bool) -> CmdResult<SourceRow> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let mut cfg = config_of(&s);
        if cfg.props_in_shard == in_shard {
            return Ok(s);
        }
        cfg.props_in_shard = in_shard;
        ws.write_db.lock().unwrap().update_source(source_id, &s.name, &s.root, &serde_json::to_value(cfg)?, s.enabled)?;
        let s = source(&ws, source_id)?;
        if in_shard {
            // What the shard already holds (another library's) wins; everything else is
            // written out.
            crate::shard_props::import(&ws, &s)?;
            ws.write_db.lock().unwrap().touch_meta(source_id)?;
            crate::shard_props::flush(&ws, &s)?;
        }
        Ok(s)
    })
    .await
}

/// Keeps faces' identity vectors in the shard too (written out now) or not (removed
/// from it now).
#[tauri::command]
pub async fn shard_set_embeddings(app: AppHandle, source_id: i64, on: bool) -> CmdResult<SourceRow> {
    let ws = workspace(&app)?;
    blocking(move || {
        let s = source(&ws, source_id)?;
        let mut cfg = config_of(&s);
        if cfg.embeddings_in_shard == on {
            return Ok(s);
        }
        cfg.embeddings_in_shard = on;
        ws.write_db.lock().unwrap().update_source(source_id, &s.name, &s.root, &serde_json::to_value(cfg)?, s.enabled)?;
        let s = source(&ws, source_id)?;
        if props_in_shard(&s) {
            let task = tasks::begin("shard_embeddings", &s.name, Unit::Items, None);
            let result = crate::shard_props::write_embeddings(&ws, &s, on, &task).map(|_| ());
            end_task(&task, &result);
            result?;
        }
        Ok(s)
    })
    .await
}

/// The average size of a face's identity vector in this library (what keeping vectors in
/// a shard costs per face), or that of the usual models when no face has one yet.
#[tauri::command]
pub async fn face_embedding_bytes(app: AppHandle) -> CmdResult<u64> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().average_embedding_bytes()?.unwrap_or(TYPICAL_EMBEDDING_BYTES))).await
}

/// A 512-value vector in half precision.
const TYPICAL_EMBEDDING_BYTES: u64 = 1024;

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
    props_in_shard: bool,
    embeddings_in_shard: Option<bool>,
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
        let cfg = ShardConfig {
            in_bundle,
            key: key.as_ref().filter(|_| remember && ws.key.is_some()).map(|k| k.to_hex().to_string()),
            props_in_shard,
            embeddings_in_shard: embeddings_in_shard.unwrap_or(false),
        };
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
    props_in_shard: bool,
    embeddings_in_shard: Option<bool>,
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
        let cfg = ShardConfig {
            in_bundle,
            key: key.as_ref().filter(|_| remember && ws.key.is_some()).map(|k| k.to_hex().to_string()),
            props_in_shard,
            embeddings_in_shard: embeddings_in_shard.unwrap_or(false),
        };
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
    let changed = blocking(move || {
        let s = source(&ws2, source_id)?;
        let info = Shard::read_info(&ws2.shard_root(&s)?)?;
        let key = Shard::unlock(&info, &password, read_keyfile(keyfile)?.as_deref())?;
        ws2.close_shard(source_id)?;
        ws2.remember_key(source_id, remember.then_some(&key))?;
        ws2.shard_keys.lock().unwrap().insert(source_id, key);
        // Properties kept in the shard may have changed elsewhere since it was last open.
        Ok(ws2.autosave() && crate::shard_props::import(&ws2, &s)? > 0)
    })
    .await?;
    if changed {
        crate::ml::notify(&app);
    }
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
        for (path, size) in shard.files_under(&p)?.into_iter().filter(|(path, _)| !vp_shard::is_reserved(path)) {
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

/// A folder of a source picked for copying (`path` "" = the whole source).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderPick {
    pub source_id: i64,
    pub path: String,
}

/// One file to copy: an item of the library and where it lands in the target.
pub(crate) struct CopyJob {
    pub(crate) id: i64,
    pub(crate) source_id: i64,
    pub(crate) rel: String,
    pub(crate) target: String,
    pub(crate) size: u64,
    pub(crate) created_ms: Option<i64>,
    pub(crate) modified_ms: Option<i64>,
}

/// Copies items (`ids`) and folders of the library into folder `dest` of source `to`, in
/// the background: from any source into a shard, or out of a shard into a writable one.
/// A picked item or folder lands in `dest` by its own name; a whole source (`path` "")
/// lands with its contents directly in `dest`. Files already at the target are skipped.
/// Tags, ratings, statistics and loop segments follow; thumbnails and (into a shard)
/// converted copies too, so nothing is made twice.
#[tauri::command]
pub async fn copy_to_source(app: AppHandle, ids: Vec<i64>, folders: Vec<FolderPick>, to: i64, dest: String) -> CmdResult<()> {
    let ws = workspace(&app)?;
    let registry = app.state::<AppState>().drivers.clone();
    let (ws2, reg2) = (ws.clone(), registry.clone());
    let (target, jobs) = blocking(move || {
        let target = ws2.read_db.lock().unwrap().source(to)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let dest = vp_shard::normalize_path(&dest).ok_or_else(|| CmdError::new("invalid", "invalid folder"))?;
        let jobs = copy_jobs(&ws2, &ids, &folders, &dest)?;
        if jobs.iter().any(|j| j.source_id == to) {
            return Err(CmdError::new("invalid", "a source cannot be copied into itself"));
        }
        if target.driver == DRIVER {
            writable(&ws2, &target)?;
        } else if !ws2.driver(&reg2, &target)?.capabilities().write {
            return Err(CmdError::new("unsupported", "files cannot be written to this source"));
        }
        Ok((target, jobs))
    })
    .await?;
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("copy_to_source", &target.name, Unit::Bytes, None, Some(cancel.clone()));
        let rescan = || {
            let _ = crate::commands::scan_now(&app, ws.clone(), target.clone());
        };
        let result = copy_files(&ws, &registry, &target, jobs, Conflicts::Ask(app.clone()), &task, &cancel, rescan).map(|r| (r.failed, r.skipped));
        match result {
            Ok((0, 0)) => task.finish(),
            // Skipping is what the user chose: done, not failed.
            Ok((0, skipped)) => {
                task.detail(format!("{skipped} skipped"));
                task.finish();
            }
            Ok((failed, skipped)) => task.fail(format!("{failed} files failed, {skipped} skipped")),
            Err(e) => end_task::<()>(&task, &Err(e)),
        }
        if target.driver == DRIVER {
            maybe_compact(&ws, &target);
        }
    });
    Ok(())
}

/// Streams `reader` into a new file at `path` (its folders are made), keeping `modified_ms`.
fn write_local(path: &Path, reader: &dyn MediaReader, modified_ms: Option<i64>, task: &Task, cancel: &AtomicBool) -> CmdResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let f = std::fs::File::create(path)?;
    let mut buf = vec![0u8; PIECE];
    let mut off = 0u64;
    let result = (|| -> CmdResult<()> {
        while off < reader.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let n = reader.read_full_at(off, &mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut &f, &buf[..n])?;
            off += n as u64;
            task.add(n as u64);
        }
        Ok(())
    })();
    if result.is_err() {
        drop(f);
        let _ = std::fs::remove_file(path);
        return result;
    }
    if let Some(m) = modified_ms.and_then(|m| u64::try_from(m).ok()) {
        let _ = f.set_modified(UNIX_EPOCH + std::time::Duration::from_millis(m));
    }
    Ok(())
}

fn copy_jobs(ws: &Workspace, ids: &[i64], folders: &[FolderPick], dest: &str) -> CmdResult<Vec<CopyJob>> {
    let db = ws.read_db.lock().unwrap();
    let job = |b: &vp_db::MediaBrief, target: String| CopyJob {
        id: b.id,
        source_id: b.source_id,
        rel: join_rel(&b.folder, &b.name),
        target,
        size: b.size,
        created_ms: b.created_ms,
        modified_ms: b.modified_ms,
    };
    let mut jobs: Vec<CopyJob> = db.media_brief(ids)?.iter().map(|b| job(b, join_rel(dest, &b.name))).collect();
    let reg = db.field_registry()?;
    let ctx = vp_db::Db::compile_context(now_ms(), 0);
    for f in folders {
        let path = vp_shard::normalize_path(&f.path).ok_or_else(|| CmdError::new("invalid", "invalid folder"))?;
        let scope = vp_db::Scope { source_id: Some(f.source_id), folder: Some(path.clone()), recursive: true, ..Default::default() };
        let found = db.search_in(&vp_query::Query::default(), &reg, &ctx, &scope)?;
        // Paths below the picked folder's parent, so the folder keeps its own name.
        let base = path.rsplit_once('/').map(|(b, _)| b.len() + 1).unwrap_or(0);
        for b in db.media_brief(&found)? {
            let rel = join_rel(&b.folder, &b.name);
            let below = if path.is_empty() { rel.as_str() } else { &rel[base..] };
            jobs.push(job(&b, join_rel(dest, below)));
        }
    }
    Ok(jobs)
}

// ---------------------------------------------------------------- name conflicts

/// What to do when a file is already at a copy's target path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnConflict {
    /// Skip it when it is the same file (size and content); otherwise copy under a new name.
    SkipSame,
    /// Skip it whenever the name is taken.
    SkipName,
    /// Copy under a new name ("name (2).ext").
    Rename,
    /// Replace the file there.
    Overwrite,
}

/// How a copy settles conflicts: one rule, or asking the user (`copy-conflict` events,
/// answered by `copy_conflict_answer`).
pub(crate) enum Conflicts {
    #[cfg_attr(not(test), allow(dead_code))]
    Fixed(OnConflict),
    Ask(AppHandle),
}

/// A conflict put to the user.
#[derive(Clone, Serialize)]
struct ConflictAsk {
    id: u64,
    /// The copy's task.
    task: u64,
    /// Path in the target source.
    target: String,
    /// Path of the file being copied, in its own source.
    source: String,
    existing_size: u64,
    existing_modified_ms: Option<i64>,
    incoming_size: u64,
    incoming_modified_ms: Option<i64>,
    /// Same size and content.
    same: Option<bool>,
}

type Answer = Option<(OnConflict, bool)>;

/// Copies waiting for an answer, by conflict id.
static ASKING: std::sync::Mutex<Option<std::collections::HashMap<u64, std::sync::mpsc::Sender<Answer>>>> = std::sync::Mutex::new(None);
static NEXT_ASK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Answers conflict `id`: what to do (`None` cancels the copy) and whether for every
/// remaining conflict of that copy.
#[tauri::command]
pub fn copy_conflict_answer(app: AppHandle, id: u64, choice: Option<OnConflict>, all: bool) {
    use tauri::Emitter;
    let tx = ASKING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|m| m.remove(&id));
    if let Some(tx) = tx {
        let _ = tx.send(choice.map(|c| (c, all)));
    }
    // Other windows showing the same question close it.
    let _ = app.emit("copy-conflict-done", id);
}

/// Puts a conflict to the user and waits for the answer (or for the copy to be cancelled).
fn ask_conflict(app: &AppHandle, mut facts: ConflictAsk, cancel: &AtomicBool, closed: &AtomicBool) -> CmdResult<(OnConflict, bool)> {
    use tauri::Emitter;
    let id = NEXT_ASK.fetch_add(1, Ordering::Relaxed);
    facts.id = id;
    let (tx, rx) = std::sync::mpsc::channel();
    ASKING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(id, tx);
    let _ = app.emit("copy-conflict", &facts);
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(Some(answer)) => return Ok(answer),
            Ok(None) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(cancelled()),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if cancel.load(Ordering::Relaxed) || closed.load(Ordering::Relaxed) {
                    ASKING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().map(|m| m.remove(&id));
                    let _ = app.emit("copy-conflict-done", id);
                    return Err(cancelled());
                }
            }
        }
    }
}

/// Whether `a` in `da` and `b` in `db` have the same content.
pub(crate) fn same_content(da: &dyn vp_driver::MediaDriver, a: &str, db: &dyn vp_driver::MediaDriver, b: &str) -> CmdResult<bool> {
    let (ra, rb) = (da.open(a)?, db.open(b)?);
    if ra.len() != rb.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; PIECE], vec![0u8; PIECE]);
    let mut off = 0u64;
    while off < ra.len() {
        let n = ra.read_full_at(off, &mut x)?;
        let m = rb.read_full_at(off, &mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            break;
        }
        off += n as u64;
    }
    Ok(true)
}

/// `path` with " (2)", " (3)", ... before its extension, the first one not taken in `out`.
fn free_name(out: &dyn vp_driver::MediaDriver, path: &str) -> String {
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
        .find(|p| out.stat(p).is_err())
        .expect("some name is free")
}

/// Runs the copies (settling taken names by `conflicts`), then `rescan` (which adds the
/// copies to the library) and records what followed them; returns `(failed, skipped)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn copy_files(
    ws: &Workspace,
    registry: &vp_driver::DriverRegistry,
    to: &SourceRow,
    jobs: Vec<CopyJob>,
    conflicts: Conflicts,
    task: &Task,
    cancel: &AtomicBool,
    rescan: impl FnOnce(),
) -> CmdResult<CopyReport> {
    let shard = if to.driver == DRIVER { Some(writable(ws, to)?) } else { None };
    let out = ws.driver(registry, to)?;
    let max_whole = ws.settings.read().unwrap().cache.max_item_bytes;
    task.set(0, Some(jobs.iter().map(|j| j.size).sum()));
    let (mut failed, mut skipped) = (0, 0);
    let mut copied = Vec::new();
    let mut thumbs = Vec::new();
    let mut variants = Vec::new();
    let mut origins: std::collections::HashMap<i64, Arc<dyn vp_driver::MediaDriver>> = Default::default();
    // An answer the user gave for every remaining conflict.
    let mut for_all = match &conflicts {
        Conflicts::Fixed(c) => Some(*c),
        Conflicts::Ask(_) => None,
    };
    for mut j in jobs {
        if ws.closed.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        task.detail(&j.target);
        let from = match origins.get(&j.source_id) {
            Some(d) => d.clone(),
            None => {
                let d = ws
                    .read_db
                    .lock()
                    .unwrap()
                    .source(j.source_id)?
                    .ok_or_else(|| CmdError::new("not_found", "source"))
                    .and_then(|s| ws.driver(registry, &s));
                match d {
                    Ok(d) => {
                        origins.insert(j.source_id, d.clone());
                        d
                    }
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                }
            }
        };
        if let Ok(existing) = out.stat(&j.target) {
            // The same file (size, then content), worked out once and only when needed.
            let mut same: Option<bool> = None;
            let is_same = |same: &mut Option<bool>| -> bool {
                *same.get_or_insert_with(|| existing.size == j.size && same_content(out.as_ref(), &j.target, from.as_ref(), &j.rel).unwrap_or(false))
            };
            let choice = match (for_all, &conflicts) {
                (Some(c), _) => c,
                (None, Conflicts::Ask(app)) => {
                    let facts = ConflictAsk {
                        id: 0,
                        task: task.id(),
                        target: j.target.clone(),
                        source: j.rel.clone(),
                        existing_size: existing.size,
                        existing_modified_ms: existing.modified_ms,
                        incoming_size: j.size,
                        incoming_modified_ms: j.modified_ms,
                        same: Some(is_same(&mut same)),
                    };
                    let (c, all) = ask_conflict(app, facts, cancel, &ws.closed)?;
                    if all {
                        for_all = Some(c);
                    }
                    c
                }
                (None, Conflicts::Fixed(c)) => *c,
            };
            let skip = match choice {
                OnConflict::SkipName => true,
                OnConflict::SkipSame => is_same(&mut same),
                OnConflict::Rename | OnConflict::Overwrite => false,
            };
            if skip {
                skipped += 1;
                task.add(j.size);
                continue;
            }
            if choice != OnConflict::Overwrite {
                j.target = free_name(out.as_ref(), &j.target);
            }
        }
        let one = (|| -> CmdResult<()> {
            let reader = from.open(&j.rel)?;
            match (&shard, out.local_path(&j.target)) {
                (Some(s), _) => put_file(s, &j.target, reader.as_ref(), j.created_ms, j.modified_ms, task, cancel)?,
                (None, Some(path)) => write_local(&path, reader.as_ref(), j.modified_ms, task, cancel)?,
                (None, None) if reader.len() <= max_whole => {
                    let mut data = vec![0u8; reader.len() as usize];
                    let n = reader.read_full_at(0, &mut data)?;
                    data.truncate(n);
                    out.write(&j.target, &data)?;
                    task.add(n as u64);
                }
                (None, None) => return Err(CmdError::new("too_large", "the file is too large to copy to this source")),
            }
            Ok(())
        })();
        match one {
            Err(e) if e.code == "cancelled" => return Err(e),
            Err(_) => {
                failed += 1;
                continue;
            }
            Ok(()) => {}
        }
        // The thumbnail goes along: into the shard, or the bundle's packs. Its row is
        // recorded after the scan below, once the copy has an id.
        if let Some(t) = ws.stored_thumb(j.id, &j.rel, j.source_id)?
            && let Some(row) = ws.keep_thumb(shard.as_deref(), &j.target, &t)?
        {
            thumbs.push((j.target.clone(), row));
        }
        if let Some(s) = &shard
            && let Some(v) = ws.variant_reader(j.id)?
        {
            let mut attrs = serde_json::Map::new();
            attrs.insert("mime".into(), "video/mp4".into());
            let len = v.len();
            if fill(s.blob_writer(Stream::Converted, &j.target, Some(len), attrs)?, v.as_ref(), task, cancel).is_ok() {
                variants.push((j.target.clone(), len));
            }
        }
        copied.push((j.id, j.target));
    }
    rescan();
    ws.write_db.lock().unwrap().copy_metadata_to(to.id, &copied)?;
    let known = ws.read_db.lock().unwrap().known_files(to.id)?;
    for (rel, row) in thumbs {
        if let Some(k) = known.get(&rel) {
            ws.write_db.lock().unwrap().set_thumbnail(k.id, &row)?;
        }
    }
    let db = ws.write_db.lock().unwrap();
    for (rel, len) in variants {
        if let Some(k) = known.get(&rel) {
            db.set_variant(k.id, VARIANT_IN_SHARD, len, now_ms())?;
        }
    }
    Ok(CopyReport { failed, skipped, copied })
}

/// What [`copy_files`] did.
pub(crate) struct CopyReport {
    pub(crate) failed: usize,
    pub(crate) skipped: usize,
    /// Each copied item and where its copy is (`CopyJob::target`, renamed if it was taken).
    pub(crate) copied: Vec<(i64, String)>,
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

#[cfg(test)]
mod copy_tests {
    use super::*;
    use vp_driver::DriverRegistry;

    fn scan(ws: &Workspace, registry: &DriverRegistry, id: i64) {
        let source = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
        let drv = ws.driver(registry, &source).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: id, include_nonmedia: true, now_ms: 1, cancel: &cancel }
            .run(&mut |_| {})
            .unwrap();
    }

    fn add(ws: &Workspace, name: &str, driver: &str, root: &str, config: serde_json::Value) -> i64 {
        ws.write_db.lock().unwrap().add_source(&NewSource { name: name.into(), driver: driver.into(), config, root: root.into() }).unwrap()
    }

    fn copy(ws: &Workspace, registry: &DriverRegistry, ids: &[i64], folders: &[FolderPick], to: i64, dest: &str) -> (usize, usize) {
        copy_as(ws, registry, ids, folders, to, dest, OnConflict::SkipName)
    }

    fn copy_as(ws: &Workspace, registry: &DriverRegistry, ids: &[i64], folders: &[FolderPick], to: i64, dest: &str, on: OnConflict) -> (usize, usize) {
        let target = ws.read_db.lock().unwrap().source(to).unwrap().unwrap();
        let jobs = copy_jobs(ws, ids, folders, dest).unwrap();
        let task = tasks::begin("copy_to_source", "test", Unit::Bytes, None);
        let cancel = AtomicBool::new(false);
        let r = copy_files(ws, registry, &target, jobs, Conflicts::Fixed(on), &task, &cancel, || scan(ws, registry, to)).unwrap();
        (r.failed, r.skipped)
    }

    #[test]
    fn items_and_folders_copy_into_a_shard_and_back_out() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        let registry = DriverRegistry::with_builtin();
        let photos = dir.path().join("photos");
        std::fs::create_dir_all(photos.join("trip/day1")).unwrap();
        std::fs::write(photos.join("a.jpg"), b"aaa").unwrap();
        std::fs::write(photos.join("trip/b.jpg"), b"bbbb").unwrap();
        std::fs::write(photos.join("trip/day1/c.jpg"), b"ccccc").unwrap();
        let local = add(&ws, "photos", "local", &photos.display().to_string(), serde_json::json!({}));
        scan(&ws, &registry, local);
        let known = ws.read_db.lock().unwrap().known_files(local).unwrap();
        let a = known["a.jpg"].id;
        // A value that has to follow the copy.
        ws.write_db.lock().unwrap().record_view(a, 1, 5_000, 1e9).unwrap();

        let shard_dir = root.join(vp_bundle::SHARDS_DIR).join("s.vpshard");
        Shard::create(&shard_dir, &CreateOptions { chunk_bytes: 1 << 20, ..Default::default() }).unwrap();
        let shard = add(&ws, "s", DRIVER, "shards/s.vpshard", serde_json::json!({ "in_bundle": true }));

        // An item and a folder (with its own name) into the shard.
        let folders = [FolderPick { source_id: local, path: "trip".into() }];
        assert_eq!(copy(&ws, &registry, &[a], &folders, shard, "in"), (0, 0));
        let inside = ws.read_db.lock().unwrap().known_files(shard).unwrap();
        let mut paths: Vec<&str> = inside.keys().map(String::as_str).collect();
        paths.sort();
        assert_eq!(paths, ["in/a.jpg", "in/trip/b.jpg", "in/trip/day1/c.jpg"]);
        let copied = ws.read_db.lock().unwrap().media_detail(inside["in/a.jpg"].id).unwrap().unwrap();
        assert_eq!(copied.view_count, 1);
        // Again: everything is already there.
        assert_eq!(copy(&ws, &registry, &[a], &folders, shard, "in"), (0, 3));

        // Taken names: the same files are skipped, a changed one goes in beside the old.
        std::fs::write(photos.join("a.jpg"), b"xyz").unwrap();
        assert_eq!(copy_as(&ws, &registry, &[a], &folders, shard, "in", OnConflict::SkipSame), (0, 2));
        let read = |p: &str| {
            let s = ws.read_db.lock().unwrap().source(shard).unwrap().unwrap();
            let r = ws.driver(&registry, &s).unwrap().open(p).unwrap();
            let mut b = vec![0u8; r.len() as usize];
            r.read_full_at(0, &mut b).unwrap();
            b
        };
        assert_eq!((read("in/a.jpg"), read("in/a (2).jpg")), (b"aaa".to_vec(), b"xyz".to_vec()));
        // Rename copies again; overwrite replaces.
        assert_eq!(copy_as(&ws, &registry, &[a], &[], shard, "in", OnConflict::Rename), (0, 0));
        assert_eq!(read("in/a (3).jpg"), b"xyz");
        assert_eq!(copy_as(&ws, &registry, &[a], &[], shard, "in", OnConflict::Overwrite), (0, 0));
        assert_eq!(read("in/a.jpg"), b"xyz");
        assert_eq!(copy_as(&ws, &registry, &[a], &[], shard, "in", OnConflict::SkipSame), (0, 1));
        std::fs::write(photos.join("a.jpg"), b"aaa").unwrap();

        // The whole shard out into a new folder of the local source.
        let out = [FolderPick { source_id: shard, path: String::new() }];
        assert_eq!(copy(&ws, &registry, &[], &out, local, "from-shard"), (0, 0));
        assert_eq!(std::fs::read(photos.join("from-shard/in/trip/day1/c.jpg")).unwrap(), b"ccccc");
        let back = ws.read_db.lock().unwrap().known_files(local).unwrap();
        assert!(back.contains_key("from-shard/in/a.jpg"));
        ws.close_shard(shard).unwrap();
    }
}
