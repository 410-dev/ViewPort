/**
 * File and folder picking. Desktop uses the native dialogs; mobile uses the in-app
 * browser (`ui/PathPicker.svelte`, mounted once in `routes/+layout.svelte`), because the
 * Android dialogs return content URIs, cannot pick folders, and the app reads media by
 * path. Every picker asks for storage access first where the OS requires it.
 */
import { open, save } from "@tauri-apps/plugin-dialog";
import { platformApi } from "./api";
import { isMobile } from "./platform";

export type PickMode = "folder" | "file" | "save";

export interface PickRequest {
  mode: PickMode;
  title: string;
  /** File extensions shown in "file" mode (all files when empty). */
  extensions: string[];
  /** "save": the proposed file or folder name. */
  name: string;
  /** "save": the extension the name must end with, e.g. "vpbun". */
  ext: string;
  /** Folders ending in this suffix are picked by tapping them ("folder" mode, e.g. ".vpbun"). */
  pickSuffix: string;
  resolve: (path: string | null) => void;
}

export const picker = $state<{ req: PickRequest | null }>({ req: null });

function inApp(req: Omit<PickRequest, "resolve" | "extensions" | "name" | "ext" | "pickSuffix"> & Partial<PickRequest>) {
  return new Promise<string | null>((resolve) => {
    picker.req?.resolve(null);
    picker.req = { extensions: [], name: "", ext: "", pickSuffix: "", ...req, resolve };
  });
}

/** Shared storage access (Android "all files access"). Asks once if missing. */
export async function ensureStorage(): Promise<boolean> {
  const info = await platformApi.info().catch(() => null);
  if (!info || info.storage_granted) return true;
  return platformApi.requestStorageAccess().catch(() => false);
}

export async function pickFolder(title: string, opts: { pickSuffix?: string } = {}): Promise<string | null> {
  if (!isMobile) {
    const p = await open({ title, directory: true });
    return typeof p === "string" ? p : null;
  }
  if (!(await ensureStorage())) return null;
  return inApp({ mode: "folder", title, pickSuffix: opts.pickSuffix ?? "" });
}

export async function pickFile(title: string, filter?: { name: string; extensions: string[] }): Promise<string | null> {
  if (!isMobile) {
    const p = await open({ title, filters: filter ? [filter] : undefined });
    return typeof p === "string" ? p : null;
  }
  if (!(await ensureStorage())) return null;
  return inApp({ mode: "file", title, extensions: filter?.extensions ?? [] });
}

/** Several files on desktop; one at a time on mobile (the in-app browser picks one). */
export async function pickFiles(title: string): Promise<string[]> {
  if (!isMobile) {
    const p = await open({ title, multiple: true });
    return Array.isArray(p) ? p : typeof p === "string" ? [p] : [];
  }
  const one = await pickFile(title);
  return one ? [one] : [];
}

/** A new path named by the user: `<folder>/<name>.<ext>`. */
export async function pickSave(title: string, name: string, filter: { name: string; extensions: string[] }): Promise<string | null> {
  if (!isMobile) {
    return save({ title, defaultPath: name, filters: [filter] });
  }
  if (!(await ensureStorage())) return null;
  const ext = filter.extensions[0] ?? "";
  return inApp({ mode: "save", title, name: name.replace(new RegExp(`\\.${ext}$`, "i"), ""), ext });
}
