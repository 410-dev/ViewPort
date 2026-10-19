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
    /// Folder being listed (relative to the source root).
    pub current: String,
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
    /// Unchanged, but its pixel size was missing and is now known.
    SeenWithSize(i64, u32, u32),
    Upsert(NewMedia),
}

/// Pixel size from a JPEG's frame header, walking the file marker by marker. Camera photos
/// can carry more EXIF data before that header than a fixed-size probe reads (Samsung:
/// over 80 KB). `read` fills the buffer at an offset and reports success.
pub fn jpeg_size(len: u64, read: &mut dyn FnMut(u64, &mut [u8]) -> bool) -> Option<(u32, u32)> {
    let mut b = [0u8; 5];
    if len < 4 || !read(0, &mut b[..2]) || b[..2] != [0xFF, 0xD8] {
        return None;
    }
    let mut pos = 2u64;
    for _ in 0..512 {
        if pos + 4 > len || !read(pos, &mut b[..4]) || b[0] != 0xFF {
            return None;
        }
        let marker = b[1];
        match marker {
            // Fill byte, or markers without a length.
            0xFF => pos += 1,
            0x01 | 0xD0..=0xD8 => pos += 2,
            // End of image or start of scan: no frame header before it.
            0xD9 | 0xDA => return None,
            // SOF0..SOF15, except DHT (C4), JPG (C8) and DAC (CC).
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                if !read(pos + 4, &mut b) {
                    return None;
                }
                let h = u16::from_be_bytes([b[1], b[2]]) as u32;
                let w = u16::from_be_bytes([b[3], b[4]]) as u32;
                return (w > 0 && h > 0).then_some((w, h));
            }
            _ => pos += 2 + u16::from_be_bytes([b[2], b[3]]) as u64,
        }
    }
    None
}

pub struct Scanner<'a> {
    pub db: &'a Mutex<Db>,
    pub driver: &'a dyn MediaDriver,
    pub settings: &'a ScanSettings,
    pub source_id: i64,
    /// Record files that are neither images nor videos too (as `NonMedia`).
    pub include_nonmedia: bool,
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
            // Listing can be slow on network shares; report where the scan is first.
            p.current.clone_from(&dir);
            progress(&p);
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
                let Some(kind) = classifier.kind(e.name()).or(self.include_nonmedia.then_some(MediaKind::NonMedia)) else {
                    continue;
                };
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
                if kind == MediaKind::Image && !k.has_size
                    && let Some((Some(w), Some(h))) = self.probe_image(&e.path)
                {
                    return Action::SeenWithSize(k.id, w, h);
                }
                return Action::Seen(k.id);
            }
            p.updated += 1;
        } else {
            p.added += 1;
        }
        let (width, height) = match kind {
            MediaKind::Image => self.probe_image(&e.path).unwrap_or((None, None)),
            // Video metadata needs the media pipeline; filled in by a later probe.
            MediaKind::Video | MediaKind::NonMedia => (None, None),
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
        let mut read = |off: u64, buf: &mut [u8]| reader.read_full_at(off, buf).is_ok_and(|n| n == buf.len());
        if let Some((w, h)) = jpeg_size(reader.len(), &mut read) {
            return Some((Some(w), Some(h)));
        }
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
                    Action::SeenWithSize(id, w, h) => {
                        db.mark_seen(*id, self.now_ms)?;
                        db.set_dimensions(*id, *w, *h)?;
                    }
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
            let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: false, now_ms, cancel: &cancel };
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

        // With non-media files included, notes.txt is listed as such and found by kind.
        let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: true, now_ms: 3, cancel: &cancel };
        let p = s.run(&mut |_| {}).unwrap();
        assert_eq!((p.files, p.added), (3, 1));
        let db = db.lock().unwrap();
        let reg = db.field_registry().unwrap();
        let q = vp_query::parse_query("유형(기타 파일)", &reg).unwrap();
        let ids = db.search(&q, &reg, &Db::compile_context(0, 0)).unwrap();
        let brief = db.media_brief(&ids).unwrap();
        assert_eq!(brief.iter().map(|b| (b.name.as_str(), b.kind.as_str())).collect::<Vec<_>>(), [("notes.txt", "nonmedia")]);
        assert_eq!(db.media_location(ids[0]).unwrap().unwrap().kind, MediaKind::NonMedia);
    }

    /// A JPEG head like a Samsung camera's: over 128 KB of EXIF before the frame header.
    fn camera_jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8];
        for _ in 0..2 {
            v.extend([0xFF, 0xE1, 0xFF, 0xFF]);
            v.extend(std::iter::repeat_n(0u8, 0xFFFF - 2));
        }
        v.extend([0xFF, 0xDB, 0x00, 0x04, 0x00, 0x00]);
        v.extend([0xFF, 0xC0, 0x00, 0x11, 0x08]);
        v.extend(h.to_be_bytes());
        v.extend(w.to_be_bytes());
        v.extend([0x03, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        v.extend([0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        v
    }

    fn reader(data: &[u8]) -> impl FnMut(u64, &mut [u8]) -> bool + '_ {
        move |off, buf| {
            let off = off as usize;
            let Some(src) = data.get(off..off + buf.len()) else { return false };
            buf.copy_from_slice(src);
            true
        }
    }

    #[test]
    fn jpeg_size_skips_large_exif() {
        let data = camera_jpeg(16320, 12240);
        assert!(data.len() > 128 * 1024);
        assert_eq!(jpeg_size(data.len() as u64, &mut reader(&data)), Some((16320, 12240)));
        let png = png(3, 4);
        assert_eq!(jpeg_size(png.len() as u64, &mut reader(&png)), None);
        assert_eq!(jpeg_size(data.len() as u64 - 100_000, &mut reader(&data[..data.len() - 100_000])), None);
    }

    #[test]
    fn rescan_fills_missing_image_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jpg");
        // First seen without a readable frame header (as by an older version)...
        let good = camera_jpeg(4000, 3000);
        let sof = good.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
        let mut bad = good.clone();
        bad[sof + 1] = 0xFE;
        fs::write(&path, &bad).unwrap();
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();

        let db = Mutex::new(Db::open_in_memory().unwrap());
        let source_id = db
            .lock()
            .unwrap()
            .add_source(&NewSource {
                name: "t".into(),
                driver: "local".into(),
                config: serde_json::json!({}),
                root: dir.path().display().to_string(),
            })
            .unwrap();
        let driver = LocalDriver::new(dir.path());
        let settings = ScanSettings::default();
        let cancel = AtomicBool::new(false);
        let scan = |now_ms| {
            let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: false, now_ms, cancel: &cancel };
            s.run(&mut |_| {}).unwrap()
        };
        let size = || {
            let db = db.lock().unwrap();
            let id = db.known_files(source_id).unwrap()["big.jpg"].id;
            let b = db.media_brief(&[id]).unwrap().pop().unwrap();
            (b.width, b.height)
        };
        scan(1);
        assert_eq!(size(), (None, None));

        // ...then readable with the same size and time: the rescan fills the size in
        // without counting the file as changed.
        fs::write(&path, &good).unwrap();
        fs::File::options().write(true).open(&path).unwrap().set_modified(mtime).unwrap();
        let p = scan(2);
        assert_eq!((p.unchanged, p.updated), (1, 0));
        assert_eq!(size(), (Some(4000), Some(3000)));
    }

    #[test]
    fn cancel_stops_scan() {
        let dir = tempfile::tempdir().unwrap();
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let driver = LocalDriver::new(dir.path());
        let settings = ScanSettings::default();
        let cancel = AtomicBool::new(true);
        let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id: 1, include_nonmedia: false, now_ms: 1, cancel: &cancel };
        assert!(matches!(s.run(&mut |_| {}), Err(ScanError::Cancelled)));
    }
}
