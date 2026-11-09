//! Scans a source through its driver and records media in the library.
//!
//! What the library already knows about the source is loaded once, folder by folder
//! ([`Known`]). Folders are listed — and the pixel size of new images read — by a few
//! workers ahead of the folder being recorded, while the results are recorded in walk
//! order (folders and files by name), so new items get their ids in that order.
//!
//! Only what changed is written, in batches of short transactions so searches stay
//! responsive: unchanged files cost no write at all. At the end, files not found again are
//! marked missing — except under folders that could not be listed — and a file that turns
//! up under a new path with the size and time of one that went away is taken as moved:
//! the item keeps its id and with it its tags, stats and thumbnail.
//!
//! A scan can cover the whole source or only some folders ([`Scope`], used when watching).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};

use serde::Serialize;
use vp_core::MediaKind;
use vp_core::settings::ScanSettings;
use vp_db::{Db, KnownFile, NewMedia};
use vp_driver::{DriverError, Entry, MediaDriver};

type Hasher = foldhash::fast::RandomState;

/// Version of the pixel size reader. An image whose size this version could not read is
/// not read again until the file changes or the version goes up.
pub const DIMS_PROBE_VERSION: u32 = 1;

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
    /// Files found under a new path (moved or renamed); their items kept their ids.
    pub moved: u64,
    /// Items that were missing and whose file is back.
    pub back: u64,
    /// Items whose file was not found this time (counted once the scan has finished).
    pub gone: u64,
    /// Items of the source marked missing; present only once the scan has finished.
    pub missing: Option<u64>,
    /// Folder being listed (relative to the source root).
    pub current: String,
    /// Directories or files that could not be read.
    pub errors: Vec<String>,
    /// Items whose recorded details changed (updated, moved, back, gone or measured), once
    /// the scan has finished — unless there were more than `scan.report_ids_max`.
    pub changed: Vec<i64>,
    /// More items changed than `changed` lists: views should reload everything.
    pub changed_all: bool,
    /// Only some folders were looked at (a watched change).
    pub partial: bool,
}

impl ScanProgress {
    /// Something in the library changed (new, changed, moved, back or gone items).
    pub fn any_change(&self) -> bool {
        self.added + self.updated + self.moved + self.back + self.gone > 0 || self.changed_all || !self.changed.is_empty()
    }

    /// Items may need thumbnails, probes or analysis.
    pub fn any_new_content(&self) -> bool {
        self.added + self.updated + self.back > 0
    }
}

/// Part of a source to scan.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// A folder and everything below it (`""`: the whole source).
    Tree(String),
    /// The files directly in a folder.
    Folder(String),
}

impl Scope {
    fn dir(&self) -> &str {
        match self {
            Scope::Tree(d) | Scope::Folder(d) => d,
        }
    }

    /// Whether files directly in `dir` belong to this scope.
    fn covers(&self, dir: &str) -> bool {
        match self {
            Scope::Tree(r) => under(dir, r),
            Scope::Folder(f) => dir == f,
        }
    }
}

/// `dir` is `root` or inside it (`""` is the source root).
fn under(dir: &str, root: &str) -> bool {
    root.is_empty() || dir == root || (dir.len() > root.len() && dir.starts_with(root) && dir.as_bytes()[root.len()] == b'/')
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

fn name_of(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, n)| n)
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

/// What the library knows about a source's files, by folder. Each folder's names are
/// sorted, so a sorted listing finds its known files in one pass. (Measured on a million
/// paths: about a tenth of the lookup time of one map keyed by path, and half its memory.)
pub struct Known {
    dirs: HashMap<Box<str>, Vec<(Box<str>, u32)>, Hasher>,
    files: Vec<KnownFile>,
    /// Sizes and modification times of the known files: a new file with one of them may
    /// be a moved one.
    same: HashSet<(u64, Option<i64>), Hasher>,
}

impl Known {
    pub fn load(db: &Db, source_id: i64) -> Result<Self, vp_db::DbError> {
        Self::load_in(db, source_id, &[Scope::Tree(String::new())])
    }

    /// Only what is known inside `scopes`.
    pub fn load_in(db: &Db, source_id: i64, scopes: &[Scope]) -> Result<Self, vp_db::DbError> {
        let mut dirs: HashMap<Box<str>, Vec<(Box<str>, u32)>, Hasher> = HashMap::default();
        let mut files = Vec::new();
        let folders: Vec<(&str, bool)> = scopes.iter().map(|s| (s.dir(), matches!(s, Scope::Tree(_)))).collect();
        db.for_each_known_in(source_id, &folders, |path, k| {
            let i = files.len() as u32;
            files.push(k);
            let (dir, name) = (parent_of(path), name_of(path));
            match dirs.get_mut(dir) {
                Some(v) => v.push((name.into(), i)),
                None => {
                    dirs.insert(dir.into(), vec![(name.into(), i)]);
                }
            }
        })?;
        for v in dirs.values_mut() {
            v.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            v.shrink_to_fit();
        }
        let same = files.iter().map(|k| (k.size, k.modified_ms)).collect();
        Ok(Self { dirs, files, same })
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Known files directly in `dir`, sorted by name.
    fn folder(&self, dir: &str) -> &[(Box<str>, u32)] {
        self.dirs.get(dir).map_or(&[], |v| v.as_slice())
    }

    /// Whether some known file has this size and time (a new file may be a moved one).
    fn has_same(&self, size: u64, modified_ms: Option<i64>) -> bool {
        self.same.contains(&(size, modified_ms))
    }

}

/// A write a scan makes.
enum Action {
    /// A missing item's file is back, unchanged.
    Found(i64),
    /// Unchanged, but its pixel size was missing and is now known.
    SetSize(i64, u32, u32),
    /// Unchanged, its pixel size still unreadable: not tried again.
    DimsFailed(i64),
    /// New or changed file; `true`: its pixel size could not be read.
    Upsert(NewMedia, bool),
    /// The file system knows an earlier added time than the one recorded.
    Added(i64, i64),
    /// A known item's file now lives at the new path.
    Moved(i64, NewMedia),
    /// Not found any more.
    Gone(i64),
}

/// A file a worker found in a folder.
enum Found {
    /// A known file (index into [`Known`]), with what to write about it.
    Known { idx: u32, actions: Vec<Action>, updated: bool, back: bool, measured: bool },
    /// A file the library does not know. `pending_size`: an image left unmeasured because
    /// it may be a moved file (measured later if it is not).
    New { media: NewMedia, dims_failed: bool, pending_size: bool },
}

/// One folder, listed and looked at by a worker.
struct Listed {
    result: Result<(Vec<Found>, Vec<String>), DriverError>,
    files: u64,
}

/// Result of reading an image's pixel size.
enum Dims {
    Size(u32, u32),
    /// Read, but the format is not one the reader understands.
    Unreadable,
    /// The file could not be read (try again next time).
    Failed,
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

/// Called before each batch of rows is written (the app holds writes back while its
/// viewer needs the database, `set_write_gate`).
static WRITE_GATE: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// Sets the function every scan calls before taking the database for a batch.
pub fn set_write_gate(gate: fn()) {
    let _ = WRITE_GATE.set(gate);
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
    /// Added times by path the driver cannot see (Android: the media store's).
    pub added_times: Option<&'a HashMap<String, i64>>,
}

/// Work shared between the recording thread and the listing workers.
struct Queue {
    jobs: VecDeque<String>,
    done: HashMap<String, Listed>,
    stop: bool,
}

impl Scanner<'_> {
    /// Scans the whole source.
    pub fn run(&self, progress: &mut dyn FnMut(&ScanProgress)) -> Result<ScanProgress, ScanError> {
        self.run_scoped(&[Scope::Tree(String::new())], progress)
    }

    /// Scans only `scopes` (folders and trees): files outside them are left as they are.
    pub fn run_scoped(&self, scopes: &[Scope], progress: &mut dyn FnMut(&ScanProgress)) -> Result<ScanProgress, ScanError> {
        let scopes = normalize(scopes);
        let whole = scopes == [Scope::Tree(String::new())];
        let classifier = Classifier::new(self.settings);
        let known = Known::load_in(&self.db.lock().unwrap(), self.source_id, &scopes)?;
        let mut p = ScanProgress { source_id: self.source_id, partial: !whole, ..Default::default() };

        let workers = self.settings.list_workers.max(1) as usize;
        let lookahead = (self.settings.list_lookahead.max(1) as usize).max(workers);
        let queue = Mutex::new(Queue { jobs: VecDeque::new(), done: HashMap::new(), stop: false });
        let (job_ready, result_ready) = (Condvar::new(), Condvar::new());

        std::thread::scope(|sc| {
            if workers > 1 {
                for _ in 0..workers {
                    sc.spawn(|| {
                        loop {
                            let dir = {
                                let mut q = queue.lock().unwrap();
                                loop {
                                    if q.stop {
                                        return;
                                    }
                                    if let Some(d) = q.jobs.pop_front() {
                                        break d;
                                    }
                                    q = job_ready.wait(q).unwrap();
                                }
                            };
                            let listed = self.look_at(&dir, &known, &classifier);
                            queue.lock().unwrap().done.insert(dir, listed);
                            result_ready.notify_all();
                        }
                    });
                }
            }
            let result = self.record(&scopes, &known, &mut p, progress, &mut |wanted: &[&str], dir: &str| {
                if workers <= 1 {
                    return self.look_at(dir, &known, &classifier);
                }
                let mut q = queue.lock().unwrap();
                // Ask for the folders needed soonest first (`wanted` is in walk order). `dir`
                // itself was asked for now or earlier (`record` names each folder once).
                for d in wanted.iter().rev() {
                    q.jobs.push_front(d.to_string());
                }
                job_ready.notify_all();
                loop {
                    if let Some(l) = q.done.remove(dir) {
                        return l;
                    }
                    q = result_ready.wait(q).unwrap();
                }
            }, lookahead);
            queue.lock().unwrap().stop = true;
            job_ready.notify_all();
            result
        })?;
        Ok(p)
    }

    /// Walks the scopes in order, records what the workers found, then settles moved and
    /// gone files. `fetch(wanted, dir)` returns `dir`'s listing; `wanted` names folders to
    /// start listing now (each named once), soonest first.
    fn record(
        &self,
        scopes: &[Scope],
        known: &Known,
        p: &mut ScanProgress,
        progress: &mut dyn FnMut(&ScanProgress),
        fetch: &mut dyn FnMut(&[&str], &str) -> Listed,
        lookahead: usize,
    ) -> Result<(), ScanError> {
        let mut seen = vec![false; known.len()];
        // Folders that could not be listed: what is known under them is kept as it is.
        let mut failed: Vec<String> = Vec::new();
        let mut batch: Vec<Action> = Vec::with_capacity(self.settings.batch_size as usize);
        let mut changed: Vec<i64> = Vec::new();
        let mut maybe_moved: Vec<(NewMedia, bool)> = Vec::new();
        let mut requested: HashSet<String, Hasher> = HashSet::default();

        // (folder, recursive), top = next.
        let mut stack: Vec<(String, bool)> =
            scopes.iter().rev().map(|s| (s.dir().to_string(), matches!(s, Scope::Tree(_)))).collect();
        while let Some((dir, recursive)) = stack.pop() {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(ScanError::Cancelled);
            }
            p.current.clone_from(&dir);
            progress(p);
            let mut wanted: Vec<&str> = Vec::new();
            for d in std::iter::once(dir.as_str()).chain(stack.iter().rev().map(|(d, _)| d.as_str())).take(lookahead) {
                if !requested.contains(d) {
                    wanted.push(d);
                }
            }
            for d in &wanted {
                requested.insert(d.to_string());
            }
            let listed = fetch(&wanted, &dir);
            requested.remove(&dir);
            let (found, subdirs) = match listed.result {
                Ok(r) => r,
                // Gone since its parent was listed (or a watched folder that went away):
                // what was known in it is gone too.
                Err(DriverError::NotFound(_)) if !dir.is_empty() => continue,
                Err(e) => {
                    p.errors.push(format!("{dir}: {e}"));
                    failed.push(dir);
                    continue;
                }
            };
            p.dirs += 1;
            p.files += listed.files;
            for f in found {
                match f {
                    Found::Known { idx, actions, updated, back, measured } => {
                        seen[idx as usize] = true;
                        let id = known.files[idx as usize].id;
                        if updated {
                            p.updated += 1;
                        } else {
                            p.unchanged += 1;
                        }
                        if back {
                            p.back += 1;
                        }
                        if updated || back || measured {
                            changed.push(id);
                        }
                        batch.extend(actions);
                    }
                    Found::New { media, dims_failed, pending_size } => {
                        if pending_size || known.has_same(media.size, media.modified_ms) {
                            maybe_moved.push((media, pending_size));
                        } else {
                            p.added += 1;
                            batch.push(Action::Upsert(media, dims_failed));
                        }
                    }
                }
            }
            if batch.len() >= self.settings.batch_size.max(1) as usize {
                self.flush(&mut batch)?;
                progress(p);
            }
            if recursive {
                // Pushed in reverse so the walk visits subfolders in name order.
                stack.extend(subdirs.into_iter().rev().map(|d| (d, true)));
            }
        }

        // Which known files are gone: in a scope, not found again, and not under a folder
        // that could not be listed (a folder not listed at all is gone with its files).
        let in_scope = |dir: &str| scopes.iter().any(|s| s.covers(dir));
        let kept = |dir: &str| failed.iter().any(|f| under(dir, f));
        let mut gone_now = vec![false; known.len()];
        for (dir, names) in &known.dirs {
            if !in_scope(dir) || kept(dir) {
                continue;
            }
            for &(_, i) in names {
                if !seen[i as usize] {
                    gone_now[i as usize] = true;
                }
            }
        }

        // New files with the size and time of a gone (or already missing) one were moved
        // there: the only such one with the same name, or else the only such one. Other
        // matches are ambiguous and left alone.
        let mut taken = vec![false; known.len()];
        let mut by_time: HashMap<(u64, Option<i64>), Vec<u32>, Hasher> = HashMap::default();
        let mut by_name: HashMap<(&str, u64, Option<i64>), Vec<u32>, Hasher> = HashMap::default();
        if !maybe_moved.is_empty() {
            for (_, names) in known.dirs.iter() {
                for (name, i) in names {
                    let k = &known.files[*i as usize];
                    if !seen[*i as usize] && (gone_now[*i as usize] || k.missing) {
                        by_time.entry((k.size, k.modified_ms)).or_default().push(*i);
                        by_name.entry((&**name, k.size, k.modified_ms)).or_default().push(*i);
                    }
                }
            }
        }
        // Two passes: files whose name settles it first, so a rename elsewhere does not
        // look ambiguous only because of a file that moved under its own name.
        let mut picks: Vec<Option<u32>> = vec![None; maybe_moved.len()];
        for pass in 0..2 {
            for (j, (media, _)) in maybe_moved.iter().enumerate() {
                if picks[j].is_some() {
                    continue;
                }
                let key = (media.size, media.modified_ms);
                let free = |v: Option<&Vec<u32>>| -> Vec<u32> {
                    v.map(|v| v.iter().copied().filter(|&i| !taken[i as usize]).collect()).unwrap_or_default()
                };
                let found = if pass == 0 {
                    free(by_name.get(&(name_of(&media.rel_path), key.0, key.1)))
                } else {
                    free(by_time.get(&key))
                };
                if let [i] = found.as_slice() {
                    taken[*i as usize] = true;
                    picks[j] = Some(*i);
                }
            }
        }
        for ((mut media, pending_size), pick) in maybe_moved.into_iter().zip(picks) {
            match pick {
                Some(i) => {
                    gone_now[i as usize] = false;
                    let id = known.files[i as usize].id;
                    p.moved += 1;
                    changed.push(id);
                    batch.push(Action::Moved(id, media));
                }
                None => {
                    let mut dims_failed = false;
                    if pending_size {
                        match self.probe_image(&media.rel_path) {
                            Dims::Size(w, h) => (media.width, media.height) = (Some(w), Some(h)),
                            Dims::Unreadable => dims_failed = true,
                            Dims::Failed => {}
                        }
                    }
                    p.added += 1;
                    batch.push(Action::Upsert(media, dims_failed));
                }
            }
            if batch.len() >= self.settings.batch_size.max(1) as usize {
                self.flush(&mut batch)?;
            }
        }

        let mut missing = 0u64;
        for (i, k) in known.files.iter().enumerate() {
            if gone_now[i] && !k.missing {
                p.gone += 1;
                changed.push(k.id);
                batch.push(Action::Gone(k.id));
            }
            if gone_now[i] || (k.missing && !seen[i] && !taken[i]) {
                missing += 1;
            }
        }
        self.flush(&mut batch)?;
        p.missing = Some(missing);
        let max = self.settings.report_ids_max as usize;
        if changed.len() > max {
            p.changed_all = true;
        } else {
            changed.sort_unstable();
            changed.dedup();
            p.changed = changed;
        }
        p.current.clear();
        progress(p);
        Ok(())
    }

    /// Lists folder `dir` and works out what to write for each file in it (pixel sizes of
    /// new images included). Runs on a worker; reads `known`, writes nothing.
    fn look_at(&self, dir: &str, known: &Known, classifier: &Classifier) -> Listed {
        let mut entries = match self.driver.list(dir) {
            Ok(e) => e,
            Err(e) => return Listed { result: Err(e), files: 0 },
        };
        entries.sort_unstable_by(|a, b| a.name().cmp(b.name()));
        let names = known.folder(dir);
        let mut at = 0usize;
        let mut found = Vec::new();
        let mut subdirs = Vec::new();
        let mut files = 0u64;
        for e in entries {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
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
            files += 1;
            let name = e.name();
            while at < names.len() && &*names[at].0 < name {
                at += 1;
            }
            let idx = (at < names.len() && &*names[at].0 == name).then(|| names[at].1);
            let added = self.added_times.and_then(|m| m.get(&e.path).copied()).or(e.added_ms).map(|t| t.min(self.now_ms));
            found.push(match idx {
                Some(idx) => self.known_file(&e, kind, added, idx, &known.files[idx as usize]),
                None => {
                    // Possibly a moved file: measured later only if it is not.
                    let pending_size = kind == MediaKind::Image && known.has_same(e.size, e.modified_ms);
                    let (media, dims_failed) = self.new_media(&e, kind, added, !pending_size);
                    Found::New { media, dims_failed, pending_size }
                }
            });
        }
        Listed { result: Ok((found, subdirs)), files }
    }

    fn known_file(&self, e: &Entry, kind: MediaKind, added: Option<i64>, idx: u32, k: &KnownFile) -> Found {
        let mut actions = Vec::new();
        if let Some(t) = added
            && t < k.added_ms
        {
            actions.push(Action::Added(k.id, t));
        }
        if k.size == e.size && k.modified_ms == e.modified_ms {
            if k.missing {
                actions.push(Action::Found(k.id));
            }
            let mut measured = false;
            if kind == MediaKind::Image && !k.has_size && k.dims_probe < DIMS_PROBE_VERSION {
                match self.probe_image(&e.path) {
                    Dims::Size(w, h) => {
                        actions.push(Action::SetSize(k.id, w, h));
                        measured = true;
                    }
                    Dims::Unreadable => actions.push(Action::DimsFailed(k.id)),
                    Dims::Failed => {}
                }
            }
            return Found::Known { idx, actions, updated: false, back: k.missing, measured };
        }
        let (media, dims_failed) = self.new_media(e, kind, added, true);
        actions.push(Action::Upsert(media, dims_failed));
        Found::Known { idx, actions, updated: true, back: k.missing, measured: false }
    }

    /// The row for a new or changed file; `measure`: read an image's pixel size now.
    fn new_media(&self, e: &Entry, kind: MediaKind, added: Option<i64>, measure: bool) -> (NewMedia, bool) {
        let mut dims_failed = false;
        let (width, height) = match kind {
            MediaKind::Image if measure => match self.probe_image(&e.path) {
                Dims::Size(w, h) => (Some(w), Some(h)),
                Dims::Unreadable => {
                    dims_failed = true;
                    (None, None)
                }
                Dims::Failed => (None, None),
            },
            // Video metadata needs the media pipeline; filled in by a later probe.
            _ => (None, None),
        };
        let media = NewMedia {
            source_id: self.source_id,
            rel_path: e.path.clone(),
            kind,
            size: e.size,
            created_ms: e.created_ms.or(e.modified_ms),
            modified_ms: e.modified_ms,
            added_ms: added.unwrap_or(self.now_ms),
            duration_ms: None,
            width,
            height,
            seen_gen: self.now_ms,
        };
        (media, dims_failed)
    }

    /// Reads an image's pixel size from its head: one read of `scan.header_probe_bytes`
    /// covers almost every file; a JPEG with more metadata than that before its frame
    /// header reads one more window where the header is.
    fn probe_image(&self, path: &str) -> Dims {
        let Ok(reader) = self.driver.open(path) else { return Dims::Failed };
        let len = reader.len();
        let window = (self.settings.header_probe_bytes.max(64) as u64).min(len) as usize;
        let mut head = vec![0u8; window];
        let Ok(n) = reader.read_full_at(0, &mut head) else { return Dims::Failed };
        head.truncate(n);
        let mut io_failed = false;
        let mut more: (u64, Vec<u8>) = (0, Vec::new());
        let mut read = |off: u64, buf: &mut [u8]| {
            let end = off + buf.len() as u64;
            for (start, data) in [(0u64, &head), (more.0, &more.1)] {
                if off >= start && end <= start + data.len() as u64 {
                    let at = (off - start) as usize;
                    buf.copy_from_slice(&data[at..at + buf.len()]);
                    return true;
                }
            }
            let mut w = vec![0u8; window.max(buf.len()).min((len.saturating_sub(off)) as usize)];
            match reader.read_full_at(off, &mut w) {
                Ok(n) if n >= buf.len() => {
                    w.truncate(n);
                    buf.copy_from_slice(&w[..buf.len()]);
                    more = (off, w);
                    true
                }
                Ok(_) => false,
                Err(_) => {
                    io_failed = true;
                    false
                }
            }
        };
        if let Some((w, h)) = jpeg_size(len, &mut read) {
            return Dims::Size(w, h);
        }
        if io_failed {
            return Dims::Failed;
        }
        match imagesize::blob_size(&head) {
            Ok(size) => match (u32::try_from(size.width), u32::try_from(size.height)) {
                (Ok(w), Ok(h)) if w > 0 && h > 0 => Dims::Size(w, h),
                _ => Dims::Unreadable,
            },
            Err(_) => Dims::Unreadable,
        }
    }

    fn flush(&self, batch: &mut Vec<Action>) -> Result<(), ScanError> {
        if batch.is_empty() {
            return Ok(());
        }
        if let Some(gate) = WRITE_GATE.get() {
            gate();
        }
        let db = self.db.lock().unwrap();
        db.batch(|db| {
            for a in batch.iter() {
                match a {
                    Action::Found(id) => db.mark_found(&[*id])?,
                    Action::SetSize(id, w, h) => db.set_dimensions(*id, *w, *h)?,
                    Action::DimsFailed(id) => db.set_dims_probe(*id, DIMS_PROBE_VERSION)?,
                    Action::Upsert(m, dims_failed) => {
                        let id = db.upsert_media(m)?;
                        if *dims_failed {
                            db.set_dims_probe(id, DIMS_PROBE_VERSION)?;
                        }
                    }
                    Action::Added(id, t) => db.set_added(*id, *t)?,
                    Action::Moved(id, m) => db.media_moved(*id, m)?,
                    Action::Gone(id) => db.mark_missing(&[*id])?,
                }
            }
            Ok(())
        })?;
        batch.clear();
        Ok(())
    }
}

/// Drops scopes covered by another one (a folder inside a tree, a tree inside a tree) and
/// duplicates, keeping the order of the rest.
fn normalize(scopes: &[Scope]) -> Vec<Scope> {
    let trees: Vec<&str> = scopes.iter().filter_map(|s| if let Scope::Tree(d) = s { Some(d.as_str()) } else { None }).collect();
    let mut out: Vec<Scope> = Vec::new();
    for s in scopes {
        let covered = match s {
            Scope::Tree(d) => trees.iter().any(|t| t != d && under(d, t)),
            Scope::Folder(d) => trees.iter().any(|t| under(d, t)),
        };
        if !covered && !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
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
    fn added_time_comes_from_the_file_system_and_only_moves_back() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.png"), png(1, 1)).unwrap();
        let born = fs::metadata(dir.path().join("a.png")).unwrap().created().ok();
        let db = Mutex::new(Db::open_in_memory().unwrap());
        let source_id = db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "t".into(), driver: "local".into(), config: serde_json::json!({}), root: String::new() })
            .unwrap();
        let driver = LocalDriver::new(dir.path());
        let settings = ScanSettings::default();
        let cancel = AtomicBool::new(false);
        let later = 4_000_000_000_000; // 2096
        let scan = |now_ms, added_times| {
            Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: false, now_ms, cancel: &cancel, added_times }
                .run(&mut |_| {})
                .unwrap();
            db.lock().unwrap().known_files(source_id).unwrap()["a.png"].added_ms
        };
        let first = scan(later, None);
        // Where the file system has a birth time, that is when the file was added.
        if born.is_some() {
            assert!(first < later);
        } else {
            assert_eq!(first, later);
        }
        let hints = HashMap::from([("a.png".to_string(), 1_000)]);
        assert_eq!(scan(later + 1, Some(&hints)), 1_000, "an earlier time is taken");
        let hints = HashMap::from([("a.png".to_string(), 5_000)]);
        assert_eq!(scan(later + 2, Some(&hints)), 1_000, "a later one is not");
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
            let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: false, now_ms, cancel: &cancel, added_times: None };
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
        let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: true, now_ms: 3, cancel: &cancel, added_times: None };
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
            let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id, include_nonmedia: false, now_ms, cancel: &cancel, added_times: None };
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
        // (An older version did not record that it failed.)
        db.lock().unwrap().conn().execute("UPDATE media SET dims_probe = 0", []).unwrap();

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
        let s = Scanner { db: &db, driver: &driver, settings: &settings, source_id: 1, include_nonmedia: false, now_ms: 1, cancel: &cancel, added_times: None };
        assert!(matches!(s.run(&mut |_| {}), Err(ScanError::Cancelled)));
    }

    // ---- helpers for the tests below

    /// A local driver that counts opens and can fail to list some folders.
    struct Probe {
        inner: LocalDriver,
        opens: std::sync::atomic::AtomicUsize,
        fail: Mutex<Vec<String>>,
    }

    impl Probe {
        fn new(root: &std::path::Path) -> Self {
            Self { inner: LocalDriver::new(root), opens: Default::default(), fail: Mutex::new(Vec::new()) }
        }
        fn opens(&self) -> usize {
            self.opens.load(Ordering::SeqCst)
        }
    }

    impl MediaDriver for Probe {
        fn kind(&self) -> &'static str {
            "local"
        }
        fn capabilities(&self) -> vp_driver::Capabilities {
            self.inner.capabilities()
        }
        fn list(&self, dir: &str) -> vp_driver::Result<Vec<Entry>> {
            if self.fail.lock().unwrap().iter().any(|d| d == dir) {
                return Err(DriverError::Io(std::io::Error::other("unreadable")));
            }
            self.inner.list(dir)
        }
        fn stat(&self, path: &str) -> vp_driver::Result<Entry> {
            self.inner.stat(path)
        }
        fn open(&self, path: &str) -> vp_driver::Result<Box<dyn vp_driver::MediaReader>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            self.inner.open(path)
        }
    }

    struct Lib {
        db: Mutex<Db>,
        source_id: i64,
        cancel: AtomicBool,
    }

    impl Lib {
        fn new(root: &std::path::Path) -> Self {
            let db = Db::open_in_memory().unwrap();
            let source_id = db
                .add_source(&NewSource { name: "t".into(), driver: "local".into(), config: serde_json::json!({}), root: root.display().to_string() })
                .unwrap();
            Self { db: Mutex::new(db), source_id, cancel: AtomicBool::new(false) }
        }

        fn scanner<'a>(&'a self, driver: &'a dyn MediaDriver, settings: &'a ScanSettings, now_ms: i64) -> Scanner<'a> {
            Scanner { db: &self.db, driver, settings, source_id: self.source_id, include_nonmedia: false, now_ms, cancel: &self.cancel, added_times: None }
        }

        fn scan(&self, driver: &dyn MediaDriver, settings: &ScanSettings, now_ms: i64) -> ScanProgress {
            self.scanner(driver, settings, now_ms).run(&mut |_| {}).unwrap()
        }

        /// (path, id, missing) of every item, by path.
        fn rows(&self) -> Vec<(String, i64, bool)> {
            let db = self.db.lock().unwrap();
            let mut stmt = db.conn().prepare("SELECT rel_path, id, missing FROM media WHERE source_id = ?1 ORDER BY rel_path").unwrap();
            stmt.query_map([self.source_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect()
        }

        fn id(&self, path: &str) -> i64 {
            self.rows().into_iter().find(|r| r.0 == path).unwrap().1
        }

        fn changes(&self) -> u64 {
            self.db.lock().unwrap().conn().total_changes()
        }
    }

    fn touch(path: &std::path::Path, data: &[u8]) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, data).unwrap();
    }

    #[test]
    fn unchanged_rescan_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..50 {
            touch(&dir.path().join(format!("f{}/p{i}.png", i % 5)), &png(i + 1, 2));
        }
        let lib = Lib::new(dir.path());
        let driver = Probe::new(dir.path());
        let settings = ScanSettings::default();
        let p = lib.scan(&driver, &settings, 1);
        assert_eq!((p.added, p.missing), (50, Some(0)));
        let (opens, writes) = (driver.opens(), lib.changes());
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!((p.unchanged, p.added, p.updated, p.gone), (50, 0, 0, 0));
        assert!(!p.any_change());
        assert_eq!(lib.changes(), writes, "no row written");
        assert_eq!(driver.opens(), opens, "no file read");
    }

    #[test]
    fn parallel_and_serial_scans_agree() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300u32 {
            touch(&dir.path().join(format!("d{}/s{}/img{i:03}.png", i % 7, i % 3)), &png(i + 1, i + 2));
        }
        touch(&dir.path().join("top.png"), &png(5, 5));
        let rows = |workers: u32, lookahead: u32| {
            let lib = Lib::new(dir.path());
            let driver = LocalDriver::new(dir.path());
            let settings = ScanSettings { list_workers: workers, list_lookahead: lookahead, batch_size: 7, ..Default::default() };
            let p = lib.scan(&driver, &settings, 1);
            assert_eq!(p.added, 301);
            let db = lib.db.lock().unwrap();
            let mut stmt = db.conn().prepare("SELECT rel_path, width, height FROM media ORDER BY id").unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<u32>>(1)?, r.get::<_, Option<u32>>(2)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect::<Vec<_>>()
        };
        let serial = rows(1, 1);
        // Ids follow the walk: files of a folder by name, then its subfolders by name.
        assert_eq!(serial[0].0, "top.png");
        assert_eq!(serial[1].0, "d0/s0/img000.png");
        assert!(serial.iter().all(|r| r.1.is_some()));
        assert_eq!(rows(4, 64), serial);
        assert_eq!(rows(8, 2), serial);
    }

    #[test]
    fn moved_and_renamed_files_keep_their_items() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a/one.png"), &png(1, 1));
        touch(&root.join("a/two.png"), &png(2, 2));
        touch(&root.join("a/three.png"), &png(3, 3));
        let lib = Lib::new(root);
        let driver = LocalDriver::new(root);
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        let (one, two, three) = (lib.id("a/one.png"), lib.id("a/two.png"), lib.id("a/three.png"));
        lib.db.lock().unwrap().add_value_many(&[one], "x.tags", &vp_db::ExtValue::Text("kept".into()), "user").unwrap();

        fs::create_dir_all(root.join("b/deep")).unwrap();
        fs::rename(root.join("a/one.png"), root.join("b/deep/one.png")).unwrap();
        fs::rename(root.join("a/two.png"), root.join("a/renamed.png")).unwrap();
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!((p.moved, p.added, p.gone, p.missing), (2, 0, 0, Some(0)));
        assert_eq!(lib.id("b/deep/one.png"), one);
        assert_eq!(lib.id("a/renamed.png"), two);
        assert_eq!(lib.id("a/three.png"), three);
        assert_eq!(lib.rows().len(), 3);
        let d = lib.db.lock().unwrap().media_detail(one).unwrap().unwrap();
        assert_eq!(d.values["x.tags"], vec![vp_db::ExtValue::Text("kept".into())]);
        assert!(p.changed.contains(&one) && p.changed.contains(&two));
    }

    #[test]
    fn a_file_back_after_going_missing_is_moved_back() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("x/p.png"), &png(4, 4));
        let lib = Lib::new(root);
        let driver = LocalDriver::new(root);
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        let id = lib.id("x/p.png");
        let held = root.parent().unwrap().join(format!("held-{}", std::process::id()));
        fs::rename(root.join("x/p.png"), &held).unwrap();
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!((p.gone, p.missing), (1, Some(1)));
        fs::create_dir_all(root.join("y")).unwrap();
        fs::rename(&held, root.join("y/p.png")).unwrap();
        let p = lib.scan(&driver, &settings, 3);
        assert_eq!((p.moved, p.added, p.missing), (1, 0, Some(0)));
        assert_eq!(lib.rows(), vec![("y/p.png".to_string(), id, false)]);
    }

    #[test]
    fn ambiguous_moves_are_not_guessed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mtime = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for name in ["a/x.png", "a/y.png", "a/w.png"] {
            touch(&root.join(name), &png(9, 9));
            fs::File::options().write(true).open(root.join(name)).unwrap().set_modified(mtime).unwrap();
        }
        let lib = Lib::new(root);
        let driver = LocalDriver::new(root);
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        let x = lib.id("a/x.png");
        // All three gone, three new files with their size and time: x keeps its name; the
        // other two could each be either of y and w.
        fs::create_dir_all(root.join("b")).unwrap();
        fs::rename(root.join("a/x.png"), root.join("b/x.png")).unwrap();
        fs::rename(root.join("a/y.png"), root.join("b/z.png")).unwrap();
        fs::rename(root.join("a/w.png"), root.join("b/v.png")).unwrap();
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!(lib.id("b/x.png"), x, "the one with the same name");
        assert_eq!((p.moved, p.added, p.gone), (1, 2, 2));
        let missing: Vec<String> = lib.rows().into_iter().filter(|r| r.2).map(|r| r.0).collect();
        assert_eq!(missing, ["a/w.png", "a/y.png"]);

        // Once only one could match, it is taken as moved.
        let lib = Lib::new(root);
        fs::rename(root.join("b/z.png"), root.join("a/y.png")).unwrap();
        fs::remove_file(root.join("b/v.png")).unwrap();
        lib.scan(&driver, &settings, 3);
        let y = lib.id("a/y.png");
        fs::rename(root.join("a/y.png"), root.join("b/renamed.png")).unwrap();
        let p = lib.scan(&driver, &settings, 4);
        assert_eq!((p.moved, lib.id("b/renamed.png")), (1, y));
    }

    #[test]
    fn unreadable_folders_keep_their_items() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("ok/a.png"), &png(1, 1));
        touch(&root.join("bad/b.png"), &png(1, 1));
        touch(&root.join("bad/inner/c.png"), &png(1, 1));
        let lib = Lib::new(root);
        let driver = Probe::new(root);
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        driver.fail.lock().unwrap().push("bad".into());
        fs::remove_file(root.join("ok/a.png")).unwrap();
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!(p.errors.len(), 1);
        assert_eq!((p.gone, p.missing), (1, Some(1)));
        assert!(lib.rows().iter().filter(|r| r.0.starts_with("bad/")).all(|r| !r.2), "kept under the unreadable folder");
    }

    #[test]
    fn scoped_scans_touch_only_their_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a/1.png"), &png(1, 1));
        touch(&root.join("a/sub/2.png"), &png(2, 2));
        touch(&root.join("b/3.png"), &png(3, 3));
        let lib = Lib::new(root);
        let driver = LocalDriver::new(root);
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);

        // Changes everywhere; a scan of folder "a" (not below it) sees only a's files.
        fs::remove_file(root.join("a/1.png")).unwrap();
        touch(&root.join("a/new.png"), &png(7, 7));
        fs::remove_file(root.join("a/sub/2.png")).unwrap();
        fs::remove_file(root.join("b/3.png")).unwrap();
        let p = lib.scanner(&driver, &settings, 2).run_scoped(&[Scope::Folder("a".into())], &mut |_| {}).unwrap();
        assert!(p.partial);
        assert_eq!((p.added, p.gone), (1, 1));
        let missing: Vec<String> = lib.rows().into_iter().filter(|r| r.2).map(|r| r.0).collect();
        assert_eq!(missing, ["a/1.png"]);

        // A tree: everything below it; a folder that is gone: everything that was in it.
        fs::remove_dir_all(root.join("b")).unwrap();
        let p = lib
            .scanner(&driver, &settings, 3)
            .run_scoped(&[Scope::Tree("a".into()), Scope::Folder("a/sub".into()), Scope::Tree("b".into())], &mut |_| {})
            .unwrap();
        assert_eq!(p.gone, 2);
        let missing: Vec<String> = lib.rows().into_iter().filter(|r| r.2).map(|r| r.0).collect();
        assert_eq!(missing, ["a/1.png", "a/sub/2.png", "b/3.png"]);
        assert_eq!(p.missing, Some(3));
    }

    #[test]
    fn unreadable_sizes_are_not_read_again() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("odd.png"), b"not a picture at all");
        touch(&dir.path().join("ok.png"), &png(3, 4));
        let lib = Lib::new(dir.path());
        let driver = Probe::new(dir.path());
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        assert_eq!(driver.opens(), 2);
        lib.scan(&driver, &settings, 2);
        lib.scan(&driver, &settings, 3);
        assert_eq!(driver.opens(), 2, "neither file is read again");
        // Changed: read again.
        touch(&dir.path().join("odd.png"), &png(8, 8));
        let p = lib.scan(&driver, &settings, 4);
        assert_eq!((p.updated, driver.opens()), (1, 3));
    }

    #[test]
    fn a_missing_file_back_unchanged_is_found() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("p.png"), &png(1, 1));
        let lib = Lib::new(dir.path());
        let driver = LocalDriver::new(dir.path());
        let settings = ScanSettings::default();
        lib.scan(&driver, &settings, 1);
        let id = lib.id("p.png");
        lib.db.lock().unwrap().mark_missing(&[id]).unwrap();
        let p = lib.scan(&driver, &settings, 2);
        assert_eq!((p.back, p.missing, p.changed.clone()), (1, Some(0), vec![id]));
        assert!(!lib.rows()[0].2);
    }

    #[test]
    fn jpeg_size_reads_past_the_head() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("cam.jpg"), &camera_jpeg(5000, 4000));
        let lib = Lib::new(dir.path());
        let driver = LocalDriver::new(dir.path());
        // A head smaller than the EXIF block before the frame header.
        let settings = ScanSettings { header_probe_bytes: 4096, ..Default::default() };
        lib.scan(&driver, &settings, 1);
        let id = lib.id("cam.jpg");
        let b = lib.db.lock().unwrap().media_brief(&[id]).unwrap().pop().unwrap();
        assert_eq!((b.width, b.height), (Some(5000), Some(4000)));
    }
}
