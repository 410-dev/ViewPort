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
    if ws.autosave() {
        crate::video::spawn_probe_backfill(app, ws.clone());
    }
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
        if result.is_ok() && ws.autosave() {
            crate::video::spawn_probe_backfill(&app, ws.clone());
        }
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

#[tauri::command]
pub fn suggest_values(state: State<'_, AppState>, key: String, prefix: String) -> CmdResult<Vec<String>> {
    Ok(workspace(&state)?.read_db.lock().unwrap().suggest_values(&key, &prefix, 20)?)
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
