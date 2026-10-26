//! Tauri commands called by the UI.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use vp_core::settings::Settings;
use vp_db::{Db, MediaBrief, NewSource, SourceRow};
use vp_query::{FieldRegistry, QueryError, ast::Span};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::recent;
use crate::workspace::{BundleSummary, Workspace, now_ms};

fn workspace(state: &AppState) -> CmdResult<Arc<Workspace>> {
    state.workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new("internal", e))?
}

// ---------------------------------------------------------------- bundles

/// Closes a workspace: stops its background work, writes what is buffered, and waits
/// (briefly) for other holders to let go so its files are closed. Returns false if
/// something still holds it after the wait.
fn retire(ws: Arc<Workspace>) -> bool {
    ws.closed.store(true, Ordering::Relaxed);
    for cancel in ws.scans.lock().unwrap().values() {
        cancel.store(true, Ordering::Relaxed);
    }
    for cancel in ws.converts.lock().unwrap().values() {
        cancel.store(true, Ordering::Relaxed);
    }
    crate::shard_props::flush_all(&ws);
    ws.flush();
    for _ in 0..100 {
        if Arc::strong_count(&ws) == 1 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

fn install(app: &AppHandle, state: &AppState, ws: Workspace) -> CmdResult<BundleSummary> {
    let summary = ws.summary();
    let ws = Arc::new(ws);
    ws.start_background(state.drivers.clone());
    if ws.autosave() {
        crate::video::spawn_probe_backfill(app, ws.clone());
        crate::shard_props::start(ws.clone());
        crate::mirror::start(app, ws.clone());
    }
    let old = state.workspace.write().unwrap().replace(ws);
    // Extra windows belong to the previous bundle.
    crate::windows::close_secondary(app);
    if let Some(old) = old {
        std::thread::spawn(move || retire(old));
    }
    recent::push(app, &summary.path);
    if let Some(ws) = state.workspace.read().unwrap().clone()
        && ws.autosave()
        && ws.settings.read().unwrap().scan.rescan_on_open
    {
        rescan_all(app, ws);
    }
    Ok(summary)
}

/// Scans every enabled, connected source one after another (`scan.rescan_on_open`).
fn rescan_all(app: &AppHandle, ws: Arc<Workspace>) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let offline = ws.offline_sources();
        let sources = ws.read_db.lock().unwrap().sources().unwrap_or_default();
        for source in sources.into_iter().filter(|s| s.enabled && !offline.contains(&s.id)) {
            if ws.closed.load(Ordering::Relaxed) {
                return;
            }
            let _ = scan_now(&app, ws.clone(), source);
        }
    });
}

#[tauri::command]
pub async fn create_bundle(app: AppHandle, path: String, name: String) -> CmdResult<BundleSummary> {
    let mut path = PathBuf::from(path);
    if path.extension().is_none_or(|e| e != vp_bundle::EXTENSION) {
        path.set_extension(vp_bundle::EXTENSION);
    }
    let ws = blocking(move || Workspace::create(&path, &name)).await?;
    install(&app, &app.state::<AppState>(), ws)
}

/// A bundle given as an archive is extracted next to it first; returns the folder to open.
fn resolve_bundle_path(path: &str) -> CmdResult<PathBuf> {
    let p = PathBuf::from(path);
    if p.is_file() { crate::archive::extract_bundle(&p) } else { Ok(p) }
}

/// Opens a bundle folder or archive. Fails with `bundle_too_new` for a bundle newer than
/// this build unless `allow_newer` (then autosave is off), and with `locked` for a locked
/// bundle (see `unlock_bundle`). Error details carry the folder actually used.
#[tauri::command]
pub async fn open_bundle(app: AppHandle, path: String, allow_newer: bool) -> CmdResult<BundleSummary> {
    let ws = blocking(move || {
        let dir = resolve_bundle_path(&path)?;
        Workspace::open(&dir, allow_newer).map_err(|e| e.with_path(&dir))
    })
    .await?;
    install(&app, &app.state::<AppState>(), ws)
}

/// Opens a locked bundle with its password (and key file, if it was locked with one).
#[tauri::command]
pub async fn unlock_bundle(
    app: AppHandle,
    path: String,
    password: String,
    keyfile: Option<String>,
    allow_newer: bool,
) -> CmdResult<BundleSummary> {
    let ws = blocking(move || {
        let kf = keyfile.filter(|k| !k.is_empty()).map(std::fs::read).transpose()?;
        Workspace::open_locked(&PathBuf::from(path), allow_newer, &password, kf.as_deref())
    })
    .await?;
    install(&app, &app.state::<AppState>(), ws)
}

#[tauri::command]
pub async fn close_bundle(app: AppHandle) {
    crate::windows::close_secondary(&app);
    let ws = app.state::<AppState>().workspace.write().unwrap().take();
    if let Some(ws) = ws {
        let _ = tauri::async_runtime::spawn_blocking(move || retire(ws)).await;
    }
}

/// Encrypts the open bundle with a password (and optional key file), then reopens it.
#[tauri::command]
pub async fn set_lock(app: AppHandle, password: String, keyfile: Option<String>) -> CmdResult<BundleSummary> {
    let state = app.state::<AppState>();
    let ws = workspace(&state)?;
    let root = ws.bundle.root.clone();
    let (ws, key) = blocking(move || {
        let kf = keyfile.filter(|k| !k.is_empty()).map(std::fs::read).transpose()?;
        let key = ws.enable_lock(&password, kf.as_deref())?;
        Ok((ws, key))
    })
    .await?;
    state.workspace.write().unwrap().take();
    let next = blocking(move || {
        if !retire(ws) {
            return Err(CmdError::new("busy", "the bundle is still in use; plaintext files were kept"));
        }
        crate::workspace::remove_files(&root, true)?;
        Workspace::open_with_key(&root, key)
    })
    .await?;
    install(&app, &state, next)
}

/// Removes encryption from the open (locked) bundle, then reopens it.
#[tauri::command]
pub async fn remove_lock(app: AppHandle) -> CmdResult<BundleSummary> {
    let state = app.state::<AppState>();
    let ws = workspace(&state)?;
    let root = ws.bundle.root.clone();
    let ws = blocking(move || {
        ws.disable_lock()?;
        Ok(ws)
    })
    .await?;
    state.workspace.write().unwrap().take();
    let next = blocking(move || {
        if !retire(ws) {
            return Err(CmdError::new("busy", "the bundle is still in use; encrypted files were kept"));
        }
        crate::workspace::remove_files(&root, false)?;
        Workspace::open(&root, false)
    })
    .await?;
    install(&app, &state, next)
}

/// Saves a bundle opened in manual-save mode. Does nothing in autosave mode.
#[tauri::command]
pub async fn save_bundle(app: AppHandle) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || ws.save()).await
}

#[tauri::command]
pub fn current_bundle(state: State<'_, AppState>) -> Option<BundleSummary> {
    state.workspace.read().unwrap().as_ref().map(|w| w.summary())
}

#[derive(Serialize)]
pub struct RecentBundle {
    pub path: String,
    /// Whether the bundle folder (or archive) is still there.
    pub exists: bool,
}

/// Recently opened bundles, newest first. Existence is checked off the UI thread because
/// a path on a disconnected drive can take a while to answer.
#[tauri::command]
pub async fn recent_bundles(app: AppHandle) -> Vec<RecentBundle> {
    tauri::async_runtime::spawn_blocking(move || {
        recent::list(&app).into_iter().map(|path| RecentBundle { exists: Path::new(&path).exists(), path }).collect()
    })
    .await
    .unwrap_or_default()
}

#[tauri::command]
pub fn forget_recent_bundle(app: AppHandle, path: String) {
    recent::remove(&app, &path);
}

/// Where a bundle was last browsed on this device (opaque UI state).
#[tauri::command]
pub fn last_view(app: AppHandle, path: String) -> Option<serde_json::Value> {
    recent::last_view(&app, &path)
}

#[tauri::command]
pub fn save_last_view(app: AppHandle, path: String, view: serde_json::Value) {
    recent::save_last_view(&app, &path, view);
}

// ---------------------------------------------------------------- settings

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    Ok(workspace(&state)?.settings.read().unwrap().clone())
}

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Settings) -> CmdResult<()> {
    workspace(&state)?.save_settings(settings)
}

#[tauri::command]
pub fn get_app_settings(app: AppHandle) -> vp_core::settings::AppSettings {
    recent::app_settings(&app)
}

#[tauri::command]
pub fn save_app_settings(app: AppHandle, mut settings: vp_core::settings::AppSettings) -> CmdResult<()> {
    // Volume and mute are live state owned by `audio`; a settings dialog opened earlier must
    // not roll them back.
    let live = crate::audio::current();
    settings.audio.volume = live.volume;
    settings.audio.muted = live.muted;
    let old_shortcut = recent::app_settings(&app).privacy.restore_shortcut;
    recent::save_app_settings(&app, &settings)?;
    if old_shortcut != settings.privacy.restore_shortcut {
        crate::windows::register_shortcut(&app, &settings.privacy.restore_shortcut);
    }
    let _ = app.emit("app-settings", ());
    Ok(())
}

// ---------------------------------------------------------------- sources

#[tauri::command]
pub fn list_sources(state: State<'_, AppState>) -> CmdResult<Vec<SourceRow>> {
    Ok(workspace(&state)?.read_db.lock().unwrap().sources()?)
}

#[tauri::command]
pub async fn add_local_source(app: AppHandle, path: String) -> CmdResult<SourceRow> {
    let state = app.state::<AppState>();
    let ws = workspace(&state)?;
    let folder = PathBuf::from(&path);
    if !folder.is_dir() {
        return Err(CmdError::new("not_found", format!("'{path}' is not a folder")));
    }
    let (driver, root) = ws.source_root_for(&folder);
    let name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
    let id = ws.write_db.lock().unwrap().add_source(&NewSource { name, driver, config: serde_json::json!({}), root })?;
    let source = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    start_scan(&app, ws, source.clone());
    Ok(source)
}

/// Removes a source. For a shard, `delete_data` also deletes the shard folder itself.
#[tauri::command]
pub async fn remove_source(app: AppHandle, id: i64, delete_data: Option<bool>) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        if let Some(cancel) = ws.scans.lock().unwrap().get(&id) {
            cancel.store(true, Ordering::Relaxed);
        }
        let source = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let shard_root = (source.driver == crate::shards::DRIVER).then(|| ws.shard_root(&source)).transpose()?;
        ws.write_db.lock().unwrap().remove_source(id)?;
        ws.shard_keys.lock().unwrap().remove(&id);
        ws.close_shard(id)?;
        if let Some(root) = shard_root.filter(|_| delete_data == Some(true))
            && vp_shard::Shard::read_info(&root).is_ok()
        {
            std::fs::remove_dir_all(&root)?;
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn scan_source(app: AppHandle, state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let ws = workspace(&state)?;
    let source = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    start_scan(&app, ws, source);
    Ok(())
}

#[tauri::command]
pub fn cancel_scan(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    if let Some(c) = workspace(&state)?.scans.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
    Ok(())
}

#[derive(Clone, Serialize)]
struct ScanEvent {
    #[serde(flatten)]
    progress: vp_scan::ScanProgress,
    done: bool,
    error: Option<String>,
}

pub(crate) fn start_scan(app: &AppHandle, ws: Arc<Workspace>, source: SourceRow) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _ = scan_now(&app, ws, source);
    });
}

/// Scans a source on this thread and returns when done (or at once if a scan of it is
/// already running).
pub(crate) fn scan_now(app: &AppHandle, ws: Arc<Workspace>, source: SourceRow) -> Result<vp_scan::ScanProgress, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut scans = ws.scans.lock().unwrap();
        if scans.contains_key(&source.id) {
            return Err("already scanning".into());
        }
        scans.insert(source.id, cancel.clone());
    }
    let registry = app.state::<AppState>().drivers.clone();
    let settings = ws.settings.read().unwrap().scan.clone();
    let task = crate::tasks::begin_with_cancel("scan", &source.name, crate::tasks::Unit::Items, None, Some(cancel.clone()));
    let interval = std::time::Duration::from_millis(crate::recent::cached_app_settings().progress.emit_interval_ms);
    let mut last = std::time::Instant::now() - interval;
    let mut emit = |progress: &vp_scan::ScanProgress, done: bool, error: Option<String>| {
        task.set(progress.files, None);
        task.detail(if progress.current.is_empty() { "/".to_string() } else { progress.current.clone() });
        // Scans report every folder; the UI only needs a few updates per second.
        if done || last.elapsed() >= interval {
            last = std::time::Instant::now();
            let _ = app.emit("scan-progress", ScanEvent { progress: progress.clone(), done, error });
        }
    };
    let result = ws.driver(&registry, &source).map_err(|e| e.message).and_then(|driver| {
        // A shard another instance writes to is open read-only here: catch up first.
        let shard = ws.shards.lock().unwrap().get(&source.id).cloned();
        if let Some(s) = shard {
            s.refresh().map_err(|e| e.to_string())?;
        }
        // Android keeps no birth times; the media store knows when files arrived.
        let added_times = if source.driver == "local" { driver.local_path("").and_then(|root| crate::platform::media_added(&root)) } else { None };
        let scanner = vp_scan::Scanner {
            db: &ws.write_db,
            driver: driver.as_ref(),
            settings: &settings,
            source_id: source.id,
            // A shard is a container: everything put into it is listed.
            include_nonmedia: settings.include_nonmedia || source.driver == crate::shards::DRIVER,
            now_ms: now_ms(),
            cancel: &cancel,
            added_times: added_times.as_ref(),
        };
        scanner.run(&mut |p| emit(p, false, None)).map_err(|e| e.to_string())
    });
    let empty = vp_scan::ScanProgress { source_id: source.id, ..Default::default() };
    match &result {
        Ok(p) => emit(p, true, None),
        Err(e) => emit(&empty, true, Some(e.clone())),
    }
    match &result {
        Ok(_) => task.finish(),
        Err(_) if cancel.load(Ordering::Relaxed) => task.cancelled(),
        Err(e) => task.fail(e.clone()),
    }
    ws.scans.lock().unwrap().remove(&source.id);
    if result.is_ok() && ws.autosave() {
        match crate::shard_props::import(&ws, &source) {
            Ok(0) => {}
            // Faces and people may have come along.
            Ok(_) => crate::ml::notify(app),
            Err(e) => crate::applog::warn("props", format!("could not take in properties of '{}': {}", source.name, e.message)),
        }
    }
    if let Ok(p) = &result
        && ws.autosave()
        && p.added + p.updated + p.missing.unwrap_or(0) > 0
    {
        crate::mirror::scanned(app, &ws, source.id);
    }
    if result.is_ok() && ws.autosave() {
        crate::video::spawn_probe_backfill(app, ws.clone());
    }
    if result.is_ok() && settings.thumbnails_after_scan && ws.autosave() {
        crate::thumbs::spawn_backfill(app.clone(), ws.clone());
    }
    if result.is_ok() && ws.autosave() && crate::recent::cached_app_settings().ml.auto_analyze {
        crate::ml::start(app, ws.clone());
    }
    result
}

// ---------------------------------------------------------------- search

#[derive(Serialize)]
pub struct SearchResult {
    pub ids: Vec<i64>,
    pub elapsed_ms: f64,
    pub sections: vp_db::Sections,
}

/// Runs a query. Empty text lists everything. `tz_offset_minutes` is the local offset
/// from UTC (JavaScript's `-getTimezoneOffset()`); query error spans are UTF-16 offsets.
#[tauri::command]
pub async fn search(
    app: AppHandle,
    query: String,
    source_id: Option<i64>,
    folder: Option<String>,
    recursive: Option<bool>,
    tz_offset_minutes: i32,
) -> CmdResult<SearchResult> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        let started = std::time::Instant::now();
        // Before taking the database lock: `offline_sources` takes it too.
        let offline = ws.offline_sources();
        let db = ws.read_db.lock().unwrap();
        let reg = db.field_registry()?;
        let mut q = if query.trim().is_empty() {
            vp_query::Query::default()
        } else {
            vp_query::parse_query(&query, &reg).map_err(|e| utf16_error(e, &query))?
        };
        if q.sorts.is_empty() {
            let default_sort = ws.settings.read().unwrap().grid.default_sort.clone();
            // An invalid default (e.g. naming a deleted field) just falls back to scan order.
            if let Ok(d) = vp_query::parse_query(&default_sort, &reg) {
                q.sorts = d.sorts;
            }
        }
        let scope =
            vp_db::Scope { source_id, folder, recursive: recursive.unwrap_or(false), exclude_sources: offline };
        let (ids, sections) = db.search_sections(&q, &reg, &Db::compile_context(now_ms(), tz_offset_minutes), &scope)?;
        Ok(SearchResult { ids, elapsed_ms: started.elapsed().as_secs_f64() * 1000.0, sections })
    })
    .await
}

fn utf16_error(mut e: QueryError, src: &str) -> QueryError {
    e.span = e.span.to_utf16(src);
    e
}

/// Sources whose storage is not connected (see [`Workspace::offline_sources`]).
#[tauri::command]
pub async fn offline_sources(app: AppHandle) -> CmdResult<Vec<i64>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.offline_sources())).await
}

/// Checks that the files of `ids` still exist (after one failed to load). Items whose file
/// is gone are marked missing, so searches leave them out until a scan finds them again.
/// Returns the ids found missing.
#[tauri::command]
pub async fn check_media(app: AppHandle, ids: Vec<i64>) -> CmdResult<Vec<i64>> {
    let ws = workspace(&app.state::<AppState>())?;
    let registry = app.state::<AppState>().drivers.clone();
    blocking(move || {
        let offline = ws.offline_sources();
        let mut gone = Vec::new();
        for id in ids {
            let Some(loc) = ws.read_db.lock().unwrap().media_location(id)? else { continue };
            if offline.contains(&loc.source_id) {
                continue;
            }
            let Some(source) = ws.read_db.lock().unwrap().source(loc.source_id)? else { continue };
            let Ok(driver) = ws.driver(&registry, &source) else { continue };
            if let Err(vp_driver::DriverError::NotFound(_)) = driver.stat(&loc.rel_path) {
                gone.push(id);
            }
        }
        if !gone.is_empty() {
            ws.write_db.lock().unwrap().mark_missing(&gone)?;
        }
        Ok(gone)
    })
    .await
}

#[tauri::command]
pub async fn media_brief(app: AppHandle, ids: Vec<i64>) -> CmdResult<Vec<MediaBrief>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.read_db.lock().unwrap().media_brief(&ids)?)).await
}

/// Result of checking query text in the editor. Spans are UTF-16 offsets for JavaScript.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum QueryCheck {
    Ok { normalized: String },
    Error { error: QueryError, span: Span },
}

#[tauri::command]
pub fn check_query(state: State<'_, AppState>, src: &str, locale: &str) -> CmdResult<QueryCheck> {
    let reg = match workspace(&state) {
        Ok(ws) => ws.read_db.lock().unwrap().field_registry()?,
        Err(_) => FieldRegistry::with_base_fields(),
    };
    Ok(match vp_query::parse_query(src, &reg) {
        Ok(q) => QueryCheck::Ok { normalized: vp_query::render(&q, &reg, locale) },
        Err(e) => {
            let span = e.span.to_utf16(src);
            QueryCheck::Error { error: e, span }
        }
    })
}

// ---------------------------------------------------------------- cache and stats

/// Loads items into the memory cache in the background, in the given order, skipping
/// ones already cached. Used for carousel look-ahead and for videos being streamed.
pub fn spawn_prefetch<R: tauri::Runtime>(app: &AppHandle<R>, ws: &Arc<Workspace>, ids: Vec<i64>) {
    let drivers = app.state::<AppState>().drivers.clone();
    for id in ids {
        if ws.cache.contains(id) {
            continue;
        }
        let (ws, drivers) = (ws.clone(), drivers.clone());
        tauri::async_runtime::spawn(async move {
            let Ok(_permit) = ws.prefetch_permits.clone().acquire_owned().await else { return };
            if ws.closed.load(Ordering::Relaxed) || ws.cache.contains(id) {
                return;
            }
            let _ = tauri::async_runtime::spawn_blocking(move || {
                crate::sched::run(crate::sched::Class::Viewer, || ws.load_into_cache(&drivers, id))
            })
            .await;
        });
    }
}

/// The viewer's look-ahead now running; a newer one stops it.
static LOOKAHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Loads the viewer's look-ahead: `ids` in the order they are wanted (the current item
/// first), each as the viewer will ask for it in a window of `screen` device pixels,
/// until the scheduler's share of the cache (`HardwareSettings::prefetch_ratio`) is
/// taken. Items already cached count against it and are marked recently used, so what
/// is evicted for the rest is what lies outside the look-ahead. Replaces the previous one.
#[tauri::command]
pub fn prefetch_plan(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>, screen: Option<crate::media::Screen>) -> CmdResult<()> {
    let ws = workspace(&state)?;
    let generation = LOOKAHEAD.fetch_add(1, Ordering::Relaxed) + 1;
    let c = ws.settings.read().unwrap().cache.clone();
    let ratio = recent::cached_app_settings().hardware.prefetch_ratio(c.prefetch_ratio);
    let budget = (ws.cache.capacity_bytes() as f64 * ratio as f64) as u64;
    let chunk = c.chunk_bytes.max(4096);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(ids)));
    let used = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let drivers = app.state::<AppState>().drivers.clone();
    for _ in 0..c.prefetch_workers.max(1) {
        let (ws, drivers, queue, used) = (ws.clone(), drivers.clone(), queue.clone(), used.clone());
        tauri::async_runtime::spawn_blocking(move || {
            loop {
                if LOOKAHEAD.load(Ordering::Relaxed) != generation || ws.closed.load(Ordering::Relaxed) {
                    return;
                }
                if used.load(Ordering::Relaxed) >= budget {
                    return;
                }
                let Some(id) = queue.lock().unwrap().pop_front() else { return };
                let held = crate::sched::run(crate::sched::Class::Viewer, || crate::media::warm(&ws, &drivers, id, screen)).unwrap_or(0);
                // Whole chunks: that is what the item takes from the cache.
                used.fetch_add(held.div_ceil(chunk) * chunk, Ordering::Relaxed);
            }
        });
    }
    Ok(())
}

/// Applies the cache size settings now: grows or shrinks the memory cache in place
/// (reserving the memory again when `cache.preallocate` is on).
#[tauri::command]
pub fn cache_resize(state: State<'_, AppState>) -> CmdResult<vp_cache::CacheStats> {
    let ws = workspace(&state)?;
    let c = ws.settings.read().unwrap().cache.clone();
    let capacity = recent::cached_app_settings().hardware.cache_bytes(c.memory_bytes);
    ws.cache.resize(capacity, c.max_item_bytes);
    if c.preallocate {
        let ws = ws.clone();
        std::thread::spawn(move || {
            let total_bytes = ws.cache.capacity_bytes();
            let task = crate::tasks::begin("reserve", "", crate::tasks::Unit::Bytes, Some(total_bytes));
            ws.cache.preallocate_with(&ws.closed, &mut |done, total| {
                if let Some(per_chunk) = total_bytes.checked_div(total) {
                    task.set(per_chunk * done, None);
                }
            });
        });
    }
    Ok(ws.cache.stats())
}

/// Empties the memory cache (items being read go when their readers finish) and,
/// unless the memory is meant to stay reserved (`cache.preallocate`), returns it.
#[tauri::command]
pub fn cache_clear(state: State<'_, AppState>) -> CmdResult<vp_cache::CacheStats> {
    let ws = workspace(&state)?;
    ws.cache.clear();
    if !ws.settings.read().unwrap().cache.preallocate {
        ws.cache.trim();
    }
    Ok(ws.cache.stats())
}

#[tauri::command]
pub fn prefetch(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<()> {
    spawn_prefetch(&app, &workspace(&state)?, ids);
    Ok(())
}

/// Records that an item was looked at for `view_ms`. Short glances are ignored.
#[tauri::command]
pub async fn record_view(app: AppHandle, id: i64, view_ms: i64) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        let stats = ws.settings.read().unwrap().stats.clone();
        if view_ms < stats.min_view_ms as i64 {
            return Ok(());
        }
        let half_life_ms = stats.half_life_days * 86_400_000.0;
        ws.write_db.lock().unwrap().record_view(id, now_ms(), view_ms, half_life_ms)?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub fn cache_stats(state: State<'_, AppState>) -> CmdResult<vp_cache::CacheStats> {
    Ok(workspace(&state)?.cache.stats())
}

// ---------------------------------------------------------------- fields and values

#[tauri::command]
pub fn list_fields(state: State<'_, AppState>) -> CmdResult<Vec<vp_db::FieldInfo>> {
    Ok(workspace(&state)?.read_db.lock().unwrap().ext_fields()?)
}

#[tauri::command]
pub fn create_field(
    state: State<'_, AppState>,
    names: Vec<(String, String)>,
    aliases: Vec<String>,
    ty: vp_query::FieldType,
) -> CmdResult<String> {
    let ws = workspace(&state)?;
    let key = ws.write_db.lock().unwrap().create_ext_field(&names, &aliases, ty)?;
    Ok(key)
}

#[tauri::command]
pub fn rename_field(
    state: State<'_, AppState>,
    key: String,
    names: Vec<(String, String)>,
    aliases: Vec<String>,
) -> CmdResult<()> {
    Ok(workspace(&state)?.write_db.lock().unwrap().rename_ext_field(&key, &names, &aliases)?)
}

#[tauri::command]
pub fn delete_field(state: State<'_, AppState>, key: String) -> CmdResult<()> {
    Ok(workspace(&state)?.write_db.lock().unwrap().delete_ext_field(&key)?)
}

#[tauri::command]
pub async fn media_detail(app: AppHandle, id: i64) -> CmdResult<Option<vp_db::MediaDetail>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.read_db.lock().unwrap().media_detail(id)?)).await
}

#[tauri::command]
pub async fn set_values(app: AppHandle, ids: Vec<i64>, key: String, values: Vec<vp_db::ExtValue>) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.write_db.lock().unwrap().set_values_many(&ids, &key, &values, "user")?)).await
}

#[tauri::command]
pub async fn add_value(app: AppHandle, ids: Vec<i64>, key: String, value: vp_db::ExtValue) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.write_db.lock().unwrap().add_value_many(&ids, &key, &value, "user")?)).await
}

#[tauri::command]
pub async fn remove_value(app: AppHandle, ids: Vec<i64>, key: String, value: vp_db::ExtValue) -> CmdResult<()> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.write_db.lock().unwrap().remove_value_many(&ids, &key, &value)?)).await
}

/// Extension field values of a folder.
#[tauri::command]
pub async fn folder_values(
    app: AppHandle,
    source_id: i64,
    path: String,
) -> CmdResult<std::collections::BTreeMap<String, Vec<vp_db::ExtValue>>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.read_db.lock().unwrap().folder_values(source_id, &path)?)).await
}

/// Changes a field of a folder; `recursive` also applies it to everything inside
/// (merging multi-valued fields, replacing single values). Returns the items changed.
#[tauri::command]
pub async fn edit_folder_values(
    app: AppHandle,
    source_id: i64,
    path: String,
    key: String,
    edit: vp_db::FolderEdit,
    recursive: bool,
) -> CmdResult<usize> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || Ok(ws.write_db.lock().unwrap().edit_folder_values(source_id, &path, &key, &edit, recursive)?)).await
}

#[tauri::command]
pub fn suggest_values(state: State<'_, AppState>, key: String, prefix: String, limit: Option<u32>) -> CmdResult<Vec<String>> {
    let limit = limit.unwrap_or(crate::recent::cached_app_settings().ui.suggest_limit).max(1) as usize;
    let ws = workspace(&state)?;
    let db = ws.read_db.lock().unwrap();
    // The `source` condition takes the names of the sources in use.
    if key == "source" {
        let prefix = prefix.to_lowercase();
        let names = db.sources()?.into_iter().filter(|s| s.enabled).map(|s| s.name).filter(|n| n.to_lowercase().starts_with(&prefix));
        return Ok(names.take(limit).collect());
    }
    Ok(db.suggest_values(&key, &prefix, limit)?)
}

/// Words the query editor offers as completions, in the given locale.
#[derive(Serialize)]
pub struct Vocabulary {
    pub fields: Vec<VocabField>,
    pub ops: Vec<VocabOp>,
    pub and: String,
    pub or: String,
    pub not: String,
    pub sort_asc: String,
    pub sort_desc: String,
    pub kinds: Vec<String>,
    /// Values of on/off fields.
    pub yes: String,
    pub no: String,
}

#[derive(Serialize)]
pub struct VocabField {
    pub key: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub ty: vp_query::FieldType,
    pub ext: bool,
}

#[derive(Serialize)]
pub struct VocabOp {
    pub op: vp_query::Op,
    pub name: String,
}

#[tauri::command]
pub fn query_vocabulary(state: State<'_, AppState>, locale: String) -> CmdResult<Vocabulary> {
    use vp_query::lang;
    let reg = match workspace(&state) {
        Ok(ws) => ws.read_db.lock().unwrap().field_registry()?,
        Err(_) => FieldRegistry::with_base_fields(),
    };
    let fields = reg
        .iter()
        .map(|d| VocabField {
            key: d.key.clone(),
            name: d.display_name(&locale).to_string(),
            aliases: d.names.iter().map(|(_, n)| n.clone()).chain(d.aliases.iter().cloned()).collect(),
            ty: d.ty,
            ext: matches!(d.source, vp_query::FieldSource::Ext { .. }),
        })
        .collect();
    let ops = vp_query::Op::ALL
        .into_iter()
        .map(|op| VocabOp { op, name: op.aliases().display(&locale).to_string() })
        .collect();
    Ok(Vocabulary {
        fields,
        ops,
        and: lang::AND.display(&locale).into(),
        or: lang::OR.display(&locale).into(),
        not: lang::NOT.display(&locale).into(),
        sort_asc: lang::SORT_ASC.display(&locale).into(),
        sort_desc: lang::SORT_DESC.display(&locale).into(),
        kinds: vp_core::MediaKind::ALL.into_iter().map(|k| lang::kind_aliases(k).display(&locale).to_string()).collect(),
        yes: lang::YES.display(&locale).into(),
        no: lang::NO.display(&locale).into(),
    })
}

// ---------------------------------------------------------------- saved queries

#[derive(Serialize)]
pub struct SavedQuery {
    pub id: i64,
    pub label: String,
    /// The query rendered in the requested locale. Fields that no longer exist are shown by key.
    pub text: String,
}

#[tauri::command]
pub fn list_saved_queries(state: State<'_, AppState>, locale: String) -> CmdResult<Vec<SavedQuery>> {
    let ws = workspace(&state)?;
    let db = ws.read_db.lock().unwrap();
    let reg = db.field_registry()?;
    Ok(db
        .saved_queries()?
        .into_iter()
        .map(|(id, label, q)| SavedQuery { id, label, text: vp_query::render(&q, &reg, &locale) })
        .collect())
}

/// Saves query text under a label. The text is stored in locale-neutral form, so it
/// shows up in whatever language the UI uses later.
#[tauri::command]
pub fn save_query(state: State<'_, AppState>, id: Option<i64>, label: String, text: String) -> CmdResult<i64> {
    let ws = workspace(&state)?;
    let db = ws.write_db.lock().unwrap();
    let reg = db.field_registry()?;
    let q = if text.trim().is_empty() {
        vp_query::Query::default()
    } else {
        vp_query::parse_query(&text, &reg).map_err(|e| utf16_error(e, &text))?
    };
    let label = label.trim();
    if label.is_empty() {
        return Err(CmdError::new("invalid", "label is empty"));
    }
    Ok(db.save_query(id, label, &q, now_ms())?)
}

#[tauri::command]
pub fn delete_saved_query(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    Ok(workspace(&state)?.write_db.lock().unwrap().delete_saved_query(id)?)
}

// ---------------------------------------------------------------- video pipeline

#[derive(Serialize)]
pub struct FfmpegStatus {
    pub available: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Without FFmpeg, the OS media framework still probes videos and grabs frames
    /// (Android); conversion stays unavailable.
    pub os_media: bool,
}

#[tauri::command]
pub async fn ffmpeg_status(app: AppHandle) -> CmdResult<FfmpegStatus> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        let ff = ws.ffmpeg();
        Ok(FfmpegStatus {
            available: ff.is_some(),
            path: ff.as_ref().map(|f| f.ffmpeg.display().to_string()),
            version: ff.and_then(|f| f.version()),
            os_media: crate::platform::media_available(),
        })
    })
    .await
}

#[tauri::command]
pub fn convert_media(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<()> {
    crate::video::spawn_convert(&app, workspace(&state)?, ids);
    Ok(())
}

#[tauri::command]
pub fn cancel_convert(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    if let Some(c) = workspace(&state)?.converts.lock().unwrap().get(&id) {
        c.store(true, Ordering::Relaxed);
    }
    Ok(())
}

/// Probed videos that need conversion to play, for the settings notice.
#[tauri::command]
pub fn incompatible_videos(state: State<'_, AppState>) -> CmdResult<Vec<i64>> {
    Ok(workspace(&state)?.read_db.lock().unwrap().incompatible_videos()?)
}

/// "Add this file to the ignore list": stop suggesting conversion for it.
#[tauri::command]
pub fn set_ignore_warning(state: State<'_, AppState>, id: i64, ignore: bool) -> CmdResult<()> {
    let ws = workspace(&state)?;
    ws.write_db.lock().unwrap().set_flag(id, vp_db::FLAG_IGNORE_PLAYBACK_WARNING, ignore)?;
    Ok(())
}

/// Probes one video now (the viewer calls this when it opens an unprobed video).
#[tauri::command]
pub async fn probe_media(app: AppHandle, id: i64) -> CmdResult<Option<vp_db::MediaBrief>> {
    let ws = workspace(&app.state::<AppState>())?;
    let drivers = app.state::<AppState>().drivers.clone();
    blocking(move || {
        ws.probe_video(&drivers, id)?;
        Ok(ws.read_db.lock().unwrap().media_brief(&[id])?.pop())
    })
    .await
}

// ---------------------------------------------------------------- loop segments

#[tauri::command]
pub fn loop_segments(state: State<'_, AppState>, id: i64) -> CmdResult<Vec<vp_db::LoopSegment>> {
    Ok(workspace(&state)?.read_db.lock().unwrap().loop_segments(id)?)
}

#[tauri::command]
pub fn save_loop_segment(state: State<'_, AppState>, segment: vp_db::LoopSegment) -> CmdResult<i64> {
    Ok(workspace(&state)?.write_db.lock().unwrap().save_loop_segment(&segment)?)
}

#[tauri::command]
pub fn delete_loop_segment(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    Ok(workspace(&state)?.write_db.lock().unwrap().delete_loop_segment(id)?)
}

// ---------------------------------------------------------------- generic sources (drivers)

#[derive(Serialize)]
pub struct DriverKind {
    pub kind: String,
    pub fields: Vec<vp_driver::ConfigField>,
}

/// Network/remote drivers and the settings each needs (the UI builds its form from this).
#[tauri::command]
pub fn driver_kinds(state: State<'_, AppState>) -> Vec<DriverKind> {
    let mut kinds: Vec<DriverKind> = state
        .drivers
        .kinds()
        .filter(|k| *k != "local" && *k != crate::shards::DRIVER)
        .filter_map(|k| state.drivers.get(k).ok().map(|f| DriverKind { kind: k.into(), fields: f.config_schema() }))
        .collect();
    kinds.sort_by(|a, b| a.kind.cmp(&b.kind));
    kinds
}

fn check_connect(drivers: &vp_driver::DriverRegistry, driver: &str, root: &str, config: &serde_json::Value) -> CmdResult<usize> {
    let d = drivers.get(driver)?.connect(root, config)?;
    Ok(d.list("")?.len())
}

/// Connects with the given settings and lists the root, without saving anything.
#[tauri::command]
pub async fn test_source(app: AppHandle, driver: String, root: String, config: serde_json::Value) -> CmdResult<usize> {
    let drivers = app.state::<AppState>().drivers.clone();
    blocking(move || check_connect(&drivers, &driver, &root, &config)).await
}

/// Adds a source for any driver after checking that it connects, then scans it.
#[tauri::command]
pub async fn add_source(
    app: AppHandle,
    driver: String,
    name: String,
    root: String,
    mut config: serde_json::Value,
) -> CmdResult<SourceRow> {
    let state = app.state::<AppState>();
    let ws = workspace(&state)?;
    let drivers = state.drivers.clone();
    let source = blocking(move || {
        check_connect(&drivers, &driver, &root, &config)?;
        let schema = drivers.get(&driver)?.config_schema();
        crate::secrets::stash(&driver, &mut config, &schema);
        let name = if name.trim().is_empty() { driver.clone() } else { name.trim().to_string() };
        let id = ws.write_db.lock().unwrap().add_source(&NewSource { name, driver, config, root })?;
        ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))
    })
    .await?;
    start_scan(&app, workspace(&state)?, source.clone());
    Ok(source)
}

/// The stored settings of a source with secrets redacted.
#[tauri::command]
pub fn source_config(state: State<'_, AppState>, id: i64) -> CmdResult<serde_json::Value> {
    let ws = workspace(&state)?;
    let source = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    let schema = state.drivers.get(&source.driver).map(|f| f.config_schema()).unwrap_or_default();
    Ok(crate::secrets::redact(&source.config, &schema))
}

/// Edits a source: rename, enable/disable, point at a moved folder or new server settings.
#[tauri::command]
pub async fn update_source(
    app: AppHandle,
    id: i64,
    name: String,
    root: String,
    config: Option<serde_json::Value>,
    enabled: bool,
) -> CmdResult<SourceRow> {
    let state = app.state::<AppState>();
    let ws = workspace(&state)?;
    let drivers = state.drivers.clone();
    blocking(move || {
        let old = ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let mut config = match (&config, old.driver.as_str()) {
            (Some(c), d) if d != "local" && d != crate::workspace::BUNDLE_DRIVER && d != crate::shards::DRIVER => {
                let schema = drivers.get(d)?.config_schema();
                let merged = crate::secrets::merge_edit(&old.config, c, &schema);
                let mut revealed = crate::secrets::reveal(d, &merged, &schema);
                if enabled {
                    check_connect(&drivers, d, &root, &revealed)?;
                }
                crate::secrets::stash(d, &mut revealed, &schema);
                revealed
            }
            _ => old.config.clone(),
        };
        if config.is_null() {
            config = serde_json::json!({});
        }
        let name = if name.trim().is_empty() { old.name.clone() } else { name.trim().to_string() };
        // A shard pointed at a new folder: kept relative when that is inside the bundle.
        let mut root = root;
        if old.driver == crate::shards::DRIVER && root != old.root && std::path::Path::new(&root).is_absolute() {
            let (r, in_bundle) = ws.shard_location(std::path::Path::new(&root));
            root = r;
            config["in_bundle"] = serde_json::Value::Bool(in_bundle);
        }
        ws.write_db.lock().unwrap().update_source(id, &name, &root, &config, enabled)?;
        ws.close_shard(id)?;
        ws.read_db.lock().unwrap().source(id)?.ok_or_else(|| CmdError::new("not_found", "source"))
    })
    .await
}

#[derive(Serialize)]
pub struct FolderEntry {
    pub name: String,
    pub count: u64,
}

#[tauri::command]
pub async fn child_folders(app: AppHandle, source_id: i64, parent: String) -> CmdResult<Vec<FolderEntry>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        // A disconnected source shows nothing, like its items.
        if ws.offline_sources().contains(&source_id) {
            return Ok(Vec::new());
        }
        let rows = ws.read_db.lock().unwrap().child_folders(source_id, &parent)?;
        Ok(rows.into_iter().map(|(name, count)| FolderEntry { name, count }).collect())
    })
    .await
}

// ---------------------------------------------------------------- background tasks

#[tauri::command]
pub fn list_tasks() -> Vec<crate::tasks::TaskInfo> {
    crate::tasks::list()
}

#[tauri::command]
pub fn cancel_task(id: u64) -> bool {
    crate::tasks::cancel(id)
}

// ---------------------------------------------------------------- export

/// Moves the file of item `id` to `new_rel` within its source and records the new path
/// (the item keeps its id, tags and stats). Refuses to replace an existing file.
fn relocate(ws: &Workspace, registry: &vp_driver::DriverRegistry, id: i64, new_rel: &str) -> CmdResult<()> {
    let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "item"))?;
    if loc.rel_path == new_rel {
        return Ok(());
    }
    let source = ws.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    let driver = ws.driver(registry, &source)?;
    // Case-only renames on case-insensitive file systems stat as the same file.
    if !loc.rel_path.eq_ignore_ascii_case(new_rel) && driver.stat(new_rel).is_ok() {
        return Err(CmdError::new("exists", new_rel));
    }
    driver.rename(&loc.rel_path, new_rel).map_err(|e| match e {
        vp_driver::DriverError::Unsupported(_) => CmdError::new("unsupported", e),
        e => CmdError::from(e),
    })?;
    ws.write_db.lock().unwrap().move_media(id, loc.source_id, new_rel)?;
    Ok(())
}

/// Renames the file of one item, in place. `name` is a file name, not a path.
#[tauri::command]
pub async fn rename_media(app: AppHandle, state: State<'_, AppState>, id: i64, name: String) -> CmdResult<()> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    let name = name.trim().to_string();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(CmdError::new("invalid_name", name));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "item"))?;
        let new_rel = match loc.rel_path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{name}"),
            None => name,
        };
        relocate(&ws, &registry, id, &new_rel)
    })
    .await
    .map_err(|e| CmdError::new("io", e))?
}

#[derive(Serialize)]
pub struct MoveReport {
    pub moved: usize,
    /// Items that could not be moved, as "name: reason".
    pub failed: Vec<String>,
}

/// Moves items into `folder` (relative to the source root, `""` = root) of their own
/// source: a folder of a local source, a share, or a shard. Names are kept.
#[tauri::command]
pub async fn move_media_to(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>, folder: String) -> CmdResult<MoveReport> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    let folder = vp_driver::checked_components(&folder)?.join("/");
    tauri::async_runtime::spawn_blocking(move || {
        let task = crate::tasks::begin("move", "", crate::tasks::Unit::Items, Some(ids.len() as u64));
        let mut report = MoveReport { moved: 0, failed: Vec::new() };
        for id in ids {
            if ws.closed.load(Ordering::Relaxed) {
                break;
            }
            let one = (|| -> CmdResult<()> {
                let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "item"))?;
                let name = crate::workspace::file_name(&loc.rel_path);
                task.detail(&name);
                let new_rel = if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") };
                relocate(&ws, &registry, id, &new_rel).map_err(|e| CmdError::new(e.code, format!("{name}: {}", e.message)))
            })();
            match one {
                Ok(()) => report.moved += 1,
                Err(e) => report.failed.push(e.message),
            }
            task.add(1);
        }
        if report.failed.is_empty() { task.finish() } else { task.fail(format!("{} files failed", report.failed.len())) }
        report
    })
    .await
    .map_err(|e| CmdError::new("io", e))
}

#[derive(Serialize)]
pub struct DeleteReport {
    pub deleted: usize,
    /// Files that could not be deleted (their items stay), as "path: reason".
    pub failed: Vec<String>,
}

/// Deletes the files of `ids` through their sources' drivers and forgets the items.
/// A file that is already gone only drops its item. Converted copies in the bundle go too.
#[tauri::command]
pub async fn delete_media(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>) -> CmdResult<DeleteReport> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let task = crate::tasks::begin("delete", "", crate::tasks::Unit::Items, Some(ids.len() as u64));
        let mut report = DeleteReport { deleted: 0, failed: Vec::new() };
        for id in ids {
            if ws.closed.load(Ordering::Relaxed) {
                break;
            }
            match delete_one(&ws, &registry, id, &task) {
                Ok(()) => report.deleted += 1,
                Err(e) => report.failed.push(e.message),
            }
            task.add(1);
        }
        if report.failed.is_empty() { task.finish() } else { task.fail(format!("{} files failed", report.failed.len())) }
        report
    })
    .await
    .map_err(|e| CmdError::new("io", e))
}

/// Deletes the file of item `id` through its source's driver and forgets the item (a
/// file that is already gone only drops the item); its converted copy goes too.
pub(crate) fn delete_one(ws: &Workspace, registry: &vp_driver::DriverRegistry, id: i64, task: &crate::tasks::Task) -> CmdResult<()> {
    let Some(loc) = ws.read_db.lock().unwrap().media_location(id)? else { return Ok(()) };
    task.detail(crate::workspace::file_name(&loc.rel_path));
    let source = ws.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    match ws.driver(registry, &source)?.delete(&loc.rel_path) {
        Ok(()) | Err(vp_driver::DriverError::NotFound(_)) => {}
        Err(e) => return Err(CmdError::new("driver", format!("{}: {e}", loc.rel_path))),
    }
    let variant = ws.read_db.lock().unwrap().variant(id)?;
    if let Some(rel) = variant.filter(|r| r != crate::shards::VARIANT_IN_SHARD) {
        if let Ok(parts) = vp_driver::checked_components(&rel) {
            let mut p = ws.bundle.root.clone();
            p.extend(parts);
            let _ = std::fs::remove_file(p);
        }
    }
    ws.write_db.lock().unwrap().delete_media(id)?;
    Ok(())
}

// ---------------------------------------------------------------- folders

/// Thumbnails standing for each folder of a source (for folder tiles), in `folders` order.
#[tauri::command]
pub async fn folder_samples(app: AppHandle, source_id: i64, folders: Vec<String>, limit: u32) -> CmdResult<Vec<Vec<i64>>> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        let db = ws.read_db.lock().unwrap();
        folders.iter().map(|f| Ok(db.folder_samples(source_id, f, limit.min(16))?)).collect()
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct FolderMoveReport {
    pub moved: usize,
    /// Items that could not be moved, as "name: reason".
    pub failed: Vec<String>,
    /// Where the folder is now.
    pub path: String,
}

/// Moves folder `from` of a source to `to` (both relative to the source root). A local
/// folder moves as a whole (files the library does not list go with it); other sources
/// move file by file. Every item keeps its id, tags and stats.
fn relocate_folder(ws: &Workspace, registry: &vp_driver::DriverRegistry, source_id: i64, from: &str, to: &str) -> CmdResult<FolderMoveReport> {
    let mut report = FolderMoveReport { moved: 0, failed: Vec::new(), path: to.to_string() };
    if from == to {
        return Ok(report);
    }
    if to.starts_with(&format!("{from}/")) {
        return Err(CmdError::new("invalid_target", to));
    }
    let source = ws.read_db.lock().unwrap().source(source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    let driver = ws.driver(registry, &source)?;
    if let (Some(src), Some(dst)) = (driver.local_path(from), driver.local_path(to)) {
        if src.is_dir() {
            // Case-only renames on case-insensitive file systems see the same folder.
            if !from.eq_ignore_ascii_case(to) && dst.exists() {
                return Err(CmdError::new("exists", to));
            }
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::rename(&src, &dst)?;
            report.moved = ws.write_db.lock().unwrap().move_folder(source_id, from, to)?;
            return Ok(report);
        }
    }
    let ids = ws.read_db.lock().unwrap().folder_media(source_id, from)?;
    let task = crate::tasks::begin("move", from, crate::tasks::Unit::Items, Some(ids.len() as u64));
    for id in ids {
        if ws.closed.load(Ordering::Relaxed) {
            break;
        }
        let one = (|| -> CmdResult<()> {
            let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "item"))?;
            let name = crate::workspace::file_name(&loc.rel_path);
            task.detail(&name);
            let new_rel = format!("{to}{}", &loc.rel_path[from.len()..]);
            // A file that is already gone only moves its record.
            if matches!(driver.stat(&loc.rel_path), Err(vp_driver::DriverError::NotFound(_))) {
                ws.write_db.lock().unwrap().move_media(id, source_id, &new_rel)?;
                return Ok(());
            }
            relocate(ws, registry, id, &new_rel).map_err(|e| CmdError::new(e.code, format!("{name}: {}", e.message)))
        })();
        match one {
            Ok(()) => report.moved += 1,
            Err(e) => report.failed.push(e.message),
        }
        task.add(1);
    }
    ws.write_db.lock().unwrap().move_folder_values(source_id, from, to)?;
    if report.failed.is_empty() { task.finish() } else { task.fail(format!("{} files failed", report.failed.len())) }
    Ok(report)
}

/// Renames a folder in place.
#[tauri::command]
pub async fn rename_folder(app: AppHandle, state: State<'_, AppState>, source_id: i64, path: String, name: String) -> CmdResult<FolderMoveReport> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    let name = name.trim().to_string();
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err(CmdError::new("invalid_name", name));
    }
    let from = vp_driver::checked_components(&path)?.join("/");
    if from.is_empty() {
        return Err(CmdError::new("invalid_target", path));
    }
    let to = match from.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{name}"),
        None => name,
    };
    blocking(move || relocate_folder(&ws, &registry, source_id, &from, &to)).await
}

/// Moves a folder into `target` (relative to the source root, `""` = root) of the same source.
#[tauri::command]
pub async fn move_folder(app: AppHandle, state: State<'_, AppState>, source_id: i64, path: String, target: String) -> CmdResult<FolderMoveReport> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    let from = vp_driver::checked_components(&path)?.join("/");
    let target = vp_driver::checked_components(&target)?.join("/");
    if from.is_empty() {
        return Err(CmdError::new("invalid_target", path));
    }
    let name = crate::workspace::file_name(&from);
    let to = if target.is_empty() { name } else { format!("{target}/{name}") };
    blocking(move || relocate_folder(&ws, &registry, source_id, &from, &to)).await
}

/// Deletes every listed file inside a folder and forgets the items. Local folders left
/// empty are removed; files the library does not list stay where they are.
#[tauri::command]
pub async fn delete_folder(app: AppHandle, state: State<'_, AppState>, source_id: i64, path: String) -> CmdResult<DeleteReport> {
    let ws = workspace(&state)?;
    let registry = app.state::<AppState>().drivers.clone();
    let folder = vp_driver::checked_components(&path)?.join("/");
    if folder.is_empty() {
        return Err(CmdError::new("invalid_target", path));
    }
    blocking(move || remove_folder(&ws, &registry, source_id, &folder)).await
}

fn remove_folder(ws: &Workspace, registry: &vp_driver::DriverRegistry, source_id: i64, folder: &str) -> CmdResult<DeleteReport> {
    let ids = ws.read_db.lock().unwrap().folder_media(source_id, folder)?;
    let task = crate::tasks::begin("delete", folder, crate::tasks::Unit::Items, Some(ids.len() as u64));
    let mut report = DeleteReport { deleted: 0, failed: Vec::new() };
    for id in ids {
        if ws.closed.load(Ordering::Relaxed) {
            break;
        }
        match delete_one(ws, registry, id, &task) {
            Ok(()) => report.deleted += 1,
            Err(e) => report.failed.push(e.message),
        }
        task.add(1);
    }
    ws.write_db.lock().unwrap().delete_folder_values(source_id, folder)?;
    let source = ws.read_db.lock().unwrap().source(source_id)?;
    if let Some(source) = source
        && let Some(dir) = ws.driver(registry, &source)?.local_path(folder)
    {
        remove_empty_dirs(&dir);
    }
    if report.failed.is_empty() { task.finish() } else { task.fail(format!("{} files failed", report.failed.len())) }
    Ok(report)
}

/// Removes `dir` and the folders under it that are (or become) empty.
fn remove_empty_dirs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            remove_empty_dirs(&e.path());
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Copies items (any kind, from any source) into a folder in the background. A name that
/// is already taken there gets a number.
#[tauri::command]
pub fn export_media(app: AppHandle, state: State<'_, AppState>, ids: Vec<i64>, dest: String) -> CmdResult<()> {
    let ws = workspace(&state)?;
    let dest = PathBuf::from(dest);
    if !dest.is_dir() {
        return Err(CmdError::new("not_found", "the destination is not a folder"));
    }
    std::thread::spawn(move || {
        let jobs = ids
            .iter()
            .filter_map(|id| ws.read_db.lock().unwrap().media_location(*id).ok().flatten())
            .map(|loc| (loc, dest.clone()))
            .collect();
        export_to(&app.state::<AppState>().drivers, &ws, "", jobs, Vec::new());
    });
    Ok(())
}

/// Exports every item inside folder `folder` of source `source_id` (any depth) into a new
/// folder of the same name in `dest`, keeping the subfolders.
#[tauri::command]
pub fn export_folder(app: AppHandle, state: State<'_, AppState>, source_id: i64, folder: String, dest: String) -> CmdResult<()> {
    let ws = workspace(&state)?;
    let dest = PathBuf::from(dest);
    if !dest.is_dir() {
        return Err(CmdError::new("not_found", "the destination is not a folder"));
    }
    std::thread::spawn(move || {
        let registry = app.state::<AppState>().drivers.clone();
        match folder_export_jobs(&ws, source_id, &folder, &dest) {
            Ok(e) => export_to(&registry, &ws, &e.top, e.jobs, e.dirs),
            Err(e) => crate::tasks::begin("export", folder, crate::tasks::Unit::Items, None).fail(e.message),
        }
    });
    Ok(())
}

/// Items to copy, each with the folder it goes to.
type ExportJobs = Vec<(vp_db::MediaLocation, PathBuf)>;
/// Folders made by an export, with the source folder each stands for (to copy its dates).
type ExportDirs = Vec<(PathBuf, i64, String)>;

struct FolderExport {
    /// The exported folder's name (the source's for its root).
    top: String,
    jobs: ExportJobs,
    dirs: ExportDirs,
}

/// What exporting `folder` into `dest` copies where, below a new folder that is not taken
/// yet.
fn folder_export_jobs(ws: &Workspace, source_id: i64, folder: &str, dest: &Path) -> CmdResult<FolderExport> {
    let folder = folder.trim_matches('/');
    let top = match folder.rsplit('/').next().filter(|s| !s.is_empty()) {
        Some(name) => name.to_string(),
        None => ws.read_db.lock().unwrap().source(source_id)?.map(|s| s.name).unwrap_or_default(),
    };
    let safe = crate::shards::safe_name(&top, "folder");
    let mut root = dest.join(&safe);
    let mut n = 2;
    while root.exists() {
        root = dest.join(format!("{safe} ({n})"));
        n += 1;
    }
    let ids = ws.read_db.lock().unwrap().folder_media(source_id, folder)?;
    let mut dirs: ExportDirs = vec![(root.clone(), source_id, folder.to_string())];
    let mut seen: HashSet<PathBuf> = HashSet::from([root.clone()]);
    let jobs = ids
        .iter()
        .filter_map(|id| ws.read_db.lock().unwrap().media_location(*id).ok().flatten())
        .map(|loc| {
            // Subfolders below `folder`, each made safe as a path component.
            let inner = loc.rel_path.strip_prefix(folder).unwrap_or(&loc.rel_path).trim_start_matches('/');
            let mut dir = root.clone();
            let mut rel = folder.to_string();
            if let Some((parents, _)) = inner.rsplit_once('/') {
                for c in parents.split('/') {
                    dir = dir.join(crate::shards::safe_name(c, "folder"));
                    rel = if rel.is_empty() { c.to_string() } else { format!("{rel}/{c}") };
                    if seen.insert(dir.clone()) {
                        dirs.push((dir.clone(), source_id, rel.clone()));
                    }
                }
            }
            (loc, dir)
        })
        .collect();
    Ok(FolderExport { top, jobs, dirs })
}

/// The modified and created dates of `path` in its source, when the driver knows them.
fn source_dates(ws: &Workspace, registry: &vp_driver::DriverRegistry, source_id: i64, path: &str) -> Option<(Option<i64>, Option<i64>)> {
    let source = ws.read_db.lock().unwrap().source(source_id).ok()??;
    let e = ws.driver(registry, &source).ok()?.stat(path).ok()?;
    Some((e.modified_ms, e.created_ms))
}

/// Sets the modified (and accessed) and, where the system keeps one, created date of a
/// file or folder.
fn set_dates(path: &Path, modified_ms: Option<i64>, created_ms: Option<i64>) -> std::io::Result<()> {
    let at = |ms: i64| u64::try_from(ms).ok().map(|ms| std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms));
    let mut times = std::fs::FileTimes::new();
    if let Some(t) = modified_ms.and_then(at) {
        times = times.set_modified(t).set_accessed(t);
    }
    #[cfg(windows)]
    if let Some(t) = created_ms.and_then(at) {
        use std::os::windows::fs::FileTimesExt;
        times = times.set_created(t);
    }
    #[cfg(target_os = "macos")]
    if let Some(t) = created_ms.and_then(at) {
        use std::os::macos::fs::FileTimesExt;
        times = times.set_created(t);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = created_ms;
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES; FILE_FLAG_BACKUP_SEMANTICS opens folders too.
        std::fs::OpenOptions::new().access_mode(0x100).custom_flags(0x0200_0000).open(path)?
    };
    #[cfg(not(windows))]
    let file = if path.is_dir() { std::fs::File::open(path)? } else { std::fs::OpenOptions::new().write(true).open(path)? };
    file.set_times(times)
}

/// Copies each item into its folder (made as needed) as task `export`; a name already
/// taken gets a number. With `export.keep_dates` the copies, and then the folders in
/// `dirs`, take the originals' dates.
fn export_to(registry: &vp_driver::DriverRegistry, ws: &Workspace, label: &str, jobs: ExportJobs, dirs: ExportDirs) {
    let keep_dates = crate::recent::cached_app_settings().export.keep_dates;
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = crate::tasks::begin_with_cancel("export", label, crate::tasks::Unit::Bytes, None, Some(cancel.clone()));
        task.set(0, Some(jobs.iter().map(|(l, _)| l.size).sum()));
        let mut failed = 0usize;
        let mut buf = vec![0u8; 1024 * 1024];
        for (loc, dir) in jobs {
            if cancel.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed) {
                return task.cancelled();
            }
            let name = crate::shards::safe_name(&crate::workspace::file_name(&loc.rel_path), "file");
            task.detail(&name);
            if std::fs::create_dir_all(&dir).is_err() {
                failed += 1;
                continue;
            }
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
                _ => (name.clone(), String::new()),
            };
            let mut target = dir.join(&name);
            let mut n = 2;
            while target.exists() {
                target = dir.join(format!("{stem} ({n}){ext}"));
                n += 1;
            }
            let one = (|| -> CmdResult<()> {
                let reader = ws.open_media(registry, &loc)?;
                let mut out = std::fs::File::create(&target)?;
                let mut off = 0u64;
                while off < reader.len() {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(CmdError::new("cancelled", "cancelled"));
                    }
                    let got = reader.read_full_at(off, &mut buf)?;
                    if got == 0 {
                        break;
                    }
                    std::io::Write::write_all(&mut out, &buf[..got])?;
                    off += got as u64;
                    task.add(got as u64);
                }
                drop(out);
                if keep_dates && let Some((modified, created)) = source_dates(ws, registry, loc.source_id, &loc.rel_path) {
                    // A date that cannot be set does not undo the copy.
                    let _ = set_dates(&target, modified, created);
                }
                Ok(())
            })();
            if let Err(e) = one {
                let _ = std::fs::remove_file(&target);
                if e.code == "cancelled" {
                    return task.cancelled();
                }
                failed += 1;
            }
        }
        // Last: copying into a folder changes its modified date.
        if keep_dates {
            for (dir, source_id, rel) in dirs {
                if dir.is_dir()
                    && let Some((modified, created)) = source_dates(ws, registry, source_id, &rel)
                {
                    let _ = set_dates(&dir, modified, created);
                }
            }
        }
        if failed > 0 { task.fail(format!("{failed} files failed")) } else { task.finish() }
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use vp_driver::DriverRegistry;

    fn jpeg() -> Vec<u8> {
        let img = image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8, y as u8, 128]));
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut out).encode_image(&img).unwrap();
        out
    }

    /// A HEIF file that did not get a thumbnail, only with `VP_TEST_HEIC=<file>`: the
    /// thumbnail and display copy paths, step by step.
    #[test]
    fn heic_from_env_gets_a_thumbnail() {
        let Some(file) = std::env::var_os("VP_TEST_HEIC") else { return };
        let bytes = std::fs::read(&file).unwrap();
        eprintln!("built-in decoder: {:?}", vp_thumb::generate(&bytes, &Default::default()).err());
        eprintln!("looks like HEIF: {}", crate::heif::looks_like_heif(&bytes));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        let media = root.join("media");
        std::fs::create_dir_all(media.join("a")).unwrap();
        std::fs::write(media.join("a/x.heic"), &bytes).unwrap();
        let registry = DriverRegistry::with_builtin();
        let (driver, rel) = ws.source_root_for(&media);
        let src = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "media".into(), driver, config: serde_json::json!({}), root: rel })
            .unwrap();
        let source = ws.read_db.lock().unwrap().source(src).unwrap().unwrap();
        let drv = ws.driver(&registry, &source).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner { db: &ws.write_db, driver: drv.as_ref(), settings: &settings, source_id: src, include_nonmedia: false, now_ms: 1, cancel: &cancel, added_times: None }
            .run(&mut |_| {})
            .unwrap();
        let id = ws.read_db.lock().unwrap().folder_media(src, "a").unwrap()[0];
        let loc = ws.read_db.lock().unwrap().media_location(id).unwrap().unwrap();
        eprintln!("kind {:?}, size {}, ffmpeg {:?}", loc.kind, loc.size, ws.ffmpeg().map(|f| f.ffmpeg));
        if let Some(ff) = ws.ffmpeg() {
            let r = ff.image(vp_media::Input::File(media.join("a/x.heic")));
            eprintln!("ffmpeg file: {:?}", r.as_ref().map(|b| b.len()).map_err(|e| e.to_string()));
            if let Ok(bmp) = r {
                eprintln!("bmp decode: {:?}", vp_thumb::generate(&bmp, &Default::default()).map(|t| (t.width, t.height)).err());
            }
            // Shares and shards hand FFmpeg a stream instead of a file.
            let reader = ws.open_media(&registry, &loc).unwrap();
            let r = ff.image(vp_media::Input::Stream(Box::new(vp_driver::ReaderStream::new(reader))));
            // Some HEIF files only open as a file (encrypted shards never give FFmpeg one).
            eprintln!("ffmpeg stream: {:?}", r.as_ref().map(|b| b.len()).map_err(|e| e.to_string()));
        }
        let t = ws.thumbnail(&registry, id);
        eprintln!("thumbnail: {:?}", t.as_ref().map(|o| o.as_ref().map(|(d, m)| (d.len(), m.clone()))).map_err(|e| e.message.clone()));
        assert!(matches!(t, Ok(Some(_))));
    }

    /// Folder rename, move and delete on a local folder; HEIC thumbnails when FFmpeg is found.
    #[test]
    fn folder_operations_and_heic() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let ws = Workspace::create(&root, "Lib").unwrap();
        let media = root.join("media");
        std::fs::create_dir_all(media.join("trip/day1/deep")).unwrap();
        std::fs::write(media.join("trip/day1/a.jpg"), jpeg()).unwrap();
        std::fs::write(media.join("trip/day1/deep/b.jpg"), jpeg()).unwrap();
        std::fs::write(media.join("trip/day1/notes.txt"), b"not listed").unwrap();
        std::fs::write(media.join("trip/c.jpg"), jpeg()).unwrap();
        let heic = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/vp-media/testdata/grid.heic")).unwrap();
        std::fs::write(media.join("trip/phone.heic"), heic).unwrap();

        let registry = DriverRegistry::with_builtin();
        let (driver, rel) = ws.source_root_for(&media);
        let src = ws
            .write_db
            .lock()
            .unwrap()
            .add_source(&NewSource { name: "media".into(), driver, config: serde_json::json!({}), root: rel })
            .unwrap();
        let source = ws.read_db.lock().unwrap().source(src).unwrap().unwrap();
        let drv = ws.driver(&registry, &source).unwrap();
        let settings = ws.settings.read().unwrap().scan.clone();
        let cancel = AtomicBool::new(false);
        vp_scan::Scanner {
            db: &ws.write_db,
            driver: drv.as_ref(),
            settings: &settings,
            source_id: src,
            include_nonmedia: false,
            now_ms: 1,
            cancel: &cancel,
            added_times: None,
        }
        .run(&mut |_| {})
        .unwrap();
        let ids = ws.read_db.lock().unwrap().folder_media(src, "trip/day1").unwrap();
        assert_eq!(ids.len(), 2);
        let tag = vp_db::FolderEdit::Add(vp_db::ExtValue::Text("trip".into()));
        ws.write_db.lock().unwrap().edit_folder_values(src, "trip/day1", "x.tags", &tag, true).unwrap();

        // HEIC (a tiled grid, like phone photos) gets a thumbnail through FFmpeg.
        if ws.ffmpeg().is_some() {
            let id = ws.read_db.lock().unwrap().folder_samples(src, "trip", 10).unwrap();
            let heic = id.into_iter().find(|i| {
                ws.read_db.lock().unwrap().media_location(*i).unwrap().unwrap().rel_path.ends_with(".heic")
            });
            let (data, mime) = ws.thumbnail(&registry, heic.expect("heic listed")).unwrap().expect("heic thumbnail");
            assert_eq!(mime, "image/jpeg");
            let t = image::load_from_memory(&data).unwrap();
            assert_eq!(t.width(), t.height(), "all four tiles");
        }

        // Export: a new folder named like the exported one, subfolders kept; again → "(2)".
        // The copies (and their folders) keep the originals' dates.
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
        set_dates(&media.join("trip/day1/a.jpg"), Some(1_500_000_000_000), Some(1_400_000_000_000)).unwrap();
        set_dates(&media.join("trip/day1/deep"), Some(1_500_000_000_000), None).unwrap();
        let out = tempfile::tempdir().unwrap();
        let e = folder_export_jobs(&ws, src, "trip", out.path()).unwrap();
        assert_eq!(e.top, "trip");
        export_to(&registry, &ws, &e.top, e.jobs, e.dirs);
        for f in ["trip/day1/a.jpg", "trip/day1/deep/b.jpg", "trip/c.jpg", "trip/phone.heic"] {
            assert!(out.path().join(f).is_file(), "{f} exported");
        }
        assert_eq!(std::fs::read(out.path().join("trip/day1/a.jpg")).unwrap(), jpeg());
        assert!(!out.path().join("trip/day1/notes.txt").exists(), "unlisted files stay");
        let copied = std::fs::metadata(out.path().join("trip/day1/a.jpg")).unwrap();
        assert_eq!(copied.modified().unwrap(), old);
        #[cfg(windows)]
        assert_eq!(copied.created().unwrap(), std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_400_000_000));
        assert_eq!(std::fs::metadata(out.path().join("trip/day1/deep")).unwrap().modified().unwrap(), old);
        let e = folder_export_jobs(&ws, src, "trip/day1", out.path()).unwrap();
        export_to(&registry, &ws, "day1", e.jobs, e.dirs);
        assert!(out.path().join("day1/deep/b.jpg").is_file());
        let e = folder_export_jobs(&ws, src, "trip/day1", out.path()).unwrap();
        export_to(&registry, &ws, "day1", e.jobs, e.dirs);
        assert!(out.path().join("day1 (2)/a.jpg").is_file());

        // Rename: the folder moves as a whole (unlisted files too); items keep their ids.
        let r = relocate_folder(&ws, &registry, src, "trip/day1", "trip/first").unwrap();
        assert_eq!(r.moved, 2);
        assert!(media.join("trip/first/notes.txt").is_file());
        assert!(!media.join("trip/day1").exists());
        let mut moved = ws.read_db.lock().unwrap().folder_media(src, "trip/first").unwrap();
        moved.sort();
        let mut before = ids.clone();
        before.sort();
        assert_eq!(moved, before);
        let deep = ws.read_db.lock().unwrap().media_location(before[1]).unwrap().unwrap();
        assert!(deep.rel_path.starts_with("trip/first/"));
        // Folder values follow, the subfolder's too.
        assert!(ws.read_db.lock().unwrap().folder_values(src, "trip/day1").unwrap().is_empty());
        assert_eq!(ws.read_db.lock().unwrap().folder_values(src, "trip/first/deep").unwrap()["x.tags"].len(), 1);

        // Never into itself; never over an existing folder.
        assert_eq!(relocate_folder(&ws, &registry, src, "trip", "trip/first/x").unwrap_err().code, "invalid_target");
        std::fs::create_dir_all(media.join("taken")).unwrap();
        assert_eq!(relocate_folder(&ws, &registry, src, "trip/first", "taken").unwrap_err().code, "exists");

        // Move to another parent.
        relocate_folder(&ws, &registry, src, "trip/first", "archive/first").unwrap();
        assert!(media.join("archive/first/deep/b.jpg").is_file());
        assert_eq!(ws.read_db.lock().unwrap().child_folders(src, "archive").unwrap(), [("first".to_string(), 2)]);

        // Delete: listed files and their items go, unlisted files and their folder stay.
        let r = remove_folder(&ws, &registry, src, "archive/first").unwrap();
        assert_eq!((r.deleted, r.failed.len()), (2, 0));
        assert!(ws.read_db.lock().unwrap().folder_media(src, "archive").unwrap().is_empty());
        assert!(ws.read_db.lock().unwrap().folder_values(src, "archive/first").unwrap().is_empty());
        assert!(!media.join("archive/first/deep").exists(), "emptied folders are removed");
        assert!(media.join("archive/first/notes.txt").is_file());
    }
}
