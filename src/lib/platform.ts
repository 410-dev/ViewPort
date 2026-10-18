import { getCurrentWindow } from "@tauri-apps/api/window";

/** Phones and tablets: no extra windows there; split view replaces them. */
export const isMobile = typeof navigator !== "undefined" && /Android|iPhone|iPad|iPod/i.test(navigator.userAgent);

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
