//! Follows changes in local source folders as they happen (`scan.watch`).
//!
//! Each enabled local source (and the bundle's own folders) gets a recursive watch of its
//! root. Changes are collected until the folder has been quiet for
//! `scan.watch_debounce_ms` (or `scan.watch_max_delay_ms` has passed), then only the folders
//! they touched are looked at (`commands::scan_scoped`): a changed or new file looks at its
//! folder, a new, renamed or removed folder at everything below it. When the system lost
//! track of changes (an event overflow), the whole source is scanned.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use tauri::AppHandle;
use vp_db::SourceRow;
use vp_scan::Scope;

use crate::workspace::Workspace;

/// A running watch; dropping it stops it.
pub(crate) struct Watch {
    root: PathBuf,
    stop: Arc<AtomicBool>,
    #[cfg(not(target_os = "ios"))]
    _watcher: notify::RecommendedWatcher,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The folder a source's files are in, when it is local.
fn local_root(ws: &Workspace, source: &SourceRow) -> Option<PathBuf> {
    let root = if source.driver == crate::workspace::BUNDLE_DRIVER {
        let mut p = ws.bundle.root.clone();
        p.extend(vp_driver::checked_components(&source.root).ok()?);
        p
    } else if source.driver == "local" {
        PathBuf::from(&source.root)
    } else {
        return None;
    };
    root.is_dir().then(|| std::fs::canonicalize(&root).unwrap_or(root))
}

/// Starts watches for the sources that should have one and stops the others.
pub(crate) fn sync(app: &AppHandle, ws: &Arc<Workspace>) {
    let on = ws.settings.read().unwrap().scan.watch && ws.autosave() && !ws.closed.load(Ordering::Relaxed);
    let sources = if on { ws.read_db.lock().unwrap().sources().unwrap_or_default() } else { Vec::new() };
    let wanted: Vec<(i64, PathBuf)> =
        sources.iter().filter(|s| s.enabled).filter_map(|s| Some((s.id, local_root(ws, s)?))).collect();
    let mut watches = ws.watches.lock().unwrap();
    watches.retain(|id, w| wanted.iter().any(|(i, root)| i == id && *root == w.root));
    for (id, root) in wanted {
        if watches.contains_key(&id) {
            continue;
        }
        match start(app, ws, id, &root) {
            Ok(w) => {
                watches.insert(id, w);
            }
            Err(e) => crate::applog::warn("watch", format!("cannot watch '{}': {e}", root.display())),
        }
    }
}

#[cfg(target_os = "ios")]
fn start(_app: &AppHandle, _ws: &Arc<Workspace>, _id: i64, _root: &Path) -> Result<Watch, String> {
    Err("not supported here".into())
}

#[cfg(not(target_os = "ios"))]
fn start(app: &AppHandle, ws: &Arc<Workspace>, source_id: i64, root: &Path) -> Result<Watch, String> {
    use notify::Watcher;
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(tx).map_err(|e| e.to_string())?;
    watcher.watch(root, notify::RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let (app, weak, root2, stop2) = (app.clone(), Arc::downgrade(ws), root.to_path_buf(), stop.clone());
    std::thread::Builder::new()
        .name(format!("watch-{source_id}"))
        .spawn(move || follow(&app, &weak, source_id, &root2, &rx, &stop2))
        .map_err(|e| e.to_string())?;
    Ok(Watch { root: root.to_path_buf(), stop, _watcher: watcher })
}

/// What a batch of changes asks to look at.
#[derive(Default)]
struct Pending {
    scopes: Vec<Scope>,
    seen: HashSet<Scope>,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Pending {
    fn add(&mut self, scope: Scope) {
        let now = Instant::now();
        self.first.get_or_insert(now);
        self.last = Some(now);
        if self.seen.insert(scope.clone()) {
            self.scopes.push(scope);
        }
    }
}

#[cfg(not(target_os = "ios"))]
fn follow(
    app: &AppHandle,
    weak: &Weak<Workspace>,
    source_id: i64,
    root: &Path,
    rx: &mpsc::Receiver<notify::Result<notify::Event>>,
    stop: &AtomicBool,
) {
    let mut pending = Pending::default();
    let poll = Duration::from_millis(250);
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // The workspace is held only while handling a change, so it can close in between.
        let Some(ws) = upgrade(weak) else { return };
        let (debounce, max_delay) = {
            let s = &ws.settings.read().unwrap().scan;
            (Duration::from_millis(s.watch_debounce_ms), Duration::from_millis(s.watch_max_delay_ms.max(s.watch_debounce_ms)))
        };
        drop(ws);
        match rx.recv_timeout(poll) {
            Ok(Ok(event)) => {
                let Some(ws) = upgrade(weak) else { return };
                let settings = ws.settings.read().unwrap().scan.clone();
                let known_folder = |rel: &str| ws.read_db.lock().unwrap().has_folder(source_id, rel).unwrap_or(false);
                for scope in scopes_for(&settings, root, &event, &known_folder) {
                    pending.add(scope);
                }
                // On to the check below: a steady stream of changes still gets taken in
                // after `watch_max_delay_ms`.
            }
            Ok(Err(e)) => crate::applog::warn("watch", format!("{}: {e}", root.display())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let (Some(first), Some(last)) = (pending.first, pending.last) else { continue };
        if last.elapsed() < debounce && first.elapsed() < max_delay {
            continue;
        }
        let Some(ws) = upgrade(weak) else { return };
        let Ok(Some(source)) = ws.read_db.lock().unwrap().source(source_id) else { return };
        let scopes = std::mem::take(&mut pending.scopes);
        match crate::commands::scan_scoped(app, ws.clone(), source, scopes.clone()) {
            Ok(_) => pending = Pending::default(),
            // A scan of the source is running: try again once it may be done.
            Err(e) if e == "busy" => {
                pending.scopes = scopes;
                pending.first = Some(Instant::now());
                pending.last = Some(Instant::now());
            }
            Err(e) => {
                crate::applog::warn("watch", format!("{}: {e}", root.display()));
                pending = Pending::default();
            }
        }
    }
}

fn upgrade(weak: &Weak<Workspace>) -> Option<Arc<Workspace>> {
    weak.upgrade().filter(|ws| !ws.closed.load(Ordering::Relaxed))
}

/// Where a change happened, relative to the source root ('/'-separated), unless it is
/// outside it or in a hidden folder the scan skips.
fn relative(root: &Path, path: &Path, skip_hidden: bool) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for c in rel.components() {
        let s = c.as_os_str().to_str()?;
        if skip_hidden && s.starts_with('.') {
            return None;
        }
        parts.push(s);
    }
    Some(parts.join("/"))
}

/// The parts of a source a change asks to look at. `known_folder(rel)`: the library has
/// items in that folder or below it.
#[cfg(not(target_os = "ios"))]
fn scopes_for(
    settings: &vp_core::settings::ScanSettings,
    root: &Path,
    event: &notify::Event,
    known_folder: &dyn Fn(&str) -> bool,
) -> Vec<Scope> {
    use notify::EventKind;
    use notify::event::{CreateKind, ModifyKind, RemoveKind};
    if event.need_rescan() {
        return vec![Scope::Tree(String::new())];
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return Vec::new();
    }
    let classifier = vp_scan::Classifier::new(settings);
    let structural = matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder | CreateKind::Any | CreateKind::Other)
            | EventKind::Modify(ModifyKind::Name(_) | ModifyKind::Any | ModifyKind::Other)
            | EventKind::Remove(RemoveKind::Folder | RemoveKind::Any | RemoveKind::Other)
            | EventKind::Any
            | EventKind::Other
    );
    let mut out = Vec::new();
    for path in &event.paths {
        // Canonical paths: the root was canonicalized too (macOS /var → /private/var).
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| {
            // Gone: canonicalize what is left of it.
            match (path.parent().and_then(|p| std::fs::canonicalize(p).ok()), path.file_name()) {
                (Some(p), Some(n)) => p.join(n),
                _ => path.clone(),
            }
        });
        let Some(rel) = relative(root, &path, settings.skip_hidden) else { continue };
        let parent = rel.rsplit_once('/').map_or("", |(p, _)| p).to_string();
        let is_media = classifier.kind(&rel).is_some() || settings.include_nonmedia;
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => {
                // A folder that appeared or was renamed: everything in it. (A folder whose
                // contents changed reports those files itself.)
                if structural {
                    out.push(Scope::Tree(rel));
                }
            }
            Ok(_) => {
                if is_media {
                    out.push(Scope::Folder(parent));
                }
            }
            Err(_) => {
                if is_media {
                    out.push(Scope::Folder(parent));
                } else if structural && known_folder(&rel) {
                    // A folder the library has items in went away.
                    out.push(Scope::Tree(rel));
                }
            }
        }
    }
    out
}

#[cfg(all(test, not(target_os = "ios")))]
mod tests {
    use super::*;
    use notify::Watcher;

    /// Real file system events, as this OS reports them, turned into scopes.
    #[test]
    fn changes_become_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("album/old")).unwrap();
        std::fs::write(root.join("album/a.jpg"), b"1").unwrap();
        std::fs::write(root.join("album/old/b.jpg"), b"2").unwrap();
        std::thread::sleep(Duration::from_millis(300));

        let (tx, rx) = mpsc::channel();
        let mut w = notify::recommended_watcher(tx).unwrap();
        w.watch(&root, notify::RecursiveMode::Recursive).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let settings = vp_core::settings::ScanSettings::default();
        let known = |rel: &str| rel == "album/old" || rel == "album";
        let collect = |rx: &mpsc::Receiver<notify::Result<notify::Event>>| {
            let mut out: HashSet<Scope> = HashSet::new();
            let end = Instant::now() + Duration::from_millis(1500);
            while let Ok(e) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                out.extend(scopes_for(&settings, &root, &e.unwrap(), &known));
            }
            out
        };
        let folder = |d: &str| Scope::Folder(d.into());
        let tree = |d: &str| Scope::Tree(d.into());

        // A new picture, a changed one, a file that is not media.
        std::fs::write(root.join("album/new.jpg"), b"3").unwrap();
        std::fs::write(root.join("album/a.jpg"), b"11").unwrap();
        std::fs::write(root.join("album/notes.txt"), b"x").unwrap();
        let got = collect(&rx);
        assert!(got.contains(&folder("album")), "{got:?}");
        assert!(got.iter().all(|s| *s == folder("album")), "nothing else: {got:?}");

        // A folder moved in with pictures, a picture renamed, a known folder removed.
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(outside.path().join("trip")).unwrap();
        std::fs::write(outside.path().join("trip/c.jpg"), b"4").unwrap();
        std::fs::rename(outside.path().join("trip"), root.join("trip")).unwrap();
        std::fs::rename(root.join("album/new.jpg"), root.join("album/renamed.jpg")).unwrap();
        std::fs::remove_dir_all(root.join("album/old")).unwrap();
        let got = collect(&rx);
        assert!(got.contains(&tree("trip")), "{got:?}");
        assert!(got.contains(&folder("album")), "{got:?}");
        assert!(got.contains(&tree("album/old")) || got.contains(&folder("album/old")), "{got:?}");

        // Hidden folders are left alone.
        std::fs::create_dir_all(root.join(".cache")).unwrap();
        std::fs::write(root.join(".cache/x.jpg"), b"5").unwrap();
        let got = collect(&rx);
        assert!(got.is_empty(), "{got:?}");
    }
}
