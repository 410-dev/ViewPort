import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type OpenMode = "autosave" | "manual_save_only";

export interface BundleSummary {
  path: string;
  name: string;
  mode: OpenMode;
  bundle_version: number;
  locked: boolean;
}

export interface CmdError {
  code: string;
  message: string;
  detail: any;
}

export interface SourceRow {
  id: number;
  name: string;
  driver: string;
  root: string;
  enabled: boolean;
}

export interface MediaBrief {
  id: number;
  name: string;
  kind: "image" | "video";
  width: number | null;
  height: number | null;
  duration_ms: number | null;
  thumb: "ok" | "failed" | "unsupported" | null;
  compatible: boolean | null;
  has_variant: boolean;
  ignore_warning: boolean;
  size: number;
  created_ms: number | null;
  modified_ms: number | null;
  folder: string;
  ext: string;
}

export interface LoopSegment {
  id: number;
  media_id: number;
  label: string;
  start_ms: number;
  end_ms: number;
  autoplay: boolean;
}

export interface ConfigField {
  key: string;
  label: string;
  kind: { type: "text" | "secret" | "integer" | "bool" | "file_path" | "choice"; min?: number; max?: number; options?: string[] };
  required: boolean;
  default: any;
}

export interface DriverKind {
  kind: string;
  fields: ConfigField[];
}

export interface ConvertEvent {
  id: number;
  progress: number;
  done: boolean;
  error: string | null;
}

export interface FfmpegStatus {
  available: boolean;
  /** The OS media framework probes videos and grabs frames without FFmpeg (Android). */
  os_media: boolean;
  path: string | null;
  version: string | null;
}

export interface SearchResult {
  ids: number[];
  elapsed_ms: number;
}

export interface ScanEvent {
  source_id: number;
  dirs: number;
  files: number;
  added: number;
  updated: number;
  unchanged: number;
  missing: number | null;
  current: string;
  errors: string[];
  done: boolean;
  error: string | null;
}

export interface ThumbsEvent {
  done: number;
  finished: boolean;
}

export interface GridSettings {
  tile_px: number;
  min_tile_px: number;
  max_tile_px: number;
  overscan_rows: number;
  show_names: boolean;
}

export interface CacheSettings {
  memory_bytes: number;
  preallocate: boolean;
  chunk_bytes: number;
  autoload_top_n: number;
  autoload_ratio: number;
  prefetch_ahead: number;
  prefetch_behind: number;
  prefetch_workers: number;
  max_item_bytes: number;
  stream_chunk_bytes: number;
}

export interface KeyBindings {
  next: string[];
  prev: string[];
  seek_forward: string[];
  seek_backward: string[];
  toggle_fullscreen: string[];
  toggle_play: string[];
  toggle_slideshow: string[];
  toggle_info: string[];
  toggle_loops: string[];
  close_viewer: string[];
  focus_search: string[];
  query_editor: string[];
  toggle_panel: string[];
  view_grid: string[];
  view_list: string[];
  view_tree: string[];
  open_new_window: string[];
  toggle_mute: string[];
  split_view: string[];
  next_pane: string[];
  zoom_in: string[];
  zoom_out: string[];
  zoom_reset: string[];
  toggle_immersive: string[];
}

export interface Settings {
  general: { locale: string; autosave_debounce_ms: number };
  grid: GridSettings;
  cache: CacheSettings;
  playback: {
    seek_step_secs: number;
    fallback_mode: string;
    suggest_conversion: boolean;
    volume: number;
    muted: boolean;
    ffmpeg_path: string;
  };
  keys: KeyBindings;
  [section: string]: any;
}

export interface UiSettings {
  locale: string;
  query_locale: string;
  theme: "dark" | "light" | "system";
  accent: string;
  font_ui: string;
  font_editor: string;
  font_size: number;
  view_mode: "grid" | "list" | "tree";
  recursive: boolean;
  panel_mode: "preview" | "both" | "info";
  panel_visible: boolean;
  panel_width: number;
  sidebar_width: number;
  preview_split: number;
  row_px: number;
  columns: string[];
  preview_autoplay: boolean;
  preview_delay_ms: number;
  preview_max_bytes: number;
  reduce_motion: boolean;
  search_debounce_ms: number;
}

export interface AppSettings {
  hardware: {
    gpu: boolean;
    gpu_rasterization: boolean;
    zero_copy: boolean;
    video_decode: boolean;
    ffmpeg_hwaccel: string;
    video_encode: boolean;
    encoders: string[];
    /** Upper bound for any bundle's memory cache on this device (0 = no limit). */
    cache_limit_bytes: number;
  };
  reopen_last_bundle: boolean;
  ui: UiSettings;
  progress: { emit_interval_ms: number; slow_read_ms: number; read_piece_bytes: number; keep_finished_secs: number };
  audio: { volume: number; muted: boolean; start_muted: boolean; focus_only: boolean };
  privacy: { panic_keys: string[]; panic_double_ms: number; panic_mute: boolean; panic_hide: boolean; restore_shortcut: string; tray_icon: boolean };
  windows: { max_viewer_panes: number; split_layout: "auto" | "columns" | "rows"; new_width: number; new_height: number };
  viewer: {
    immersive_on_fullscreen: boolean;
    zoom_step: number;
    max_zoom: number;
    double_click_zoom: number;
    wheel_zoom: boolean;
    tap_to_advance: boolean;
    double_tap_ms: number;
    swipe_threshold: number;
    swipe_ms: number;
    display_max_px: number;
    display_quality: number;
  };
}

export interface AudioState {
  volume: number;
  muted: boolean;
  focus: string | null;
}

/** What an extra window was opened to show. */
export type WindowPayload =
  | { kind: "viewer"; ids: number[]; index: number }
  | { kind: "library"; sourceId: number | null; folder: string; query: string };

export type TaskState = "running" | "done" | "failed" | "cancelled";

export interface TaskInfo {
  id: number;
  kind: string;
  label: string;
  detail: string;
  media_id: number | null;
  done: number;
  total: number | null;
  unit: "items" | "bytes" | "permille";
  state: TaskState;
  error: string | null;
  cancellable: boolean;
  started_ms: number;
  rate: number;
}

export type FieldType =
  | { type: "text" }
  | { type: "integer" }
  | { type: "real" }
  | { type: "date" }
  | { type: "size" }
  | { type: "duration" }
  | { type: "kind" }
  | { type: "enum" }
  | { type: "ranged_int"; min: number; max: number };

export interface FieldInfo {
  key: string;
  names: [string, string][];
  aliases: string[];
  ty: FieldType;
  builtin: boolean;
}

export type ExtValue = number | string;

export interface MediaDetail {
  id: number;
  source_id: number;
  source_name: string;
  rel_path: string;
  name: string;
  kind: "image" | "video";
  size: number;
  created_ms: number | null;
  modified_ms: number | null;
  added_ms: number;
  duration_ms: number | null;
  width: number | null;
  height: number | null;
  view_count: number;
  last_viewed_ms: number | null;
  values: Record<string, ExtValue[]>;
}

export interface Vocabulary {
  fields: { key: string; name: string; aliases: string[]; ty: FieldType; ext: boolean }[];
  ops: { op: string; name: string }[];
  and: string;
  or: string;
  not: string;
  sort_asc: string;
  sort_desc: string;
  kinds: string[];
}

export interface SavedQuery {
  id: number;
  label: string;
  text: string;
}

export type QueryCheck =
  | { status: "ok"; normalized: string }
  | { status: "error"; error: { kind: { code: string }; span: { start: number; end: number } }; span: { start: number; end: number } };

export interface CacheStats {
  capacity_bytes: number;
  allocated_bytes: number;
  used_bytes: number;
  reserved_bytes: number;
  items: number;
  hits: number;
  misses: number;
}

export const api = {
  createBundle: (path: string, name: string) => invoke<BundleSummary>("create_bundle", { path, name }),
  openBundle: (path: string, allowNewer = false) => invoke<BundleSummary>("open_bundle", { path, allowNewer }),
  closeBundle: () => invoke<void>("close_bundle"),
  currentBundle: () => invoke<BundleSummary | null>("current_bundle"),
  recentBundles: () => invoke<string[]>("recent_bundles"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  listSources: () => invoke<SourceRow[]>("list_sources"),
  addLocalSource: (path: string) => invoke<SourceRow>("add_local_source", { path }),
  removeSource: (id: number) => invoke<void>("remove_source", { id }),
  scanSource: (id: number) => invoke<void>("scan_source", { id }),
  cancelScan: (id: number) => invoke<void>("cancel_scan", { id }),
  search: (query: string, sourceId: number | null, folder: string | null = null, recursive = false) =>
    invoke<SearchResult>("search", { query, sourceId, folder, recursive, tzOffsetMinutes: -new Date().getTimezoneOffset() }),
  childFolders: (sourceId: number, parent: string) =>
    invoke<{ name: string; count: number }[]>("child_folders", { sourceId, parent }),
  mediaBrief: (ids: number[]) => invoke<MediaBrief[]>("media_brief", { ids }),
  saveBundle: () => invoke<void>("save_bundle"),
  prefetch: (ids: number[]) => invoke<void>("prefetch", { ids }),
  recordView: (id: number, viewMs: number) => invoke<void>("record_view", { id, viewMs }),
  cacheStats: () => invoke<CacheStats>("cache_stats"),
  listFields: () => invoke<FieldInfo[]>("list_fields"),
  createField: (names: [string, string][], aliases: string[], ty: FieldType) =>
    invoke<string>("create_field", { names, aliases, ty }),
  renameField: (key: string, names: [string, string][], aliases: string[]) =>
    invoke<void>("rename_field", { key, names, aliases }),
  deleteField: (key: string) => invoke<void>("delete_field", { key }),
  mediaDetail: (id: number) => invoke<MediaDetail | null>("media_detail", { id }),
  setValues: (ids: number[], key: string, values: ExtValue[]) => invoke<void>("set_values", { ids, key, values }),
  addValue: (ids: number[], key: string, value: ExtValue) => invoke<void>("add_value", { ids, key, value }),
  removeValue: (ids: number[], key: string, value: ExtValue) => invoke<void>("remove_value", { ids, key, value }),
  suggestValues: (key: string, prefix: string) => invoke<string[]>("suggest_values", { key, prefix }),
  queryVocabulary: (locale: string) => invoke<Vocabulary>("query_vocabulary", { locale }),
  listSavedQueries: (locale: string) => invoke<SavedQuery[]>("list_saved_queries", { locale }),
  saveQuery: (id: number | null, label: string, text: string) => invoke<number>("save_query", { id, label, text }),
  deleteSavedQuery: (id: number) => invoke<void>("delete_saved_query", { id }),
  checkQuery: (src: string, locale: string) => invoke<QueryCheck>("check_query", { src, locale }),
  ffmpegStatus: () => invoke<FfmpegStatus>("ffmpeg_status"),
  convertMedia: (ids: number[]) => invoke<void>("convert_media", { ids }),
  cancelConvert: (id: number) => invoke<void>("cancel_convert", { id }),
  incompatibleVideos: () => invoke<number[]>("incompatible_videos"),
  setIgnoreWarning: (id: number, ignore: boolean) => invoke<void>("set_ignore_warning", { id, ignore }),
  probeMedia: (id: number) => invoke<MediaBrief | null>("probe_media", { id }),
  loopSegments: (id: number) => invoke<LoopSegment[]>("loop_segments", { id }),
  saveLoopSegment: (segment: LoopSegment) => invoke<number>("save_loop_segment", { segment }),
  deleteLoopSegment: (id: number) => invoke<void>("delete_loop_segment", { id }),
  driverKinds: () => invoke<DriverKind[]>("driver_kinds"),
  testSource: (driver: string, root: string, config: Record<string, any>) => invoke<number>("test_source", { driver, root, config }),
  addSource: (driver: string, name: string, root: string, config: Record<string, any>) =>
    invoke<SourceRow>("add_source", { driver, name, root, config }),
  updateSource: (id: number, name: string, root: string, config: Record<string, any> | null, enabled: boolean) =>
    invoke<SourceRow>("update_source", { id, name, root, config, enabled }),
  sourceConfig: (id: number) => invoke<Record<string, any>>("source_config", { id }),
  unlockBundle: (path: string, password: string, keyfile: string | null, allowNewer = false) =>
    invoke<BundleSummary>("unlock_bundle", { path, password, keyfile, allowNewer }),
  setLock: (password: string, keyfile: string | null) => invoke<BundleSummary>("set_lock", { password, keyfile }),
  removeLock: () => invoke<BundleSummary>("remove_lock"),
  getAppSettings: () => invoke<AppSettings>("get_app_settings"),
  saveAppSettings: (settings: AppSettings) => invoke<void>("save_app_settings", { settings }),
  listTasks: () => invoke<TaskInfo[]>("list_tasks"),
  getAudio: () => invoke<AudioState>("get_audio"),
  setAudio: (volume: number | null, muted: boolean | null) => invoke<AudioState>("set_audio", { volume, muted }),
  setAudioFocus: (label: string) => invoke<void>("set_audio_focus", { label }),
  openWindow: (payload: WindowPayload) => invoke<string>("open_window", { kind: payload.kind, payload }),
  takeWindowPayload: () => invoke<WindowPayload | null>("take_window_payload"),
  panic: () => invoke<void>("panic"),
  cancelTask: (id: number) => invoke<boolean>("cancel_task", { id }),
};

export const onAudio = (f: (e: AudioState) => void): Promise<UnlistenFn> => listen<AudioState>("audio", (e) => f(e.payload));
export const onAppSettings = (f: () => void): Promise<UnlistenFn> => listen("app-settings", () => f());

export const onTask = (f: (e: TaskInfo) => void): Promise<UnlistenFn> => listen<TaskInfo>("task", (e) => f(e.payload));

export const onScan = (f: (e: ScanEvent) => void): Promise<UnlistenFn> =>
  listen<ScanEvent>("scan-progress", (e) => f(e.payload));

export const onThumbs = (f: (e: ThumbsEvent) => void): Promise<UnlistenFn> =>
  listen<ThumbsEvent>("thumbs-progress", (e) => f(e.payload));

export const onConvert = (f: (e: ConvertEvent) => void): Promise<UnlistenFn> =>
  listen<ConvertEvent>("convert-progress", (e) => f(e.payload));

export const thumbUrl = (id: number) => convertFileSrc(String(id), "vpthumb");
let mediaBase: string | null = null;
/** Android serves original media over loopback HTTP (`media_server.rs`); call once at startup. */
export async function initMediaUrls() {
  mediaBase = await invoke<string | null>("media_base_url").catch(() => null);
}
/** The original file, or with `fit` a display copy scaled to fit that many px (`media.rs`). */
export const mediaUrl = (id: number, fit?: number) => {
  const url = mediaBase ? `${mediaBase}/${id}` : convertFileSrc(String(id), "vpmedia");
  return fit ? `${url}?fit=${fit}` : url;
};

/**
 * What to show for an item: huge images (longer edge over `maxPx`, or size unknown) as a
 * display copy, since decoding a 200-megapixel original stalls the WebView; else the original.
 */
export function displayUrl(b: MediaBrief, maxPx: number): string {
  if (b.kind !== "image" || maxPx <= 0) return mediaUrl(b.id);
  const edge = Math.max(b.width ?? 0, b.height ?? 0);
  return edge > 0 && edge <= maxPx ? mediaUrl(b.id) : mediaUrl(b.id, maxPx);
}

export interface PlatformInfo {
  /** "android", "ios", "windows", "macos" or "linux". */
  os: string;
  mobile: boolean;
  /** Shared storage may be read and written (always true on desktop). */
  storage_granted: boolean;
  os_media: boolean;
}

export interface StorageRoot {
  kind: "internal" | "external" | "home";
  name: string;
  path: string;
}

export interface DirListing {
  path: string;
  parent: string | null;
  entries: { name: string; dir: boolean; size: number }[];
}

/** Platform services (`src-tauri/src/platform.rs`). */
export const platformApi = {
  info: () => invoke<PlatformInfo>("platform_info"),
  requestStorageAccess: () => invoke<boolean>("request_storage_access"),
  setSystemFullscreen: (on: boolean) => invoke<void>("set_system_fullscreen", { on }),
  setSystemBars: (dark: boolean, background: string) => invoke<void>("set_system_bars", { dark, background }),
  storageRoots: () => invoke<StorageRoot[]>("storage_roots"),
  browseDir: (path: string, files: boolean, extensions: string[] = []) => invoke<DirListing>("browse_dir", { path, files, extensions }),
};

export function asCmdError(e: unknown): CmdError {
  if (e && typeof e === "object" && "code" in e) return e as CmdError;
  return { code: "unknown", message: String(e), detail: null };
}
