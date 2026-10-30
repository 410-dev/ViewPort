/**
 * Change notices between windows of the same workspace. A window that changes shared
 * data broadcasts what kind of data changed; the others refresh. The backend's own
 * events (scan, task, audio, app-settings) already reach every window.
 */
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { windowLabel } from "./platform";

export type SyncKind = "media" | "edited" | "settings" | "saved" | "sources" | "fields" | "people" | "zoom";

const EVENT = "vp-sync";

export function broadcast(kind: SyncKind, ids?: number[]) {
  emit(EVENT, { kind, ids, from: windowLabel() }).catch(() => {});
}

/** Calls `f` when another window changed `kind`. */
export function onSync(kind: SyncKind, f: (ids?: number[]) => void): Promise<UnlistenFn> {
  return listen<{ kind: SyncKind; ids?: number[]; from: string }>(EVENT, (e) => {
    if (e.payload.kind === kind && e.payload.from !== windowLabel()) f(e.payload.ids);
  });
}
