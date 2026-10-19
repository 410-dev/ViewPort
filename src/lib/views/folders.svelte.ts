import { SvelteMap } from "svelte/reactivity";
import { api } from "../api";

/**
 * Thumbnails standing for folders (folder tiles), fetched in batches for the folders a
 * view shows. Keyed by source, sample count and path.
 */
export const folderSamples = new SvelteMap<string, number[]>();
const pending = new Map<number, { limit: number; paths: Set<string> }>();
let timer: ReturnType<typeof setTimeout> | undefined;

export const sampleKey = (sourceId: number, path: string, limit: number) => `${sourceId}:${limit}:${path}`;

export function wantFolders(folders: { sourceId: number; path: string }[], limit: number) {
  if (limit <= 0) return;
  let added = false;
  for (const f of folders) {
    if (folderSamples.has(sampleKey(f.sourceId, f.path, limit))) continue;
    let p = pending.get(f.sourceId);
    if (!p || p.limit !== limit) pending.set(f.sourceId, (p = { limit, paths: new Set() }));
    if (!p.paths.has(f.path)) {
      p.paths.add(f.path);
      added = true;
    }
  }
  if (!added) return;
  clearTimeout(timer);
  timer = setTimeout(flush, 30);
}

async function flush() {
  const batches = [...pending];
  pending.clear();
  for (const [sourceId, { limit, paths }] of batches) {
    const list = [...paths];
    try {
      const samples = await api.folderSamples(sourceId, list, limit);
      list.forEach((path, i) => folderSamples.set(sampleKey(sourceId, path, limit), samples[i] ?? []));
    } catch {
      // The bundle may have closed; ignore.
    }
  }
}

/** Forgets every folder's samples (after scans, moves and deletes). */
export function invalidateFolders() {
  folderSamples.clear();
}
