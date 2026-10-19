/**
 * Settings panel layout as data. Each field points at a path in the bundle settings
 * (`bundle`) or the device settings (`app`); the panel renders and saves them. Adding a
 * setting means adding the Rust field (vp-core/settings.rs) and one entry here.
 */
import type { Key } from "../i18n.svelte";
import type { IconName } from "../ui/Icon.svelte";

export type Target = "bundle" | "app";

interface Base {
  path: string;
  target: Target;
  label: Key;
  help?: Key;
}

export type Field =
  | (Base & { kind: "toggle" })
  | (Base & { kind: "number"; min?: number; max?: number; step?: number; scale?: number; unit?: string })
  | (Base & { kind: "select"; options: { value: string; label: Key | { raw: string } }[] })
  | (Base & { kind: "text"; placeholder?: string })
  | (Base & { kind: "list"; placeholder?: string })
  | (Base & { kind: "color" })
  | (Base & { kind: "columns" })
  | { kind: "custom"; id: "cache" | "ffmpeg" | "lock"; label?: Key };

export interface Group {
  title?: Key;
  note?: Key;
  fields: Field[];
}

export interface Section {
  id: string;
  icon: IconName;
  title: Key;
  groups: Group[];
}

const MB = 1024 ** 2;
const GB = 1024 ** 3;

const app = (f: Omit<Base, "target">) => ({ ...f, target: "app" as const });
const bundle = (f: Omit<Base, "target">) => ({ ...f, target: "bundle" as const });

const key = (path: string, label: Key): Field => ({ kind: "list", ...bundle({ path: `keys.${path}`, label }) });

/** Settings with no effect on phones and tablets (WebView switches, windows, tray, FFmpeg). */
export const DESKTOP_ONLY = new Set([
  "privacy.panic_hide",
  "privacy.restore_shortcut",
  "privacy.tray_icon",
  "windows.new_width",
  "windows.new_height",
  "hardware.gpu",
  "hardware.gpu_rasterization",
  "hardware.zero_copy",
  "hardware.ffmpeg_hwaccel",
  "hardware.video_encode",
  "hardware.encoders",
  "playback.ffmpeg_path",
]);

export const SECTIONS: Section[] = [
  {
    id: "general",
    icon: "globe",
    title: "set.general",
    groups: [
      {
        title: "set.language",
        fields: [
          { kind: "select", ...app({ path: "ui.locale", label: "set.uiLocale", help: "set.uiLocale.help" }), options: [] },
          { kind: "select", ...app({ path: "ui.query_locale", label: "set.queryLocale", help: "set.queryLocale.help" }), options: [] },
        ],
      },
      {
        title: "set.startup",
        fields: [
          { kind: "toggle", ...app({ path: "reopen_last_bundle", label: "set.reopen", help: "set.reopen.help" }) },
          { kind: "toggle", ...app({ path: "hide_missing_recent", label: "start.hideMissing" }) },
          { kind: "number", ...bundle({ path: "general.autosave_debounce_ms", label: "set.autosaveDelay" }), min: 0, step: 100, unit: "ms" },
        ],
      },
      {
        title: "set.stats",
        fields: [
          { kind: "number", ...bundle({ path: "stats.half_life_days", label: "set.halfLife", help: "set.halfLife.help" }), min: 1, step: 1, unit: "d" },
          { kind: "number", ...bundle({ path: "stats.min_view_ms", label: "set.minView" }), min: 0, step: 100, unit: "ms" },
        ],
      },
    ],
  },
  {
    id: "appearance",
    icon: "palette",
    title: "set.appearance",
    groups: [
      {
        fields: [
          {
            kind: "select",
            ...app({ path: "ui.theme", label: "set.theme" }),
            options: [
              { value: "dark", label: "set.theme.dark" },
              { value: "light", label: "set.theme.light" },
              { value: "system", label: "set.theme.system" },
            ],
          },
          { kind: "color", ...app({ path: "ui.accent", label: "set.accent" }) },
          { kind: "toggle", ...app({ path: "ui.reduce_motion", label: "set.reduceMotion" }) },
        ],
      },
      {
        title: "set.fonts",
        note: "set.fonts.note",
        fields: [
          { kind: "text", ...app({ path: "ui.font_ui", label: "set.fontUi" }), placeholder: "Segoe UI, Malgun Gothic" },
          { kind: "text", ...app({ path: "ui.font_editor", label: "set.fontEditor", help: "set.fontEditor.help" }), placeholder: "—" },
          { kind: "number", ...app({ path: "ui.font_size", label: "set.fontSize" }), min: 11, max: 20, step: 1, unit: "px" },
        ],
      },
    ],
  },
  {
    id: "browse",
    icon: "grid",
    title: "set.browse",
    groups: [
      {
        title: "set.views",
        fields: [
          {
            kind: "select",
            ...app({ path: "ui.view_mode", label: "set.viewMode" }),
            options: [
              { value: "grid", label: "view.grid" },
              { value: "list", label: "view.list" },
              { value: "tree", label: "view.tree" },
            ],
          },
          { kind: "toggle", ...app({ path: "ui.recursive", label: "set.recursive", help: "set.recursive.help" }) },
          { kind: "toggle", ...app({ path: "ui.media_only", label: "set.mediaOnly", help: "set.mediaOnly.help" }) },
          { kind: "number", ...app({ path: "ui.search_debounce_ms", label: "set.searchDelay" }), min: 0, max: 2000, step: 50, unit: "ms" },
          { kind: "text", ...bundle({ path: "grid.default_sort", label: "set.defaultSort", help: "set.defaultSort.help" }) },
        ],
      },
      {
        title: "set.grid",
        fields: [
          { kind: "toggle", ...bundle({ path: "grid.show_names", label: "set.showNames" }) },
          { kind: "number", ...bundle({ path: "grid.min_tile_px", label: "set.minTile" }), min: 48, max: 400, step: 8, unit: "px" },
          { kind: "number", ...bundle({ path: "grid.max_tile_px", label: "set.maxTile" }), min: 96, max: 1024, step: 8, unit: "px" },
          { kind: "number", ...bundle({ path: "grid.overscan_rows", label: "set.overscan", help: "set.overscan.help" }), min: 0, max: 20, step: 1 },
        ],
      },
      {
        title: "set.listTree",
        fields: [
          { kind: "number", ...app({ path: "ui.row_px", label: "set.rowHeight" }), min: 22, max: 56, step: 2, unit: "px" },
          { kind: "columns", ...app({ path: "ui.columns", label: "set.columns" }) },
        ],
      },
    ],
  },
  {
    id: "preview",
    icon: "eye",
    title: "set.preview",
    groups: [
      {
        fields: [
          { kind: "toggle", ...app({ path: "ui.preview_autoplay", label: "set.previewAutoplay" }) },
          { kind: "number", ...app({ path: "ui.preview_delay_ms", label: "set.previewDelay", help: "set.previewDelay.help" }), min: 0, max: 2000, step: 50, unit: "ms" },
          {
            kind: "number",
            ...app({ path: "ui.preview_max_bytes", label: "set.previewMax", help: "set.previewMax.help" }),
            min: 0,
            step: 10,
            scale: MB,
            unit: "MB",
          },
        ],
      },
    ],
  },
  {
    id: "playback",
    icon: "play",
    title: "set.playback",
    groups: [
      {
        fields: [
          { kind: "number", ...bundle({ path: "playback.seek_step_secs", label: "set.seekStep" }), min: 1, step: 1, unit: "s" },
        ],
      },
      {
        title: "set.compat",
        fields: [
          { kind: "custom", id: "ffmpeg" },
          {
            kind: "select",
            ...bundle({ path: "playback.fallback_mode", label: "set.fallback" }),
            options: [
              { value: "auto", label: "set.fallback.auto" },
              { value: "always", label: "set.fallback.always" },
              { value: "never", label: "set.fallback.never" },
            ],
          },
          { kind: "toggle", ...bundle({ path: "playback.suggest_conversion", label: "set.suggestConvert" }) },
          { kind: "text", ...bundle({ path: "playback.ffmpeg_path", label: "set.ffmpegPath", help: "set.ffmpegPath.help" }), placeholder: "auto" },
        ],
      },
    ],
  },
  {
    id: "audio",
    icon: "volume",
    title: "set.audio",
    groups: [
      {
        note: "set.audio.note",
        fields: [
          { kind: "toggle", ...app({ path: "audio.start_muted", label: "set.startMuted", help: "set.startMuted.help" }) },
          { kind: "toggle", ...app({ path: "audio.focus_only", label: "set.focusOnly", help: "set.focusOnly.help" }) },
        ],
      },
    ],
  },
  {
    id: "privacy",
    icon: "eyeOff",
    title: "set.privacy",
    groups: [
      {
        note: "set.privacy.note",
        fields: [
          { kind: "list", ...app({ path: "privacy.panic_keys", label: "set.panicKeys", help: "set.panicKeys.help" }) },
          { kind: "number", ...app({ path: "privacy.panic_double_ms", label: "set.panicDouble", help: "set.panicDouble.help" }), min: 150, max: 1500, step: 50, unit: "ms" },
          { kind: "toggle", ...app({ path: "privacy.panic_mute", label: "set.panicMute" }) },
          { kind: "toggle", ...app({ path: "privacy.panic_hide", label: "set.panicHide" }) },
          { kind: "text", ...app({ path: "privacy.restore_shortcut", label: "set.restoreShortcut", help: "set.restoreShortcut.help" }), placeholder: "CommandOrControl+Shift+Space" },
          { kind: "toggle", ...app({ path: "privacy.tray_icon", label: "set.trayIcon", help: "set.trayIcon.help" }) },
        ],
      },
    ],
  },
  {
    id: "windows",
    icon: "columns",
    title: "set.windows",
    groups: [
      {
        title: "set.split",
        fields: [
          { kind: "number", ...app({ path: "windows.max_viewer_panes", label: "set.maxPanes" }), min: 1, max: 3, step: 1 },
          {
            kind: "select",
            ...app({ path: "windows.split_layout", label: "set.splitLayout" }),
            options: [
              { value: "auto", label: "set.splitLayout.auto" },
              { value: "columns", label: "set.splitLayout.columns" },
              { value: "rows", label: "set.splitLayout.rows" },
            ],
          },
        ],
      },
      {
        title: "set.newWindows",
        note: "set.newWindows.note",
        fields: [
          { kind: "number", ...app({ path: "windows.new_width", label: "set.newWidth" }), min: 480, step: 20, unit: "px" },
          { kind: "number", ...app({ path: "windows.new_height", label: "set.newHeight" }), min: 360, step: 20, unit: "px" },
        ],
      },
    ],
  },
  {
    id: "viewer",
    icon: "eye",
    title: "set.viewer",
    groups: [
      {
        title: "set.immersive",
        note: "set.immersive.note",
        fields: [{ kind: "toggle", ...app({ path: "viewer.immersive_on_fullscreen", label: "set.immersiveOnFullscreen" }) }],
      },
      {
        title: "set.zoom",
        fields: [
          { kind: "number", ...app({ path: "viewer.zoom_step", label: "set.zoomStep" }), min: 1.05, max: 3, step: 0.05, unit: "×" },
          { kind: "number", ...app({ path: "viewer.max_zoom", label: "set.maxZoom" }), min: 1.5, max: 32, step: 0.5, unit: "×" },
          { kind: "number", ...app({ path: "viewer.double_click_zoom", label: "set.doubleClickZoom" }), min: 1.25, max: 16, step: 0.25, unit: "×" },
          { kind: "toggle", ...app({ path: "viewer.wheel_zoom", label: "set.wheelZoom" }) },
          { kind: "toggle", ...app({ path: "viewer.tap_to_advance", label: "set.tapAdvance", help: "set.tapAdvance.help" }) },
          { kind: "number", ...app({ path: "viewer.double_tap_ms", label: "set.doubleTap", help: "set.doubleTap.help" }), min: 120, max: 800, step: 20, unit: "ms" },
          { kind: "number", ...app({ path: "viewer.swipe_threshold", label: "set.swipeThreshold", help: "set.swipeThreshold.help" }), min: 5, max: 60, step: 5, scale: 0.01, unit: "%" },
          { kind: "number", ...app({ path: "viewer.swipe_ms", label: "set.swipeMs" }), min: 0, max: 1000, step: 20, unit: "ms" },
          { kind: "number", ...app({ path: "viewer.display_max_px", label: "set.displayMax", help: "set.displayMax.help" }), min: 0, max: 16384, step: 256, unit: "px" },
          { kind: "number", ...app({ path: "viewer.display_quality", label: "set.displayQuality" }), min: 1, max: 100, step: 1 },
        ],
      },
    ],
  },
  {
    id: "slideshow",
    icon: "film",
    title: "set.slideshow",
    groups: [
      {
        fields: [
          { kind: "number", ...bundle({ path: "slideshow.image_interval_secs", label: "set.ssImage" }), min: 0.5, step: 0.5, unit: "s" },
          {
            kind: "select",
            ...bundle({ path: "slideshow.video_mode", label: "set.ssVideo" }),
            options: [
              { value: "play_count", label: "set.ssVideo.count" },
              { value: "clip", label: "set.ssVideo.clip" },
            ],
          },
          { kind: "number", ...bundle({ path: "slideshow.video_play_count", label: "set.ssPlays" }), min: 1, step: 1 },
          { kind: "number", ...bundle({ path: "slideshow.clip_start_secs", label: "set.ssClipStart", help: "set.ssClipStart.help" }), step: 1, unit: "s" },
          { kind: "number", ...bundle({ path: "slideshow.clip_secs", label: "set.ssClipLen" }), min: 1, step: 1, unit: "s" },
          { kind: "toggle", ...bundle({ path: "slideshow.wrap_around", label: "set.ssWrap" }) },
        ],
      },
    ],
  },
  {
    id: "thumbs",
    icon: "image",
    title: "set.thumbs",
    groups: [
      {
        fields: [
          { kind: "number", ...bundle({ path: "thumbnails.max_edge_px", label: "set.thumbSize" }), min: 64, max: 1024, step: 32, unit: "px" },
          { kind: "number", ...bundle({ path: "thumbnails.quality", label: "set.thumbQuality" }), min: 1, max: 100, step: 1 },
          { kind: "number", ...bundle({ path: "thumbnails.workers", label: "set.thumbWorkers", help: "set.thumbWorkers.help" }), min: 0, max: 64, step: 1 },
          {
            kind: "number",
            ...bundle({ path: "thumbnails.video_frame_position", label: "set.thumbFrame" }),
            min: 0,
            max: 100,
            step: 5,
            scale: 0.01,
            unit: "%",
          },
          { kind: "number", ...bundle({ path: "thumbnails.max_source_bytes", label: "set.thumbMaxSource" }), min: 1, step: 16, scale: MB, unit: "MB" },
        ],
      },
    ],
  },
  {
    id: "scan",
    icon: "scan",
    title: "set.scan",
    groups: [
      {
        fields: [
          { kind: "list", ...bundle({ path: "scan.image_extensions", label: "set.imageExt" }) },
          { kind: "list", ...bundle({ path: "scan.video_extensions", label: "set.videoExt" }) },
          { kind: "toggle", ...bundle({ path: "scan.skip_hidden", label: "set.skipHidden" }) },
          { kind: "toggle", ...bundle({ path: "scan.include_nonmedia", label: "set.includeNonmedia", help: "set.includeNonmedia.help" }) },
          { kind: "toggle", ...bundle({ path: "scan.thumbnails_after_scan", label: "set.thumbsAfterScan" }) },
          { kind: "number", ...bundle({ path: "scan.batch_size", label: "set.batchSize", help: "set.batchSize.help" }), min: 10, step: 50 },
          { kind: "number", ...bundle({ path: "scan.header_probe_bytes", label: "set.probeBytes" }), min: 4, step: 4, scale: 1024, unit: "KB" },
        ],
      },
    ],
  },
  {
    id: "shard",
    icon: "archive",
    title: "set.shard",
    groups: [
      {
        title: "set.shardNew",
        fields: [
          {
            kind: "number",
            ...bundle({ path: "shard.chunk_bytes", label: "set.shardChunk", help: "set.shardChunk.help" }),
            min: 1,
            max: 4095,
            step: 64,
            scale: MB,
            unit: "MB",
          },
          {
            kind: "number",
            ...bundle({ path: "shard.segment_bytes", label: "set.shardSegment", help: "set.shardSegment.help" }),
            min: 1,
            max: 1024,
            step: 16,
            scale: 1024,
            unit: "KB",
          },
        ],
      },
      {
        title: "set.shardUpkeep",
        fields: [
          { kind: "toggle", ...bundle({ path: "shard.verify_moves", label: "set.shardVerifyMoves", help: "set.shardVerifyMoves.help" }) },
          { kind: "toggle", ...bundle({ path: "shard.auto_compact", label: "set.shardAutoCompact" }) },
          {
            kind: "number",
            ...bundle({ path: "shard.compact_garbage_ratio", label: "set.shardCompactRatio", help: "set.shardCompactRatio.help" }),
            min: 5,
            max: 90,
            step: 5,
            scale: 0.01,
            unit: "%",
          },
          {
            kind: "number",
            ...bundle({ path: "shard.snapshot_min_records", label: "set.shardSnapshot", help: "set.shardSnapshot.help" }),
            min: 100,
            step: 500,
          },
        ],
      },
    ],
  },
  {
    id: "cache",
    icon: "database",
    title: "set.cache",
    groups: [
      { fields: [{ kind: "custom", id: "cache" }] },
      {
        title: "set.memory",
        note: "set.memory.note",
        fields: [
          { kind: "number", ...bundle({ path: "cache.memory_bytes", label: "set.cacheSize" }), min: 0.25, step: 0.25, scale: GB, unit: "GB" },
          {
            kind: "number",
            ...app({ path: "hardware.cache_limit_bytes", label: "set.cacheLimit", help: "set.cacheLimit.help" }),
            min: 0,
            step: 64,
            scale: 1024 ** 2,
            unit: "MB",
          },
          { kind: "toggle", ...bundle({ path: "cache.preallocate", label: "set.preallocate", help: "set.preallocate.help" }) },
          { kind: "number", ...bundle({ path: "cache.chunk_bytes", label: "set.chunk" }), min: 64, step: 64, scale: 1024, unit: "KB" },
          { kind: "number", ...bundle({ path: "cache.max_item_bytes", label: "set.maxItem" }), min: 1, step: 16, scale: MB, unit: "MB" },
          { kind: "number", ...bundle({ path: "cache.stream_chunk_bytes", label: "set.streamChunk" }), min: 0.25, step: 0.25, scale: MB, unit: "MB" },
        ],
      },
      {
        title: "set.prefetch",
        fields: [
          { kind: "number", ...bundle({ path: "cache.autoload_top_n", label: "set.autoloadN" }), min: 0, step: 5 },
          { kind: "number", ...bundle({ path: "cache.autoload_ratio", label: "set.autoloadRatio" }), min: 0, max: 100, step: 5, scale: 0.01, unit: "%" },
          { kind: "number", ...bundle({ path: "cache.prefetch_ahead", label: "set.prefetchAhead" }), min: 0, max: 20, step: 1 },
          { kind: "number", ...bundle({ path: "cache.prefetch_behind", label: "set.prefetchBehind" }), min: 0, max: 20, step: 1 },
          { kind: "number", ...bundle({ path: "cache.prefetch_workers", label: "set.prefetchWorkers" }), min: 1, max: 16, step: 1 },
        ],
      },
    ],
  },
  {
    id: "hardware",
    icon: "cpu",
    title: "set.hardware",
    groups: [
      {
        title: "set.hwRender",
        note: "set.restartNote",
        fields: [
          { kind: "toggle", ...app({ path: "hardware.gpu", label: "set.gpu", help: "set.gpu.help" }) },
          { kind: "toggle", ...app({ path: "hardware.gpu_rasterization", label: "set.gpuRaster", help: "set.gpuRaster.help" }) },
          { kind: "toggle", ...app({ path: "hardware.zero_copy", label: "set.zeroCopy", help: "set.zeroCopy.help" }) },
        ],
      },
      {
        title: "set.hwVideo",
        fields: [
          { kind: "toggle", ...app({ path: "hardware.video_decode", label: "set.hwDecode", help: "set.hwDecode.help" }) },
          {
            kind: "select",
            ...app({ path: "hardware.ffmpeg_hwaccel", label: "set.hwaccel", help: "set.hwaccel.help" }),
            options: [
              { value: "auto", label: "set.hwaccel.auto" },
              { value: "none", label: "set.hwaccel.none" },
              { value: "d3d11va", label: { raw: "D3D11VA (Windows)" } },
              { value: "dxva2", label: { raw: "DXVA2 (Windows)" } },
              { value: "cuda", label: { raw: "CUDA (NVIDIA)" } },
              { value: "qsv", label: { raw: "Quick Sync (Intel)" } },
              { value: "videotoolbox", label: { raw: "VideoToolbox (macOS)" } },
              { value: "vaapi", label: { raw: "VA-API (Linux)" } },
            ],
          },
          { kind: "toggle", ...app({ path: "hardware.video_encode", label: "set.hwEncode" }) },
          { kind: "list", ...app({ path: "hardware.encoders", label: "set.encoders", help: "set.encoders.help" }) },
        ],
      },
    ],
  },
  {
    id: "progress",
    icon: "activity",
    title: "set.progress",
    groups: [
      {
        fields: [
          { kind: "number", ...app({ path: "progress.slow_read_ms", label: "set.slowRead", help: "set.slowRead.help" }), min: 0, step: 50, unit: "ms" },
          { kind: "number", ...app({ path: "progress.read_piece_bytes", label: "set.readPiece", help: "set.readPiece.help" }), min: 0.25, step: 0.25, scale: MB, unit: "MB" },
          { kind: "number", ...app({ path: "progress.emit_interval_ms", label: "set.emitInterval" }), min: 16, step: 10, unit: "ms" },
          { kind: "number", ...app({ path: "progress.keep_finished_secs", label: "set.keepFinished" }), min: 0, step: 1, unit: "s" },
        ],
      },
    ],
  },
  {
    id: "keys",
    icon: "keyboard",
    title: "set.keys",
    groups: [
      {
        title: "set.keysLibrary",
        note: "set.keys.note",
        fields: [
          key("focus_search", "key.focus_search"),
          key("query_editor", "key.query_editor"),
          key("toggle_panel", "key.toggle_panel"),
          key("view_grid", "key.view_grid"),
          key("view_list", "key.view_list"),
          key("view_tree", "key.view_tree"),
          key("open_new_window", "key.open_new_window"),
        ],
      },
      {
        title: "set.keysViewer",
        fields: [
          key("next", "key.next"),
          key("prev", "key.prev"),
          key("seek_forward", "key.seek_forward"),
          key("seek_backward", "key.seek_backward"),
          key("toggle_play", "key.toggle_play"),
          key("toggle_fullscreen", "key.toggle_fullscreen"),
          key("toggle_slideshow", "key.toggle_slideshow"),
          key("toggle_info", "key.toggle_info"),
          key("toggle_loops", "key.toggle_loops"),
          key("close_viewer", "key.close_viewer"),
          key("toggle_mute", "key.toggle_mute"),
          key("split_view", "key.split_view"),
          key("next_pane", "key.next_pane"),
          key("zoom_in", "key.zoom_in"),
          key("zoom_out", "key.zoom_out"),
          key("zoom_reset", "key.zoom_reset"),
          key("toggle_immersive", "key.toggle_immersive"),
        ],
      },
    ],
  },
  {
    id: "security",
    icon: "shield",
    title: "set.security",
    groups: [{ fields: [{ kind: "custom", id: "lock" }] }],
  },
];
