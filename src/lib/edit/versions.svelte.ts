/**
 * Items whose file was replaced (edited) while the app runs, and when. Media and thumbnail
 * URLs carry it, so the WebView loads the new file instead of what it cached.
 */
import { onSync } from "../sync";

const versions = $state<Record<number, number>>({});

export function fileVersion(id: number): number | undefined {
  return versions[id];
}

export function bumpFile(id: number) {
  versions[id] = Date.now();
}

onSync("edited", (ids) => ids?.forEach(bumpFile)).catch(() => {});
