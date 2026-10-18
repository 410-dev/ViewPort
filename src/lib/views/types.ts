import type { Column, SortSpec } from "./columns";
import type { Selection } from "./selection.svelte";

export interface FolderItem {
  name: string;
  /** Path relative to the source root. */
  path: string;
  count: number;
}

/** Where the tree view starts: every source, or one folder of one source. */
export interface TreeScope {
  sources: { id: number; name: string }[];
  sourceId: number | null;
  folder: string;
  /** Query text applied to the files listed in each folder. */
  query: string;
}

/** Props every view receives; each view uses the ones it needs. */
export interface ViewProps {
  ids: number[];
  folders: FolderItem[];
  selection: Selection;
  resetKey: unknown;
  tile: number;
  showNames: boolean;
  overscan: number;
  columns: Column[];
  rowPx: number;
  sort: SortSpec | null;
  tree: TreeScope;
  onsort: (field: string) => void;
  /** Opens the viewer at `id` within `list` (the order the view shows). */
  onopen: (id: number, list: number[]) => void;
  onfolder: (path: string) => void;
}
