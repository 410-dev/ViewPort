//! An open bundle: its database connections, settings, drivers and thumbnail store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use vp_bundle::{Bundle, OpenMode};
use vp_core::MediaKind;
use vp_core::settings::Settings;
use vp_cache::{CachedMedia, MediaCache};
use vp_db::{Db, DbOptions, MediaLocation, SourceRow, ThumbRow, ThumbStatus};
use vp_driver::{DriverRegistry, MediaDriver, MediaReader};
use vp_thumb::PackStore;

use crate::error::{CmdError, CmdResult};

pub const BUNDLE_DRIVER: &str = "bundle";

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleSummary {
    pub path: String,
    pub name: String,
    pub mode: OpenMode,
    pub bundle_version: u32,
    pub locked: bool,
}

pub struct Workspace {
    pub bundle: Bundle,
    pub settings: RwLock<Settings>,
    /// Writer connection (scans, edits). Reads use `read_db` so they are not blocked by
    /// long write batches; SQLite WAL lets both run at once. In a locked bundle the
    /// database lives in memory and both are the same connection.
    pub write_db: Arc<Mutex<Db>>,
    pub read_db: Arc<Mutex<Db>>,
    /// Key of a locked bundle.
    pub key: Option<vp_crypto::BundleKey>,
    /// `total_changes` of the in-memory database when it was last written to disk.
    saved_changes: AtomicU64,
    pub thumbs: PackStore,
    pub thumb_permits: Arc<tokio::sync::Semaphore>,
    drivers: Mutex<HashMap<i64, Arc<dyn MediaDriver>>>,
    pub scans: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    pub cache: Arc<MediaCache>,
    pub prefetch_permits: Arc<tokio::sync::Semaphore>,
    /// Set when the bundle is closed so background jobs stop.
    pub closed: Arc<AtomicBool>,
    /// In manual-save mode the database is a temporary working copy; see [`Workspace::save`].
    working_db: Option<PathBuf>,
    pub ffmpeg: RwLock<Option<vp_media::Ffmpeg>>,
    /// Running conversions by media id, with their cancel flags.
    pub converts: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// A video probe backfill is running.
    pub probing: AtomicBool,
}

impl Workspace {
    pub fn create(path: &Path, name: &str) -> CmdResult<Self> {
        let bundle = Bundle::create(path, name, env!("CARGO_PKG_VERSION"), now_ms())?;
        Self::load(bundle, None)
    }

    /// Opens an unlocked bundle. A locked one fails with `locked`; see [`Self::open_locked`].
    pub fn open(path: &Path, allow_newer: bool) -> CmdResult<Self> {
        let bundle = Bundle::open(path, allow_newer)?;
        if let Some(lock) = &bundle.info.lock {
            return Err(CmdError::new("locked", "the bundle is locked")
                .with_detail(serde_json::json!({ "requires_keyfile": lock.requires_keyfile })));
        }
        Self::load(bundle, None)
    }

    pub fn open_locked(path: &Path, allow_newer: bool, password: &str, keyfile: Option<&[u8]>) -> CmdResult<Self> {
        let bundle = Bundle::open(path, allow_newer)?;
        let lock = bundle.info.lock.clone().ok_or_else(|| CmdError::new("not_locked", "the bundle is not locked"))?;
        let key = vp_crypto::unlock(password, keyfile, &lock)?;
        Self::load(bundle, Some(key))
    }

    pub fn open_with_key(path: &Path, key: vp_crypto::BundleKey) -> CmdResult<Self> {
        let bundle = Bundle::open(path, false)?;
        Self::load(bundle, Some(key))
    }

    fn read_sealed(bundle: &Bundle, rel: &str, key: &vp_crypto::BundleKey, label: &[u8]) -> CmdResult<Option<Vec<u8>>> {
        match std::fs::read(bundle.path(rel)) {
            Ok(data) => Ok(Some(vp_crypto::open(key, &data, label)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn load(bundle: Bundle, key: Option<vp_crypto::BundleKey>) -> CmdResult<Self> {
        let settings_text = match &key {
            Some(k) => Self::read_sealed(&bundle, vp_bundle::SETTINGS_ENC_FILE, k, b"settings")?
                .map(|b| String::from_utf8_lossy(&b).into_owned()),
            None => match std::fs::read_to_string(bundle.path(vp_bundle::SETTINGS_FILE)) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
        };
        let settings: Settings = match settings_text {
            Some(t) => serde_json::from_str(&t)?,
            None => Settings::default(),
        };
        let bundle_db = bundle.path(vp_bundle::DB_FILE);
        if let Some(dir) = bundle_db.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let manual = bundle.mode == OpenMode::ManualSaveOnly;
        // A bundle from a newer build is never written implicitly: work on a copy.
        let working_db = if manual {
            let dir = std::env::temp_dir().join(format!("viewport-{}-{}", std::process::id(), now_ms()));
            std::fs::create_dir_all(&dir)?;
            let copy = dir.join("library.sqlite");
            if bundle_db.exists() {
                Db::open_with(&bundle_db, &DbOptions { allow_newer_schema: true })?.backup_to(&copy)?;
            }
            Some(copy)
        } else {
            None
        };
        let db_path = working_db.clone().unwrap_or(bundle_db);
        let opts = DbOptions { allow_newer_schema: manual };
        let (write_db, read_db) = match &key {
            Some(k) => {
                let bytes = Self::read_sealed(&bundle, vp_bundle::DB_ENC_FILE, k, b"db")?;
                let db = Arc::new(Mutex::new(Db::open_memory_from(bytes.as_deref(), &opts)?));
                (db.clone(), db)
            }
            None => (Arc::new(Mutex::new(Db::open_with(&db_path, &opts)?)), Arc::new(Mutex::new(Db::open_with(&db_path, &opts)?))),
        };
        let thumbs = PackStore::open(&bundle.path(vp_bundle::THUMBS_DIR), settings.thumbnails.pack_bytes)?;
        let workers = match settings.thumbnails.workers {
            0 => std::thread::available_parallelism().map(|n| (n.get() / 2).max(1)).unwrap_or(2),
            n => n as usize,
        };
        let ffmpeg = crate::video::locate(&settings.playback.ffmpeg_path);
        if ffmpeg.is_some() && !manual {
            // Retry video thumbnails that failed while FFmpeg was unavailable.
            write_db.lock().unwrap().reset_video_thumbnails()?;
        }
        let saved_changes = AtomicU64::new(write_db.lock().unwrap().total_changes());
        let c = &settings.cache;
        let cache = MediaCache::new(c.memory_bytes, c.chunk_bytes, c.max_item_bytes);
        let prefetch_permits = Arc::new(tokio::sync::Semaphore::new(c.prefetch_workers.max(1) as usize));
        Ok(Self {
            bundle,
            write_db,
            read_db,
            key,
            saved_changes,
            thumbs,
            thumb_permits: Arc::new(tokio::sync::Semaphore::new(workers)),
            drivers: Mutex::default(),
            scans: Mutex::default(),
            cache,
            prefetch_permits,
            closed: Arc::new(AtomicBool::new(false)),
            working_db,
            ffmpeg: RwLock::new(ffmpeg),
            converts: Mutex::default(),
            probing: AtomicBool::new(false),
            settings: RwLock::new(settings),
        })
    }

    /// Whether changes are written to the bundle as they happen.
    pub fn autosave(&self) -> bool {
        self.bundle.mode == OpenMode::Autosave
    }

    /// Explicit save for manual-save mode: copies the working database and settings into
    /// the bundle. A no-op in autosave mode, where everything is already saved.
    pub fn save(&self) -> CmdResult<()> {
        if self.key.is_some() {
            return self.persist_locked(true);
        }
        if self.working_db.is_none() {
            return Ok(());
        }
        let dest = self.bundle.path(vp_bundle::DB_FILE);
        let tmp = dest.with_extension("sqlite.saving");
        self.write_db.lock().unwrap().backup_to(&tmp)?;
        for ext in ["sqlite-wal", "sqlite-shm"] {
            let _ = std::fs::remove_file(dest.with_extension(ext));
        }
        std::fs::rename(&tmp, &dest)?;
        let settings = self.settings.read().unwrap().clone();
        let json = serde_json::to_vec_pretty(&settings)?;
        vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::SETTINGS_FILE), &json)?;
        Ok(())
    }

    /// Starts background work that runs while the bundle is open: preloading the most
    /// popular items, then committing the rest of the cache memory.
    pub fn start_background(self: &Arc<Self>, drivers: Arc<DriverRegistry>) {
        if self.key.is_some() && self.autosave() {
            let ws = self.clone();
            let _ = std::thread::Builder::new().name("encrypted-save".into()).spawn(move || {
                while !ws.closed.load(Ordering::Relaxed) {
                    let secs = ws.settings.read().unwrap().general.encrypted_save_secs.max(1);
                    for _ in 0..secs * 4 {
                        if ws.closed.load(Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(250));
                    }
                    let _ = ws.persist_locked(false);
                }
            });
        }
        let ws = self.clone();
        let _ = std::thread::Builder::new().name("cache-warmup".into()).spawn(move || {
            let c = ws.settings.read().unwrap().cache.clone();
            ws.autoload(&drivers, c.autoload_top_n as usize, c.autoload_ratio as f64);
            if c.preallocate {
                let total_bytes = ws.cache.capacity_bytes();
                let task = crate::tasks::begin("reserve", "", crate::tasks::Unit::Bytes, Some(total_bytes));
                ws.cache.preallocate_with(&ws.closed, &mut |done, total| {
                    if total > 0 {
                        task.set(total_bytes / total * done, None);
                    }
                });
            }
        });
    }

    fn autoload(&self, drivers: &DriverRegistry, top_n: usize, ratio: f64) {
        let budget = (self.cache.capacity_bytes() as f64 * ratio.clamp(0.0, 1.0)) as u64;
        let Ok(popular) = self.read_db.lock().unwrap().most_popular(top_n) else { return };
        if popular.is_empty() {
            return;
        }
        let task = crate::tasks::begin("preload", "", crate::tasks::Unit::Items, Some(popular.len() as u64));
        let mut used = 0u64;
        for (id, size) in popular {
            task.add(1);
            if self.closed.load(Ordering::Relaxed) || used + size > budget {
                break;
            }
            if self.load_into_cache(drivers, id).ok().flatten().is_some() {
                used += size;
            }
        }
    }

    /// Loads an item into the memory cache (or returns it if already there).
    /// `None` if it is too large to cache or the cache is full of items in use.
    pub fn load_into_cache(&self, drivers: &DriverRegistry, id: i64) -> CmdResult<Option<CachedMedia>> {
        if let Some(m) = self.cache.get(id) {
            return Ok(Some(m));
        }
        let Some(loc) = self.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
        if !self.cache.fits(loc.size) {
            return Ok(None);
        }
        let reader = self.open_media(drivers, &loc)?;
        let len = reader.len();
        let mut tracker = crate::tasks::ReadTracker::new(file_name(&loc.rel_path), len);
        let result = self.cache.insert_with(id, len, |offset, buf| {
            let n = reader.read_full_at(offset, buf)?;
            if n < buf.len() {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            tracker.advance(n as u64);
            Ok(())
        });
        if let Err(e) = &result {
            tracker.fail(e.to_string());
        }
        Ok(result?)
    }

    pub fn open_media(&self, drivers: &DriverRegistry, loc: &MediaLocation) -> CmdResult<Box<dyn MediaReader>> {
        let source =
            self.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        Ok(self.driver(drivers, &source)?.open(&loc.rel_path)?)
    }

    pub fn summary(&self) -> BundleSummary {
        BundleSummary {
            path: self.bundle.root.display().to_string(),
            name: self.bundle.info.name.clone(),
            mode: self.bundle.mode,
            bundle_version: self.bundle.info.bundle_version,
            locked: self.key.is_some(),
        }
    }

    pub fn save_settings(&self, settings: Settings) -> CmdResult<()> {
        if self.autosave() {
            self.write_settings(&settings)?;
        }
        let ffmpeg_changed = self.settings.read().unwrap().playback.ffmpeg_path != settings.playback.ffmpeg_path;
        *self.settings.write().unwrap() = settings;
        if ffmpeg_changed {
            self.relocate_ffmpeg();
        }
        Ok(())
    }

    fn write_settings(&self, settings: &Settings) -> CmdResult<()> {
        let json = serde_json::to_vec_pretty(settings)?;
        match &self.key {
            Some(k) => {
                let sealed = vp_crypto::seal(k, &json, b"settings")?;
                vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::SETTINGS_ENC_FILE), &sealed)?;
            }
            None => vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::SETTINGS_FILE), &json)?,
        }
        Ok(())
    }

    /// Re-encrypts the in-memory database to the bundle if it changed (or always with `force`).
    pub fn persist_locked(&self, force: bool) -> CmdResult<()> {
        let Some(key) = &self.key else { return Ok(()) };
        if !self.autosave() && !force {
            return Ok(());
        }
        let (bytes, changes) = {
            let db = self.write_db.lock().unwrap();
            let changes = db.total_changes();
            if !force && changes == self.saved_changes.load(Ordering::Relaxed) {
                return Ok(());
            }
            (db.serialize()?, changes)
        };
        let sealed = vp_crypto::seal(key, &bytes, b"db")?;
        vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::DB_ENC_FILE), &sealed)?;
        self.saved_changes.store(changes, Ordering::Relaxed);
        Ok(())
    }

    /// Writes everything that is buffered. Call before closing the bundle.
    pub fn flush(&self) {
        let _ = self.persist_locked(false);
        let _ = self.thumbs.sync();
    }

    /// Encrypts this (unlocked) bundle with a new key and returns the key. Thumbnails are
    /// dropped and regenerate encrypted. The caller reopens the bundle afterwards and then
    /// removes the plaintext files with [`remove_files`].
    pub fn enable_lock(&self, password: &str, keyfile: Option<&[u8]>) -> CmdResult<vp_crypto::BundleKey> {
        if self.key.is_some() {
            return Err(CmdError::new("already_locked", "the bundle is already locked"));
        }
        if !self.autosave() {
            return Err(CmdError::new("read_only", "cannot lock a bundle opened without autosave"));
        }
        if password.is_empty() {
            return Err(CmdError::new("invalid", "password is empty"));
        }
        let (key, params) = vp_crypto::new_key(password, keyfile, vp_crypto::DEFAULT_MEMORY_KIB, vp_crypto::DEFAULT_ITERATIONS)?;
        let bytes = {
            let db = self.write_db.lock().unwrap();
            db.conn().execute("DELETE FROM thumbnail", [])?;
            db.serialize()?
        };
        let sealed = vp_crypto::seal(&key, &bytes, b"db")?;
        vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::DB_ENC_FILE), &sealed)?;
        let json = serde_json::to_vec_pretty(&*self.settings.read().unwrap())?;
        let sealed = vp_crypto::seal(&key, &json, b"settings")?;
        vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::SETTINGS_ENC_FILE), &sealed)?;
        let mut info = self.bundle.clone();
        info.info.lock = Some(params);
        info.write_info()?;
        Ok(key)
    }

    /// Decrypts this locked bundle back to plain files. The caller reopens it afterwards
    /// and removes the encrypted files with [`remove_files`].
    pub fn disable_lock(&self) -> CmdResult<()> {
        if self.key.is_none() {
            return Err(CmdError::new("not_locked", "the bundle is not locked"));
        }
        {
            let db = self.write_db.lock().unwrap();
            db.conn().execute("DELETE FROM thumbnail", [])?;
            db.backup_to(&self.bundle.path(vp_bundle::DB_FILE))?;
        }
        let json = serde_json::to_vec_pretty(&*self.settings.read().unwrap())?;
        vp_driver::local::write_atomic(&self.bundle.path(vp_bundle::SETTINGS_FILE), &json)?;
        let mut info = self.bundle.clone();
        info.info.lock = None;
        info.write_info()?;
        Ok(())
    }

    /// Splits a picked folder into `(driver, root)`. Folders inside the bundle are stored
    /// relative to it so the bundle keeps working when moved to another machine.
    pub fn source_root_for(&self, folder: &Path) -> (String, String) {
        let bundle_root = std::fs::canonicalize(&self.bundle.root).unwrap_or_else(|_| self.bundle.root.clone());
        let folder_abs = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
        match folder_abs.strip_prefix(&bundle_root) {
            Ok(rel) => {
                let rel: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                (BUNDLE_DRIVER.into(), rel.join("/"))
            }
            Err(_) => ("local".into(), folder.display().to_string()),
        }
    }

    pub fn driver(&self, registry: &DriverRegistry, source: &SourceRow) -> CmdResult<Arc<dyn MediaDriver>> {
        if let Some(d) = self.drivers.lock().unwrap().get(&source.id) {
            return Ok(d.clone());
        }
        let driver = if source.driver == BUNDLE_DRIVER {
            let mut root: PathBuf = self.bundle.root.clone();
            root.extend(vp_driver::checked_components(&source.root)?);
            registry.get("local")?.connect(&root.display().to_string(), &source.config)?
        } else {
            let factory = registry.get(&source.driver)?;
            let config = crate::secrets::reveal(&source.driver, &source.config, &factory.config_schema());
            factory.connect(&source.root, &config)?
        };
        self.drivers.lock().unwrap().insert(source.id, driver.clone());
        Ok(driver)
    }

    fn seal_thumb(&self, data: &[u8]) -> CmdResult<Vec<u8>> {
        Ok(match &self.key {
            Some(k) => vp_crypto::seal(k, data, b"thumb")?,
            None => data.to_vec(),
        })
    }

    pub fn forget_driver(&self, source_id: i64) {
        self.drivers.lock().unwrap().remove(&source_id);
    }

    /// Returns the thumbnail bytes and MIME type, generating and storing it on first use.
    /// `None` means there is no thumbnail (unsupported type or a failed decode).
    /// Blocking; call from a worker thread.
    pub fn thumbnail(&self, registry: &DriverRegistry, media_id: i64) -> CmdResult<Option<(Vec<u8>, String)>> {
        let existing = self.read_db.lock().unwrap().thumbnail(media_id)?;
        if let Some(row) = existing {
            return Ok(match row.status {
                ThumbStatus::Ok => {
                    let data = self.thumbs.read(row.pack, row.offset, row.len)?;
                    let data = match &self.key {
                        Some(k) => vp_crypto::open(k, &data, b"thumb")?,
                        None => data,
                    };
                    Some((data, row.mime))
                }
                _ => None,
            });
        }
        let Some(loc) = self.read_db.lock().unwrap().media_location(media_id)? else { return Ok(None) };
        let settings = self.settings.read().unwrap().thumbnails.clone();

        // In manual-save mode thumbnails are generated but not stored.
        let record = |row: ThumbRow| -> CmdResult<()> {
            if self.autosave() {
                self.write_db.lock().unwrap().set_thumbnail(media_id, &row)?;
            }
            Ok(())
        };
        if loc.kind == MediaKind::Video {
            let _generating = GENERATING.guard();
            return match self.video_frame(registry, media_id, &loc)? {
                Some(jpeg) if self.autosave() => {
                    let stored = self.seal_thumb(&jpeg)?;
                    let (pack, offset) = self.thumbs.append(&stored)?;
                    let row = ThumbRow {
                        status: ThumbStatus::Ok,
                        pack,
                        offset,
                        len: stored.len() as u32,
                        width: 0,
                        height: 0,
                        mime: "image/jpeg".into(),
                    };
                    record(row)?;
                    Ok(Some((jpeg, "image/jpeg".into())))
                }
                Some(jpeg) => Ok(Some((jpeg, "image/jpeg".into()))),
                None => {
                    record(ThumbRow::without_data(ThumbStatus::Unsupported))?;
                    Ok(None)
                }
            };
        }
        if loc.size > settings.max_source_bytes {
            record(ThumbRow::without_data(ThumbStatus::Unsupported))?;
            return Ok(None);
        }
        let _generating = GENERATING.guard();
        let reader = self.open_media(registry, &loc)?;
        let bytes = read_tracked(reader.as_ref(), &loc.rel_path)?;

        match vp_thumb::generate(&bytes, &settings) {
            Ok(t) if !self.autosave() => Ok(Some((t.data, t.mime.into()))),
            Ok(t) => {
                let stored = self.seal_thumb(&t.data)?;
                let (pack, offset) = self.thumbs.append(&stored)?;
                record(ThumbRow {
                    status: ThumbStatus::Ok,
                    pack,
                    offset,
                    len: stored.len() as u32,
                    width: t.width,
                    height: t.height,
                    mime: t.mime.into(),
                })?;
                Ok(Some((t.data, t.mime.into())))
            }
            Err(vp_thumb::ThumbError::Io(e)) => Err(e.into()),
            Err(vp_thumb::ThumbError::Unsupported) => {
                record(ThumbRow::without_data(ThumbStatus::Unsupported))?;
                Ok(None)
            }
            Err(_) => {
                record(ThumbRow::without_data(ThumbStatus::Failed))?;
                Ok(None)
            }
        }
    }
}

/// Thumbnails being generated on demand (for what is on screen), shown as one task.
static GENERATING: GenerateBurst = GenerateBurst(crate::tasks::Burst::new("thumbs_visible"));

struct GenerateBurst(crate::tasks::Burst);

impl GenerateBurst {
    fn guard(&'static self) -> impl Drop {
        struct Guard(&'static crate::tasks::Burst);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.leave();
            }
        }
        self.0.enter();
        Guard(&self.0)
    }
}

pub fn file_name(rel_path: &str) -> String {
    rel_path.rsplit(['/', '\\']).next().unwrap_or(rel_path).to_string()
}

/// Reads a whole file in pieces of `progress.read_piece_bytes`, reporting slow reads.
pub fn read_tracked(reader: &dyn MediaReader, rel_path: &str) -> CmdResult<Vec<u8>> {
    let len = reader.len() as usize;
    let piece = (crate::recent::cached_app_settings().progress.read_piece_bytes as usize).max(64 * 1024);
    let mut bytes = vec![0u8; len];
    let mut tracker = crate::tasks::ReadTracker::new(file_name(rel_path), len as u64);
    let mut off = 0usize;
    while off < len {
        let end = (off + piece).min(len);
        let want = end - off;
        let n = match reader.read_full_at(off as u64, &mut bytes[off..end]) {
            Ok(n) => n,
            Err(e) => {
                tracker.fail(e.to_string());
                return Err(e.into());
            }
        };
        tracker.advance(n as u64);
        off += n;
        if n < want {
            break; // the file got shorter since it was scanned
        }
    }
    bytes.truncate(off);
    Ok(bytes)
}

/// Deletes bundle files that a lock change left behind: the plaintext ones after
/// locking (`plain = true`) or the encrypted ones after unlocking. Thumbnail packs go
/// either way. Call with the bundle closed.
pub fn remove_files(root: &Path, plain: bool) -> std::io::Result<()> {
    let names: &[&str] = if plain {
        &[vp_bundle::DB_FILE, "db/library.sqlite-wal", "db/library.sqlite-shm", vp_bundle::SETTINGS_FILE]
    } else {
        &[vp_bundle::DB_ENC_FILE, vp_bundle::SETTINGS_ENC_FILE]
    };
    for n in names {
        match std::fs::remove_file(root.join(n)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join(vp_bundle::THUMBS_DIR)) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "pack") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_db::NewSource;

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode_image(&img).unwrap();
        out
    }

    #[test]
    fn bundle_scan_thumbnail_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();

        // Media placed inside the bundle is stored relative to it.
        let media = root.join("media/trip");
        std::fs::create_dir_all(&media).unwrap();
        std::fs::write(media.join("a.jpg"), jpeg(800, 600)).unwrap();
        std::fs::write(media.join("broken.jpg"), b"not a jpeg").unwrap();
        std::fs::write(media.join("clip.mp4"), b"video").unwrap();
        let (driver, rel) = ws.source_root_for(&root.join("media"));
        assert_eq!((driver.as_str(), rel.as_str()), (BUNDLE_DRIVER, "media"));

        let registry = DriverRegistry::with_builtin();
        let id = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "media".into(), driver, config: serde_json::json!({}), root: rel })
            .unwrap();
        let source = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
        let drv = ws.driver(&registry, &source).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        let p = vp_scan::Scanner {
            db: &ws.write_db,
            driver: drv.as_ref(),
            settings: &settings,
            source_id: id,
            now_ms: 1,
            cancel: &cancel,
        }
        .run(&mut |_| {})
        .unwrap();
        assert_eq!(p.files, 3);

        let db = ws.read_db.lock().unwrap();
        let reg = db.field_registry().unwrap();
        let q = vp_query::parse_query("이름(a.jpg)", &reg).unwrap();
        let a = db.search(&q, &reg, &Db::compile_context(0, 0)).unwrap()[0];
        let all = db.search(&vp_query::Query::default(), &reg, &Db::compile_context(0, 0)).unwrap();
        drop(db);

        let (data, mime) = ws.thumbnail(&registry, a).unwrap().expect("thumbnail");
        assert_eq!(mime, "image/jpeg");
        let t = image::load_from_memory(&data).unwrap();
        assert_eq!((t.width(), t.height()), (320, 240));
        // Second request is served from the pack.
        assert_eq!(ws.thumbnail(&registry, a).unwrap().unwrap().0, data);
        for id in all.iter().filter(|i| **i != a) {
            assert!(ws.thumbnail(&registry, *id).unwrap().is_none());
        }
        assert!(ws.read_db.lock().unwrap().missing_thumbnails(10).unwrap().is_empty());
        drop(ws);

        // Reopening finds everything again.
        let ws = Workspace::open(&root, false).unwrap();
        assert_eq!(ws.thumbnail(&registry, a).unwrap().unwrap().0, data);
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use vp_db::NewSource;

    #[test]
    fn lock_unlock_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Secret.vpbun");
        let ws = Workspace::create(&root, "Secret").unwrap();
        let id = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "비밀 폴더".into(), driver: "local".into(), config: serde_json::json!({}), root: "/x".into() })
            .unwrap();

        let key = ws.enable_lock("pw", None).unwrap();
        ws.closed.store(true, Ordering::Relaxed);
        drop(ws);
        remove_files(&root, true).unwrap();
        assert!(!root.join(vp_bundle::DB_FILE).exists());
        let raw = std::fs::read(root.join(vp_bundle::DB_ENC_FILE)).unwrap();
        assert!(!raw.windows("비밀".len()).any(|w| w == "비밀".as_bytes()), "no plaintext in the bundle");

        let ws = Workspace::open_with_key(&root, key).unwrap();
        ws.write_db.lock().unwrap().update_source(id, "renamed", "/y", &serde_json::json!({}), true).unwrap();
        ws.persist_locked(false).unwrap();
        drop(ws);

        assert!(matches!(Workspace::open(&root, false), Err(CmdError { code: "locked", .. })));
        assert!(Workspace::open_locked(&root, false, "wrong", None).is_err());
        let ws = Workspace::open_locked(&root, false, "pw", None).unwrap();
        assert_eq!(ws.read_db.lock().unwrap().source(id).unwrap().unwrap().name, "renamed", "changes were saved");

        ws.disable_lock().unwrap();
        drop(ws);
        remove_files(&root, false).unwrap();
        let ws = Workspace::open(&root, false).unwrap();
        assert_eq!(ws.read_db.lock().unwrap().source(id).unwrap().unwrap().name, "renamed");
        assert!(!root.join(vp_bundle::DB_ENC_FILE).exists());
    }
}
