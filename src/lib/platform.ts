import { getCurrentWindow } from "@tauri-apps/api/window";

const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";

/** Phones and tablets: no extra windows there; split view replaces them. */
export const isMobile = /Android|iPhone|iPad|iPod/i.test(ua);
/** Android: native folder dialogs are missing (see `pick.svelte.ts`) and there is a back button. */
export const isAndroid = /Android/i.test(ua);

let label: string | null = null;
/** Label of this window: `main`, `viewer-N` or `library-N`. */
export function windowLabel(): string {
  if (label === null) {
    try {
      label = getCurrentWindow().label;
    } catch {
      label = "main";
    }
  }
  return label;
}
