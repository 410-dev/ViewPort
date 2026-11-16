/**
 * Each folder's own sort, kept in the library (`view_sort`) so it is the same after a
 * restart and on other devices. The whole table is read once per open library (from the
 * backend's memory); a change writes only its own row. A folder without its own sort
 * follows the nearest folder above it that has one; the "all media" view has its own.
 */
import { api } from "../api";
import { broadcast } from "../sync";
import type { SortSpec } from "./columns";

/** The "all media" view's sort until one is chosen there. */
export const ALL_MEDIA_SORT: SortSpec = { field: "created", desc: true };

const store = $state({
  /** The library the table was read for. */
  bundle: "",
  ready: false,
  map: {} as Record<string, SortSpec>,
});

const keyOf = (src: number | null, folder: string) => `${src ?? 0}\u0000${src === null ? "" : folder}`;
const same = (a: SortSpec | null, b: SortSpec | null) => (!a && !b) || (!!a && !!b && a.field === b.field && a.desc === b.desc);

/** The table has been read for the open library (until then a listing would be in the wrong order). */
export const sortsReady = () => store.ready;

/** Reads the table for library `bundle` once (again with `force`, e.g. after folders moved). */
export async function loadSorts(bundle: string, force = false) {
  if (!force && store.bundle === bundle) return;
  if (store.bundle !== bundle) {
    store.bundle = bundle;
    store.ready = false;
    store.map = {};
  }
  const list = await api.viewSorts().catch(() => []);
  if (store.bundle !== bundle) return;
  const map: Record<string, SortSpec> = {};
  for (const e of list) map[keyOf(e.source_id, e.folder)] = { field: e.field, desc: e.desc };
  store.map = map;
  store.ready = true;
}

/** The folder's own sort, if it has one. */
export function ownSort(src: number | null, folder: string): SortSpec | null {
  return store.map[keyOf(src, folder)] ?? null;
}

/** What the folder follows when it has no sort of its own: the nearest folder above with one. */
export function inheritedSort(src: number | null, folder: string): SortSpec | null {
  if (src === null) return ALL_MEDIA_SORT;
  let path = folder;
  while (path) {
    const cut = path.lastIndexOf("/");
    path = cut < 0 ? "" : path.slice(0, cut);
    const s = store.map[keyOf(src, path)];
    if (s) return s;
  }
  return null;
}

/** The sort the folder is shown in. */
export function folderSort(src: number | null, folder: string): SortSpec | null {
  return ownSort(src, folder) ?? inheritedSort(src, folder);
}

/**
 * Chooses the folder's sort. One equal to what it would inherit (or `null`) forgets its own,
 * so it follows the folder above again; only a different one is kept.
 */
export async function setFolderSort(src: number | null, folder: string, sort: SortSpec | null) {
  const own = sort && !same(sort, inheritedSort(src, folder)) ? { field: sort.field, desc: sort.desc } : null;
  const k = keyOf(src, folder);
  if (same(own, store.map[k] ?? null)) return;
  if (own) store.map[k] = own;
  else delete store.map[k];
  try {
    await api.setViewSort(src, src === null ? "" : folder, own);
    broadcast("sorts");
  } catch {
    await loadSorts(store.bundle, true);
  }
}
