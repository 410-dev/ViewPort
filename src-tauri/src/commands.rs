//! Tauri commands called by the UI.

use std::path::PathBuf;
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

fn install(app: &AppHandle, state: &AppState, ws: Workspace) -> CmdResult<BundleSummary> {
    let summary = ws.summary();
    let ws = Arc::new(ws);
    ws.start_background(state.drivers.clone());
    let old = state.workspace.write().unwrap().replace(ws);
    if let Some(old) = old {
        old.closed.store(true, Ordering::Relaxed);
    }
    recent::push(app, &summary.path);
    Ok(summary)
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

/// Opens a bundle. A bundle newer than this build fails with `bundle_too_new` unless
/// `allow_newer` is set, in which case it opens without autosave.
#[tauri::command]
pub async fn open_bundle(app: AppHandle, path: String, allow_newer: bool) -> CmdResult<BundleSummary> {
    let ws = blocking(move || Workspace::open(&PathBuf::from(path), allow_newer)).await?;
    install(&app, &app.state::<AppState>(), ws)
}

#[tauri::command]
pub fn close_bundle(state: State<'_, AppState>) {
    if let Some(ws) = state.workspace.write().unwrap().take() {
        ws.closed.store(true, Ordering::Relaxed);
        for cancel in ws.scans.lock().unwrap().values() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
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

#[tauri::command]
pub fn recent_bundles(app: AppHandle) -> Vec<String> {
    recent::list(&app)
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
pub fn save_app_settings(app: AppHandle, settings: vp_core::settings::AppSettings) -> CmdResult<()> {
    Ok(recent::save_app_settings(&app, &settings)?)
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

#[tauri::command]
pub fn remove_source(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    let ws = workspace(&state)?;
    if let Some(cancel) = ws.scans.lock().unwrap().get(&id) {
        cancel.store(true, Ordering::Relaxed);
    }
    ws.write_db.lock().unwrap().remove_source(id)?;
    ws.forget_driver(id);
    Ok(())
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

fn start_scan(app: &AppHandle, ws: Arc<Workspace>, source: SourceRow) {
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut scans = ws.scans.lock().unwrap();
        if scans.contains_key(&source.id) {
            return; // already running
        }
        scans.insert(source.id, cancel.clone());
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let registry = app.state::<AppState>().drivers.clone();
        let settings = ws.settings.read().unwrap().scan.clone();
        let emit = |progress: &vp_scan::ScanProgress, done: bool, error: Option<String>| {
            let _ = app.emit("scan-progress", ScanEvent { progress: progress.clone(), done, error });
        };
        let result = ws.driver(&registry, &source).map_err(|e| e.message).and_then(|driver| {
            let scanner = vp_scan::Scanner {
                db: &ws.write_db,
                driver: driver.as_ref(),
                settings: &settings,
                source_id: source.id,
                now_ms: now_ms(),
                cancel: &cancel,
            };
            scanner.run(&mut |p| emit(p, false, None)).map_err(|e| e.to_string())
        });
        let empty = vp_scan::ScanProgress { source_id: source.id, ..Default::default() };
        match &result {
            Ok(p) => emit(p, true, None),
            Err(e) => emit(&empty, true, Some(e.clone())),
        }
        ws.scans.lock().unwrap().remove(&source.id);
        if result.is_ok() && settings.thumbnails_after_scan && ws.autosave() {
            crate::thumbs::spawn_backfill(app.clone(), ws.clone());
        }
    });
}

// ---------------------------------------------------------------- search

#[derive(Serialize)]
pub struct SearchResult {
    pub ids: Vec<i64>,
    pub elapsed_ms: f64,
}

/// Runs a query. Empty text lists everything. `tz_offset_minutes` is the local offset
/// from UTC (JavaScript's `-getTimezoneOffset()`); query error spans are UTF-16 offsets.
#[tauri::command]
pub async fn search(
    app: AppHandle,
    query: String,
    source_id: Option<i64>,
    tz_offset_minutes: i32,
) -> CmdResult<SearchResult> {
    let ws = workspace(&app.state::<AppState>())?;
    blocking(move || {
        let started = std::time::Instant::now();
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
        let ids = db.search_in(&q, &reg, &Db::compile_context(now_ms(), tz_offset_minutes), source_id)?;
        Ok(SearchResult { ids, elapsed_ms: started.elapsed().as_secs_f64() * 1000.0 })
    })
    .await
}

fn utf16_error(mut e: QueryError, src: &str) -> QueryError {
    e.span = e.span.to_utf16(src);
    e
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
            let _ = tauri::async_runtime::spawn_blocking(move || ws.load_into_cache(&drivers, id)).await;
        });
    }
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
