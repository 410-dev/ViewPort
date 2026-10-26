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

/// Where an item is and how large, for the viewer.
#[derive(Debug, Clone)]
pub struct ViewLoc {
    pub loc: vp_db::MediaLocation,
    pub size: Option<(u32, u32)>,
}

/// Entries kept in `Workspace::view_locations` before it starts over.
const MAX_VIEW_LOCATIONS: usize = 20_000;

/// Looks item `id` up for the viewer: on its own connection, and when that is busy, from
/// what it found before (an item moved meanwhile then fails to load, as it would anyway).
fn locate(ws: &Workspace, id: i64) -> CmdResult<Option<ViewLoc>> {
    let db = ws.view_db.as_ref().unwrap_or(&*ws.read_db);
    let guard = match db.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            if let Some(v) = ws.view_locations.lock().unwrap().get(&id).cloned() {
                STAGES.with(|s| s.borrow_mut().push(("db_busy_cached", std::time::Duration::ZERO)));
                return Ok(Some(v));
            }
            stage("db_lock", || db.lock().unwrap_or_else(|e| e.into_inner()))
        }
    };
    let found = stage("db_query", || -> CmdResult<Option<ViewLoc>> {
        let Some(loc) = guard.media_location(id)? else { return Ok(None) };
        let size = if loc.kind == vp_core::MediaKind::Image { guard.media_dimensions(id)? } else { None };
        Ok(Some(ViewLoc { loc, size }))
    })?;
    drop(guard);
    if let Some(v) = &found {
        let mut cache = ws.view_locations.lock().unwrap();
        if cache.len() >= MAX_VIEW_LOCATIONS {
            cache.clear();
        }
        cache.insert(id, v.clone());
    }
    Ok(found)
}

/// Looks up many items at once for the viewer (its look-ahead), so its requests find them
/// even while the database is busy.
pub fn remember_locations(ws: &Workspace, ids: &[i64]) {
    let Ok(found) = ws.view_db.as_ref().unwrap_or(&*ws.read_db).lock().unwrap_or_else(|e| e.into_inner()).media_view_locations(ids) else {
        return;
    };
    let mut cache = ws.view_locations.lock().unwrap();
    if cache.len() + found.len() > MAX_VIEW_LOCATIONS {
        cache.clear();
    }
    for (id, loc, size) in found {
        cache.insert(id, ViewLoc { loc, size });
    }
}

/// The driver of a source, without the database when it is already connected.
fn source_driver(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    source_id: i64,
) -> CmdResult<std::sync::Arc<dyn vp_driver::MediaDriver>> {
    if let Some(d) = ws.connected_driver(source_id) {
        return Ok(d);
    }
    // First use of this source since the bundle opened.
    let source = ws.read_db.lock().unwrap().source(source_id)?.ok_or_else(|| CmdError::new("not_found", "source"))?;
    ws.driver(drivers, &source)
}

/// Display copies are made one at a time for the viewer and one for its look-ahead
/// (decoding a huge image takes a lot of memory), so a picture the viewer waits for never
/// queues behind one it may want later.
static DISPLAY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static LOOKAHEAD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Display copies being made now (cache keys).
static MAKING: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

struct Making(i64);
impl Drop for Making {
    fn drop(&mut self) {
        MAKING.lock().unwrap_or_else(|e| e.into_inner()).retain(|k| *k != self.0);
    }
}

thread_local! {
    /// Where the time of the request on this thread went, for the slow-view log.
    static STAGES: std::cell::RefCell<Vec<(&'static str, std::time::Duration)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Runs `f` as stage `name` of the request, for the slow-view log.
fn stage<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
    let start = std::time::Instant::now();
    let r = f();
    STAGES.with(|s| s.borrow_mut().push((name, start.elapsed())));
    r
}

/// Image `id` scaled to fit `edge` px (EXIF orientation applied), made once and kept in the
/// memory cache. The OS decoder subsamples while decoding where available (Android);
/// elsewhere the original is decoded and scaled. `None` if the image cannot be decoded.
fn display_copy(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    loc: &vp_db::MediaLocation,
    edge: u32,
    urgent: bool,
) -> CmdResult<Option<Vec<u8>>> {
    let key = display_key(id, edge);
    if let Some(c) = ws.cache.get(key) {
        return Ok(Some(c.to_vec(0, c.len())));
    }
    let _one = stage("copy_wait", || {
        // The other queue is making this very copy: wait for it rather than decode twice.
        while MAKING.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if urgent {
            DISPLAY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
        } else {
            crate::sched::wait_urgent(std::time::Duration::from_secs(5));
            LOOKAHEAD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
        }
    });
    if let Some(c) = ws.cache.get(key) {
        return Ok(Some(c.to_vec(0, c.len())));
    }
    MAKING.lock().unwrap_or_else(|e| e.into_inner()).push(key);
    let _making = Making(key);
    stage("copy_make", || make_display_copy(ws, drivers, id, loc, edge, key))
}

fn make_display_copy(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    loc: &vp_db::MediaLocation,
    edge: u32,
    key: i64,
) -> CmdResult<Option<Vec<u8>>> {
    let quality = crate::recent::cached_app_settings().viewer.display_quality.clamp(1, 100);
    let local = source_driver(ws, drivers, loc.source_id)?.local_path(&loc.rel_path);
    let mut data = local.and_then(|p| crate::platform::scaled_image(&p, edge, quality)).map(|s| s.data);
    if data.is_none() {
        let original = match ws.load_into_cache_at(drivers, id, loc)? {
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
    loc: &vp_db::MediaLocation,
    known: Option<(u32, u32)>,
    edge: u32,
) -> CmdResult<bool> {
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

/// The window the viewer draws in (device pixels), for the size of display copies.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Screen {
    pub width: f64,
    pub height: f64,
}

/// Edge of the display copy the viewer asks for first (its `defaultTier`): the size the
/// image is drawn at when fitted to `screen`, rounded up to `fit_screen_step_px`, at most
/// `display_max_px` (0: originals).
fn display_edge(v: &vp_core::settings::ViewerSettings, screen: Screen, size: Option<(u32, u32)>) -> u32 {
    let max = v.display_max_px;
    if max == 0 || !v.fit_screen {
        return max;
    }
    let step = v.fit_screen_step_px.max(64) as f64;
    let (sw, sh) = (screen.width.max(1.0), screen.height.max(1.0));
    let need = match size {
        Some((w, h)) if w > 0 && h > 0 => {
            let (w, h) = (w as f64, h as f64);
            let fit = (sw / w).min(sh / h).max((sh / w).min(sw / h));
            w.max(h) * fit.min(1.0)
        }
        _ => sw.max(sh),
    };
    max.min(((need / step).ceil() * step) as u32)
}

/// Loads what the viewer will ask for item `id` into the memory cache (the original, or
/// the display copy `screen` calls for), or marks it recently used if it is there.
/// Returns the bytes it holds in the cache (0 if it could not be cached).
pub fn warm(ws: &Workspace, drivers: &vp_driver::DriverRegistry, id: i64, screen: Option<Screen>) -> CmdResult<u64> {
    let Some(ViewLoc { loc, size }) = locate(ws, id)? else { return Ok(0) };
    if loc.kind == vp_core::MediaKind::Image {
        let convert = needs_conversion(&loc.rel_path);
        let v = crate::recent::cached_app_settings().viewer;
        let edge = match screen {
            Some(s) => display_edge(&v, s, size),
            None => v.display_max_px,
        };
        // As `serve` decides it for `displayUrl(brief, edge)`.
        let copy = match edge {
            0 if convert => Some(MAX_DISPLAY_EDGE),
            0 => None,
            e if convert || larger_than(ws, drivers, &loc, size, e)? => Some(e),
            _ => None,
        };
        if let Some(edge) = copy {
            return Ok(display_copy(ws, drivers, id, &loc, edge, false)?.map_or(0, |d| d.len() as u64));
        }
    }
    Ok(ws.load_into_cache_at(drivers, id, &loc)?.map_or(0, |c| c.len()))
}

fn serve(
    ws: &Workspace,
    drivers: &vp_driver::DriverRegistry,
    id: i64,
    range: Option<String>,
    fit: Option<u32>,
) -> CmdResult<Option<Body>> {
    let Some(ViewLoc { loc, size: known }) = locate(ws, id)? else { return Ok(None) };
    // Formats the WebView cannot decode are always served as a copy, at full size when
    // no size was asked for.
    let convert = loc.kind == vp_core::MediaKind::Image && needs_conversion(&loc.rel_path);
    let edge = match fit.filter(|_| loc.kind == vp_core::MediaKind::Image) {
        Some(e) if convert || stage("size", || larger_than(ws, drivers, &loc, known, e))? => Some(e),
        None if convert => Some(MAX_DISPLAY_EDGE),
        _ => None,
    };
    if let Some(edge) = edge
        && let Some(data) = display_copy(ws, drivers, id, &loc, edge, true)?
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
        vp_core::MediaKind::Image => stage("load", || ws.load_into_cache_at(drivers, id, &loc))?,
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
        None => stage("read", || -> CmdResult<Vec<u8>> {
            let reader = ws.open_media(drivers, &loc)?;
            let mut buf = vec![0u8; want.min(stream_max) as usize];
            let n = reader.read_full_at(start, &mut buf)?;
            buf.truncate(n);
            Ok(buf)
        })?,
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
    let started = std::time::Instant::now();
    let busy = crate::sched::busy_now();
    let was_cached = ws.cache.contains(id);
    STAGES.with(|s| s.borrow_mut().clear());
    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::sched::urgent(|| crate::sched::run(crate::sched::Class::Viewer, || serve(&ws, &state.drivers, id, range.clone(), fit)))
    }))
    .unwrap_or_else(|_| Err(CmdError::new("panic", "media handler panicked")));
    let slow_ms = crate::recent::cached_app_settings().log.slow_view_ms as u128;
    let took = started.elapsed();
    if slow_ms > 0 && took.as_millis() >= slow_ms {
        let stages: Vec<String> =
            STAGES.with(|s| s.borrow().iter().map(|(n, d)| format!("{n} {}ms", d.as_millis())).collect());
        crate::applog::info(
            "viewer",
            format!(
                "slow item {id}{}{}: {}ms [{}] busy before: {} original cached: {}",
                fit.map(|f| format!(" fit {f}")).unwrap_or_default(),
                range.as_deref().map(|r| format!(" {r}")).unwrap_or_default(),
                took.as_millis(),
                stages.join(", "),
                if busy.is_empty() { "-" } else { &busy },
                was_cached,
            ),
        );
    }
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
