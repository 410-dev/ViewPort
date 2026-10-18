import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type OpenMode = "autosave" | "manual_save_only";

export interface BundleSummary {
  path: string;
  name: string;
  mode: OpenMode;
  bundle_version: number;
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

export interface AppSettings {
  hardware: { gpu: boolean; video_decode: boolean; video_encode: boolean };
  reopen_last_bundle: boolean;
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
  search: (query: string, sourceId: number | null) =>
    invoke<SearchResult>("search", { query, sourceId, tzOffsetMinutes: -new Date().getTimezoneOffset() }),
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
  getAppSettings: () => invoke<AppSettings>("get_app_settings"),
  saveAppSettings: (settings: AppSettings) => invoke<void>("save_app_settings", { settings }),
};

export const onScan = (f: (e: ScanEvent) => void): Promise<UnlistenFn> =>
  listen<ScanEvent>("scan-progress", (e) => f(e.payload));

export const onThumbs = (f: (e: ThumbsEvent) => void): Promise<UnlistenFn> =>
  listen<ThumbsEvent>("thumbs-progress", (e) => f(e.payload));

export const onConvert = (f: (e: ConvertEvent) => void): Promise<UnlistenFn> =>
  listen<ConvertEvent>("convert-progress", (e) => f(e.payload));

export const thumbUrl = (id: number) => convertFileSrc(String(id), "vpthumb");
export const mediaUrl = (id: number) => convertFileSrc(String(id), "vpmedia");

export function asCmdError(e: unknown): CmdError {
  if (e && typeof e === "object" && "code" in e) return e as CmdError;
  return { code: "unknown", message: String(e), detail: null };
}
