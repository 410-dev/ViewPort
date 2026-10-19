import { SvelteMap } from "svelte/reactivity";
import { api, type MediaBrief } from "../api";
import { invalidateFolders } from "./folders.svelte";

/**
 * Metadata for visible items, fetched in small batches as views scroll. Shared by the
 * grid, list, tree and preview so switching views never refetches.
 */
const LIMIT = 30_000;

export const briefs = new SvelteMap<number, MediaBrief>();
const pending = new Set<number>();
let timer: ReturnType<typeof setTimeout> | undefined;

export function want(ids: Iterable<number>) {
  let added = false;
  for (const id of ids) {
    if (!briefs.has(id) && !pending.has(id)) {
      pending.add(id);
      added = true;
    }
  }
  if (!added) return;
  clearTimeout(timer);
  timer = setTimeout(flush, 30);
}

async function flush() {
  const batch = [...pending];
  if (batch.length === 0) return;
  try {
    if (briefs.size > LIMIT) briefs.clear();
    for (const b of await api.mediaBrief(batch)) briefs.set(b.id, b);
  } catch {
    // The bundle may have closed; ignore.
  } finally {
    for (const id of batch) pending.delete(id);
  }
}

/** Forgets cached metadata (after edits, rescans or conversions). */
export function invalidate(ids?: number[]) {
  invalidateFolders();
  if (!ids) briefs.clear();
  else for (const id of ids) briefs.delete(id);
}
