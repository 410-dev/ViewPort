/**
 * Where each listing was scrolled to, so a view returns there after its results are
 * reloaded or it is remounted (back from a folder, out of the viewer). Keys come from the
 * library (`ViewProps.scrollKey`); positions are in item slots (folders first, then
 * files), fractional, so grid and list views and different column counts share them.
 */
const memory = new Map<string, number>();

export function rememberScroll(key: string, slot: number) {
  memory.set(key, slot);
}

export function recallScroll(key: string): number {
  return memory.get(key) ?? 0;
}

/** A section of the result: where it starts (item index) and what it is called. */
export interface SectionMark {
  index: number;
  label: string;
}

/** The label of the section holding item `index` (marks sorted by index), or null. */
export function sectionAt(marks: SectionMark[], index: number): string | null {
  let lo = 0;
  let hi = marks.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (marks[mid].index <= index) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  return found < 0 ? null : marks[found].label;
}
