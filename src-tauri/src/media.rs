//! `vpmedia://` protocol: serves original media to the viewer with HTTP Range support.
//!
//! Requests are answered from the memory cache when possible. Uncached items that fit
//! the cache are loaded into it first (images, small videos); larger files are streamed
//! from their driver in bounded ranges, which is what `<video>` seeking needs.

use tauri::http::{Request, Response, StatusCode, header};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

use crate::AppState;
use crate::error::{CmdError, CmdResult};
use crate::workspace::Workspace;

pub const SCHEME: &str = "vpmedia";

pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "ts" | "m2ts" => "video/mp2t",
        _ => "application/octet-stream",
    }
}

/// Parses a single `bytes=` range against `len`. Returns an inclusive `(start, end)`.
pub fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (a, b) = spec.split_once('-')?;
    if len == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        (len.saturating_sub(suffix), len - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { len - 1 } else { b.parse::<u64>().ok()?.min(len - 1) };
        (start, end)
    };
    (start <= end && start < len).then_some((start, end))
}

fn status(code: StatusCode) -> Response<Vec<u8>> {
    Response::builder().status(code).body(Vec::new()).unwrap()
}

struct Body {
    data: Vec<u8>,
    start: u64,
    total: u64,
    partial: bool,
    mime: &'static str,
}

fn serve(ws: &Workspace, drivers: &vp_driver::DriverRegistry, id: i64, range: Option<String>) -> CmdResult<Option<Body>> {
    let Some(loc) = ws.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
    let mime = mime_for(&loc.rel_path);
    let stream_max = ws.settings.read().unwrap().cache.stream_chunk_bytes.max(64 * 1024);

    // Images are needed whole, so cache them right away. Videos start streaming at once
    // and are cached in the background (see `handle`).
    let cached = match loc.kind {
        vp_core::MediaKind::Image => ws.load_into_cache(drivers, id)?,
        vp_core::MediaKind::Video => ws.cache.get(id),
    };
    let total = cached.as_ref().map(|c| c.len()).unwrap_or(loc.size);
    let (start, end, partial) = match range.as_deref().and_then(|r| parse_range(r, total)) {
        Some((s, e)) => (s, e, true),
        // No range: whole file if cached, otherwise the first bounded piece.
        None if cached.is_some() => (0, total.saturating_sub(1), false),
        None => (0, total.saturating_sub(1).min(stream_max - 1), true),
    };
    let want = end - start + 1;
    let data = match &cached {
        Some(c) => c.to_vec(start, want),
        None => {
            let reader = ws.open_media(drivers, &loc)?;
            let mut buf = vec![0u8; want.min(stream_max) as usize];
            let n = reader.read_full_at(start, &mut buf)?;
            buf.truncate(n);
            buf
        }
    };
    let partial = partial || (data.len() as u64) < total;
    Ok(Some(Body { data, start, total, partial, mime }))
}

pub fn handle<R: Runtime>(ctx: UriSchemeContext<'_, R>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = ctx.app_handle().clone();
    let id = request.uri().path().trim_start_matches('/').parse::<i64>();
    let range = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    tauri::async_runtime::spawn_blocking(move || {
        let Ok(id) = id else { return responder.respond(status(StatusCode::BAD_REQUEST)) };
        let state = app.state::<AppState>();
        let Some(ws) = state.workspace.read().unwrap().clone() else {
            return responder.respond(status(StatusCode::SERVICE_UNAVAILABLE));
        };
        let response = match serve(&ws, &state.drivers, id, range) {
            Ok(Some(b)) => {
                let end = b.start + b.data.len().max(1) as u64 - 1;
                let mut r = Response::builder()
                    .header(header::CONTENT_TYPE, b.mime)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                    .header(header::CONTENT_LENGTH, b.data.len());
                if b.partial {
                    r = r
                        .status(StatusCode::PARTIAL_CONTENT)
                        .header(header::CONTENT_RANGE, format!("bytes {}-{}/{}", b.start, end, b.total));
                }
                r.body(b.data).unwrap()
            }
            Ok(None) => status(StatusCode::NOT_FOUND),
            Err(CmdError { code: "driver", .. }) => status(StatusCode::BAD_GATEWAY),
            Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
        };
        responder.respond(response);
        if !ws.cache.contains(id) {
            crate::commands::spawn_prefetch(&app, &ws, vec![id]);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-", 100), Some((0, 99)));
        assert_eq!(parse_range("bytes=10-19", 100), Some((10, 19)));
        assert_eq!(parse_range("bytes=90-200", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Some((90, 99)));
        assert_eq!(parse_range("bytes=100-", 100), None);
        assert_eq!(parse_range("bytes=5-1", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
    }
}
