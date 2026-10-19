//! Video features backed by FFmpeg: probing, frame thumbnails, and conversion to a
//! high-compatibility copy stored in the bundle.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use vp_db::MediaLocation;
use vp_driver::{DriverRegistry, ReaderStream};
use vp_media::{Ffmpeg, Input, Probe};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::workspace::{Workspace, now_ms};

pub const CONVERTED_DIR: &str = "converted";

/// Folders searched for FFmpeg before `PATH`: next to the executable and its `ffmpeg` subfolder.
fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        dirs.push(dir.join("ffmpeg"));
        dirs.push(dir.join("ffmpeg").join("bin"));
        dirs.push(dir);
    }
    dirs
}

pub fn locate(explicit: &str) -> Option<Ffmpeg> {
    Ffmpeg::locate(explicit, &default_dirs())
}

impl Workspace {
    pub fn ffmpeg(&self) -> Option<Ffmpeg> {
        self.ffmpeg.read().unwrap().clone()
    }

    /// Re-detects FFmpeg (after the path setting changed). Returns whether it was found.
    pub fn relocate_ffmpeg(&self) -> bool {
        let path = self.settings.read().unwrap().playback.ffmpeg_path.clone();
        let found = locate(&path);
        let available = found.is_some();
        let was = std::mem::replace(&mut *self.ffmpeg.write().unwrap(), found).is_some();
        if available && !was {
            // Videos may have been recorded as having no thumbnail while FFmpeg was missing.
            let _ = self.write_db.lock().unwrap().reset_video_thumbnails();
        }
        available
    }

    /// Input for FFmpeg: a direct path for local files, otherwise a stream from the driver.
    fn media_input(&self, drivers: &DriverRegistry, loc: &MediaLocation) -> CmdResult<Input> {
        let source =
            self.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        let driver = self.driver(drivers, &source)?;
        if let Some(p) = driver.local_path(&loc.rel_path) {
            return Ok(Input::File(p));
        }
        Ok(Input::Stream(Box::new(ReaderStream::new(driver.open(&loc.rel_path)?))))
    }

    /// A filesystem path for the OS media framework, which cannot read from drivers.
    pub(crate) fn os_media_path(&self, drivers: &DriverRegistry, loc: &MediaLocation) -> CmdResult<Option<PathBuf>> {
        if !crate::platform::media_available() {
            return Ok(None);
        }
        let source =
            self.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
        Ok(self.driver(drivers, &source)?.local_path(&loc.rel_path))
    }

    /// Whether videos can be probed and get frame thumbnails here (FFmpeg or the OS).
    pub fn can_probe(&self) -> bool {
        self.ffmpeg().is_some() || crate::platform::media_available()
    }

    /// Probes a video and records codecs, compatibility, duration and size.
    pub fn probe_video(&self, drivers: &DriverRegistry, id: i64) -> CmdResult<Option<Probe>> {
        if !self.can_probe() {
            return Ok(None);
        }
        let Some(loc) = self.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
        let ext = loc.rel_path.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
        // FFmpeg judges compatibility by codec; the OS asks its own decoders. Without
        // FFmpeg, files the OS cannot open by path (network shares) count as failed.
        let result = match self.ffmpeg() {
            Some(ff) => ff.probe(self.media_input(drivers, &loc)?).map(|p| {
                let compatible = p.webview_compatible(&ext);
                (p, compatible)
            }),
            None => self
                .os_media_path(drivers, &loc)?
                .and_then(|path| crate::platform::probe(&path))
                .map(|p| (p.to_probe(), p.playable))
                .ok_or_else(|| vp_media::MediaError::Failed("the OS could not read this video".into())),
        };
        let db = self.write_db.lock().unwrap();
        match result {
            Ok((p, compatible)) => {
                db.set_probe(
                    id,
                    true,
                    Some(&p.container),
                    p.video_codec.as_deref(),
                    p.audio_codec.as_deref(),
                    compatible,
                    Some((p.duration_ms, p.width, p.height)),
                    now_ms(),
                )?;
                Ok(Some(p))
            }
            Err(_) => {
                db.set_probe(id, false, None, None, None, false, None, now_ms())?;
                Ok(None)
            }
        }
    }

    /// A JPEG frame for a video thumbnail, or `None` without FFmpeg or on failure.
    pub fn video_frame(&self, drivers: &DriverRegistry, id: i64, loc: &MediaLocation) -> CmdResult<Option<Vec<u8>>> {
        if !self.can_probe() {
            return Ok(None);
        }
        let known = self.read_db.lock().unwrap().media_brief(&[id])?.first().and_then(|b| b.duration_ms);
        let duration = match known {
            Some(d) => Some(d),
            None => self.probe_video(drivers, id)?.and_then(|p| p.duration_ms),
        };
        let s = self.settings.read().unwrap().thumbnails.clone();
        let at = duration.map(|d| d as f64 / 1000.0 * s.video_frame_position as f64).unwrap_or(0.0);
        let Some(ff) = self.ffmpeg() else {
            let Some(path) = self.os_media_path(drivers, loc)? else { return Ok(None) };
            return Ok(crate::platform::frame(&path, (at * 1000.0) as i64, s.max_edge_px, s.quality));
        };
        let hw = crate::recent::cached_app_settings().hardware;
        let hwaccel = hw.hwaccel();
        // Attempts in order: hardware decode at the chosen position, software decode there
        // (a specific -hwaccel may be unavailable), then the first frame (very short clips:
        // the chosen position may be past the end).
        let mut attempts = vec![(at, hwaccel)];
        if hwaccel.is_some() {
            attempts.push((at, None));
        }
        if at > 0.0 {
            attempts.push((0.0, None));
        }
        for (pos, accel) in attempts {
            if let Ok(jpeg) = ff.frame(self.media_input(drivers, loc)?, pos, s.max_edge_px, accel) {
                return Ok(Some(jpeg));
            }
        }
        Ok(None)
    }

    /// The converted copy of `id`: a file in the bundle, or kept by the item's shard.
    pub fn variant_reader(&self, id: i64) -> CmdResult<Option<Box<dyn vp_driver::MediaReader>>> {
        let Some(rel) = self.read_db.lock().unwrap().variant(id)? else { return Ok(None) };
        if rel == crate::shards::VARIANT_IN_SHARD {
            let Some(loc) = self.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
            let Some(shard) = self.shard_for(loc.source_id)? else { return Ok(None) };
            return Ok(shard.blob(vp_shard::Stream::Converted, &loc.rel_path)?.map(vp_driver::shard::media_reader));
        }
        let path = self.bundle.root.join(rel);
        if !path.is_file() {
            return Ok(None);
        }
        Ok(Some(vp_driver::local::open_file(&path)?))
    }
}

#[derive(Clone, Serialize)]
pub struct ConvertEvent {
    pub id: i64,
    pub progress: f64,
    pub done: bool,
    pub error: Option<String>,
}

/// Converts `ids` one after another in the background, emitting `convert-progress`.
pub fn spawn_convert<R: Runtime>(app: &AppHandle<R>, ws: Arc<Workspace>, ids: Vec<i64>) {
    let app = app.clone();
    std::thread::spawn(move || {
        let drivers = app.state::<AppState>().drivers.clone();
        for id in ids {
            if ws.closed.load(Ordering::Relaxed) {
                break;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            {
                let mut jobs = ws.converts.lock().unwrap();
                if jobs.contains_key(&id) {
                    continue;
                }
                jobs.insert(id, cancel.clone());
            }
            let name = ws
                .read_db
                .lock()
                .unwrap()
                .media_location(id)
                .ok()
                .flatten()
                .map(|l| crate::workspace::file_name(&l.rel_path))
                .unwrap_or_default();
            let task = crate::tasks::begin_with_cancel(
                "convert",
                name,
                crate::tasks::Unit::Permille,
                Some(1000),
                Some(cancel.clone()),
            )
            .media(id);
            let emit = |progress: f64, done: bool, error: Option<String>| {
                task.set((progress.clamp(0.0, 1.0) * 1000.0) as u64, None);
                let _ = app.emit("convert-progress", ConvertEvent { id, progress, done, error });
            };
            emit(0.0, false, None);
            let result = convert_one(&ws, &drivers, id, &cancel, &mut |p| emit(p, false, None));
            ws.converts.lock().unwrap().remove(&id);
            match result {
                Ok(()) => emit(1.0, true, None),
                Err(e) => {
                    if e.code != "cancelled" {
                        task.fail(e.message.clone());
                    }
                    emit(0.0, true, Some(e.message))
                }
            }
        }
    });
}

fn convert_one(
    ws: &Workspace,
    drivers: &DriverRegistry,
    id: i64,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> CmdResult<()> {
    if !ws.autosave() {
        return Err(CmdError::new("read_only", "conversion is disabled for bundles opened without autosave"));
    }
    let ff = ws.ffmpeg().ok_or_else(|| CmdError::new("no_ffmpeg", "FFmpeg was not found"))?;
    let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "media"))?;
    let probe = match ws.probe_video(drivers, id)? {
        Some(p) => p,
        None => return Err(CmdError::new("probe_failed", "could not read the video")),
    };
    let hw = crate::recent::cached_app_settings().hardware;
    let encoders = if hw.video_encode { hw.encoders.clone() } else { Vec::new() };
    let mut make_input = || ws.media_input(drivers, &loc).map_err(|e| vp_media::MediaError::Failed(e.message));
    let failed = |e: vp_media::MediaError| match e {
        vp_media::MediaError::Cancelled => CmdError::new("cancelled", "cancelled"),
        other => CmdError::new("convert_failed", other),
    };
    // Items in a shard keep their converted copy there, streamed straight in so it is
    // never on disk unencrypted.
    if let Some(shard) = ws.shard_for(loc.source_id)? {
        if !shard.is_writable() {
            return Err(CmdError::new("read_only", "the shard is open read-only"));
        }
        let fail = |e: vp_shard::ShardError| vp_media::MediaError::Failed(e.to_string());
        let mut attrs = serde_json::Map::new();
        attrs.insert("mime".into(), "video/mp4".into());
        ff.convert_to_writer(
            &mut make_input,
            &mut || shard.blob_writer(vp_shard::Stream::Converted, &loc.rel_path, None, attrs.clone()).map_err(fail),
            &mut |w| w.finish().map_err(fail),
            &probe,
            &encoders,
            progress,
            cancel,
        )
        .map_err(failed)?;
        let size = shard.blob(vp_shard::Stream::Converted, &loc.rel_path)?.map(|r| r.len()).unwrap_or(0);
        ws.write_db.lock().unwrap().set_variant(id, crate::shards::VARIANT_IN_SHARD, size, now_ms())?;
        ws.cache.remove(-id);
        return Ok(());
    }
    let rel = format!("{CONVERTED_DIR}/{id}.mp4");
    let out = ws.bundle.root.join(&rel);
    std::fs::create_dir_all(out.parent().unwrap())?;
    ff.convert(&mut make_input, &out, &probe, &encoders, progress, cancel).map_err(failed)?;
    let size = std::fs::metadata(&out)?.len();
    ws.write_db.lock().unwrap().set_variant(id, &rel, size, now_ms())?;
    ws.cache.remove(-id);
    Ok(())
}

/// Probes every unprobed video in the background (duration, size, codecs). One at a time.
pub fn spawn_probe_backfill<R: Runtime>(app: &AppHandle<R>, ws: Arc<Workspace>) {
    if !ws.can_probe() || ws.probing.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let drivers = app.state::<AppState>().drivers.clone();
        let skip = ws.locked_sources();
        let total = ws.read_db.lock().unwrap().count_unprobed_videos(&skip).ok();
        let task = (total != Some(0)).then(|| crate::tasks::begin("probe", "", crate::tasks::Unit::Items, total));
        loop {
            if ws.closed.load(Ordering::Relaxed) {
                break;
            }
            let batch = ws.read_db.lock().unwrap().unprobed_videos(32, &skip).unwrap_or_default();
            if batch.is_empty() {
                break;
            }
            for id in batch {
                if ws.closed.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(t) = &task {
                    t.add(1);
                }
                if ws.probe_video(&drivers, id).is_err() {
                    // Record the failure so the loop always advances.
                    let _ = ws.write_db.lock().unwrap().set_probe(id, false, None, None, None, false, None, now_ms());
                }
            }
            let _ = app.emit("probe-progress", ());
        }
        ws.probing.store(false, Ordering::Release);
        let _ = app.emit("probe-progress", ());
    });
}
