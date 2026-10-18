/**
 * Master audio, mirrored from the backend (`src-tauri/src/audio.rs`) so every window and
 * split pane uses one volume and one mute. With `audio.focus_only`, only the active pane
 * of the focused window is audible.
 */
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, onAudio } from "./api";
import { windowLabel } from "./platform";
import { prefs } from "./prefs.svelte";

export const audio = $state({ volume: 1, muted: true, focus: null as string | null });

let started = false;
export function startAudio() {
  if (started) return;
  started = true;
  api.getAudio().then((s) => Object.assign(audio, s)).catch(() => {});
  onAudio((s) => Object.assign(audio, s));
  const claim = () => api.setAudioFocus(windowLabel()).catch(() => {});
  try {
    getCurrentWindow().onFocusChanged(({ payload }) => payload && claim());
  } catch {
    // Not running in Tauri.
  }
  if (document.hasFocus()) claim();
}

/** Whether a pane should play sound. `paneActive` is the pane's focus within its window. */
export function audible(paneActive: boolean): boolean {
  if (audio.muted) return false;
  if (!prefs.app?.audio.focus_only) return true;
  return audio.focus === windowLabel() && paneActive;
}

let volumeTimer: ReturnType<typeof setTimeout> | undefined;
/** Sets the master volume (applied locally at once, sent to the backend throttled). */
export function setVolume(v: number) {
  audio.volume = Math.min(1, Math.max(0, v));
  if (audio.volume > 0 && audio.muted) setMuted(false);
  clearTimeout(volumeTimer);
  volumeTimer = setTimeout(() => api.setAudio(audio.volume, null).catch(() => {}), 60);
}

export function setMuted(m: boolean) {
  audio.muted = m;
  api.setAudio(null, m).catch(() => {});
}

export function toggleMute() {
  setMuted(!audio.muted);
}

/** Claims audio focus for this window (e.g. when the user unmutes a pane here). */
export function claimFocus() {
  audio.focus = windowLabel();
  api.setAudioFocus(windowLabel()).catch(() => {});
}
