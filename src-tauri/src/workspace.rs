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
    /// Open shards of shard sources (see `shards.rs`).
    pub(crate) shards: Mutex<HashMap<i64, Arc<vp_shard::Shard>>>,
    /// Master keys of encrypted shards unlocked in this session, by source.
    pub(crate) shard_keys: Mutex<HashMap<i64, vp_crypto::BundleKey>>,
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
        let mut settings: Settings = match settings_text {
            Some(t) => serde_json::from_str(&t)?,
            None => Settings::default(),
        };
        // Kept in memory; written with the next settings save.
        settings.upgrade();
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
        if (ffmpeg.is_some() || crate::platform::media_available()) && !manual {
            // Retry thumbnails that failed while no video (or HEIF) decoder was available.
            write_db.lock().unwrap().reset_video_thumbnails()?;
            write_db.lock().unwrap().reset_unsupported_image_thumbnails()?;
        }
        let saved_changes = AtomicU64::new(write_db.lock().unwrap().total_changes());
        let c = &settings.cache;
        let capacity = crate::recent::cached_app_settings().hardware.cache_bytes(c.memory_bytes);
        let cache = MediaCache::new(capacity, c.chunk_bytes, c.max_item_bytes);
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
            shards: Mutex::default(),
            shard_keys: Mutex::default(),
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
                    if let Some(per_chunk) = total_bytes.checked_div(total) {
                        task.set(per_chunk * done, None);
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
        let mut tracker = crate::tasks::ReadTracker::new(file_name(&loc.rel_path), len, Some(id));
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
        let shards: Vec<_> = self.shards.lock().unwrap().values().cloned().collect();
        for s in shards {
            let _ = s.flush();
        }
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
            // Shard keys may only be remembered inside an encrypted bundle.
            db.conn().execute(
                "UPDATE source SET config = json_remove(config, '$.key') WHERE driver = ?1",
                [crate::shards::DRIVER],
            )?;
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

    /// Sources whose storage is not reachable now (an unplugged drive, a missing shard
    /// folder). Their items are left out of searches. Network sources are not probed here:
    /// that could block for the connection timeout.
    pub fn offline_sources(&self) -> Vec<i64> {
        let sources = self.read_db.lock().unwrap().sources().unwrap_or_default();
        sources
            .iter()
            .filter(|s| {
                let root = if s.driver == BUNDLE_DRIVER {
                    vp_driver::checked_components(&s.root).ok().map(|c| {
                        let mut p = self.bundle.root.clone();
                        p.extend(c);
                        p
                    })
                } else if s.driver == crate::shards::DRIVER {
                    self.shard_root(s).ok()
                } else if s.driver == "local" {
                    Some(PathBuf::from(&s.root))
                } else {
                    return false;
                };
                !root.is_some_and(|p| p.is_dir())
            })
            .map(|s| s.id)
            .collect()
    }

    pub fn driver(&self, registry: &DriverRegistry, source: &SourceRow) -> CmdResult<Arc<dyn MediaDriver>> {
        if let Some(d) = self.drivers.lock().unwrap().get(&source.id) {
            return Ok(d.clone());
        }
        let driver: Arc<dyn MediaDriver> = if source.driver == BUNDLE_DRIVER {
            let mut root: PathBuf = self.bundle.root.clone();
            root.extend(vp_driver::checked_components(&source.root)?);
            registry.get("local")?.connect(&root.display().to_string(), &source.config)?
        } else if source.driver == crate::shards::DRIVER {
            Arc::new(vp_driver::shard::ShardDriver::new(self.shard(source)?))
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

    /// Drops the cached connection (and open shard) of a source; the next use reconnects.
    pub fn forget_driver(&self, source_id: i64) -> Option<Arc<vp_shard::Shard>> {
        self.drivers.lock().unwrap().remove(&source_id);
        self.shards.lock().unwrap().remove(&source_id)
    }

    /// Stores a newly made thumbnail: next to the item for shard sources, otherwise in the
    /// bundle's packs. `None` when it cannot be kept (manual-save mode, read-only shard).
    fn keep_thumb(&self, shard: Option<&vp_shard::Shard>, rel_path: &str, t: &vp_thumb::Thumb) -> CmdResult<Option<ThumbRow>> {
        if !self.autosave() {
            return Ok(None);
        }
        let row = |pack, offset, len| ThumbRow {
            status: ThumbStatus::Ok,
            pack,
            offset,
            len,
            width: t.width,
            height: t.height,
            mime: t.mime.into(),
        };
        match shard {
            Some(s) if !s.is_writable() => Ok(None),
            Some(s) => {
                let mut attrs = serde_json::Map::new();
                attrs.insert("mime".into(), t.mime.into());
                attrs.insert("width".into(), t.width.into());
                attrs.insert("height".into(), t.height.into());
                s.put_blob(vp_shard::Stream::Thumbs, rel_path, &t.data, attrs)?;
                Ok(Some(row(vp_db::THUMB_IN_SOURCE, 0, t.data.len() as u32)))
            }
            None => {
                let stored = self.seal_thumb(&t.data)?;
                let (pack, offset) = self.thumbs.append(&stored)?;
                Ok(Some(row(pack, offset, stored.len() as u32)))
            }
        }
    }

    /// Thumbnail bytes as stored in the bundle packs or a shard, for moving them elsewhere.
    pub fn stored_thumb(&self, media_id: i64, rel_path: &str, source_id: i64) -> CmdResult<Option<vp_thumb::Thumb>> {
        let Some(row) = self.read_db.lock().unwrap().thumbnail(media_id)? else { return Ok(None) };
        if row.status != ThumbStatus::Ok {
            return Ok(None);
        }
        let data = if row.pack == vp_db::THUMB_IN_SOURCE {
            let Some(shard) = self.shard_for(source_id)? else { return Ok(None) };
            match shard.blob(vp_shard::Stream::Thumbs, rel_path)? {
                Some(r) => r.read_all()?,
                None => return Ok(None),
            }
        } else {
            let data = self.thumbs.read(row.pack, row.offset, row.len)?;
            match &self.key {
                Some(k) => vp_crypto::open(k, &data, b"thumb")?,
                None => data,
            }
        };
        let mime = if row.mime == "image/png" { "image/png" } else { "image/jpeg" };
        Ok(Some(vp_thumb::Thumb { data, width: row.width, height: row.height, mime }))
    }

    /// Returns the thumbnail bytes and MIME type, generating and storing it on first use.
    /// `None` means there is no thumbnail (unsupported type or a failed decode).
    /// Blocking; call from a worker thread.
    pub fn thumbnail(&self, registry: &DriverRegistry, media_id: i64) -> CmdResult<Option<(Vec<u8>, String)>> {
        let existing = self.read_db.lock().unwrap().thumbnail(media_id)?;
        if let Some(row) = &existing {
            match row.status {
                // Kept by the item's shard: read below.
                ThumbStatus::Ok if row.pack == vp_db::THUMB_IN_SOURCE => {}
                ThumbStatus::Ok => {
                    let data = self.thumbs.read(row.pack, row.offset, row.len)?;
                    let data = match &self.key {
                        Some(k) => vp_crypto::open(k, &data, b"thumb")?,
                        None => data,
                    };
                    return Ok(Some((data, row.mime.clone())));
                }
                _ => return Ok(None),
            }
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
        if !loc.kind.is_media() {
            record(ThumbRow::without_data(ThumbStatus::Unsupported))?;
            return Ok(None);
        }
        // A shard keeps its items' thumbnails, also from other bundles it was attached to.
        let shard = self.shard_for(loc.source_id)?;
        if let Some(s) = &shard
            && let Some(r) = s.blob(vp_shard::Stream::Thumbs, &loc.rel_path)?
        {
            let data = r.read_all()?;
            let attr = |k: &str| r.attrs().get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let mime = r.attrs().get("mime").and_then(|v| v.as_str()).unwrap_or("image/jpeg").to_string();
            if existing.is_none() {
                record(ThumbRow {
                    status: ThumbStatus::Ok,
                    pack: vp_db::THUMB_IN_SOURCE,
                    offset: 0,
                    len: data.len() as u32,
                    width: attr("width"),
                    height: attr("height"),
                    mime: mime.clone(),
                })?;
            }
            return Ok(Some((data, mime)));
        }
        if loc.kind == MediaKind::Video {
            let _generating = GENERATING.guard();
            return match self.video_frame(registry, media_id, &loc)? {
                Some(jpeg) => {
                    let t = vp_thumb::Thumb { data: jpeg, width: 0, height: 0, mime: "image/jpeg" };
                    if let Some(row) = self.keep_thumb(shard.as_deref(), &loc.rel_path, &t)? {
                        record(row)?;
                    }
                    Ok(Some((t.data, "image/jpeg".into())))
                }
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
        // The OS decoder subsamples while decoding (Android): much faster and lighter than
        // decoding a camera photo at full size. Anything it cannot read goes the usual way.
        let os = self
            .os_media_path(registry, &loc)?
            .and_then(|p| crate::platform::scaled_image(&p, settings.max_edge_px, settings.quality))
            .map(|s| vp_thumb::Thumb {
                data: s.data,
                width: s.width,
                height: s.height,
                mime: if s.png { "image/png" } else { "image/jpeg" },
            });
        let generated = match os {
            Some(t) => Ok(t),
            None => {
                let reader = self.open_media(registry, &loc)?;
                let bytes = read_tracked(reader.as_ref(), &loc.rel_path, Some(media_id))?;
                match vp_thumb::generate(&bytes, &settings) {
                    // HEIF/HEIC, AVIF and other formats the built-in decoder lacks. A file
                    // no decoder reads counts as failed, not unsupported (those are retried).
                    Err(e @ (vp_thumb::ThumbError::Unsupported | vp_thumb::ThumbError::Decode(_))) => {
                        let decodable = self.ffmpeg().is_some() || crate::platform::media_available();
                        match self.foreign_image(registry, media_id, &loc, Some(&bytes), settings.max_edge_px, settings.quality)? {
                            Some(t) => Ok(t),
                            None if decodable => Err(vp_thumb::ThumbError::Decode(e.to_string())),
                            None => Err(e),
                        }
                    }
                    r => r,
                }
            }
        };

        match generated {
            Ok(t) => {
                if let Some(row) = self.keep_thumb(shard.as_deref(), &loc.rel_path, &t)? {
                    record(row)?;
                }
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
pub fn read_tracked(reader: &dyn MediaReader, rel_path: &str, media_id: Option<i64>) -> CmdResult<Vec<u8>> {
    let len = reader.len() as usize;
    let piece = (crate::recent::cached_app_settings().progress.read_piece_bytes as usize).max(64 * 1024);
    let mut bytes = vec![0u8; len];
    let mut tracker = crate::tasks::ReadTracker::new(file_name(rel_path), len as u64, media_id);
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
            include_nonmedia: false,
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
        assert!(ws.read_db.lock().unwrap().missing_thumbnails(10, &[]).unwrap().is_empty());
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
    fn sources_on_missing_folders_are_offline() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        std::fs::create_dir_all(root.join("media")).unwrap();
        let add = |driver: &str, root: &str| {
            let source = NewSource { name: root.into(), driver: driver.into(), config: serde_json::json!({}), root: root.into() };
            ws.write_db.lock().unwrap().add_source(&source).unwrap()
        };
        let here = add(BUNDLE_DRIVER, "media");
        let gone = add(BUNDLE_DRIVER, "unplugged");
        let local = add("local", &dir.path().display().to_string());
        let drive = add("local", &dir.path().join("E-drive").display().to_string());
        let nas = add("smb", "share");
        let offline = ws.offline_sources();
        assert_eq!(offline, [gone, drive]);
        assert!(![here, local, nas].iter().any(|id| offline.contains(id)));
    }

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

#[cfg(test)]
mod shard_tests {
    use super::*;
    use vp_db::NewSource;

    fn jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_fn(640, 480, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 7]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode_image(&img).unwrap();
        out
    }

    fn scan(ws: &Workspace, registry: &DriverRegistry, id: i64) -> vp_scan::ScanProgress {
        let source = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();
        let drv = ws.driver(registry, &source).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: id, include_nonmedia: true, now_ms: 1, cancel: &cancel }
            .run(&mut |_| {})
            .unwrap()
    }

    #[test]
    fn encrypted_shard_source_keeps_thumbnails_inside() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        let shard_dir = root.join(vp_bundle::SHARDS_DIR).join("s.vpshard");
        let opts = vp_shard::CreateOptions {
            password: Some("pw".into()),
            kdf_memory_kib: 64,
            kdf_iterations: 1,
            chunk_bytes: 1 << 20,
            ..Default::default()
        };
        let key = vp_shard::Shard::create(&shard_dir, &opts).unwrap().unwrap();
        let id = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource {
                name: "s".into(),
                driver: crate::shards::DRIVER.into(),
                config: serde_json::json!({ "in_bundle": true }),
                root: "shards/s.vpshard".into(),
            })
            .unwrap();
        let registry = DriverRegistry::with_builtin();
        let source = ws.read_db.lock().unwrap().source(id).unwrap().unwrap();

        // Locked: no driver, and background jobs leave it alone.
        let err = ws.driver(&registry, &source).err().expect("locked");
        assert_eq!(err.code, "shard_locked");
        assert_eq!(ws.locked_sources(), [id]);

        ws.shard_keys.lock().unwrap().insert(id, key);
        let shard = ws.shard(&source).unwrap();
        shard.put("여행/secret-photo.jpg", &jpeg(), None, Some(5)).unwrap();
        assert!(ws.locked_sources().is_empty());
        assert_eq!(scan(&ws, &registry, id).files, 1);

        let media = ws.read_db.lock().unwrap().known_files(id).unwrap()["여행/secret-photo.jpg"].id;
        let (data, mime) = ws.thumbnail(&registry, media).unwrap().expect("thumbnail");
        assert_eq!(mime, "image/jpeg");
        let row = ws.read_db.lock().unwrap().thumbnail(media).unwrap().unwrap();
        assert_eq!(row.pack, vp_db::THUMB_IN_SOURCE, "kept by the shard, not the bundle packs");
        assert!(shard.has_blob(vp_shard::Stream::Thumbs, "여행/secret-photo.jpg"));
        assert_eq!(ws.thumbnail(&registry, media).unwrap().unwrap().0, data);

        // Nothing readable in the shard folder; the bundle's thumbnail packs stay empty.
        ws.flush();
        let packs: u64 = std::fs::read_dir(root.join(vp_bundle::THUMBS_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.metadata().unwrap().len())
            .sum();
        assert_eq!(packs, 0);
        for f in std::fs::read_dir(shard_dir.join("data")).unwrap().flatten() {
            let raw = std::fs::read(f.path()).unwrap();
            assert!(!raw.windows(12).any(|w| w == b"secret-photo"));
        }

        // A thumbnail cleared from the bundle is found again in the shard.
        ws.write_db.lock().unwrap().conn().execute("DELETE FROM thumbnail", []).unwrap();
        assert_eq!(ws.thumbnail(&registry, media).unwrap().unwrap().0, data);
        drop(shard);
        ws.close_shard(id).unwrap();
    }

    #[test]
    fn unlocking_the_bundle_forgets_remembered_shard_keys() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("K.vpbun");
        let ws = Workspace::create(&root, "K").unwrap();
        let key = ws.enable_lock("pw", None).unwrap();
        drop(ws);
        remove_files(&root, true).unwrap();
        let ws = Workspace::open_with_key(&root, key).unwrap();
        let id = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource {
                name: "s".into(),
                driver: crate::shards::DRIVER.into(),
                config: serde_json::json!({ "in_bundle": true, "key": "ab".repeat(32) }),
                root: "shards/s.vpshard".into(),
            })
            .unwrap();
        ws.disable_lock().unwrap();
        drop(ws);
        remove_files(&root, false).unwrap();
        let ws = Workspace::open(&root, false).unwrap();
        let config = ws.read_db.lock().unwrap().source(id).unwrap().unwrap().config;
        assert_eq!(config, serde_json::json!({ "in_bundle": true }));
    }
}
