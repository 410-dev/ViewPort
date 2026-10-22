//! Workspace settings. Every tunable lives here so nothing is hard-coded elsewhere.
//!
//! All structs use `#[serde(default)]`, so a settings file written by an older
//! build (missing fields) or a newer one (extra fields) still loads.

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Phones and tablets: several defaults differ there (memory, touch-sized UI).
const MOBILE: bool = cfg!(any(target_os = "android", target_os = "ios"));

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// Defaults changes applied to this file (see [`Settings::upgrade`]). Missing in older
    /// files, so they read as 0.
    pub revision: u32,
    pub general: GeneralSettings,
    pub cache: CacheSettings,
    pub stats: StatsSettings,
    pub playback: PlaybackSettings,
    pub slideshow: SlideshowSettings,
    pub thumbnails: ThumbnailSettings,
    pub scan: ScanSettings,
    pub grid: GridSettings,
    pub shard: ShardSettings,
    pub keys: KeyBindings,
    pub analysis: AnalysisSettings,
}

/// What image analysis finds in this library and how it groups it. Model packs are named
/// by id; an empty id uses the first installed pack of that kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisSettings {
    /// Find and group faces in photos (people).
    pub faces: bool,
    /// Find and group characters in illustrations.
    pub characters: bool,
    /// Tag whole images (general tags and known character names).
    pub tagger: bool,
    pub face_detector: String,
    pub face_embedder: String,
    pub anime_detector: String,
    pub anime_embedder: String,
    pub tagger_pack: String,
    /// Detections scoring below this are dropped.
    pub detect_score: f32,
    /// Overlapping detections above this IoU are merged.
    pub nms_iou: f32,
    /// Faces smaller than this (shorter edge, in analysed pixels) are ignored: too small to
    /// tell people apart.
    pub min_face_px: u32,
    /// Two faces this similar (cosine) can start the same group.
    pub group_similarity: f32,
    /// A new face this similar to a named person joins them automatically.
    pub assign_similarity: f32,
    /// Smaller groups are not shown (one-off faces in the background).
    pub min_group_size: u32,
    /// Tagger scores at or above this become automatic tags.
    pub tag_threshold: f32,
    /// Tagger character scores at or above this become automatic character names.
    pub character_threshold: f32,
    /// Scores below this are not stored at all (keeps the database small).
    pub keep_score: f32,
    /// A character crop is the detected face grown by this factor (embedders of whole
    /// characters want more than the face).
    pub character_crop_scale: f32,
    /// Edge of the face pictures shown in the people view.
    pub face_crop_px: u32,
    /// What to look for per source (by source id): "auto" (both kinds, the default for
    /// sources not listed), "real" (photos), "anime" (illustrations) or "off". The tagger
    /// runs on every source that is not "off".
    pub source_modes: std::collections::BTreeMap<String, String>,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        Self {
            faces: true,
            characters: true,
            tagger: false,
            face_detector: String::new(),
            face_embedder: String::new(),
            anime_detector: String::new(),
            anime_embedder: String::new(),
            tagger_pack: String::new(),
            detect_score: 0.6,
            nms_iou: 0.4,
            min_face_px: 32,
            group_similarity: 0.45,
            assign_similarity: 0.5,
            min_group_size: 3,
            tag_threshold: 0.35,
            character_threshold: 0.85,
            keep_score: 0.2,
            character_crop_scale: 2.5,
            face_crop_px: 128,
            source_modes: Default::default(),
        }
    }
}

/// The latest [`Settings::revision`].
pub const SETTINGS_REVISION: u32 = 1;

impl Settings {
    /// Brings a settings file up to new defaults where it still holds the old ones (values
    /// the user changed are kept). Returns whether anything changed.
    pub fn upgrade(&mut self) -> bool {
        let before = self.clone();
        if self.revision < 1 {
            // Prefetch went from 3 ahead / 1 behind to 4 each way.
            if (self.cache.prefetch_ahead, self.cache.prefetch_behind) == (3, 1) {
                self.cache.prefetch_ahead = 4;
                self.cache.prefetch_behind = 4;
            }
        }
        self.revision = SETTINGS_REVISION;
        *self != before
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    /// UI and query keyword locale ("ko", "en").
    pub locale: String,
    /// Debounce before writing changes to the bundle.
    pub autosave_debounce_ms: u64,
    /// How often a locked bundle's in-memory database is re-encrypted to disk when changed.
    pub encrypted_save_secs: u64,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self { locale: "ko".into(), autosave_debounce_ms: 500, encrypted_save_secs: 3 }
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
    /// Largest response served for one video range request (cached or streamed).
    pub stream_chunk_bytes: u64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            memory_bytes: 2 * GIB,
            preallocate: !MOBILE,
            chunk_bytes: 256 * 1024,
            autoload_top_n: 30,
            autoload_ratio: 0.5,
            prefetch_ahead: 4,
            prefetch_behind: 4,
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
    /// Superseded by the device's master audio (`AppSettings.audio`); kept so older
    /// settings files round-trip.
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
    /// Record files that are neither images nor videos (always on for shards).
    pub include_nonmedia: bool,
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
            image_extensions: v(&["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "avif", "heic", "heif", "hif"]),
            video_extensions: v(&["mp4", "m4v", "mov", "webm", "mkv", "avi", "wmv", "flv", "ts", "m2ts", "mpg", "mpeg"]),
            include_nonmedia: false,
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
    /// Folder tiles show an N x N grid of thumbnails from inside the folder (0 = icon only).
    pub folder_preview: u32,
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
            folder_preview: 2,
        }
    }
}

/// Shard stores (`.vpshard`): many small files packed into large, optionally encrypted chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShardSettings {
    /// Size of each chunk file of a new shard. At most 4 GiB - 1 so it fits on FAT32.
    pub chunk_bytes: u64,
    /// Encrypted data is sealed in pieces of this size; reads decrypt whole pieces.
    pub segment_bytes: u32,
    /// Save the shard index after this many changes...
    pub snapshot_min_records: u64,
    /// ...once they are also this fraction of the stored files.
    pub snapshot_ratio: f32,
    /// Compact a shard in the background once this fraction of it is garbage.
    pub compact_garbage_ratio: f32,
    pub auto_compact: bool,
    /// When moving files into a shard, read each back and compare before deleting the original.
    pub verify_moves: bool,
}

impl Default for ShardSettings {
    fn default() -> Self {
        Self {
            chunk_bytes: GIB,
            segment_bytes: 64 * 1024,
            snapshot_min_records: 2000,
            snapshot_ratio: 0.25,
            compact_garbage_ratio: 0.3,
            auto_compact: true,
            verify_moves: true,
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
    /// Leave bundles that no longer exist out of the start screen's recent list.
    pub hide_missing_recent: bool,
    pub ui: UiSettings,
    pub progress: ProgressSettings,
    pub audio: AudioSettings,
    pub privacy: PrivacySettings,
    pub windows: WindowSettings,
    pub viewer: ViewerSettings,
    pub ml: MlSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hardware: HardwareSettings::default(),
            reopen_last_bundle: true,
            hide_missing_recent: false,
            ui: UiSettings::default(),
            progress: ProgressSettings::default(),
            audio: AudioSettings::default(),
            privacy: PrivacySettings::default(),
            windows: WindowSettings::default(),
            viewer: ViewerSettings::default(),
            ml: MlSettings::default(),
        }
    }
}

/// Local image analysis (faces, characters, tags) on this device: where ONNX Runtime and
/// the model packs are, and which hardware runs them. Neither is shipped with the app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MlSettings {
    /// ONNX Runtime library (`onnxruntime.dll` / `libonnxruntime.so` / `.dylib`) or the
    /// folder holding it. Empty: the app folder, then `ORT_DYLIB_PATH`, then the system
    /// search path. A CUDA build (onnxruntime-gpu) or a DirectML build (onnxruntime-directml)
    /// decides which GPU paths below can work.
    pub runtime_path: String,
    /// Execution providers to try in order; the first that loads runs the models.
    /// "cuda" (NVIDIA), "directml" (any GPU on Windows), "cpu".
    pub providers: Vec<String>,
    /// GPU index for CUDA / DirectML.
    pub device_id: i32,
    /// Extra folders holding model packs (each pack is a folder with `manifest.json`). The
    /// app folder's `models/` and the config folder's `models/` are always searched.
    pub model_dirs: Vec<String>,
    /// Images are scaled down to this longer edge before analysis.
    pub max_edge_px: u32,
    /// CPU threads per model (0 = ONNX Runtime's choice).
    pub threads: u32,
    /// Analyse new items in the background after scans, while a bundle is open.
    pub auto_analyze: bool,
    /// How many items of what the library shows (in its order) are analysed before the rest.
    pub priority_items: u32,
}

impl Default for MlSettings {
    fn default() -> Self {
        let providers: &[&str] = if cfg!(windows) {
            &["cuda", "directml", "cpu"]
        } else if cfg!(target_os = "linux") {
            &["cuda", "cpu"]
        } else {
            &["cpu"]
        };
        Self {
            runtime_path: String::new(),
            providers: providers.iter().map(|p| p.to_string()).collect(),
            device_id: 0,
            model_dirs: Vec::new(),
            max_edge_px: 1280,
            threads: 0,
            auto_analyze: false,
            priority_items: 500,
        }
    }
}

/// Viewer input and immersive mode on this device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSettings {
    /// Entering fullscreen also hides every viewer control (immersive mode). Escape leaves
    /// immersive mode.
    pub immersive_on_fullscreen: bool,
    /// Zoom factor per key press or wheel notch.
    pub zoom_step: f32,
    /// Largest zoom, relative to fit-to-screen.
    pub max_zoom: f32,
    /// Zoom a double click (or double tap) jumps to.
    pub double_click_zoom: f32,
    /// The mouse wheel zooms images at the cursor.
    pub wheel_zoom: bool,
    /// Touch screens: tapping the image goes to the next item.
    pub tap_to_advance: bool,
    /// Touch screens: swiping an item up marks it as a favorite, down unmarks it.
    pub swipe_favorite: bool,
    /// Two taps within this time are a double tap (zoom), not two single taps; a single
    /// tap acts only after it has passed.
    pub double_tap_ms: u32,
    /// A swipe moves to the neighbouring item when it travels this fraction of the width
    /// (or is flicked); shorter ones spring back.
    pub swipe_threshold: f32,
    /// Duration of the slide to the next item or back after a swipe.
    pub swipe_ms: u32,
    /// Images whose longer edge exceeds this are shown as a copy scaled to it: decoding a
    /// 200-megapixel photo takes the WebView seconds and hundreds of MB. The original loads
    /// only when zooming in past the copy's resolution. 0 always shows originals.
    pub display_max_px: u32,
    /// JPEG quality of those display copies.
    pub display_quality: u8,
    /// Show images no larger than the screen needs (its longer edge in device pixels, at
    /// most `display_max_px`). Sliding a picture several times the screen's size in
    /// means decoding and rasterizing it on the fly, which shows as black or torn tiles.
    /// Zooming in loads larger copies and finally the original.
    pub fit_screen: bool,
    /// The screen size is rounded up to a multiple of this, so window size changes reuse
    /// the same copies.
    pub fit_screen_step_px: u32,
    /// Image formats the WebView cannot show (HEIF/HEIC): they are always shown as a JPEG
    /// copy, decoded by the OS (Android) or FFmpeg.
    pub convert_image_extensions: Vec<String>,
}

impl Default for ViewerSettings {
    fn default() -> Self {
        Self {
            immersive_on_fullscreen: true,
            zoom_step: 1.25,
            max_zoom: 8.0,
            double_click_zoom: 2.5,
            wheel_zoom: true,
            tap_to_advance: true,
            swipe_favorite: true,
            double_tap_ms: 280,
            swipe_threshold: 0.2,
            swipe_ms: 220,
            display_max_px: if MOBILE { 4096 } else { 8192 },
            display_quality: 90,
            fit_screen: true,
            fit_screen_step_px: 512,
            convert_image_extensions: ["heic", "heif", "hif"].map(String::from).to_vec(),
        }
    }
}

/// App-wide (master) audio, shared by every window and split pane.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Master volume, 0.0..=1.0. Remembered across runs.
    pub volume: f32,
    /// Master mute as last left by the user.
    pub muted: bool,
    /// Always start muted, whatever the state was at exit.
    pub start_muted: bool,
    /// Only the focused window / split pane plays sound; the others play muted.
    pub focus_only: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { volume: 1.0, muted: false, start_muted: true, focus_only: true }
    }
}

/// The panic key: mute everything and hide every window at once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacySettings {
    /// Keys (`KeyboardEvent.key` style) that trigger it in any window when pressed twice
    /// within `panic_double_ms`. A single press keeps its normal meaning.
    pub panic_keys: Vec<String>,
    pub panic_double_ms: u32,
    pub panic_mute: bool,
    pub panic_hide: bool,
    /// System-wide shortcut that brings hidden windows back (Tauri accelerator syntax).
    pub restore_shortcut: String,
    /// Tray / menu bar icon for restoring hidden windows. Applies after restart.
    pub tray_icon: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            panic_keys: vec!["Escape".into()],
            panic_double_ms: 400,
            panic_mute: true,
            panic_hide: true,
            restore_shortcut: "CommandOrControl+Shift+Space".into(),
            tray_icon: true,
        }
    }
}

/// Extra windows (desktop) and split view (all platforms; the only option on mobile).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    /// Most viewer panes shown side by side in one window.
    pub max_viewer_panes: u32,
    /// "auto" (by window orientation), "columns" or "rows".
    pub split_layout: String,
    /// Size of newly opened windows in logical pixels.
    pub new_width: u32,
    pub new_height: u32,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self { max_viewer_panes: 3, split_layout: "auto".into(), new_width: 1100, new_height: 760 }
    }
}

/// Look and layout of the app on this device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiSettings {
    /// UI language: "auto" (system), "ko", "en".
    pub locale: String,
    /// Language of query keywords: "auto" follows the UI language.
    pub query_locale: String,
    /// "dark", "light" or "system".
    pub theme: String,
    /// Accent color as `#rrggbb`.
    pub accent: String,
    /// CSS font family list for the UI; empty uses the system sans-serif (Segoe UI /
    /// Malgun Gothic on Windows, SF / Apple SD Gothic Neo on macOS).
    pub font_ui: String,
    /// Font family for the query editor; empty uses the UI font.
    pub font_editor: String,
    /// Base font size in CSS pixels.
    pub font_size: u32,
    /// Main view: "grid", "list" or "tree".
    pub view_mode: String,
    /// Show files of subfolders too when browsing a folder (flat view).
    pub recursive: bool,
    /// Right panel content: "preview", "both" or "info".
    pub panel_mode: String,
    pub panel_visible: bool,
    pub panel_width: u32,
    pub sidebar_width: u32,
    /// Fraction of the right panel height used by the preview in "both" mode.
    pub preview_split: f32,
    /// Row height of the list and tree views.
    pub row_px: u32,
    /// Columns shown in the list and tree views, in order.
    pub columns: Vec<String>,
    /// Play videos in the preview pane automatically (muted).
    pub preview_autoplay: bool,
    /// Delay after the selection changes before the preview loads the original file.
    /// Thumbnails show immediately; this keeps fast keyboard browsing from reading
    /// every file on a slow drive.
    pub preview_delay_ms: u32,
    /// Files larger than this are not loaded into the preview automatically (0 = no limit).
    pub preview_max_bytes: u64,
    /// Turn off UI animations.
    pub reduce_motion: bool,
    /// List only images and videos; other files stay in the library but are hidden.
    pub media_only: bool,
    /// Pause after typing before the search runs.
    pub search_debounce_ms: u32,
    /// Suggestions shown while typing a list (enum) value.
    pub suggest_limit: u32,
    /// Values listed by a value field's dropdown button.
    pub value_list_limit: u32,
    /// How often the library checks whether each source's storage (an external drive,
    /// a shard folder) is connected. Offline sources are struck through and their items
    /// hidden. 0 = only when the window gets focus.
    pub source_check_secs: u32,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            locale: "auto".into(),
            query_locale: "auto".into(),
            theme: "dark".into(),
            accent: "#6b7cff".into(),
            font_ui: String::new(),
            font_editor: String::new(),
            // Phones are read at arm's length and tapped with fingers.
            font_size: if MOBILE { 15 } else { 13 },
            view_mode: "grid".into(),
            recursive: false,
            panel_mode: "both".into(),
            panel_visible: !MOBILE,
            panel_width: 340,
            sidebar_width: 232,
            preview_split: 0.45,
            row_px: if MOBILE { 44 } else { 30 },
            columns: ["name", "kind", "size", "dimensions", "duration", "modified"].map(String::from).to_vec(),
            preview_autoplay: true,
            preview_delay_ms: 150,
            preview_max_bytes: 0,
            reduce_motion: false,
            media_only: false,
            search_debounce_ms: 200,
            suggest_limit: 20,
            value_list_limit: 500,
            source_check_secs: 5,
        }
    }
}

/// How background work and slow reads are reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressSettings {
    /// Minimum time between progress events for one task.
    pub emit_interval_ms: u64,
    /// Reading a file is shown as a task once it has taken this long.
    pub slow_read_ms: u64,
    /// Reads are done in pieces of this size so progress can be reported.
    pub read_piece_bytes: u64,
    /// Finished tasks stay in the task list this long.
    pub keep_finished_secs: u64,
}

impl Default for ProgressSettings {
    fn default() -> Self {
        Self { emit_interval_ms: 150, slow_read_ms: 250, read_piece_bytes: 4 * MIB, keep_finished_secs: 5 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HardwareSettings {
    /// GPU compositing and rasterization in the WebView.
    pub gpu: bool,
    /// GPU rasterization of page content (Chromium WebView on Windows).
    pub gpu_rasterization: bool,
    /// Upload decoded images straight to GPU memory (Chromium WebView on Windows).
    pub zero_copy: bool,
    /// Hardware video decoding (WebView and conversion pipeline).
    pub video_decode: bool,
    /// FFmpeg `-hwaccel` method for thumbnails and conversion input: "auto", "none", or a
    /// specific one ("d3d11va", "dxva2", "cuda", "qsv", "videotoolbox", "vaapi").
    pub ffmpeg_hwaccel: String,
    /// Hardware encoding for conversions, when FFmpeg supports it on this device.
    pub video_encode: bool,
    /// Hardware encoders tried in order before falling back to libx264.
    pub encoders: Vec<String>,
    /// Upper bound for any bundle's memory cache on this device (0 = no limit). Bundle
    /// settings travel between devices; a phone should not take a desktop-sized cache.
    pub cache_limit_bytes: u64,
}

impl Default for HardwareSettings {
    fn default() -> Self {
        let encoders: &[&str] = if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
            &["h264_videotoolbox"]
        } else if cfg!(target_os = "linux") {
            &["h264_nvenc", "h264_qsv", "h264_vaapi"]
        } else if cfg!(target_os = "android") {
            &["h264_mediacodec"]
        } else {
            &["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf"]
        };
        Self {
            gpu: true,
            gpu_rasterization: true,
            zero_copy: true,
            video_decode: true,
            ffmpeg_hwaccel: "auto".into(),
            video_encode: true,
            encoders: encoders.iter().map(|e| e.to_string()).collect(),
            cache_limit_bytes: if MOBILE { 512 * MIB } else { 0 },
        }
    }
}

impl HardwareSettings {
    /// The cache size to use for a bundle that asks for `requested` bytes.
    pub fn cache_bytes(&self, requested: u64) -> u64 {
        if self.cache_limit_bytes == 0 { requested } else { requested.min(self.cache_limit_bytes) }
    }

    /// The `-hwaccel` value to pass to FFmpeg, if any.
    pub fn hwaccel(&self) -> Option<&str> {
        if !self.video_decode {
            return None;
        }
        match self.ffmpeg_hwaccel.trim() {
            "" | "none" => None,
            m => Some(m),
        }
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
    /// Library window.
    pub focus_search: Vec<String>,
    pub query_editor: Vec<String>,
    pub toggle_panel: Vec<String>,
    pub view_grid: Vec<String>,
    pub view_list: Vec<String>,
    pub view_tree: Vec<String>,
    /// Open the focused item in a new viewer window (desktop).
    pub open_new_window: Vec<String>,
    /// Viewer: master mute, add a split pane, move focus to the next pane.
    pub toggle_mute: Vec<String>,
    pub split_view: Vec<String>,
    pub next_pane: Vec<String>,
    /// Viewer: zoom images, and toggle immersive mode (all controls hidden).
    pub zoom_in: Vec<String>,
    pub zoom_out: Vec<String>,
    pub zoom_reset: Vec<String>,
    pub toggle_immersive: Vec<String>,
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
            // A fast double Escape is the panic key (see `PrivacySettings`); one press closes.
            close_viewer: v(&["Escape", "Backspace", "q"]),
            focus_search: v(&["Ctrl+f", "Meta+f"]),
            query_editor: v(&["Ctrl+e", "Meta+e"]),
            toggle_panel: v(&["Ctrl+p", "Meta+p"]),
            view_grid: v(&["Ctrl+1", "Meta+1"]),
            view_list: v(&["Ctrl+2", "Meta+2"]),
            view_tree: v(&["Ctrl+3", "Meta+3"]),
            open_new_window: v(&["Ctrl+Enter", "Meta+Enter"]),
            toggle_mute: v(&["m"]),
            split_view: v(&["v"]),
            next_pane: v(&["Tab"]),
            zoom_in: v(&["=", "+"]),
            zoom_out: v(&["-"]),
            zoom_reset: v(&["0"]),
            toggle_immersive: v(&["h"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrade_moves_old_defaults_only() {
        let mut old = Settings::default();
        old.revision = 0;
        (old.cache.prefetch_ahead, old.cache.prefetch_behind) = (3, 1);
        assert!(old.upgrade());
        assert_eq!((old.cache.prefetch_ahead, old.cache.prefetch_behind, old.revision), (4, 4, SETTINGS_REVISION));

        let mut custom = Settings { revision: 0, ..Default::default() };
        (custom.cache.prefetch_ahead, custom.cache.prefetch_behind) = (2, 0);
        custom.upgrade();
        assert_eq!((custom.cache.prefetch_ahead, custom.cache.prefetch_behind), (2, 0));
        assert!(!custom.upgrade());
    }
}
