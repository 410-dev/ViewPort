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
}

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
  getAppSettings: () => invoke<AppSettings>("get_app_settings"),
  saveAppSettings: (settings: AppSettings) => invoke<void>("save_app_settings", { settings }),
};

export const onScan = (f: (e: ScanEvent) => void): Promise<UnlistenFn> =>
  listen<ScanEvent>("scan-progress", (e) => f(e.payload));

export const onThumbs = (f: (e: ThumbsEvent) => void): Promise<UnlistenFn> =>
  listen<ThumbsEvent>("thumbs-progress", (e) => f(e.payload));

export const thumbUrl = (id: number) => convertFileSrc(String(id), "vpthumb");
export const mediaUrl = (id: number) => convertFileSrc(String(id), "vpmedia");

export function asCmdError(e: unknown): CmdError {
  if (e && typeof e === "object" && "code" in e) return e as CmdError;
  return { code: "unknown", message: String(e), detail: null };
}
