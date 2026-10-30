/**
 * Device preferences (`AppSettings`): theme, language, fonts, layout. Loaded once at
 * startup, applied to the document as CSS variables, and saved back with a short delay
 * so dragging a splitter or a slider does not write on every frame.
 */
import { api, platformApi, type AppSettings, type UiSettings } from "./api";
import { i18n, resolveLocale } from "./i18n.svelte";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isMobile } from "./platform";

export const prefs = $state<{ app: AppSettings | null }>({ app: null });

/** The UI part of the preferences. Only valid after `loadPrefs()`. */
export function ui(): UiSettings {
  return prefs.app!.ui;
}

const systemDark = typeof matchMedia !== "undefined" ? matchMedia("(prefers-color-scheme: dark)") : null;
systemDark?.addEventListener("change", () => prefs.app && applyPrefs(prefs.app));

export async function loadPrefs() {
  prefs.app = await api.getAppSettings();
  applyPrefs(prefs.app);
}

/** Applies preferences to the document. Also used to preview unsaved settings. */
export function applyPrefs(app: AppSettings) {
  const u = app.ui;
  const root = document.documentElement;
  const theme = u.theme === "system" ? (systemDark?.matches === false ? "light" : "dark") : u.theme;
  root.dataset.theme = theme;
  root.dataset.motion = u.reduce_motion ? "reduce" : "full";
  const set = (k: string, v: string | null) => (v ? root.style.setProperty(k, v) : root.style.removeProperty(k));
  set("--accent", /^#[0-9a-f]{6}$/i.test(u.accent) ? u.accent : null);
  set("--font-ui", u.font_ui.trim() ? `${u.font_ui}, ${FALLBACK_FONTS}` : null);
  set("--font-editor", u.font_editor.trim() ? `${u.font_editor}, var(--font-ui)` : null);
  set("--font-size", `${Math.min(20, Math.max(11, u.font_size || 13))}px`);
  i18n.locale = resolveLocale(u.locale);
  i18n.queryLocale = u.query_locale === "auto" ? i18n.locale : resolveLocale(u.query_locale);
  root.lang = i18n.locale;
  matchSystemBars(theme);
  applyZoom(u);
}

/** Desktop: the window's scale, `ui.page_zoom` clamped to its range. */
export function pageZoom(u: UiSettings, z = u.page_zoom): number {
  return Math.min(u.page_zoom_max || 200, Math.max(u.page_zoom_min || 50, z || 100));
}

let lastZoom = 100;
function applyZoom(u: UiSettings) {
  if (isMobile) return;
  const z = pageZoom(u);
  if (z === lastZoom) return;
  lastZoom = z;
  getCurrentWebview().setZoom(z / 100).catch(() => {});
}

let lastBars = "";
/** Mobile: the status and navigation bars (iOS: the safe areas) follow the theme. */
function matchSystemBars(theme: string) {
  if (!isMobile) return;
  const bg = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
  const key = `${theme}|${bg}`;
  if (key === lastBars) return;
  lastBars = key;
  platformApi.setSystemBars(theme === "dark", bg).catch(() => {});
}

const FALLBACK_FONTS = `"Segoe UI", "Malgun Gothic", -apple-system, "Apple SD Gothic Neo", system-ui, sans-serif`;

let timer: ReturnType<typeof setTimeout> | undefined;
/** Applies and saves the current preferences after `delay` ms, then calls `saved`. */
export function savePrefs(delay = 400, saved?: () => void) {
  if (!prefs.app) return;
  applyPrefs(prefs.app);
  clearTimeout(timer);
  timer = setTimeout(
    () =>
      api
        .saveAppSettings($state.snapshot(prefs.app!) as AppSettings)
        .then(() => saved?.())
        .catch(() => {}),
    delay,
  );
}
