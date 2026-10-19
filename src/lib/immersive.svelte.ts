/**
 * Immersive mode: the viewer hides every control. It turns on with fullscreen (when
 * `viewer.immersive_on_fullscreen`) and is the one place Escape means something other
 * than the panic key: the first Escape leaves immersive mode, the next one is the panic key
 * (see `routes/+layout.svelte`). State is per window.
 */
import { prefs } from "./prefs.svelte";

export const immersive = $state({ on: false });

let lastFullscreen = false;

/** Call whenever the window's fullscreen state may have changed. */
export function fullscreenChanged(fullscreen: boolean) {
  if (fullscreen === lastFullscreen) return;
  lastFullscreen = fullscreen;
  immersive.on = fullscreen && (prefs.app?.viewer.immersive_on_fullscreen ?? true);
}

/** Leaves immersive mode; returns whether it was on. */
export function exitImmersive(): boolean {
  if (!immersive.on) return false;
  immersive.on = false;
  return true;
}

export function resetImmersive() {
  immersive.on = false;
  lastFullscreen = false;
}
