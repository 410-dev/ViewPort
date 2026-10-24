import type { MediaBrief } from "../api";
import { fmtBytes, fmtDate, fmtDuration } from "../format";
import type { Key } from "../i18n.svelte";

/**
 * Columns available to the list and tree views. To add one, add an entry here; the
 * settings panel and the column headers pick it up. `sort` is the (English) query field
 * name used when the header is clicked.
 */
export interface Column {
  key: string;
  label: Key;
  /** Width in px; `0` means it takes the remaining space. */
  width: number;
  align?: "left" | "right";
  sort?: string;
  value: (b: MediaBrief) => string;
}

export const COLUMNS: Column[] = [
  { key: "name", label: "col.name", width: 0, sort: "name", value: (b) => b.name },
  { key: "kind", label: "col.kind", width: 90, sort: "type", value: (b) => b.ext.toUpperCase() },
  { key: "size", label: "col.size", width: 96, align: "right", sort: "size", value: (b) => fmtBytes(b.size) },
  {
    key: "dimensions",
    label: "col.dimensions",
    width: 110,
    align: "right",
    sort: "width",
    value: (b) => (b.width ? `${b.width} × ${b.height}` : ""),
  },
  { key: "duration", label: "col.duration", width: 80, align: "right", sort: "duration", value: (b) => fmtDuration(b.duration_ms) },
  { key: "modified", label: "col.modified", width: 150, sort: "modified", value: (b) => fmtDate(b.modified_ms) },
  { key: "created", label: "col.created", width: 150, sort: "created", value: (b) => fmtDate(b.created_ms) },
  { key: "added", label: "col.added", width: 150, sort: "added", value: (b) => fmtDate(b.added_ms) },
  { key: "folder", label: "col.folder", width: 180, sort: "folder", value: (b) => b.folder || "/" },
];

export function columnsFor(keys: string[]): Column[] {
  const list = keys.map((k) => COLUMNS.find((c) => c.key === k)).filter((c): c is Column => !!c);
  // The name column is always present and first.
  return list.some((c) => c.key === "name") ? list : [COLUMNS[0], ...list];
}

export interface SortSpec {
  field: string;
  desc: boolean;
}
