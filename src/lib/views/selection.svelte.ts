import { SvelteSet } from "svelte/reactivity";

/** A folder of a source, as views select it. */
export interface FolderRef {
  sourceId: number;
  /** Relative to the source root. */
  path: string;
  /** Items inside (any depth), for confirmations. */
  count: number;
}

/**
 * Selection over an ordered list of media ids, shared by every view: click selects,
 * Ctrl/Cmd toggles, Shift extends from the anchor; arrow keys move the focus.
 * `focus` is the item the preview follows.
 *
 * Touch: a tap opens the item (a folder: enters it), a long press starts a selection, and
 * while one is in progress taps toggle items. Views call `pointer` on pointerdown so the
 * following click knows how it was made.
 */
export class Selection {
  readonly set = new SvelteSet<number>();
  focus = $state<number | null>(null);
  /** An item to scroll into view once the view lists it (cleared by the view). */
  revealing = $state<number | null>(null);
  /** Where the selection's context menu is open (the library draws it), or null. */
  menu = $state<{ x: number; y: number } | null>(null);
  /** Selected folders: never together with items. */
  folders = $state<FolderRef[]>([]);
  /** Where the folder's context menu is open, or null. */
  folderMenu = $state<{ x: number; y: number } | null>(null);
  #anchor = -1;
  #folderAnchor: FolderRef | null = null;
  #pointer = "mouse";

  /** The one selected folder, or null (none, or several). */
  get folder(): FolderRef | null {
    return this.folders.length === 1 ? this.folders[0] : null;
  }

  pointer(e: PointerEvent) {
    this.#pointer = e.pointerType;
  }

  /** The last press was a finger. */
  get touch() {
    return this.#pointer === "touch";
  }

  /** A click on item `index`: touch semantics on touch, `click` otherwise. */
  activate(e: MouseEvent, index: number, ids: number[], open: (id: number, ids: number[]) => void) {
    if (!this.touch) return this.click(e, index, ids);
    if (this.set.size > 0) this.toggle(index, ids);
    else if (ids[index] !== undefined) open(ids[index], ids);
  }

  /**
   * Long press (touch) or right click on item `index`. Touch: the first long press starts a
   * selection; within one, it adds the item and opens the context menu. Mouse: the item
   * joins the selection (or replaces it when not already selected) and the menu opens.
   */
  hold(e: MouseEvent, index: number, ids: number[]) {
    const id = ids[index];
    if (id === undefined) return;
    e.preventDefault();
    this.folders = [];
    if (this.touch) {
      navigator.vibrate?.(12);
      if (this.set.size === 0) return this.toggle(index, ids);
      if (!this.set.has(id)) this.toggle(index, ids);
    } else if (!this.set.has(id)) {
      this.set.clear();
      this.set.add(id);
      this.#anchor = index;
      this.focus = id;
    }
    this.menu = { x: e.clientX, y: e.clientY };
  }

  /** Selects folder `f` alone. */
  selectFolder(f: FolderRef) {
    this.#clearItems();
    this.folders = [f];
    this.#folderAnchor = f;
  }

  #clearItems() {
    this.set.clear();
    this.focus = null;
    this.#anchor = -1;
  }

  isFolder(sourceId: number, path: string) {
    return this.folders.some((f) => f.sourceId === sourceId && f.path === path);
  }

  toggleFolder(f: FolderRef) {
    this.#clearItems();
    const was = this.folders.length;
    this.folders = this.folders.filter((x) => x.sourceId !== f.sourceId || x.path !== f.path);
    if (this.folders.length === was) this.folders = [...this.folders, f];
    this.#folderAnchor = f;
  }

  /**
   * A click on a folder: selects it alone, Ctrl/Cmd toggles it, Shift extends over `list`
   * from the last folder clicked. On touch it only toggles while folders are selected.
   * Returns false when the click was not taken (touch: the caller opens the folder).
   */
  clickFolder(e: MouseEvent, f: FolderRef, list: FolderRef[] = []): boolean {
    if (this.touch) {
      if (this.folders.length === 0) return false;
      this.toggleFolder(f);
      return true;
    }
    const same = (a: FolderRef) => (b: FolderRef) => a.sourceId === b.sourceId && a.path === b.path;
    const from = this.#folderAnchor ? list.findIndex(same(this.#folderAnchor)) : -1;
    const to = list.findIndex(same(f));
    if (e.shiftKey && from >= 0 && to >= 0) {
      const [a, b] = from < to ? [from, to] : [to, from];
      const range = list.slice(a, b + 1);
      const keep = e.ctrlKey || e.metaKey ? this.folders.filter((x) => !range.some(same(x))) : [];
      this.#clearItems();
      this.folders = [...keep, ...range];
    } else if (e.ctrlKey || e.metaKey) this.toggleFolder(f);
    else this.selectFolder(f);
    return true;
  }

  /**
   * Long press or right click on a folder: it joins the selected folders (mouse: replaces
   * them unless already selected) and the menu opens.
   */
  holdFolder(e: MouseEvent, f: FolderRef) {
    e.preventDefault();
    if (this.touch) navigator.vibrate?.(12);
    if (!this.isFolder(f.sourceId, f.path)) {
      if (this.touch) {
        this.#clearItems();
        this.folders = [...this.folders, f];
        this.#folderAnchor = f;
      } else this.selectFolder(f);
    }
    this.folderMenu = { x: e.clientX, y: e.clientY };
  }

  toggle(index: number, ids: number[]) {
    const id = ids[index];
    if (id === undefined) return;
    this.folders = [];
    if (this.set.has(id)) {
      this.set.delete(id);
      if (this.focus === id) this.focus = this.set.size ? [...this.set].at(-1)! : null;
    } else {
      this.set.add(id);
      this.focus = id;
    }
    this.#anchor = index;
  }

  click(e: MouseEvent | KeyboardEvent, index: number, ids: number[]) {
    const id = ids[index];
    if (id === undefined) return;
    const additive = e.ctrlKey || e.metaKey;
    this.folders = [];
    if (e.shiftKey && this.#anchor >= 0) {
      const [a, b] = this.#anchor < index ? [this.#anchor, index] : [index, this.#anchor];
      if (!additive) this.set.clear();
      for (let i = a; i <= b; i++) this.set.add(ids[i]);
    } else if (additive) {
      if (this.set.has(id)) this.set.delete(id);
      else this.set.add(id);
      this.#anchor = index;
    } else {
      this.set.clear();
      this.set.add(id);
      this.#anchor = index;
    }
    this.focus = id;
  }

  /** Moves the focus by `delta` items; with `extend`, grows the selection from the anchor. */
  move(delta: number, ids: number[], extend: boolean): number {
    if (ids.length === 0) return -1;
    this.folders = [];
    const cur = this.focus == null ? -1 : ids.indexOf(this.focus);
    const next = cur < 0 ? (delta > 0 ? 0 : ids.length - 1) : Math.min(ids.length - 1, Math.max(0, cur + delta));
    if (extend && this.#anchor >= 0) {
      this.set.clear();
      const [a, b] = this.#anchor < next ? [this.#anchor, next] : [next, this.#anchor];
      for (let i = a; i <= b; i++) this.set.add(ids[i]);
    } else {
      this.set.clear();
      this.set.add(ids[next]);
      this.#anchor = next;
    }
    this.focus = ids[next];
    return next;
  }

  selectAll(ids: number[]) {
    this.folders = [];
    for (const id of ids) this.set.add(id);
  }

  clear() {
    this.folders = [];
    this.#folderAnchor = null;
    this.set.clear();
    this.focus = null;
    this.#anchor = -1;
  }

  /** Drops ids that are no longer listed (after a new search). */
  retain(ids: number[]) {
    if (this.set.size === 0) return;
    const keep = new Set(ids);
    for (const id of [...this.set]) if (!keep.has(id)) this.set.delete(id);
    if (this.focus != null && !keep.has(this.focus)) this.focus = null;
  }
}

/** Shared keyboard handling: returns the step for an arrow key, or 0. */
export function arrowStep(e: KeyboardEvent, cols: number, pageRows: number): number {
  switch (e.key) {
    case "ArrowRight":
      return cols > 1 ? 1 : 0;
    case "ArrowLeft":
      return cols > 1 ? -1 : 0;
    case "ArrowDown":
      return cols;
    case "ArrowUp":
      return -cols;
    case "PageDown":
      return cols * pageRows;
    case "PageUp":
      return -cols * pageRows;
    case "Home":
      return -Infinity;
    case "End":
      return Infinity;
    default:
      return 0;
  }
}
