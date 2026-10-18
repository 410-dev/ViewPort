//! An open bundle: its database connections, settings, drivers and thumbnail store.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use vp_bundle::{Bundle, OpenMode};
use vp_core::MediaKind;
use vp_core::settings::Settings;
use vp_db::{Db, SourceRow, ThumbRow, ThumbStatus};
use vp_driver::{DriverRegistry, MediaDriver};
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
}

pub struct Workspace {
    pub bundle: Bundle,
    pub settings: RwLock<Settings>,
    /// Writer connection (scans, edits). Reads use `read_db` so they are not blocked by
    /// long write batches; SQLite WAL lets both run at once.
    pub write_db: Mutex<Db>,
    pub read_db: Mutex<Db>,
    pub thumbs: PackStore,
    pub thumb_permits: Arc<tokio::sync::Semaphore>,
    drivers: Mutex<HashMap<i64, Arc<dyn MediaDriver>>>,
    pub scans: Mutex<HashMap<i64, Arc<AtomicBool>>>,
    /// Set when the bundle is closed so background jobs stop.
    pub closed: AtomicBool,
}

impl Workspace {
    pub fn create(path: &Path, name: &str) -> CmdResult<Self> {
        let bundle = Bundle::create(path, name, env!("CARGO_PKG_VERSION"), now_ms())?;
        Self::load(bundle)
    }

    pub fn open(path: &Path, allow_newer: bool) -> CmdResult<Self> {
        let bundle = Bundle::open(path, allow_newer)?;
        if bundle.is_locked() {
            return Err(CmdError::new("locked_not_supported", "encrypted bundles are not supported yet"));
        }
        Self::load(bundle)
    }

    fn load(bundle: Bundle) -> CmdResult<Self> {
        let settings = match std::fs::read_to_string(bundle.path(vp_bundle::SETTINGS_FILE)) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e.into()),
        };
        let db_path = bundle.path(vp_bundle::DB_FILE);
        if let Some(dir) = db_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let write_db = Db::open(&db_path)?;
        let read_db = Db::open(&db_path)?;
        let thumbs = PackStore::open(&bundle.path(vp_bundle::THUMBS_DIR), settings.thumbnails.pack_bytes)?;
        let workers = match settings.thumbnails.workers {
            0 => std::thread::available_parallelism().map(|n| (n.get() / 2).max(1)).unwrap_or(2),
            n => n as usize,
        };
        Ok(Self {
            bundle,
            settings: RwLock::new(settings),
            write_db: Mutex::new(write_db),
            read_db: Mutex::new(read_db),
            thumbs,
            thumb_permits: Arc::new(tokio::sync::Semaphore::new(workers)),
            drivers: Mutex::default(),
            scans: Mutex::default(),
            closed: AtomicBool::new(false),
        })
    }

    pub fn summary(&self) -> BundleSummary {
        BundleSummary {
            path: self.bundle.root.display().to_string(),
            name: self.bundle.info.name.clone(),
            mode: self.bundle.mode,
            bundle_version: self.bundle.info.bundle_version,
        }
    }

    pub fn save_settings(&self, settings: Settings) -> CmdResult<()> {
        if self.bundle.mode == OpenMode::Autosave {
            let path = self.bundle.path(vp_bundle::SETTINGS_FILE);
            vp_driver::local::write_atomic(&path, &serde_json::to_vec_pretty(&settings)?)?;
        }
        *self.settings.write().unwrap() = settings;
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
            registry.get(&source.driver)?.connect(&source.root, &source.config)?
        };
        self.drivers.lock().unwrap().insert(source.id, driver.clone());
        Ok(driver)
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
                ThumbStatus::Ok => Some((self.thumbs.read(row.pack, row.offset, row.len)?, row.mime)),
                _ => None,
            });
        }
        let Some(loc) = self.read_db.lock().unwrap().media_location(media_id)? else { return Ok(None) };
        let settings = self.settings.read().unwrap().thumbnails.clone();

        let record = |row: ThumbRow| -> CmdResult<()> {
            self.write_db.lock().unwrap().set_thumbnail(media_id, &row)?;
            Ok(())
        };
        // Video frames need the media pipeline, which is not in place yet.
        if loc.kind == MediaKind::Video || loc.size > settings.max_source_bytes {
            record(ThumbRow::without_data(ThumbStatus::Unsupported))?;
            return Ok(None);
        }
        let source = self.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let driver = self.driver(registry, &source)?;
        let reader = driver.open(&loc.rel_path)?;
        let mut bytes = vec![0u8; reader.len() as usize];
        let n = reader.read_full_at(0, &mut bytes)?;
        bytes.truncate(n);

        match vp_thumb::generate(&bytes, &settings) {
            Ok(t) => {
                let (pack, offset) = self.thumbs.append(&t.data)?;
                record(ThumbRow {
                    status: ThumbStatus::Ok,
                    pack,
                    offset,
                    len: t.data.len() as u32,
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
