//! Scans a source through its driver and records media in the library.
//!
//! Directory walking happens without holding the database; rows are written in batches,
//! each in its own short transaction, so searches stay responsive during a scan.
//! Unchanged files (same size and modification time) are only marked as seen.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use vp_core::MediaKind;
use vp_core::settings::ScanSettings;
use vp_db::{Db, KnownFile, NewMedia};
use vp_driver::{Entry, MediaDriver};

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Db(#[from] vp_db::DbError),
    #[error(transparent)]
    Driver(#[from] vp_driver::DriverError),
    #[error("scan cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ScanProgress {
    pub source_id: i64,
    pub dirs: u64,
    pub files: u64,
    pub added: u64,
    pub updated: u64,
    pub unchanged: u64,
    /// Present only once the scan has finished.
    pub missing: Option<u64>,
    /// Directories or files that could not be read.
    pub errors: Vec<String>,
}

/// Classifies files by extension using the configured lists.
pub struct Classifier {
    image: HashSet<String>,
    video: HashSet<String>,
}

impl Classifier {
    pub fn new(s: &ScanSettings) -> Self {
        let set = |v: &[String]| v.iter().map(|e| e.trim_start_matches('.').to_lowercase()).collect();
        Self { image: set(&s.image_extensions), video: set(&s.video_extensions) }
    }

    pub fn kind(&self, name: &str) -> Option<MediaKind> {
        let ext = name.rsplit_once('.')?.1.to_lowercase();
        if self.image.contains(&ext) {
            Some(MediaKind::Image)
        } else if self.video.contains(&ext) {
            Some(MediaKind::Video)
        } else {
            None
        }
    }
}

enum Action {
    Seen(i64),
    Upsert(NewMedia),
}

pub struct Scanner<'a> {
    pub db: &'a Mutex<Db>,
    pub driver: &'a dyn MediaDriver,
    pub settings: &'a ScanSettings,
    pub source_id: i64,
    /// Also used as the scan generation id.
    pub now_ms: i64,
    pub cancel: &'a AtomicBool,
}

impl Scanner<'_> {
    pub fn run(&self, progress: &mut dyn FnMut(&ScanProgress)) -> Result<ScanProgress, ScanError> {
        let classifier = Classifier::new(self.settings);
        let known = self.db.lock().unwrap().known_files(self.source_id)?;
        let mut p = ScanProgress { source_id: self.source_id, ..Default::default() };
        let mut batch = Vec::with_capacity(self.settings.batch_size as usize);
        let mut stack = vec![String::new()];

        while let Some(dir) = stack.pop() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            let mut entries = match self.driver.list(&dir) {
                Ok(e) => e,
                Err(e) => {
                    p.errors.push(format!("{dir}: {e}"));
                    continue;
                }
            };
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            p.dirs += 1;
            let mut subdirs = Vec::new();
            for e in entries {
                if self.settings.skip_hidden && e.name().starts_with('.') {
                    continue;
                }
                if e.is_dir {
                    subdirs.push(e.path);
                    continue;
                }
                let Some(kind) = classifier.kind(e.name()) else { continue };
                p.files += 1;
                batch.push(self.action(&e, kind, &known, &mut p));
                if batch.len() >= self.settings.batch_size.max(1) as usize {
                    self.flush(&mut batch)?;
                    progress(&p);
                }
            }
            // Pushed in reverse so the walk visits subfolders in name order.
            stack.extend(subdirs.into_iter().rev());
        }
        self.flush(&mut batch)?;
        let missing = self.db.lock().unwrap().finish_scan(self.source_id, self.now_ms)?;
        p.missing = Some(missing as u64);
        progress(&p);
        Ok(p)
    }

    fn action(&self, e: &Entry, kind: MediaKind, known: &HashMap<String, KnownFile>, p: &mut ScanProgress) -> Action {
        if let Some(k) = known.get(&e.path) {
            if k.size == e.size && k.modified_ms == e.modified_ms {
                p.unchanged += 1;
                return Action::Seen(k.id);
            }
            p.updated += 1;
        } else {
            p.added += 1;
        }
        let (width, height) = match kind {
            MediaKind::Image => self.probe_image(&e.path).unwrap_or((None, None)),
            // Video metadata needs the media pipeline; filled in by a later probe.
            MediaKind::Video => (None, None),
        };
        Action::Upsert(NewMedia {
            source_id: self.source_id,
            rel_path: e.path.clone(),
            kind,
            size: e.size,
            created_ms: e.created_ms.or(e.modified_ms),
            modified_ms: e.modified_ms,
            added_ms: self.now_ms,
            duration_ms: None,
            width,
            height,
            seen_gen: self.now_ms,
        })
    }

    fn probe_image(&self, path: &str) -> Option<(Option<u32>, Option<u32>)> {
        let reader = self.driver.open(path).ok()?;
        let mut buf = vec![0u8; (self.settings.header_probe_bytes as u64).min(reader.len()) as usize];
        let n = reader.read_full_at(0, &mut buf).ok()?;
        let size = imagesize::blob_size(&buf[..n]).ok()?;
        Some((u32::try_from(size.width).ok(), u32::try_from(size.height).ok()))
    }

    fn flush(&self, batch: &mut Vec<Action>) -> Result<(), ScanError> {
        if batch.is_empty() {
            return Ok(());
        }
        let db = self.db.lock().unwrap();
        db.batch(|db| {
            for a in batch.iter() {
                match a {
                    Action::Seen(id) => db.mark_seen(*id, self.now_ms)?,
                    Action::Upsert(m) => {
                        db.upsert_media(m)?;
                    }
                }
            }
            Ok(())
        })?;
        batch.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use vp_db::NewSource;
    use vp_driver::local::LocalDriver;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        v.extend(w.to_be_bytes());
        v.extend(h.to_be_bytes());
        v.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        v
    }

    #[test]
    fn scan_and_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("trip/day1")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join("trip/a.png"), png(640, 480)).unwrap();
        fs::write(root.join("trip/day1/b.PNG"), png(10, 20)).unwrap();
        fs::write(root.join("trip/clip.mp4"), b"not really").unwrap();
        fs::write(root.join("notes.txt"), b"skip").unwrap();
        fs::write(root.join(".hidden/c.png"), png(1, 1)).unwrap();

        let db = Mutex::new(Db::open_in_memory().unwrap());
        let source_id = db
            .lock()
            .unwrap()
            .add_source(&NewSource {
                name: "t".into(),
                driver: "local".into(),
                config: serde_json::json!({}),
                root: root.display().to_string(),
            })
            .unwrap();
        let driver = LocalDriver::new(root);
        let settings = ScanSettings { batch_size: 2, ..Default::default() };
        let cancel = AtomicBool::new(false);
        let scan = |now_ms| {
            let mut events = 0;
            let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, now_ms, cancel: &cancel };
            let p = s.run(&mut |_| events += 1).unwrap();
            (p, events)
        };

        let (p, events) = scan(1);
        assert_eq!((p.files, p.added, p.unchanged, p.missing), (3, 3, 0, Some(0)));
        assert!(events >= 2);
        {
            let db = db.lock().unwrap();
            let reg = db.field_registry().unwrap();
            let q = vp_query::Query::default();
            let ids = db.search(&q, &reg, &Db::compile_context(0, 0)).unwrap();
            let brief = db.media_brief(&ids).unwrap();
            let a = brief.iter().find(|b| b.name == "a.png").unwrap();
            assert_eq!((a.width, a.height), (Some(640), Some(480)));
            assert!(brief.iter().any(|b| b.name == "clip.mp4" && b.kind == "video"));
        }

        fs::remove_file(root.join("trip/day1/b.PNG")).unwrap();
        fs::write(root.join("trip/a.png"), png(100, 100)).unwrap();
        let (p, _) = scan(2);
        assert_eq!(p.files, 2);
        assert_eq!(p.missing, Some(1));
        assert_eq!(p.added, 0);
        assert_eq!(p.unchanged + p.updated, 2);
    }

    #[test]
    fn cancel_stops_scan() {
        let dir = tempfile::tempdir().unwrap();
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let driver = LocalDriver::new(dir.path());
        let settings = ScanSettings::default();
        let cancel = AtomicBool::new(true);
        let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id: 1, now_ms: 1, cancel: &cancel };
        assert!(matches!(s.run(&mut |_| {}), Err(ScanError::Cancelled)));
    }
}
