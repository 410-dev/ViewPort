//! `vpthumb://` protocol and background thumbnail generation.
//!
//! The grid requests `vpthumb://localhost/<media id>` (on Windows and Android
//! `http://vpthumb.localhost/<media id>`). Missing thumbnails are generated on demand,
//! so what the user is looking at is always served first; a background job fills in
//! the rest after a scan. Both share one permit pool sized by `thumbnails.workers`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::http::{Response, StatusCode, header};
use tauri::{AppHandle, Emitter, Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::AppState;
use crate::workspace::Workspace;

pub const SCHEME: &str = "vpthumb";

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(code).body(Vec::new()).unwrap()
}

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: tauri::http::Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    let path = request.uri().path().trim_start_matches('/').to_string();
    // `face-<id>`: a face picture of the people view.
    if let Some(face) = path.strip_prefix("face-").and_then(|f| f.parse::<i64>().ok()) {
        tauri::async_runtime::spawn(async move {
            let ws = app.state::<AppState>().workspace.read().unwrap().clone();
            let Some(ws) = ws else { return responder.respond(status(StatusCode::SERVICE_UNAVAILABLE)) };
            let result = tauri::async_runtime::spawn_blocking(move || crate::ml::face_crop(&ws, face)).await;
            responder.respond(match result {
                Ok(Ok(Some(data))) => Response::builder()
                    .header(header::CONTENT_TYPE, "image/jpeg")
                    .header(header::CACHE_CONTROL, "max-age=3600")
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .body(data)
                    .unwrap(),
                Ok(Ok(None)) => status(StatusCode::NOT_FOUND),
                _ => status(StatusCode::INTERNAL_SERVER_ERROR),
            });
        });
        return;
    }
    let id = path.parse::<i64>();
    tauri::async_runtime::spawn(async move {
        let Ok(id) = id else { return responder.respond(status(StatusCode::BAD_REQUEST)) };
        let state = app.state::<AppState>();
        let Some(ws) = state.workspace.read().unwrap().clone() else {
            return responder.respond(status(StatusCode::SERVICE_UNAVAILABLE));
        };
        let drivers = state.drivers.clone();
        let Ok(_permit) = ws.thumb_permits.clone().acquire_owned().await else {
            return responder.respond(status(StatusCode::SERVICE_UNAVAILABLE));
        };
        let result = tauri::async_runtime::spawn_blocking(move || ws.thumbnail(&drivers, id)).await;
        let response = match result {
            Ok(Ok(Some((data, mime)))) => Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "max-age=3600")
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .body(data)
                .unwrap(),
            Ok(Ok(None)) => status(StatusCode::NOT_FOUND),
            _ => status(StatusCode::INTERNAL_SERVER_ERROR),
        };
        responder.respond(response);
    });
}

#[derive(Clone, Serialize)]
struct BackfillEvent {
    done: u64,
    finished: bool,
}

/// Generates thumbnails for every item that has none yet. Only one runs at a time.
pub fn spawn_backfill(app: AppHandle, ws: Arc<Workspace>) {
    let state = app.state::<AppState>();
    if state.backfill_running.swap(true, Ordering::AcqRel) {
        return;
    }
    let drivers = state.drivers.clone();
    tauri::async_runtime::spawn(async move {
        let mut done = 0u64;
        // Items of locked shards are left for when they are unlocked.
        let skip = ws.locked_sources();
        let total = ws.read_db.lock().unwrap().count_missing_thumbnails(&skip).ok();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task = crate::tasks::begin_with_cancel("thumbs", "", crate::tasks::Unit::Items, total, Some(cancel.clone()));
        'outer: loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let batch = {
                let db = ws.read_db.lock().unwrap();
                db.missing_thumbnails(64, &skip).unwrap_or_default()
            };
            if batch.is_empty() {
                break;
            }
            let mut tasks = Vec::with_capacity(batch.len());
            for id in batch {
                if ws.closed.load(Ordering::Relaxed) {
                    break 'outer;
                }
                let Ok(permit) = ws.thumb_permits.clone().acquire_owned().await else { break 'outer };
                let (ws, drivers) = (ws.clone(), drivers.clone());
                tasks.push(tauri::async_runtime::spawn_blocking(move || {
                    let _permit = permit;
                    // A read error (e.g. a disconnected share) is recorded as a failed
                    // thumbnail, so the loop always makes progress.
                    if ws.thumbnail(&drivers, id).is_err() {
                        let row = vp_db::ThumbRow::without_data(vp_db::ThumbStatus::Failed);
                        let _ = ws.write_db.lock().unwrap().set_thumbnail(id, &row);
                    }
                }));
            }
            for t in tasks {
                let _ = t.await;
                done += 1;
                task.set(done, None);
            }
            let _ = app.emit("thumbs-progress", BackfillEvent { done, finished: false });
        }
        drop(task);
        let _ = ws.thumbs.sync();
        let _ = app.emit("thumbs-progress", BackfillEvent { done, finished: true });
        app.state::<AppState>().backfill_running.store(false, Ordering::Release);
    });
}
