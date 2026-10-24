import type { Column, SortSpec } from "./columns";
import type { SectionMark } from "./scroll";
import type { Selection } from "./selection.svelte";

export interface FolderItem {
  sourceId: number;
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
  /** Names the listing shown (source, folder, query): its scroll position is remembered under it. */
  scrollKey: string;
  /** Where sections of the current order start (scrollbar lines and drag label). */
  sections: SectionMark[];
  tile: number;
  showNames: boolean;
  /** Folder tiles show an N x N grid of thumbnails (0 = icon only). */
  folderPreview: number;
  overscan: number;
  columns: Column[];
  rowPx: number;
  sort: SortSpec | null;
  tree: TreeScope;
  onsort: (field: string) => void;
  /** Opens the viewer at `id` within `list` (the order the view shows). */
  onopen: (id: number, list: number[]) => void;
  onfolder: (path: string) => void;
  /** Opens `id` in a new viewer window (middle click); absent on mobile. */
  onopenwindow?: (id: number, list: number[]) => void;
}
