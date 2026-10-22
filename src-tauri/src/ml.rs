//! Image analysis jobs (`vp-ml`): finding faces and characters, grouping them into people,
//! and tagging. Models run on this device (ONNX Runtime and model packs the user installs);
//! results live in the bundle database, so a library analysed on a desktop GPU shows its
//! people on a phone too. Face pictures go into the thumbnail packs (sealed in a locked
//! bundle), not the database.
//!
//! One job at a time: a reader thread decodes images ahead while the models run, so the
//! GPU does not wait for the disk. After the job (and after every change to people) the
//! automatic `x.people` / `x.characters` / `x.auto_tags` values are rewritten.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use image::RgbImage;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use vp_db::faces::{AnalysisCounts, CropRef, FaceRow, NewFace, PersonRow};
use vp_ml::{Choice, Engine, FaceKind, PackKind, cluster, runtime};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::tasks::{self, Unit};
use crate::workspace::{Workspace, now_ms};

static RUNNING: AtomicBool = AtomicBool::new(false);

fn workspace(app: &AppHandle) -> CmdResult<Arc<Workspace>> {
    app.state::<AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| CmdError::new("internal", e))?
}

/// Where model packs are looked for: the app folder's `models/`, the config folder's
/// `models/`, then `ml.model_dirs`.
fn model_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.join("models"))) {
        dirs.push(d);
    }
    if let Ok(d) = app.path().app_config_dir() {
        dirs.push(d.join("models"));
    }
    dirs.extend(crate::recent::cached_app_settings().ml.model_dirs.iter().filter(|d| !d.trim().is_empty()).map(PathBuf::from));
    dirs
}

fn load_runtime() -> CmdResult<PathBuf> {
    let ml = crate::recent::cached_app_settings().ml;
    let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf()));
    Ok(runtime::init(&runtime::candidates(&ml.runtime_path, exe_dir.as_deref()))?)
}

#[derive(Serialize)]
pub struct PackInfo {
    pub id: String,
    pub key: String,
    pub name: String,
    pub kind: PackKind,
    pub license: String,
    pub license_url: String,
    pub source: String,
    pub path: String,
}

#[derive(Serialize)]
pub struct MlStatus {
    /// This build can run models (desktop).
    pub supported: bool,
    /// The ONNX Runtime library in use, once loaded.
    pub runtime: Option<String>,
    pub runtime_error: Option<String>,
    pub providers: Vec<&'static str>,
    pub packs: Vec<PackInfo>,
    pub pack_errors: Vec<String>,
    pub model_dirs: Vec<String>,
    pub running: bool,
    pub counts: Option<AnalysisCounts>,
}

/// The packs chosen in the bundle settings for what is switched on.
fn choice(s: &vp_core::settings::AnalysisSettings) -> Choice {
    let on = |enabled: bool, id: &str| enabled.then(|| id.to_string());
    Choice {
        face_detector: on(s.faces, &s.face_detector),
        face_embedder: on(s.faces, &s.face_embedder),
        anime_detector: on(s.characters, &s.anime_detector),
        anime_embedder: on(s.characters, &s.anime_embedder),
        tagger: on(s.tagger, &s.tagger_pack),
    }
}

/// Keys of the packs the settings would use (installed ones only), to count progress.
fn chosen_keys(packs: &[vp_ml::Pack], c: &Choice) -> Vec<(String, PackKind)> {
    [
        (PackKind::FaceDetector, &c.face_detector),
        (PackKind::FaceEmbedder, &c.face_embedder),
        (PackKind::AnimeDetector, &c.anime_detector),
        (PackKind::AnimeEmbedder, &c.anime_embedder),
        (PackKind::Tagger, &c.tagger),
    ]
    .into_iter()
    .filter_map(|(k, id)| vp_ml::pack::pick(packs, k, id.as_deref()?).map(|p| (p.manifest.key(), k)))
    .collect()
}

fn kind_name(k: PackKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

#[tauri::command]
pub async fn ml_status(app: AppHandle) -> CmdResult<MlStatus> {
    blocking(move || {
        let (runtime, runtime_error) = if runtime::SUPPORTED {
            match load_runtime() {
                Ok(p) => (Some(p.display().to_string()), None),
                Err(e) => (None, Some(e.message)),
            }
        } else {
            (None, None)
        };
        let dirs = model_dirs(&app);
        let (packs, pack_errors) = vp_ml::pack::find_packs(&dirs);
        let counts = match workspace(&app) {
            Ok(ws) => {
                let settings = ws.settings.read().unwrap().analysis.clone();
                let models = Models::from_keys(&ws, &chosen_keys(&packs, &choice(&settings)))?;
                Some(counts(&ws, &settings, &models)?)
            }
            Err(_) => None,
        };
        Ok(MlStatus {
            supported: runtime::SUPPORTED,
            runtime,
            runtime_error,
            providers: runtime::known_providers().to_vec(),
            packs: packs
                .iter()
                .map(|p| PackInfo {
                    id: p.manifest.id.clone(),
                    key: p.manifest.key(),
                    name: p.manifest.name.clone(),
                    kind: p.manifest.kind,
                    license: p.manifest.license.name.clone(),
                    license_url: p.manifest.license.url.clone(),
                    source: p.manifest.source.clone(),
                    path: p.dir.display().to_string(),
                })
                .collect(),
            pack_errors,
            model_dirs: dirs.iter().map(|d| d.display().to_string()).collect(),
            running: RUNNING.load(Ordering::Relaxed),
            counts,
        })
    })
    .await
}

/// Starts analysing every image not yet seen by the chosen models (no-op while a job runs).
#[tauri::command]
pub async fn ml_analyze(app: AppHandle) -> CmdResult<()> {
    let ws = workspace(&app)?;
    if !runtime::SUPPORTED {
        return Err(CmdError::new("unsupported", "image analysis is not available on this device"));
    }
    if !ws.autosave() {
        return Err(CmdError::new("read_only", "results cannot be saved in this bundle (opened from a newer version)"));
    }
    blocking(load_runtime).await?;
    start(&app, ws);
    Ok(())
}

/// What the library shows, in its order: analysed first (now, or when a job starts).
#[tauri::command]
pub fn ml_prioritize(ids: Vec<i64>) {
    let n = crate::recent::cached_app_settings().ml.priority_items as usize;
    *PRIORITY.lock().unwrap_or_else(|e| e.into_inner()) = ids.into_iter().take(n).collect();
}

/// Forgets failed analyses and starts a job that tries them again.
#[tauri::command]
pub async fn ml_retry_failed(app: AppHandle) -> CmdResult<usize> {
    let ws = workspace(&app)?;
    let ws2 = ws.clone();
    let n = blocking(move || Ok(ws2.write_db.lock().unwrap().retry_failed()?)).await?;
    if n > 0 && runtime::SUPPORTED && ws.autosave() {
        start(&app, ws);
    }
    Ok(n)
}

/// Runs a job in the background unless one is running.
pub fn start(app: &AppHandle, ws: Arc<Workspace>) {
    if !runtime::SUPPORTED || RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = tasks::begin_with_cancel("analyze", "", Unit::Items, None, Some(cancel.clone()));
        let result = run(&app, &ws, &task, &cancel);
        match &result {
            Ok(_) => task.finish(),
            Err(e) if e.code == "cancelled" => task.cancelled(),
            Err(e) => task.fail(e.message.clone()),
        }
        let _ = regroup_all(&ws);
        notify(&app);
        RUNNING.store(false, Ordering::SeqCst);
    });
}

/// Tells every window that item values changed (as `sync.ts` broadcasts do).
fn notify(app: &AppHandle) {
    let _ = app.emit("vp-sync", serde_json::json!({ "kind": "media", "from": "backend" }));
    let _ = app.emit("vp-sync", serde_json::json!({ "kind": "people", "from": "backend" }));
}

fn cancelled() -> CmdError {
    CmdError::new("cancelled", "cancelled")
}

/// An item's image, scaled for analysis; `None` when it cannot be decoded here.
fn decode(ws: &Workspace, drivers: &vp_driver::DriverRegistry, id: i64, max_edge: u32) -> Option<RgbImage> {
    let loc = ws.read_db.lock().unwrap().media_location(id).ok()??;
    if loc.size > ws.settings.read().unwrap().thumbnails.max_source_bytes {
        return None;
    }
    let reader = ws.open_media(drivers, &loc).ok()?;
    let bytes = crate::workspace::read_tracked(reader.as_ref(), &loc.rel_path, Some(id)).ok()?;
    match vp_thumb::decode_scaled(&bytes, max_edge) {
        Ok(img) => Some(img.to_rgb8()),
        // HEIF and other formats only the OS or FFmpeg read.
        Err(_) => {
            let t = ws.foreign_image(drivers, id, &loc, Some(&bytes), max_edge, 95).ok()??;
            vp_thumb::decode_scaled(&t.data, max_edge).ok().map(|i| i.to_rgb8())
        }
    }
}

/// Stores a face picture in the thumbnail packs (sealed in a locked bundle).
fn keep_crop(ws: &Workspace, crop: &RgbImage, quality: u8) -> Option<CropRef> {
    if !ws.autosave() {
        return None;
    }
    let mut data = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut data, quality).encode_image(crop).ok()?;
    let sealed = ws.seal_thumb(&data).ok()?;
    let (pack, offset) = ws.thumbs.append(&sealed).ok()?;
    Some(CropRef { pack, offset, len: sealed.len() as u32 })
}

/// What a source is analysed for (`analysis.source_modes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Auto,
    Real,
    Anime,
    Off,
}

fn mode_of(s: &vp_core::settings::AnalysisSettings, source: i64) -> Mode {
    match s.source_modes.get(&source.to_string()).map(String::as_str) {
        Some("real") => Mode::Real,
        Some("anime") => Mode::Anime,
        Some("off") => Mode::Off,
        _ => Mode::Auto,
    }
}

/// Database ids of the chosen models, by what they find.
#[derive(Debug, Clone, Default)]
struct Models {
    real: Vec<i64>,
    anime: Vec<i64>,
    tagger: Option<i64>,
}

impl Models {
    fn from_keys(ws: &Workspace, keys: &[(String, PackKind)]) -> CmdResult<Self> {
        let db = ws.write_db.lock().unwrap();
        let mut m = Self::default();
        for (key, kind) in keys {
            let id = db.ml_model_id(key, &kind_name(*kind))?;
            match kind {
                PackKind::FaceDetector | PackKind::FaceEmbedder => m.real.push(id),
                PackKind::AnimeDetector | PackKind::AnimeEmbedder => m.anime.push(id),
                PackKind::Tagger => m.tagger = Some(id),
            }
        }
        Ok(m)
    }

    fn real_for(&self, mode: Mode) -> &[i64] {
        if matches!(mode, Mode::Auto | Mode::Real) { &self.real } else { &[] }
    }

    fn anime_for(&self, mode: Mode) -> &[i64] {
        if matches!(mode, Mode::Auto | Mode::Anime) { &self.anime } else { &[] }
    }

    fn tagger_for(&self, mode: Mode) -> Option<i64> {
        self.tagger.filter(|_| mode != Mode::Off)
    }

    fn for_mode(&self, mode: Mode) -> Vec<i64> {
        self.real_for(mode).iter().chain(self.anime_for(mode)).copied().chain(self.tagger_for(mode)).collect()
    }
}

/// Disconnected sources and locked shards: they wait, their items are not "unreadable".
fn waiting_sources(ws: &Workspace) -> Vec<i64> {
    let mut waiting = ws.offline_sources();
    waiting.extend(ws.locked_sources());
    waiting
}

/// `(mode, models, sources to leave out)`.
type Group = (Mode, Vec<i64>, Vec<i64>);

/// Sources split by mode, as `(mode, models, sources to leave out)` for the queries.
/// Waiting sources are left out of every group.
fn groups(ws: &Workspace, s: &vp_core::settings::AnalysisSettings, models: &Models) -> CmdResult<Vec<Group>> {
    let sources = ws.read_db.lock().unwrap().sources()?;
    let waiting = waiting_sources(ws);
    let mut out = Vec::new();
    for mode in [Mode::Auto, Mode::Real, Mode::Anime] {
        let wanted = models.for_mode(mode);
        if wanted.is_empty() || !sources.iter().any(|src| mode_of(s, src.id) == mode) {
            continue;
        }
        let mut exclude: Vec<i64> = sources.iter().filter(|src| mode_of(s, src.id) != mode).map(|src| src.id).collect();
        exclude.extend(&waiting);
        out.push((mode, wanted, exclude));
    }
    Ok(out)
}

/// Item counts summed over the groups; face and people totals of the whole library.
fn counts(ws: &Workspace, s: &vp_core::settings::AnalysisSettings, models: &Models) -> CmdResult<AnalysisCounts> {
    let plan = groups(ws, s, models)?;
    let db = ws.read_db.lock().unwrap();
    let mut total = AnalysisCounts { images: 0, done: 0, failed: 0, ..db.analysis_counts(&[], &[])? };
    for (_, wanted, exclude) in plan {
        total += db.analysis_counts(&wanted, &exclude)?;
    }
    Ok(total)
}

/// Items the library shows, in its order: analysed before the rest (`ml_prioritize`).
static PRIORITY: Mutex<Vec<i64>> = Mutex::new(Vec::new());

fn take_priority() -> Vec<i64> {
    std::mem::take(&mut *PRIORITY.lock().unwrap_or_else(|e| e.into_inner()))
}

fn run(app: &AppHandle, ws: &Arc<Workspace>, task: &tasks::Task, cancel: &AtomicBool) -> CmdResult<()> {
    let device = crate::recent::cached_app_settings().ml;
    let settings = ws.settings.read().unwrap().analysis.clone();
    let quality = ws.settings.read().unwrap().thumbnails.quality.clamp(1, 100);
    load_runtime()?;
    let (packs, _) = vp_ml::pack::find_packs(&model_dirs(app));
    let (engine, errors) = Engine::load(&packs, &choice(&settings), &device.providers, device.device_id, device.threads);
    if engine.models().next().is_none() {
        let why = if errors.is_empty() { "no analysis is switched on".to_string() } else { errors.join("; ") };
        return Err(CmdError::new("ml_pack", why));
    }
    // Model ids; a face belongs to its embedder (or its detector when there is none).
    let db_id = |l: &Option<vp_ml::Loaded>| -> CmdResult<Option<i64>> {
        l.as_ref().map(|l| ws.write_db.lock().unwrap().ml_model_id(&l.pack.manifest.key(), &kind_name(l.pack.manifest.kind))).transpose().map_err(Into::into)
    };
    let face_det = db_id(&engine.face_detector)?;
    let face_emb = db_id(&engine.face_embedder)?;
    let anime_det = db_id(&engine.anime_detector)?;
    let anime_emb = db_id(&engine.anime_embedder)?;
    let models = Models {
        real: [face_det, face_emb].into_iter().flatten().collect(),
        anime: [anime_det, anime_emb].into_iter().flatten().collect(),
        tagger: db_id(&engine.tagger)?,
    };
    let used: Vec<String> = engine.models().map(|m| format!("{} ({})", m.pack.manifest.name, m.model.provider)).collect();
    task.detail(used.join(", "));

    let plan = groups(ws, &settings, &models)?;
    let total = counts(ws, &settings, &models)?;
    task.set(0, Some(total.images.saturating_sub(total.done)));
    let opts = vp_ml::Options {
        detect_score: settings.detect_score,
        nms_iou: settings.nms_iou,
        min_face_px: settings.min_face_px as f32,
        keep_score: settings.keep_score,
        crop_scale: settings.character_crop_scale,
        crop_px: settings.face_crop_px.clamp(32, 512),
    };

    // Reader thread: decodes ahead into a small queue. What the library shows goes first
    // (checked before every item), then each group of sources in id order.
    let (tx, rx) = std::sync::mpsc::sync_channel::<(i64, Mode, Option<RgbImage>)>(4);
    let reader = {
        let ws = ws.clone();
        let drivers = app.state::<AppState>().drivers.clone();
        let models = models.clone();
        let settings = settings.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let waiting: HashSet<i64> = waiting_sources(&ws).into_iter().collect();
        let handle = std::thread::spawn(move || {
            let mut sent: HashSet<i64> = HashSet::new();
            let max_edge = device.max_edge_px.max(64);
            // false: stop reading.
            let send = |id: i64, mode: Mode, sent: &mut HashSet<i64>| -> bool {
                if stop2.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed) {
                    return false;
                }
                if !sent.insert(id) {
                    return true;
                }
                tx.send((id, mode, decode(&ws, &drivers, id, max_edge))).is_ok()
            };
            let priority = |sent: &mut HashSet<i64>| -> bool {
                for id in take_priority() {
                    if sent.contains(&id) {
                        continue;
                    }
                    let Ok(Some(loc)) = ws.read_db.lock().unwrap().media_location(id) else { continue };
                    if loc.kind != vp_core::MediaKind::Image {
                        continue;
                    }
                    let mode = mode_of(&settings, loc.source_id);
                    let wanted = models.for_mode(mode);
                    let done: HashSet<i64> = ws.read_db.lock().unwrap().models_done(id).unwrap_or_default().into_iter().collect();
                    if wanted.is_empty() || waiting.contains(&loc.source_id) || wanted.iter().all(|m| done.contains(m)) {
                        continue;
                    }
                    if !send(id, mode, sent) {
                        return false;
                    }
                }
                true
            };
            for (mode, wanted, exclude) in &plan {
                let mut after = 0;
                loop {
                    let batch = match ws.read_db.lock().unwrap().media_to_analyze(wanted, exclude, after, 64) {
                        Ok(b) if !b.is_empty() => b,
                        _ => break,
                    };
                    after = *batch.last().unwrap();
                    for id in batch {
                        if !priority(&mut sent) || !send(id, *mode, &mut sent) {
                            return;
                        }
                    }
                }
            }
            priority(&mut sent);
        });
        (handle, stop)
    };

    let result = (|| -> CmdResult<()> {
        let mut since_regroup = 0usize;
        let mut failed = 0usize;
        for (id, mode, img) in rx.iter() {
            if cancel.load(Ordering::Relaxed) || ws.closed.load(Ordering::Relaxed) {
                return Err(cancelled());
            }
            let done: HashSet<i64> = ws.read_db.lock().unwrap().models_done(id)?.into_iter().collect();
            let pending = |models: &[i64]| !models.is_empty() && models.iter().any(|m| !done.contains(m));
            let mut models_run = Vec::new();
            let mut faces = Vec::new();
            let mut tags = None;
            let mut ok = img.is_some();
            if let Some(img) = &img {
                for (kind, wanted, emb, det) in
                    [(FaceKind::Real, models.real_for(mode), face_emb, face_det), (FaceKind::Anime, models.anime_for(mode), anime_emb, anime_det)]
                {
                    if !pending(wanted) {
                        continue;
                    }
                    let owner = emb.or(det).unwrap_or_default();
                    // A model failing on one image is that image's problem, not the job's.
                    match engine.faces(kind, img, &opts) {
                        Ok(found) => {
                            for f in found {
                                faces.push(NewFace {
                                    model_id: owner,
                                    kind: kind.as_str().into(),
                                    bbox: f.bbox,
                                    score: f.score,
                                    embedding: f.embedding.as_deref().map(cluster::pack),
                                    crop: keep_crop(ws, &f.crop, quality),
                                });
                            }
                        }
                        Err(_) => ok = false,
                    }
                    models_run.extend(wanted.iter().copied());
                }
                if let Some(t) = models.tagger_for(mode).filter(|t| !done.contains(t)) {
                    match engine.tags(img, settings.keep_score) {
                        Ok(found) => tags = Some((t, found.into_iter().map(|s| (s.name, s.category.code(), s.score)).collect::<Vec<_>>())),
                        Err(_) => ok = false,
                    }
                    models_run.push(t);
                }
            } else {
                // Unreadable here: marked (as failed, so "retry" can bring it back).
                models_run = models.for_mode(mode).into_iter().filter(|m| !done.contains(m)).collect();
            }
            if models_run.is_empty() {
                continue;
            }
            if !ok {
                failed += 1;
                task.detail(format!("{} · {failed} failed", used.join(", ")));
            }
            let tags_ref = tags.as_ref().map(|(m, t)| (*m, t.as_slice()));
            ws.write_db.lock().unwrap().record_analysis(id, &models_run, ok, &faces, tags_ref, now_ms())?;
            task.add(1);
            since_regroup += 1;
            // Groups show up while a long job is still running.
            if since_regroup >= 2000 {
                since_regroup = 0;
                let _ = regroup_all(ws);
                notify(app);
            }
        }
        Ok(())
    })();
    reader.1.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = reader.0.join();
    if let Some(t) = models.tagger {
        ws.write_db.lock().unwrap().sync_tag_values(t, settings.tag_threshold, settings.character_threshold)?;
    }
    result
}

/// Regroups both kinds with the current embedders and rewrites the automatic names.
pub fn regroup_all(ws: &Workspace) -> CmdResult<()> {
    let settings = ws.settings.read().unwrap().analysis.clone();
    for kind in ["real", "anime"] {
        // The embedder with the most faces of this kind is the current one.
        let model: Option<i64> = ws.read_db.lock().unwrap().main_face_model(kind)?;
        if let Some(model) = model {
            regroup(ws, kind, model, &settings)?;
        }
    }
    ws.write_db.lock().unwrap().sync_people_values()?;
    Ok(())
}

/// Faces of named people stay; every other face joins the most similar named person (if
/// similar enough and not rejected there) or is grouped with the rest.
fn regroup(ws: &Workspace, kind: &str, model: i64, s: &vp_core::settings::AnalysisSettings) -> CmdResult<()> {
    let faces = ws.read_db.lock().unwrap().faces_for_grouping(kind, model)?;
    let rejects = ws.read_db.lock().unwrap().face_rejects()?;
    let vecs: Vec<Vec<f32>> = faces.iter().map(|f| cluster::unpack(&f.embedding)).collect();
    let mut by_person: HashMap<i64, Vec<usize>> = HashMap::new();
    for (i, f) in faces.iter().enumerate() {
        if let (Some(p), true) = (f.person_id, f.named) {
            by_person.entry(p).or_default().push(i);
        }
    }
    let people: Vec<i64> = by_person.keys().copied().collect();
    let centres: Vec<Vec<f32>> =
        people.iter().map(|p| cluster::centroid(by_person[p].iter().map(|&i| vecs[i].as_slice())).unwrap_or_default()).collect();
    let mut to_people = Vec::new();
    let mut rest = Vec::new();
    for (i, f) in faces.iter().enumerate() {
        if f.named || f.confirmed {
            continue;
        }
        let best = centres
            .iter()
            .enumerate()
            .filter(|(n, _)| !rejects.contains(&(f.id, people[*n])))
            .map(|(n, c)| (n, cluster::dot(&vecs[i], c)))
            .filter(|(_, sim)| *sim >= s.assign_similarity)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((n, _)) => to_people.push((f.id, people[n])),
            None => rest.push(i),
        }
    }
    let rest_vecs: Vec<Vec<f32>> = rest.iter().map(|&i| vecs[i].clone()).collect();
    let groups = cluster::group(&rest_vecs, s.group_similarity, s.min_group_size as usize);
    let to_groups: Vec<(i64, usize)> = rest.iter().zip(groups).filter_map(|(&i, g)| g.map(|g| (faces[i].id, g))).collect();
    ws.write_db.lock().unwrap().apply_grouping(kind, &to_people, &to_groups, now_ms())?;
    Ok(())
}

// ---------------------------------------------------------------- people

#[tauri::command]
pub async fn ml_regroup(app: AppHandle) -> CmdResult<()> {
    let ws = workspace(&app)?;
    blocking(move || regroup_all(&ws)).await?;
    notify(&app);
    Ok(())
}

#[tauri::command]
pub async fn ml_people(app: AppHandle, kind: String, hidden: Option<bool>) -> CmdResult<Vec<PersonRow>> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().people(&kind, hidden.unwrap_or(false))?)).await
}

#[tauri::command]
pub async fn ml_person_faces(app: AppHandle, person: i64, offset: u64, limit: u64) -> CmdResult<Vec<FaceRow>> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().person_faces(person, offset, limit)?)).await
}

#[tauri::command]
pub async fn ml_media_faces(app: AppHandle, media: i64) -> CmdResult<Vec<FaceRow>> {
    let ws = workspace(&app)?;
    blocking(move || Ok(ws.read_db.lock().unwrap().media_faces(media)?)).await
}

/// A change to people: the automatic names follow, and every window hears of it.
async fn people_change<T: Send + 'static>(app: &AppHandle, f: impl FnOnce(&Workspace) -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    let ws = workspace(app)?;
    let out = blocking(move || {
        let out = f(&ws)?;
        ws.write_db.lock().unwrap().sync_people_values()?;
        Ok(out)
    })
    .await?;
    notify(app);
    Ok(out)
}

/// Names a person (or group); naming it like another person of its kind merges the two.
#[tauri::command]
pub async fn ml_rename_person(app: AppHandle, id: i64, name: String) -> CmdResult<i64> {
    people_change(&app, move |ws| Ok(ws.write_db.lock().unwrap().rename_person(id, &name)?)).await
}

#[tauri::command]
pub async fn ml_merge_people(app: AppHandle, into: i64, from: Vec<i64>) -> CmdResult<()> {
    people_change(&app, move |ws| Ok(ws.write_db.lock().unwrap().merge_people(into, &from)?)).await
}

/// Puts faces in a person, or (`person` absent) takes them out: "not this person".
#[tauri::command]
pub async fn ml_assign_faces(app: AppHandle, faces: Vec<i64>, person: Option<i64>) -> CmdResult<()> {
    people_change(&app, move |ws| Ok(ws.write_db.lock().unwrap().assign_faces(&faces, person)?)).await
}

#[tauri::command]
pub async fn ml_hide_person(app: AppHandle, id: i64, hidden: bool) -> CmdResult<()> {
    people_change(&app, move |ws| Ok(ws.write_db.lock().unwrap().set_person_hidden(id, hidden)?)).await
}

/// The stored picture of a face (JPEG), for `vpthumb://localhost/face-<id>`.
pub fn face_crop(ws: &Workspace, face: i64) -> CmdResult<Option<Vec<u8>>> {
    let Some(c) = ws.read_db.lock().unwrap().face_crop(face)? else { return Ok(None) };
    let data = ws.thumbs.read(c.pack, c.offset, c.len)?;
    Ok(Some(match &ws.key {
        Some(k) => vp_crypto::open(k, &data, b"thumb")?,
        None => data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_db::{NewMedia, NewSource};

    #[test]
    fn regrouping_keeps_named_people_and_groups_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::create(&dir.path().join("L.vpbun"), "L").unwrap();
        let db = ws.write_db.lock().unwrap();
        let src = db.add_source(&NewSource { name: "p".into(), driver: "local".into(), config: serde_json::json!({}), root: "/p".into() }).unwrap();
        let media: Vec<i64> = (0..8)
            .map(|i| {
                db.upsert_media(&NewMedia {
                    source_id: src,
                    rel_path: format!("{i}.jpg"),
                    kind: vp_core::MediaKind::Image,
                    size: 1,
                    created_ms: None,
                    modified_ms: None,
                    added_ms: 0,
                    duration_ms: None,
                    width: None,
                    height: None,
                    seen_gen: 1,
                })
                .unwrap()
            })
            .collect();
        let emb = db.ml_model_id("sface@1", "face_embedder").unwrap();
        let unit = |v: [f32; 3]| {
            let mut v = v.to_vec();
            cluster::normalize(&mut v);
            cluster::pack(&v)
        };
        // Items 0-3: person A (along x); 4-6: person B (along y); 7: a stray.
        let vecs = [[1.0, 0.0, 0.0], [1.0, 0.05, 0.0], [1.0, 0.1, 0.0], [1.0, 0.0, 0.05], [0.0, 1.0, 0.0], [0.05, 1.0, 0.0], [0.0, 1.0, 0.1], [0.0, 0.0, 1.0]];
        let crop = keep_crop(&ws, &RgbImage::from_pixel(16, 16, image::Rgb([10, 200, 30])), 90);
        for (m, v) in media.iter().zip(vecs) {
            let f = NewFace { model_id: emb, kind: "real".into(), bbox: [0.0; 4], score: 0.9, embedding: Some(unit(v)), crop };
            db.record_analysis(*m, &[emb], true, &[f], None, 1).unwrap();
        }
        drop(db);
        regroup_all(&ws).unwrap();
        let people = ws.read_db.lock().unwrap().people("real", false).unwrap();
        assert_eq!(people.iter().map(|p| p.faces).collect::<Vec<_>>(), [4, 3], "two groups; the stray is left out");

        // Name A, then a new face like A joins A on the next regroup.
        let a = people[0].id;
        ws.write_db.lock().unwrap().rename_person(a, "A").unwrap();
        let f = NewFace { model_id: emb, kind: "real".into(), bbox: [0.0; 4], score: 0.9, embedding: Some(unit([1.0, 0.02, 0.02])), crop: None };
        ws.write_db.lock().unwrap().record_analysis(media[7], &[emb], true, &[f], None, 1).unwrap();
        regroup_all(&ws).unwrap();
        let people = ws.read_db.lock().unwrap().people("real", false).unwrap();
        assert_eq!((people[0].id, people[0].name.as_str(), people[0].faces), (a, "A", 5));
        assert_eq!(people[1].name, "", "B is still an unnamed group");

        // The face picture comes back from the packs.
        let face = ws.read_db.lock().unwrap().person_faces(a, 0, 1).unwrap()[0].id;
        let jpeg = face_crop(&ws, face).unwrap().expect("stored");
        assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 16);
    }

    #[test]
    fn source_modes_choose_what_each_source_is_analysed_for() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::create(&dir.path().join("L.vpbun"), "L").unwrap();
        // Three folders that exist (so none is "disconnected").
        let add = |name: &str| {
            let root = dir.path().join(name);
            std::fs::create_dir_all(&root).unwrap();
            let s = NewSource { name: name.into(), driver: "local".into(), config: serde_json::json!({}), root: root.display().to_string() };
            ws.write_db.lock().unwrap().add_source(&s).unwrap()
        };
        let (photos, art, skip) = (add("photos"), add("art"), add("skip"));
        let mut s = vp_core::settings::AnalysisSettings::default();
        s.source_modes.insert(art.to_string(), "anime".into());
        s.source_modes.insert(skip.to_string(), "off".into());
        let models = Models { real: vec![1, 2], anime: vec![3], tagger: Some(4) };
        assert_eq!(mode_of(&s, photos), Mode::Auto);
        assert_eq!(models.for_mode(Mode::Anime), [3, 4]);
        assert!(models.for_mode(Mode::Off).is_empty());

        let plan = groups(&ws, &s, &models).unwrap();
        assert_eq!(plan.len(), 2, "auto and anime; nothing runs on the 'off' source");
        let (mode, wanted, exclude) = &plan[0];
        assert_eq!((*mode, wanted.as_slice()), (Mode::Auto, [1, 2, 3, 4].as_slice()));
        assert!(!exclude.contains(&photos) && exclude.contains(&art) && exclude.contains(&skip));
        let (mode, wanted, exclude) = &plan[1];
        assert_eq!((*mode, wanted.as_slice()), (Mode::Anime, [3, 4].as_slice()));
        assert!(exclude.contains(&photos) && !exclude.contains(&art));

        // Without a tagger, a photos-only source has nothing to do for anime-only models.
        let anime_only = Models { real: vec![], anime: vec![3], tagger: None };
        s.source_modes.insert(photos.to_string(), "real".into());
        let plan = groups(&ws, &s, &anime_only).unwrap();
        assert_eq!(plan.iter().map(|g| g.0).collect::<Vec<_>>(), [Mode::Anime]);
    }
}
