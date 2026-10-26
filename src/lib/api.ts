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

/** `nonmedia`: any other file, listed and exportable but never previewed. */
export type MediaKind = "image" | "video" | "nonmedia";

export interface SourceRow {
  id: number;
  name: string;
  driver: string;
  root: string;
  enabled: boolean;
}

/** A file in a duplicate group (`src-tauri/src/dedup.rs`). */
export interface DupItem {
  media_id: number;
  source_id: number;
  rel_path: string;
  size: number;
  created_ms: number | null;
  modified_ms: number | null;
  added_ms: number;
  /** Stored-data id of a shard item: the same for items that already share their data. */
  body: string | null;
  /** From a saved report: "deleted", "linked", or "gone" (no longer in the library). */
  action: string;
}

export interface DupGroup {
  sha256: string;
  size: number;
  items: DupItem[];
}

export interface DupScan {
  groups: DupGroup[];
  checked: number;
  read_bytes: number;
  failed: string[];
  elapsed_ms: number;
}

export interface DupScope {
  sources: number[];
  across: boolean;
}

export interface DupApplyGroup {
  items: number[];
  delete: number[];
  link: number[];
}

export interface DupApplyReport {
  deleted: number;
  linked: number;
  freed: number;
  failed: string[];
}

export interface DupReportRow {
  id: number;
  created_ms: number;
  label: string;
  scope: DupScope;
  groups: number;
  files: number;
  wasted: number;
}

export interface DupEntry {
  grp: number;
  media_id: number | null;
  source_id: number;
  rel_path: string;
  size: number;
  sha256: string;
  action: string;
}

export interface MediaBrief {
  id: number;
  name: string;
  kind: MediaKind;
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
  source_id: number;
  /** The built-in favorite is on. */
  favorite: boolean;
  /** When a scan first found the item. */
  added_ms: number;
}

/** Key of the built-in on/off favorite field. */
export const FAVORITE_FIELD = "x.favorite";

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

/** Optional HEIF/HEIC decoder (libheif, `heif.rs`). */
export interface HeifStatus {
  available: boolean;
  version: string | null;
  path: string | null;
  /** The app can download it on this device (Windows). */
  installable: boolean;
  installing: boolean;
  /** An image could not be shown without it since the app started. */
  needed: boolean;
  /** Download size in bytes. */
  size: number;
  /** Why it did not load from where it should have been. */
  load_errors: string[];
}

/** The end of the app's log file (`applog.rs`). */
export interface LogInfo {
  path: string;
  text: string;
  bytes: number;
}

/**
 * Where the result's most significant sort key changes: `day` keys are local days since
 * 1970-01-01 ("" = no date), `text` keys are shown as they are.
 */
export interface Sections {
  kind: "none" | "day" | "text";
  marks: { index: number; key: string }[];
}

export interface SearchResult {
  ids: number[];
  elapsed_ms: number;
  sections: Sections;
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
  /** Folder tiles show an N x N grid of thumbnails (0 = icon only). */
  folder_preview: number;
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
  prefetch_ratio: number;
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
  revision: number;
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
  media_only: boolean;
  search_debounce_ms: number;
  scan_refresh_ms: number;
  scan_refresh_cost_factor: number;
  suggest_limit: number;
  value_list_limit: number;
  source_check_secs: number;
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
    scheduler: SchedulerMode;
    viewer_share_performance: number;
    viewer_share_predictive: number;
    viewer_share_preprocess: number;
    thumb_share: number;
    sched_idle_ms: number;
    pause_background: boolean;
    predict_decay: number;
    predict_min_side: number;
    prefetch_max_items: number;
  };
  reopen_last_bundle: boolean;
  hide_missing_recent: boolean;
  ui: UiSettings;
  progress: { emit_interval_ms: number; slow_read_ms: number; read_piece_bytes: number; keep_finished_secs: number };
  audio: { volume: number; muted: boolean; start_muted: boolean; focus_only: boolean };
  privacy: { panic_keys: string[]; panic_double_ms: number; panic_mute: boolean; panic_hide: boolean; restore_shortcut: string; tray_icon: boolean };
  windows: { max_viewer_panes: number; split_layout: "auto" | "columns" | "rows";
    switch_at_percent: number;
    switch_main_percent: number;
    switch_anim_ms: number;
    new_width: number; new_height: number };
  viewer: {
    immersive_on_fullscreen: boolean;
    zoom_step: number;
    max_zoom: number;
    double_click_zoom: number;
    wheel_zoom: boolean;
    tap_to_advance: boolean;
    swipe_favorite: boolean;
    brief_window: number;
    display_copy_workers: number;
    pull_close: boolean;
    pull_close_threshold: number;
    pull_close_min_scale: number;
    double_tap_ms: number;
    swipe_threshold: number;
    swipe_ms: number;
    display_max_px: number;
    display_quality: number;
    fit_screen: boolean;
    fit_screen_step_px: number;
    convert_image_extensions: string[];
  };
  ml: {
    runtime_path: string;
    providers: string[];
    device_id: number;
    model_dirs: string[];
    max_edge_px: number;
    threads: number;
    auto_analyze: boolean;
    priority_items: number;
    decode_workers: number;
    batch_size: number;
  };
  decoders: {
    heif_download_urls: string[];
    heif_offer: boolean;
  };
  export: {
    keep_dates: boolean;
  };
  log: {
    max_bytes: number;
    slow_view_ms: number;
    slow_db_ms: number;
  };
}

// ---- image analysis (`ml.rs`)
export type PackKind = "face_detector" | "face_embedder" | "anime_detector" | "anime_embedder" | "tagger";
/** "real": people in photos; "anime": characters in illustrations. */
export type FaceKind = "real" | "anime";

export interface PackInfo {
  id: string;
  key: string;
  name: string;
  kind: PackKind;
  license: string;
  license_url: string;
  source: string;
  path: string;
}

export interface AnalysisCounts {
  images: number;
  done: number;
  faces: number;
  people: number;
  unnamed: number;
  failed: number;
}

/** A downloadable model pack (`ml_store.rs`, `models/catalog.json`). */
export interface StoreItem {
  id: string;
  name: string;
  kind: PackKind;
  summary: Record<string, string>;
  license: { name: string; url: string };
  homepage: string;
  files: { url: string; path: string; size: number }[];
  size: number;
  installed: boolean;
  downloading: boolean;
  path: string;
}

export interface MlStatus {
  supported: boolean;
  runtime: string | null;
  runtime_error: string | null;
  providers: string[];
  packs: PackInfo[];
  pack_errors: string[];
  model_dirs: string[];
  running: boolean;
  counts: AnalysisCounts | null;
}

export interface PersonRow {
  id: number;
  kind: FaceKind;
  /** Empty: a group nobody has named yet. */
  name: string;
  items: number;
  faces: number;
  cover: number | null;
}

export interface FaceRow {
  id: number;
  media_id: number;
  /** x, y, width, height as fractions of the image. */
  bbox: [number, number, number, number];
  score: number;
  person_id: number | null;
  confirmed: boolean;
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

/** A change to a folder's field: replace its values, or add/remove one value of an enum field. */
export type FolderEdit = { op: "set"; value: ExtValue[] } | { op: "add"; value: ExtValue } | { op: "remove"; value: ExtValue };

export type FieldType =
  | { type: "text" }
  | { type: "integer" }
  | { type: "real" }
  | { type: "date" }
  | { type: "size" }
  | { type: "duration" }
  | { type: "kind" }
  | { type: "enum" }
  | { type: "ranged_int"; min: number; max: number }
  | { type: "toggle" };

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
  kind: MediaKind;
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
  /** Values of on/off fields. */
  yes: string;
  no: string;
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
  /** Where a bundle was last browsed on this device (state stored by the library view). */
  lastView: (path: string) => invoke<unknown>("last_view", { path }),
  saveLastView: (path: string, view: unknown) => invoke<void>("save_last_view", { path, view }),
  recentBundles: () => invoke<RecentBundle[]>("recent_bundles"),
  forgetRecentBundle: (path: string) => invoke<void>("forget_recent_bundle", { path }),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  listSources: () => invoke<SourceRow[]>("list_sources"),
  addLocalSource: (path: string) => invoke<SourceRow>("add_local_source", { path }),
  removeSource: (id: number, deleteData = false) => invoke<void>("remove_source", { id, deleteData }),
  scanSource: (id: number) => invoke<void>("scan_source", { id }),
  cancelScan: (id: number) => invoke<void>("cancel_scan", { id }),
  search: (query: string, sourceId: number | null, folder: string | null = null, recursive = false) =>
    invoke<SearchResult>("search", { query, sourceId, folder, recursive, tzOffsetMinutes: -new Date().getTimezoneOffset() }),
  childFolders: (sourceId: number, parent: string) =>
    invoke<{ name: string; count: number }[]>("child_folders", { sourceId, parent }),
  /** Thumbnail ids standing for each folder (same order as `folders`). */
  folderSamples: (sourceId: number, folders: string[], limit: number) =>
    invoke<number[][]>("folder_samples", { sourceId, folders, limit }),
  renameFolder: (sourceId: number, path: string, name: string) =>
    invoke<{ moved: number; failed: string[]; path: string }>("rename_folder", { sourceId, path, name }),
  moveFolder: (sourceId: number, path: string, target: string) =>
    invoke<{ moved: number; failed: string[]; path: string }>("move_folder", { sourceId, path, target }),
  deleteFolder: (sourceId: number, path: string) => invoke<{ deleted: number; failed: string[] }>("delete_folder", { sourceId, path }),
  mediaBrief: (ids: number[]) => invoke<MediaBrief[]>("media_brief", { ids }),
  /** Sources whose storage is not connected; searches leave their items out. */
  offlineSources: () => invoke<number[]>("offline_sources"),
  /** After a load failed: marks items whose file is gone as missing, returns those ids. */
  checkMedia: (ids: number[]) => invoke<number[]>("check_media", { ids }),
  saveBundle: () => invoke<void>("save_bundle"),
  prefetch: (ids: number[]) => invoke<void>("prefetch", { ids }),
  recordView: (id: number, viewMs: number) => invoke<void>("record_view", { id, viewMs }),
  cacheStats: () => invoke<CacheStats>("cache_stats"),
  /** The viewer's look-ahead, most wanted first; replaces the previous one. */
  prefetchPlan: (ids: number[], screen: { width: number; height: number }) => invoke<void>("prefetch_plan", { ids, screen }),
  cacheResize: () => invoke<CacheStats>("cache_resize"),
  cacheClear: () => invoke<CacheStats>("cache_clear"),
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
  folderValues: (sourceId: number, path: string) => invoke<Record<string, ExtValue[]>>("folder_values", { sourceId, path }),
  /** `recursive` also applies the change to every item and subfolder inside. */
  editFolderValues: (sourceId: number, path: string, key: string, edit: FolderEdit, recursive: boolean) =>
    invoke<number>("edit_folder_values", { sourceId, path, key, edit, recursive }),
  /** Known values of a text or enum field starting with `prefix`, most used first. */
  suggestValues: (key: string, prefix: string, limit?: number) => invoke<string[]>("suggest_values", { key, prefix, limit }),
  queryVocabulary: (locale: string) => invoke<Vocabulary>("query_vocabulary", { locale }),
  listSavedQueries: (locale: string) => invoke<SavedQuery[]>("list_saved_queries", { locale }),
  saveQuery: (id: number | null, label: string, text: string) => invoke<number>("save_query", { id, label, text }),
  deleteSavedQuery: (id: number) => invoke<void>("delete_saved_query", { id }),
  checkQuery: (src: string, locale: string) => invoke<QueryCheck>("check_query", { src, locale }),
  ffmpegStatus: () => invoke<FfmpegStatus>("ffmpeg_status"),
  heifStatus: () => invoke<HeifStatus>("heif_status"),
  heifInstall: () => invoke<void>("heif_install"),
  /** Makes failed HEIF thumbnails again; returns how many. */
  heifRetryFailed: () => invoke<number>("heif_retry_failed"),
  logRead: (maxBytes?: number) => invoke<LogInfo>("log_read", { maxBytes }),
  /** Notes in the app log that a picture took `ms` to show in the viewer. */
  logSlowView: (id: number, ms: number, detail: string) => invoke<void>("log_slow_view", { id, ms, detail }),
  logOpen: (folder: boolean) => invoke<void>("log_open", { folder }),
  logClear: () => invoke<void>("log_clear"),
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
  exportMedia: (ids: number[], dest: string) => invoke<void>("export_media", { ids, dest }),
  /** Copies a folder (with its subfolders) into a new folder of the same name in `dest`. */
  exportFolder: (sourceId: number, folder: string, dest: string) => invoke<void>("export_folder", { sourceId, folder, dest }),
  /** Deletes the files of `ids` from their sources and forgets the items. */
  renameMedia: (id: number, name: string) => invoke<void>("rename_media", { id, name }),
  /** Moves items into `folder` of their own source (names kept). */
  moveMediaTo: (ids: number[], folder: string) => invoke<{ moved: number; failed: string[] }>("move_media_to", { ids, folder }),
  deleteMedia: (ids: number[]) => invoke<{ deleted: number; failed: string[] }>("delete_media", { ids }),

  // Shard sources (src-tauri/src/shards.rs)
  shardStatus: () => invoke<ShardStatus[]>("shard_status"),
  inspectShard: (path: string) => invoke<ShardPeek>("inspect_shard", { path }),
  createShard: (
    name: string,
    location: string | null,
    password: string | null,
    keyfile: string | null,
    remember: boolean,
    propsInShard: boolean,
    embeddingsInShard: boolean,
  ) => invoke<SourceRow>("create_shard", { name, location, password, keyfile, remember, propsInShard, embeddingsInShard }),
  attachShard: (
    path: string,
    mode: ShardPlace,
    password: string | null,
    keyfile: string | null,
    remember: boolean,
    propsInShard: boolean,
    embeddingsInShard: boolean,
  ) => invoke<SourceRow>("attach_shard", { path, mode, password, keyfile, remember, propsInShard, embeddingsInShard }),
  shardSetProps: (sourceId: number, inShard: boolean) => invoke<SourceRow>("shard_set_props", { sourceId, inShard }),
  shardSetEmbeddings: (sourceId: number, on: boolean) => invoke<SourceRow>("shard_set_embeddings", { sourceId, on }),
  /** Average bytes of a face's identity vector here (what keeping vectors in a shard costs per face). */
  faceEmbeddingBytes: () => invoke<number>("face_embedding_bytes"),
  mirrorList: () => invoke<MirrorRow[]>("mirror_list"),
  /** `triggers` absent keeps them (and syncs a mirror that is on); given, only they change. */
  mirrorSave: (
    id: number | null,
    sourceId: number,
    targetId: number,
    targetFolder: string,
    enabled: boolean,
    propagateDeletes: boolean,
    triggers?: MirrorTriggers,
  ) => invoke<number>("mirror_save", { id, sourceId, targetId, targetFolder, enabled, propagateDeletes, triggers: triggers ?? null }),
  mirrorDelete: (id: number) => invoke<void>("mirror_delete", { id }),
  mirrorSync: (id: number) => invoke<void>("mirror_sync", { id }),
  dupScan: (sources: number[], across: boolean) => invoke<DupScan>("dup_scan", { sources, across }),
  dupApply: (groups: DupApplyGroup[]) => invoke<DupApplyReport>("dup_apply", { groups }),
  dupSaveReport: (label: string, scope: DupScope, entries: DupEntry[]) => invoke<number>("dup_save_report", { label, scope, entries }),
  dupReports: () => invoke<DupReportRow[]>("dup_reports"),
  dupReport: (id: number) => invoke<{ report: DupReportRow; groups: DupGroup[] } | null>("dup_report", { id }),
  dupRenameReport: (id: number, label: string) => invoke<void>("dup_rename_report", { id, label }),
  dupDeleteReport: (id: number) => invoke<void>("dup_delete_report", { id }),
  unlockShard: (sourceId: number, password: string, keyfile: string | null, remember: boolean) =>
    invoke<void>("unlock_shard", { sourceId, password, keyfile, remember }),
  lockShard: (sourceId: number) => invoke<void>("lock_shard", { sourceId }),
  shardChangePassword: (sourceId: number, password: string, keyfile: string | null) =>
    invoke<void>("shard_change_password", { sourceId, password, keyfile }),
  shardImport: (sourceId: number, paths: string[], dest: string, mode: TransferMode) =>
    invoke<void>("shard_import", { sourceId, paths, dest, mode }),
  shardFromSource: (from: number, to: number, dest: string, mode: TransferMode) =>
    invoke<void>("shard_from_source", { from, to, dest, mode }),
  shardExport: (sourceId: number, paths: string[], dest: string) => invoke<void>("shard_export", { sourceId, paths, dest }),
  /** Copies items and folders (`path` "" = a whole source) into folder `dest` of source `to`, in the background. */
  /** `move`: each item copied is then deleted where it was. */
  copyToSource: (ids: number[], folders: { sourceId: number; path: string }[], to: number, dest: string, move = false) =>
    invoke<void>("copy_to_source", { ids, folders, to, dest, remove: move }),
  /** Answers a copy conflict; `null` cancels the copy. `all`: the same for every remaining one. */
  copyConflictAnswer: (id: number, choice: OnConflict | null, all: boolean) => invoke<void>("copy_conflict_answer", { id, choice, all }),
  mlStatus: () => invoke<MlStatus>("ml_status"),
  /** Analyses every image not seen by the chosen models yet, in the background. */
  mlAnalyze: () => invoke<void>("ml_analyze"),
  mlRegroup: () => invoke<void>("ml_regroup"),
  mlPeople: (kind: FaceKind, hidden = false) => invoke<PersonRow[]>("ml_people", { kind, hidden }),
  mlPersonFaces: (person: number, offset: number, limit: number) => invoke<FaceRow[]>("ml_person_faces", { person, offset, limit }),
  mlMediaFaces: (media: number) => invoke<FaceRow[]>("ml_media_faces", { media }),
  /** Returns the id carrying the name: naming like another person merges into them. */
  mlRenamePerson: (id: number, name: string) => invoke<number>("ml_rename_person", { id, name }),
  mlMergePeople: (into: number, from: number[]) => invoke<void>("ml_merge_people", { into, from }),
  /** `person` null: "not this person". */
  mlAssignFaces: (faces: number[], person: number | null) => invoke<void>("ml_assign_faces", { faces, person }),
  /** Moves faces into a new person (naming it like an existing one merges them); returns its id. */
  mlNewPerson: (kind: FaceKind, name: string, faces: number[]) => invoke<number>("ml_new_person", { kind, name, faces }),
  mlHidePerson: (id: number, hidden: boolean) => invoke<void>("ml_hide_person", { id, hidden }),
  /** What the library shows, in order: analysed before the rest. */
  mlPrioritize: (ids: number[]) => invoke<void>("ml_prioritize", { ids }),
  /** Forgets failed analyses and analyses those items again; returns how many. */
  mlRetryFailed: () => invoke<number>("ml_retry_failed"),
  mlCatalog: () => invoke<StoreItem[]>("ml_catalog"),
  /** `license`: the name of the license the user accepted. */
  mlDownload: (id: string, license: string) => invoke<void>("ml_download", { id, license }),
  shardVerify: (sourceId: number) => invoke<VerifyReport>("shard_verify", { sourceId }),
  shardCompact: (sourceId: number) => invoke<CompactReport>("shard_compact", { sourceId }),
  shardSetEncryption: (sourceId: number, password: string | null, keyfile: string | null, remember: boolean) =>
    invoke<void>("shard_set_encryption", { sourceId, password, keyfile, remember }),
  shardRelocate: (sourceId: number, target: string | null, mode: TransferMode) =>
    invoke<SourceRow>("shard_relocate", { sourceId, target, mode }),
};

export type TransferMode = "copy" | "move";
/** Attaching a shard: use it where it is, or copy/move it into the bundle. */
export type ShardPlace = "link" | TransferMode;

export interface ShardStats {
  files: number;
  bytes: number;
  thumbs: number;
  converted: number;
  chunks: number;
  disk_bytes: number;
  garbage_bytes: number;
}

export interface ShardStatus {
  source_id: number;
  path: string;
  in_bundle: boolean;
  found: boolean;
  encrypted: boolean;
  requires_keyfile: boolean;
  unlocked: boolean;
  remembered: boolean;
  writable: boolean;
  busy: boolean;
  chunk_bytes: number;
  stats: ShardStats | null;
  /** Item properties are kept in the shard. */
  props_in_shard: boolean;
  /** Faces' identity vectors are kept in the shard too. */
  embeddings_in_shard: boolean;
  /** Faces found on its items, and the bytes their vectors take. */
  faces: number;
  embedding_bytes: number;
}

export interface ShardPeek {
  name: string;
  encrypted: boolean;
  requires_keyfile: boolean;
  shard_version: number;
  /** It already keeps item properties; null while encrypted (unknown). */
  has_props: boolean | null;
}

/** A source kept copied into a folder of another source (`mirror.rs`). */
export interface MirrorRow {
  id: number;
  source_id: number;
  target_id: number;
  target_folder: string;
  enabled: boolean;
  propagate_deletes: boolean;
  created_ms: number;
  last_sync_ms: number | null;
  last_result: string | null;
  triggers: MirrorTriggers;
}

/** When a mirror syncs by itself (`vp_db::mirror::MirrorTriggers`). */
export interface MirrorTriggers {
  on_open: boolean;
  on_connect: boolean;
  /** When a scan finds the source changed. */
  on_scan: boolean;
  /** Minutes between syncs while both stay connected: null follows the library setting, 0 never. */
  interval_mins: number | null;
}

export interface VerifyReport {
  items: number;
  damaged: string[];
}

export interface CompactReport {
  chunks_removed: number;
  bytes_freed: number;
}

export const onAudio = (f: (e: AudioState) => void): Promise<UnlistenFn> => listen<AudioState>("audio", (e) => f(e.payload));
export const onAppSettings = (f: () => void): Promise<UnlistenFn> => listen("app-settings", () => f());

/** What to do when a copy's target name is taken (`shards.rs` `OnConflict`). */
export type OnConflict = "skip_same" | "skip_name" | "rename" | "overwrite";

/** A copy waits for the user: a file is already at the target path. */
export interface CopyConflict {
  id: number;
  task: number;
  target: string;
  source: string;
  existing_size: number;
  existing_modified_ms: number | null;
  incoming_size: number;
  incoming_modified_ms: number | null;
  /** Same size and content. */
  same: boolean | null;
}

export const onCopyConflict = (f: (c: CopyConflict) => void): Promise<UnlistenFn> =>
  listen<CopyConflict>("copy-conflict", (e) => f(e.payload));
/** Conflict `id` was answered (in some window) or its copy was cancelled. */
export const onCopyConflictDone = (f: (id: number) => void): Promise<UnlistenFn> =>
  listen<number>("copy-conflict-done", (e) => f(e.payload));

/** An image could not be shown for want of the HEIF decoder (sent once per run). */
export const onHeifNeeded = (f: () => void): Promise<UnlistenFn> => listen("heif-needed", () => f());
/** The HEIF decoder was downloaded and loads. */
export const onHeifInstalled = (f: () => void): Promise<UnlistenFn> => listen("heif-installed", () => f());

export const onTask =(f: (e: TaskInfo) => void): Promise<UnlistenFn> => listen<TaskInfo>("task", (e) => f(e.payload));

export const onScan = (f: (e: ScanEvent) => void): Promise<UnlistenFn> =>
  listen<ScanEvent>("scan-progress", (e) => f(e.payload));

export const onThumbs = (f: (e: ThumbsEvent) => void): Promise<UnlistenFn> =>
  listen<ThumbsEvent>("thumbs-progress", (e) => f(e.payload));

export const onConvert = (f: (e: ConvertEvent) => void): Promise<UnlistenFn> =>
  listen<ConvertEvent>("convert-progress", (e) => f(e.payload));

export const thumbUrl = (id: number) => convertFileSrc(String(id), "vpthumb");
/** A face picture of the people view (`thumbs.rs`). */
export const faceUrl = (id: number) => convertFileSrc(`face-${id}`, "vpthumb");
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
/** How the disk is shared and how far the viewer reads ahead (`HardwareSettings::scheduler`). */
export type SchedulerMode = "performance" | "predictive" | "preprocess";

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
  kind: "internal" | "external" | "app" | "home";
  name: string;
  path: string;
}

/** A pick from the OS document picker (`system_pick`). */
export interface SystemPick {
  cancelled: boolean;
  path: string | null;
  readable: boolean;
  /** A file the app could not read in place, copied into the app folder. */
  copied: boolean;
}

export interface RecentBundle {
  path: string;
  /** Whether the bundle is still there. */
  exists: boolean;
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
  systemPick: (mode: "file" | "folder") => invoke<SystemPick>("system_pick", { mode }),
  browseDir: (path: string, files: boolean, extensions: string[] = []) => invoke<DirListing>("browse_dir", { path, files, extensions }),
};

export function asCmdError(e: unknown): CmdError {
  if (e && typeof e === "object" && "code" in e) return e as CmdError;
  return { code: "unknown", message: String(e), detail: null };
}
