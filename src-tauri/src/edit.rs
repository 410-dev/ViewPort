//! Editing pictures and videos (`vp_core::edit::EditSpec`) from the viewer, saved as a copy
//! beside the original or over it.
//!
//! Pictures are edited here (`vp-edit`) in their own format. Videos go through FFmpeg, or
//! Media3 on Android (`platform::edit_video`), and are re-encoded as H.264/AAC in the same
//! container where it can hold them (MP4, MOV, MKV), else as MP4. Overwriting keeps the
//! item (id, tags, statistics); a changed extension moves it to the new name.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::{AppHandle, Manager};
use vp_core::MediaKind;
use vp_core::edit::EditSpec;
use vp_db::MediaLocation;
use vp_driver::{DriverRegistry, MediaDriver};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::tasks::{self, Unit};
use crate::workspace::{Workspace, file_name, now_ms};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveMode {
    /// A new file beside the original (`name_edited.ext`).
    Copy,
    /// Replaces the original.
    Overwrite,
}

/// Whether item kinds can be edited here: pictures always, videos with FFmpeg or (Android)
/// the OS media framework.
#[derive(Debug, serde::Serialize)]
pub struct EditSupport {
    pub image: bool,
    pub video: bool,
}

#[tauri::command]
pub async fn edit_support(app: AppHandle) -> EditSupport {
    let ws = app.state::<AppState>().workspace.read().unwrap().clone();
    let video = ws.is_some_and(|ws| ws.ffmpeg().is_some()) || (cfg!(target_os = "android") && crate::platform::media_available());
    EditSupport { image: true, video }
}

/// Applies `spec` to item `id` and saves it as `mode` says. Returns the id of the item
/// holding the result (a new one for a copy, the same one when overwritten).
#[tauri::command]
pub async fn edit_media(app: AppHandle, id: i64, spec: EditSpec, mode: SaveMode) -> CmdResult<i64> {
    let ws = app.state::<AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))?;
    let registry = app.state::<AppState>().drivers.clone();
    tauri::async_runtime::spawn_blocking(move || edit(&ws, &registry, id, &spec, mode))
        .await
        .map_err(|e| CmdError::new("internal", e))?
}

fn edit(ws: &Arc<Workspace>, registry: &DriverRegistry, id: i64, spec: &EditSpec, mode: SaveMode) -> CmdResult<i64> {
    if !ws.autosave() {
        return Err(CmdError::new("read_only", "editing is disabled for bundles opened without autosave"));
    }
    let loc = ws.read_db.lock().unwrap().media_location(id)?.ok_or_else(|| CmdError::new("not_found", "item"))?;
    let source = ws.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    let driver = ws.driver(registry, &source)?;
    if !driver.capabilities().write {
        return Err(CmdError::new("read_only", "this source cannot be written to"));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let task = tasks::begin_with_cancel("edit", file_name(&loc.rel_path), Unit::Permille, Some(1000), Some(cancel.clone())).media(id);
    let r = match loc.kind {
        MediaKind::Image => edit_image(ws, driver.as_ref(), id, &loc, spec, mode),
        MediaKind::Video => edit_video(ws, registry, driver.as_ref(), id, &loc, spec, mode, &task, &cancel),
        MediaKind::NonMedia => Err(CmdError::new("unsupported", "not a picture or video")),
    };
    match &r {
        Ok(_) => task.finish(),
        Err(e) if e.code == "cancelled" => task.cancelled(),
        Err(e) => {
            crate::applog::warn("edit", format!("{}: {}", loc.rel_path, e.message));
            task.fail(e.message.clone());
        }
    }
    r
}

fn split_ext(rel: &str) -> (&str, String) {
    match rel.rsplit_once('.') {
        Some((stem, ext)) if !stem.ends_with('/') && !stem.is_empty() => (stem, ext.to_ascii_lowercase()),
        _ => (rel, String::new()),
    }
}

/// `folder/clip` + `mp4` → `folder/clip_edited.mp4`, then ` (2)`, ` (3)`, ... while taken.
fn edited_name(stem: &str, ext: &str, taken: impl Fn(&str) -> bool) -> String {
    let mut name = format!("{stem}_edited.{ext}");
    let mut n = 2;
    while taken(&name) {
        name = format!("{stem}_edited ({n}).{ext}");
        n += 1;
    }
    name
}

fn exists(driver: &dyn MediaDriver, rel: &str) -> bool {
    driver.stat(rel).is_ok()
}

fn edit_image(ws: &Workspace, driver: &dyn MediaDriver, id: i64, loc: &MediaLocation, spec: &EditSpec, mode: SaveMode) -> CmdResult<i64> {
    spec.validate(None).map_err(|e| CmdError::new("invalid", e))?;
    let (stem, ext) = split_ext(&loc.rel_path);
    let format = vp_edit::Format::from_ext(&ext).ok_or_else(|| CmdError::new("unsupported", format!("cannot edit .{ext} pictures")))?;
    let data = crate::workspace::read_tracked(driver.open(&loc.rel_path)?.as_ref(), &loc.rel_path, Some(id))?;
    let quality = crate::recent::cached_app_settings().viewer.edit_quality;
    let out = vp_edit::edit_image(&data, format, spec, quality).map_err(|e| CmdError::new("edit_failed", e))?;
    let target = match mode {
        SaveMode::Copy => edited_name(stem, &ext, |p| exists(driver, p)),
        SaveMode::Overwrite => loc.rel_path.clone(),
    };
    driver.write(&target, &out.data).map_err(|e| CmdError::new("driver", format!("{target}: {e}")))?;
    let (w, h) = (Some(out.width), Some(out.height));
    let db = ws.write_db.lock().unwrap();
    match mode {
        SaveMode::Copy => {
            let new_id = db.add_beside(id, &target, MediaKind::Image, out.data.len() as u64, w, h, now_ms())?;
            db.copy_values(id, new_id)?;
            Ok(new_id)
        }
        SaveMode::Overwrite => {
            let modified = driver.stat(&target).ok().and_then(|e| e.modified_ms).unwrap_or_else(now_ms);
            db.media_replaced(id, out.data.len() as u64, modified, w, h)?;
            db.remap_faces(id, |x, y, bw, bh| spec.map_box(out.source_width, out.source_height, (x, y, bw, bh)))?;
            drop(db);
            crate::media::forget(ws, id);
            Ok(id)
        }
    }
}

/// A temporary file removed when dropped (unless taken).
struct Temp(Option<PathBuf>);

impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn edit_video(
    ws: &Arc<Workspace>,
    registry: &DriverRegistry,
    driver: &dyn MediaDriver,
    id: i64,
    loc: &MediaLocation,
    spec: &EditSpec,
    mode: SaveMode,
    task: &tasks::Task,
    cancel: &AtomicBool,
) -> CmdResult<i64> {
    let probe = ws.probe_video(registry, id)?.ok_or_else(|| CmdError::new("probe_failed", "could not read the video"))?;
    spec.validate(Some(probe.duration_ms.unwrap_or(0))).map_err(|e| CmdError::new("invalid", e))?;
    let (stem, ext) = split_ext(&loc.rel_path);
    let ff = ws.ffmpeg();
    // FFmpeg keeps the container where it can hold H.264/AAC; Media3 always writes MP4.
    let (format, out_ext) = match (&ff, vp_media::edit_container(&ext)) {
        (Some(_), Some(f)) => (f, ext.clone()),
        _ => ("mp4", if matches!(ext.as_str(), "mp4" | "m4v") { ext.clone() } else { "mp4".to_string() }),
    };
    let target = match mode {
        SaveMode::Copy => edited_name(stem, &out_ext, |p| exists(driver, p)),
        SaveMode::Overwrite if out_ext == ext => loc.rel_path.clone(),
        SaveMode::Overwrite => {
            let plain = format!("{stem}.{out_ext}");
            if exists(driver, &plain) { edited_name(stem, &out_ext, |p| exists(driver, p)) } else { plain }
        }
    };
    let mut progress = |p: f64| task.set((p * 1000.0) as u64, Some(1000));
    let failed = |e: vp_media::MediaError| match e {
        vp_media::MediaError::Cancelled => CmdError::new("cancelled", "cancelled"),
        other => CmdError::new("edit_failed", other),
    };

    if let Some(ff) = &ff {
        let hw = crate::recent::cached_app_settings().hardware;
        let encoders = if hw.video_encode { hw.encoders.clone() } else { Vec::new() };
        let mut make_input = || ws.media_input(registry, loc).map_err(|e| vp_media::MediaError::Failed(e.message));
        if let Some(shard) = ws.shard_for(loc.source_id)? {
            // Streamed straight into the shard: never on disk unencrypted. A file replaced
            // there stays readable until the new one is finished.
            if !shard.is_writable() {
                return Err(CmdError::new("read_only", "the shard is open read-only"));
            }
            let fail = |e: vp_shard::ShardError| vp_media::MediaError::Failed(e.to_string());
            let now = Some(now_ms());
            ff.edit_to_writer(
                &mut make_input,
                &mut || shard.writer(&target, None, now, now).map_err(fail),
                &mut |w| w.finish().map_err(fail),
                format,
                spec,
                &probe,
                &encoders,
                &mut progress,
                cancel,
            )
            .map_err(failed)?;
        } else if let Some(final_path) = driver.local_path(&target) {
            let part = Temp(Some(part_path(&final_path)));
            ff.edit(&mut make_input, part.0.as_ref().unwrap(), format, spec, &probe, &encoders, &mut progress, cancel).map_err(failed)?;
            std::fs::rename(part.0.as_ref().unwrap(), &final_path)?;
        } else {
            // Network sources: made in a temporary file, then written through the driver.
            let tmp = Temp(Some(std::env::temp_dir().join(format!("viewport-edit-{id}-{}.{out_ext}", now_ms()))));
            ff.edit(&mut make_input, tmp.0.as_ref().unwrap(), format, spec, &probe, &encoders, &mut progress, cancel).map_err(failed)?;
            let data = std::fs::read(tmp.0.as_ref().unwrap())?;
            driver.write(&target, &data).map_err(|e| CmdError::new("driver", format!("{target}: {e}")))?;
        }
    } else {
        // Android: Media3 reads and writes files by path.
        let input = driver.local_path(&loc.rel_path).ok_or_else(|| CmdError::new("unsupported", "only files on this device can be edited"))?;
        let final_path = driver.local_path(&target).ok_or_else(|| CmdError::new("unsupported", "only files on this device can be edited"))?;
        let part = Temp(Some(part_path(&final_path)));
        crate::platform::edit_video(&input, part.0.as_ref().unwrap(), spec, &mut progress, cancel).map_err(|e| {
            if e == "cancelled" { CmdError::new("cancelled", e) } else { CmdError::new("edit_failed", e) }
        })?;
        std::fs::rename(part.0.as_ref().unwrap(), &final_path)?;
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(CmdError::new("cancelled", "cancelled"));
    }

    let entry = driver.stat(&target).map_err(|e| CmdError::new("driver", format!("{target}: {e}")))?;
    match mode {
        SaveMode::Copy => {
            let db = ws.write_db.lock().unwrap();
            let new_id = db.add_beside(id, &target, MediaKind::Video, entry.size, None, None, now_ms())?;
            db.copy_values(id, new_id)?;
            Ok(new_id)
        }
        SaveMode::Overwrite => {
            // The converted copy of the old video is useless now.
            let variant = ws.read_db.lock().unwrap().variant(id)?;
            if let Some(rel) = variant.filter(|r| r != crate::shards::VARIANT_IN_SHARD)
                && let Ok(parts) = vp_driver::checked_components(&rel)
            {
                let mut p = ws.bundle.root.clone();
                p.extend(parts);
                let _ = std::fs::remove_file(p);
            }
            if target != loc.rel_path {
                driver.delete(&loc.rel_path).map_err(|e| CmdError::new("driver", format!("{}: {e}", loc.rel_path)))?;
                ws.write_db.lock().unwrap().move_media(id, loc.source_id, &target)?;
            }
            let db = ws.write_db.lock().unwrap();
            db.media_replaced(id, entry.size, entry.modified_ms.unwrap_or_else(now_ms), None, None)?;
            let duration = probe.duration_ms.unwrap_or(i64::MAX);
            let (a, b) = spec.trim.map_or((0, duration), |t| (t.start_ms, t.end_ms.min(duration)));
            db.remap_loop_segments(id, a, b, spec.speed)?;
            drop(db);
            crate::media::forget(ws, id);
            Ok(id)
        }
    }
}

/// A hidden working file next to `path` (same file system, so it can be renamed into place).
fn part_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.part", now_ms()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(split_ext("a/b.MP4"), ("a/b", "mp4".to_string()));
        assert_eq!(split_ext("a/.hidden"), ("a/.hidden", String::new()));
        let taken = ["a/b_edited.jpg", "a/b_edited (2).jpg"];
        assert_eq!(edited_name("a/b", "jpg", |p| taken.contains(&p)), "a/b_edited (3).jpg");
    }
}
