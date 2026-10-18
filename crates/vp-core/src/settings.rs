//! Workspace settings. Every tunable lives here so nothing is hard-coded elsewhere.
//!
//! All structs use `#[serde(default)]`, so a settings file written by an older
//! build (missing fields) or a newer one (extra fields) still loads.

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub general: GeneralSettings,
    pub cache: CacheSettings,
    pub stats: StatsSettings,
    pub playback: PlaybackSettings,
    pub slideshow: SlideshowSettings,
    pub thumbnails: ThumbnailSettings,
    pub scan: ScanSettings,
    pub grid: GridSettings,
    pub keys: KeyBindings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// UI and query keyword locale ("ko", "en").
    pub locale: String,
    /// Debounce before writing changes to the bundle.
    pub autosave_debounce_ms: u64,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self { locale: "ko".into(), autosave_debounce_ms: 500 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    /// Total in-memory media cache size.
    pub memory_bytes: u64,
    /// Allocate and commit the whole cache when a bundle opens, so the memory is
    /// reserved up front. Off by default on mobile, where the OS may kill the app.
    pub preallocate: bool,
    /// Cache allocation unit. Smaller wastes less on small files; larger means fewer chunks.
    pub chunk_bytes: u64,
    /// Number of most-viewed items to preload when a bundle opens.
    pub autoload_top_n: u32,
    /// Fraction of `memory_bytes` the startup preload may use.
    pub autoload_ratio: f32,
    /// Items to prefetch ahead / behind the current carousel position.
    pub prefetch_ahead: u32,
    pub prefetch_behind: u32,
    /// Concurrent prefetch / preload reads.
    pub prefetch_workers: u32,
    /// Files larger than this are streamed instead of cached whole.
    pub max_item_bytes: u64,
    /// Largest response served for one range request when streaming uncached files.
    pub stream_chunk_bytes: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            memory_bytes: 2 * GIB,
            preallocate: !cfg!(any(target_os = "android", target_os = "ios")),
            chunk_bytes: 256 * 1024,
            autoload_top_n: 30,
            autoload_ratio: 0.5,
            prefetch_ahead: 3,
            prefetch_behind: 1,
            prefetch_workers: 2,
            max_item_bytes: GIB,
            stream_chunk_bytes: 4 * MIB,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsSettings {
    /// Popularity halves after this many days without views.
    pub half_life_days: f64,
    /// Views shorter than this are not counted.
    pub min_view_ms: u64,
}

impl Default for StatsSettings {
    fn default() -> Self {
        Self { half_life_days: 14.0, min_view_ms: 700 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoFallbackMode {
    /// Use the WebView player; fall back to remux/transcode only when it cannot play the file.
    Auto,
    /// Always route video through the fallback pipeline.
    Always,
    /// Never fall back; unsupported files show an error banner.
    Never,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlaybackSettings {
    /// Seek step for Shift + arrow / Shift + A/D.
    pub seek_step_secs: f64,
    pub fallback_mode: VideoFallbackMode,
    /// Suggest converting unsupported files to a high-compatibility format.
    pub suggest_conversion: bool,
    /// Initial volume, 0.0..=1.0.
    pub volume: f32,
    pub muted: bool,
    /// FFmpeg executable; empty means look next to the app, then on PATH.
    pub ffmpeg_path: String,
}

impl Default for PlaybackSettings {
    fn default() -> Self {
        Self {
            seek_step_secs: 5.0,
            fallback_mode: VideoFallbackMode::Auto,
            suggest_conversion: true,
            volume: 1.0,
            muted: false,
            ffmpeg_path: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlideshowVideoMode {
    /// Play the whole video (or its auto-play loop segment) `count` times, then advance.
    PlayCount,
    /// Play `clip_secs` starting at `clip_start_secs`, then advance.
    Clip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlideshowSettings {
    /// Time each image stays on screen.
    pub image_interval_secs: f64,
    pub video_mode: SlideshowVideoMode,
    /// Plays per video in `PlayCount` mode.
    pub video_play_count: u32,
    /// Start of the clip in `Clip` mode; negative counts from the end.
    pub clip_start_secs: f64,
    pub clip_secs: f64,
    /// Restart from the first item after the last one.
    pub wrap_around: bool,
}

impl Default for SlideshowSettings {
    fn default() -> Self {
        Self {
            image_interval_secs: 5.0,
            video_mode: SlideshowVideoMode::PlayCount,
            video_play_count: 1,
            clip_start_secs: 0.0,
            clip_secs: 10.0,
            wrap_around: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThumbnailFormat {
    /// Lossy JPEG; images with transparency are stored as PNG.
    Jpeg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThumbnailSettings {
    /// Longest edge of generated thumbnails in pixels.
    pub max_edge_px: u32,
    pub format: ThumbnailFormat,
    /// Encoder quality, 0..=100.
    pub quality: u8,
    /// Size of each thumbnail pack file in the bundle.
    pub pack_bytes: u64,
    /// Background generation workers; 0 = number of CPUs / 2.
    pub workers: u32,
    /// Position in a video to grab the thumbnail frame from, as a fraction of its length.
    pub video_frame_position: f32,
    /// Number of thumbnails the grid keeps decoded in memory.
    pub grid_memory_items: u32,
    /// Images larger than this are not decoded for thumbnails.
    pub max_source_bytes: u64,
}

impl Default for ThumbnailSettings {
    fn default() -> Self {
        Self {
            max_edge_px: 320,
            format: ThumbnailFormat::Jpeg,
            quality: 80,
            pack_bytes: 256 * MIB,
            workers: 0,
            video_frame_position: 0.1,
            grid_memory_items: 2000,
            max_source_bytes: 512 * MIB,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanSettings {
    /// Lowercase extensions (without dot) treated as images.
    pub image_extensions: Vec<String>,
    /// Lowercase extensions (without dot) treated as videos.
    pub video_extensions: Vec<String>,
    /// Skip files and folders whose name starts with '.'.
    pub skip_hidden: bool,
    /// Bytes read from the start of an image to find its dimensions.
    pub header_probe_bytes: u32,
    /// Rows written per database transaction while scanning.
    pub batch_size: u32,
    /// Generate missing thumbnails in the background after a scan.
    pub thumbnails_after_scan: bool,
}

impl Default for ScanSettings {
    fn default() -> Self {
        let v = |exts: &[&str]| exts.iter().map(|e| e.to_string()).collect();
        Self {
            image_extensions: v(&["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "avif", "heic", "heif"]),
            video_extensions: v(&["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "ts", "m2ts", "mpg", "mpeg"]),
            skip_hidden: true,
            header_probe_bytes: 64 * 1024,
            batch_size: 500,
            thumbnails_after_scan: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GridSettings {
    /// Tile edge in CSS pixels.
    pub tile_px: u32,
    pub min_tile_px: u32,
    pub max_tile_px: u32,
    /// Rows rendered above and below the viewport.
    pub overscan_rows: u32,
    /// Show file names under tiles.
    pub show_names: bool,
    /// Sorts applied when a query has none, written as query text (any locale).
    /// The last sort is the most significant key.
    pub default_sort: String,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            tile_px: 180,
            min_tile_px: 80,
            max_tile_px: 480,
            overscan_rows: 3,
            show_names: true,
            default_sort: "sort(name) AND sort(folder) AND sort(source)".into(),
        }
    }
}

/// Settings that belong to this device rather than to a bundle. Stored in the app's
/// config directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub hardware: HardwareSettings,
    /// Open the most recent bundle when the app starts (so its cache warms up right away).
    pub reopen_last_bundle: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self { hardware: HardwareSettings::default(), reopen_last_bundle: true }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HardwareSettings {
    /// GPU compositing and rasterization in the WebView.
    pub gpu: bool,
    /// Hardware video decoding (WebView and conversion pipeline).
    pub video_decode: bool,
    /// Hardware encoding for conversions, when FFmpeg supports it on this device.
    pub video_encode: bool,
}

impl Default for HardwareSettings {
    fn default() -> Self {
        Self { gpu: true, video_decode: true, video_encode: true }
    }
}

/// Key bindings by action. Values are `KeyboardEvent.key`-style names, with
/// optional `Shift+` / `Ctrl+` / `Alt+` / `Meta+` prefixes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyBindings {
    pub next: Vec<String>,
    pub prev: Vec<String>,
    pub seek_forward: Vec<String>,
    pub seek_backward: Vec<String>,
    pub toggle_fullscreen: Vec<String>,
    pub toggle_play: Vec<String>,
    pub toggle_slideshow: Vec<String>,
    pub toggle_info: Vec<String>,
    pub toggle_loops: Vec<String>,
    pub close_viewer: Vec<String>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        let v = |keys: &[&str]| keys.iter().map(|k| k.to_string()).collect();
        Self {
            next: v(&["ArrowRight", "d"]),
            prev: v(&["ArrowLeft", "a"]),
            seek_forward: v(&["Shift+ArrowRight", "Shift+d"]),
            seek_backward: v(&["Shift+ArrowLeft", "Shift+a"]),
            toggle_fullscreen: v(&["f", "F11"]),
            toggle_play: v(&[" "]),
            toggle_slideshow: v(&["s"]),
            toggle_info: v(&["i"]),
            toggle_loops: v(&["l"]),
            close_viewer: v(&["Escape"]),
        }
    }
}
