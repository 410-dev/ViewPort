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

/// Serves a converted copy (from the bundle or a shard), cached under the negated media id.
fn serve_variant(ws: &Workspace, id: i64, reader: Box<dyn vp_driver::MediaReader>, range: Option<&str>) -> CmdResult<Body> {
    let key = -id;
    let cached = match ws.cache.get(key) {
        Some(c) => Some(c),
        None => ws.cache.insert_with(key, reader.len(), |off, buf| {
            let n = reader.read_full_at(off, buf)?;
            if n < buf.len() { Err(std::io::ErrorKind::UnexpectedEof.into()) } else { Ok(()) }
        })?,
    };
    let total = match &cached {
        Some(c) => c.len(),
        None => reader.len(),
    };
    let stream_max = ws.settings.read().unwrap().cache.stream_chunk_bytes.max(64 * 1024);
    let (start, end) = range.and_then(|r| parse_range(r, total)).unwrap_or((0, total.saturating_sub(1)));
    let want = (end - start + 1).min(stream_max);
    let data = match &cached {
        Some(c) => c.to_vec(start, want),
        None => {
            let mut buf = vec![0u8; want as usize];
            let n = reader.read_full_at(start, &mut buf)?;
            buf.truncate(n);
            buf
        }
    };
    let partial = range.is_some() || (data.len() as u64) < total;
    Ok(Body { data, start, total, partial, mime: "video/mp4" })
}

/// `fit=<px>` from a query string: the viewer asks for a display copy of a huge image.
pub fn fit_param(query: Option<&str>) -> Option<u32> {
    query?.split('&').find_map(|kv| kv.strip_prefix("fit=")).and_then(|v| v.parse().ok()).filter(|&v: &u32| v > 0)
}

/// Cache key of the display copy of `id` at `edge` px. Originals use `id`, converted
/// videos `-id`; ids stay below 2^40 and edges below 2^14.
fn display_key(id: i64, edge: u32) -> i64 {
    -((1i64 << 62) | ((edge.min(16383) as i64) << 40) | (id & ((1i64 << 40) - 1)))
}

/// Largest display copy edge (`display_key` keeps 14 bits of it).
const MAX_DISPLAY_EDGE: u32 = 16383;

/// Whether the WebView cannot show images like `rel_path` (`viewer.convert_image_extensions`).
fn needs_conversion(rel_path: &str) -> bool {
    let Some((_, ext)) = rel_path.rsplit_once('.') else { return false };
    crate::recent::cached_app_settings().viewer.convert_image_extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// One display copy is made at a time: decoding a huge image takes a lot of memory.
static DISPLAY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Image `id` scaled to fit `edge` px (EXIF orientation applied), made once and kept in the
/// memory cache. The OS decoder subsamples while decoding where available (Android);
/// elsewhere the original is decoded and scaled. `None` if the image cannot be decoded.
fn display_copy(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    loc: &vp_db::MediaLocation,
    edge: u32,
) -> CmdResult<Option<Vec<u8>>> {
    let key = display_key(id, edge);
    if let Some(c) = ws.cache.get(key) {
        return Ok(Some(c.to_vec(0, c.len())));
    }
    let _one = DISPLAY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = ws.cache.get(key) {
        return Ok(Some(c.to_vec(0, c.len())));
    }
    let quality = crate::recent::cached_app_settings().viewer.display_quality.clamp(1, 100);
    let source =
        ws.read_db.lock().unwrap().source(loc.source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    let local = ws.driver(drivers, &source)?.local_path(&loc.rel_path);
    let mut data = local.and_then(|p| crate::platform::scaled_image(&p, edge, quality)).map(|s| s.data);
    if data.is_none() {
        let original = match ws.load_into_cache(drivers, id)? {
            Some(c) => c.to_vec(0, c.len()),
            None => crate::workspace::read_tracked(ws.open_media(drivers, loc)?.as_ref(), &loc.rel_path, Some(id))?,
        };
        let s = vp_core::settings::ThumbnailSettings { max_edge_px: edge, quality, ..Default::default() };
        data = match vp_thumb::generate(&original, &s) {
            Ok(t) => Some(t.data),
            // HEIF/HEIC and other formats only the OS or FFmpeg decode.
            Err(_) => ws.foreign_image(drivers, id, loc, Some(&original), edge, quality)?.map(|t| t.data),
        };
    }
    let Some(data) = data else { return Ok(None) };
    let _ = ws.cache.insert_with(key, data.len() as u64, |off, buf| {
        buf.copy_from_slice(&data[off as usize..off as usize + buf.len()]);
        Ok(())
    });
    Ok(Some(data))
}

/// Whether image `id` exceeds `edge` px. The viewer asks for display copies of images whose
/// size the scan could not read too; those are checked here (JPEG frame header), and an
/// unknown size counts as large.
fn larger_than(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    loc: &vp_db::MediaLocation,
    edge: u32,
) -> CmdResult<bool> {
    let known = ws.read_db.lock().unwrap().media_brief(&[id])?.pop().and_then(|b| b.width.zip(b.height));
    let size = match known {
        Some(s) => Some(s),
        None => {
            let reader = ws.open_media(drivers, loc)?;
            let mut read = |off: u64, buf: &mut [u8]| reader.read_full_at(off, buf).is_ok_and(|n| n == buf.len());
            vp_scan::jpeg_size(reader.len(), &mut read)
        }
    };
    Ok(size.is_none_or(|(w, h)| w.max(h) > edge))
}

fn serve(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    range: Option<String>,
    fit: Option<u32>,
) -> CmdResult<Option<Body>> {
    let Some(loc) = ws.read_db.lock().unwrap().media_location(id)? else { return Ok(None) };
    // Formats the WebView cannot decode are always served as a copy, at full size when
    // no size was asked for.
    let convert = loc.kind == vp_core::MediaKind::Image && needs_conversion(&loc.rel_path);
    let edge = match fit.filter(|_| loc.kind == vp_core::MediaKind::Image) {
        Some(e) if convert || larger_than(ws, drivers, id, &loc, e)? => Some(e),
        None if convert => Some(MAX_DISPLAY_EDGE),
        _ => None,
    };
    if let Some(edge) = edge
        && let Some(data) = display_copy(ws, drivers, id, &loc, edge)?
    {
        let mime = if data.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
        let total = data.len() as u64;
        return Ok(Some(Body { data, start: 0, total, partial: false, mime }));
    }
    let mode = ws.settings.read().unwrap().playback.fallback_mode;
    if loc.kind == vp_core::MediaKind::Video && mode != vp_core::settings::VideoFallbackMode::Never
        && let Some(reader) = ws.variant_reader(id)? {
            return serve_variant(ws, id, reader, range.as_deref()).map(Some);
        }
    let mime = mime_for(&loc.rel_path);
    let stream_max = ws.settings.read().unwrap().cache.stream_chunk_bytes.max(64 * 1024);

    // Images are needed whole, so cache them right away. Videos start streaming at once
    // and are cached in the background (see `handle`).
    let cached = match loc.kind {
        vp_core::MediaKind::Image => ws.load_into_cache(drivers, id)?,
        vp_core::MediaKind::Video | vp_core::MediaKind::NonMedia => ws.cache.get(id),
    };
    let total = cached.as_ref().map(|c| c.len()).unwrap_or(loc.size);
    let (start, end, partial) = match range.as_deref().and_then(|r| parse_range(r, total)) {
        Some((s, e)) => (s, e, true),
        // No range: whole file if cached, otherwise the first bounded piece.
        None if cached.is_some() => (0, total.saturating_sub(1), false),
        None => (0, total.saturating_sub(1).min(stream_max - 1), true),
    };
    let want = match loc.kind {
        vp_core::MediaKind::Video | vp_core::MediaKind::NonMedia => (end - start + 1).min(stream_max),
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
    let fit = fit_param(request.uri().query());
    tauri::async_runtime::spawn_blocking(move || responder.respond(answer(&app, id, range, fit)));
}

/// The response for media `id` (with an optional `Range` header, and `fit` for a display
/// copy of a huge image), shared by the `vpmedia://` protocol and the loopback server
/// (`media_server.rs`).
pub fn answer<R: Runtime>(app: &AppHandle<R>, id: Option<i64>, range: Option<String>, fit: Option<u32>) -> Response<Vec<u8>> {
    let Some(id) = id else { return status(StatusCode::BAD_REQUEST) };
    let state = app.state::<AppState>();
    let Some(ws) = state.workspace.read().unwrap().clone() else {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    };
    // A panic must still answer: a dropped responder leaves the WebView request hanging.
    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve(&ws, &state.drivers, id, range, fit)))
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
    // A display copy stands in for the original, which need not be cached for it.
    if fit.is_none() && !ws.cache.contains(id) {
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

    #[test]
    fn fit_and_display_keys() {
        assert_eq!(fit_param(Some("fit=4096")), Some(4096));
        assert_eq!(fit_param(Some("v=3&fit=2048")), Some(2048));
        assert_eq!(fit_param(Some("fit=0")), None);
        assert_eq!(fit_param(Some("v=1")), None);
        assert_eq!(fit_param(None), None);
        // Never collides with originals (id) or converted copies (-id).
        let k = display_key(27, 4096);
        assert!(k < 0 && k != -27);
        assert_ne!(display_key(27, 4096), display_key(27, 8192));
        assert_ne!(display_key(27, 4096), display_key(28, 4096));
    }
}
