//! `vpmedia://` protocol: serves original media to the viewer with HTTP Range support.
//!
//! Requests are answered from the memory cache when possible. Uncached items that fit
//! the cache are loaded into it first (images, small videos); larger files are streamed
//! from their driver in bounded ranges, which is what `<video>` seeking needs. Video
//! responses are always bounded, cached or not: the player asks for the next range
//! anyway, and Android's WebView copies every response body into one Java array.

use tauri::http::{Request, Response, StatusCode, header};
use tauri::{AppHandle, Manager, Runtime, UriSchemeContext, UriSchemeResponder};

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

/// Serves a converted copy from the bundle, cached under the negated media id.
fn serve_variant(ws: &Workspace, id: i64, path: &std::path::Path, range: Option<&str>) -> CmdResult<Body> {
    let key = -id;
    let cached = match ws.cache.get(key) {
        Some(c) => Some(c),
        None => {
            let file = std::fs::File::open(path)?;
            let len = file.metadata()?.len();
            let reader = LocalFile(file);
            ws.cache.insert_with(key, len, |off, buf| {
                let n = vp_driver::MediaReader::read_full_at(&reader, off, buf)?;
                if n < buf.len() { Err(std::io::ErrorKind::UnexpectedEof.into()) } else { Ok(()) }
            })?
        }
    };
    let total = match &cached {
        Some(c) => c.len(),
        None => std::fs::metadata(path)?.len(),
    };
    let stream_max = ws.settings.read().unwrap().cache.stream_chunk_bytes.max(64 * 1024);
    let (start, end) = range.and_then(|r| parse_range(r, total)).unwrap_or((0, total.saturating_sub(1)));
    let want = (end - start + 1).min(stream_max);
    let data = match &cached {
        Some(c) => c.to_vec(start, want),
        None => {
            let reader = LocalFile(std::fs::File::open(path)?);
            let mut buf = vec![0u8; want as usize];
            let n = vp_driver::MediaReader::read_full_at(&reader, start, &mut buf)?;
            buf.truncate(n);
            buf
        }
    };
    let partial = range.is_some() || (data.len() as u64) < total;
    Ok(Body { data, start, total, partial, mime: "video/mp4" })
}

/// Minimal positional reader over a local file.
struct LocalFile(std::fs::File);

impl vp_driver::MediaReader for LocalFile {
    fn len(&self) -> u64 {
        self.0.metadata().map(|m| m.len()).unwrap_or(0)
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.0, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(&self.0, buf, offset)
    }
}

fn serve(ws: &Workspace, drivers: &vp_driver::DriverRegistry, id: i64, range: Option<String>) -> CmdResult<Option<Body>> {
    let Some(loc) = ws.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
    let mode = ws.settings.read().unwrap().playback.fallback_mode;
    if loc.kind == vp_core::MediaKind::Video && mode != vp_core::settings::VideoFallbackMode::Never
        && let Some(path) = ws.variant_path(id)? {
            return serve_variant(ws, id, &path, range.as_deref()).map(Some);
        }
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
    let want = match loc.kind {
        vp_core::MediaKind::Video => (end - start + 1).min(stream_max),
        _ => end - start + 1,
    };
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
    let id = request.uri().path().trim_start_matches('/').parse::<i64>().ok();
    let range = request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).map(str::to_string);
    tauri::async_runtime::spawn_blocking(move || responder.respond(answer(&app, id, range)));
}

/// The response for media `id` (and an optional `Range` header), shared by the
/// `vpmedia://` protocol and the loopback server (`media_server.rs`).
pub fn answer<R: Runtime>(app: &AppHandle<R>, id: Option<i64>, range: Option<String>) -> Response<Vec<u8>> {
    let Some(id) = id else { return status(StatusCode::BAD_REQUEST) };
    let state = app.state::<AppState>();
    let Some(ws) = state.workspace.read().unwrap().clone() else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    // A panic must still answer: a dropped responder leaves the WebView request hanging.
    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&ws, &state.drivers, id, range)))
        .unwrap_or_else(|_| Err(CmdError::new("panic", "media handler panicked")));
    let response = match served {
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
    if !ws.cache.contains(id) {
        crate::commands::spawn_prefetch(app, &ws, vec![id]);
    }
    response
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
