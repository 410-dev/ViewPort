import type { Selection } from "./selection.svelte";

/** Movement (px) before a press counts as a drag. */
const SLOP = 8;
/** Within this distance (px) of the top or bottom, a sweep scrolls the view. */
const EDGE = 56;

/** What a view tells a sweep about itself. */
export interface SweepHost {
  el: () => HTMLElement | undefined;
  /** The item index under a viewport point, clamped to the list (-1: empty list). */
  indexAt: (x: number, y: number) => number;
  ids: () => number[];
  selection: () => Selection;
  /** Where holding then dragging an item moves it (temporary groups): how long the hold is. */
  reorderHoldMs: () => number | null;
  /** Touch, during a selection: a drag that starts sideways sweeps (false where sideways scrolls). */
  sideways: () => boolean;
  /** Top of the scroller not showing items (a header row), px. */
  inset?: number;
}

/** A press on an item that may become a sweep. */
interface Press {
  index: number;
  x: number;
  y: number;
  t: number;
  pointerId: number;
  touch: boolean;
  held: boolean;
  /** The selection before the press (a touch hold may have changed it since). */
  before: number[];
  wasSelected: boolean;
  additive: boolean;
}

/**
 * Drag-selecting a run of items ("sweep"). Every item between where the drag started and
 * where the pointer is joins the selection (or, when the first item was already selected,
 * leaves it); near the top or bottom edge the view scrolls and the run keeps growing.
 *
 * Touch: a long press then a drag sweeps, and during a selection so does a drag that starts
 * sideways (an upward or downward start still scrolls). Mouse: a drag from an item sweeps,
 * replacing the selection (Ctrl/Cmd: adding to it).
 */
export class Sweep {
  active = $state(false);
  #p: Press | null = null;
  #base: number[] = [];
  #add = true;
  #x = 0;
  #y = 0;
  #frame = 0;
  #dropped = false;

  constructor(private host: SweepHost) {}

  /** The click that ends a sweep is not a click on the item. */
  get dropped() {
    return this.#dropped;
  }

  /** Touch: the finger must not scroll (sweeping, or held where a hold sweeps). */
  get blocksScroll() {
    return this.active || (!!this.#p?.held && this.host.reorderHoldMs() == null);
  }

  /** Pointerdown on item `index`. */
  down(e: PointerEvent, index: number) {
    if (!e.isPrimary || (e.pointerType === "mouse" && e.button !== 0)) return;
    this.cancel();
    const sel = this.host.selection();
    this.#p = {
      index,
      x: e.clientX,
      y: e.clientY,
      t: performance.now(),
      pointerId: e.pointerId,
      touch: e.pointerType !== "mouse",
      held: false,
      before: [...sel.set],
      wasSelected: sel.set.has(this.host.ids()[index]),
      additive: e.ctrlKey || e.metaKey,
    };
    window.addEventListener("pointermove", this.#move);
    window.addEventListener("pointerup", this.#up);
    window.addEventListener("pointercancel", this.#cancel);
  }

  /** The press was held (a touch long press: the view's context menu event). */
  held() {
    if (this.#p) this.#p.held = true;
  }

  cancel() {
    cancelAnimationFrame(this.#frame);
    this.#p = null;
    this.active = false;
    window.removeEventListener("pointermove", this.#move);
    window.removeEventListener("pointerup", this.#up);
    window.removeEventListener("pointercancel", this.#cancel);
  }

  #move = (e: PointerEvent) => {
    const p = this.#p;
    if (!p || e.pointerId !== p.pointerId) return;
    this.#x = e.clientX;
    this.#y = e.clientY;
    if (!this.active) {
      const dx = e.clientX - p.x;
      const dy = e.clientY - p.y;
      if (Math.hypot(dx, dy) <= SLOP) return;
      const reorder = this.host.reorderHoldMs();
      const ok = p.touch
        ? (p.held && reorder == null) || (p.before.length > 0 && Math.abs(dx) > Math.abs(dy) && this.host.sideways())
        : reorder == null || performance.now() - p.t < reorder;
      if (!ok) return this.cancel();
      this.#start(p);
    }
    this.#update();
  };

  #start(p: Press) {
    const replace = !p.touch && !p.additive;
    this.#base = replace ? [] : p.before;
    this.#add = replace || !p.wasSelected;
    this.host.selection().menu = null;
    this.active = true;
    this.#frame = requestAnimationFrame(this.#edge);
  }

  #update() {
    const p = this.#p;
    if (!p) return;
    const to = this.host.indexAt(this.#x, this.#y);
    if (to < 0) return;
    this.host.selection().sweep(this.#base, p.index, to, this.host.ids(), this.#add);
  }

  /** Near the top or bottom the view scrolls (faster the closer) and the run follows. */
  #edge = () => {
    const el = this.host.el();
    if (!this.active || !el) return;
    const r = el.getBoundingClientRect();
    const up = this.#y - (r.top + (this.host.inset ?? 0));
    const down = r.bottom - this.#y;
    const step = up < EDGE ? -(EDGE - Math.max(up, -EDGE)) / 4 : down < EDGE ? (EDGE - Math.max(down, -EDGE)) / 4 : 0;
    if (step) {
      const before = el.scrollTop;
      el.scrollTop += step;
      if (el.scrollTop !== before) this.#update();
    }
    this.#frame = requestAnimationFrame(this.#edge);
  };

  #up = (e: PointerEvent) => {
    if (!this.#p || e.pointerId !== this.#p.pointerId) return;
    const swept = this.active;
    this.cancel();
    if (swept) {
      this.#dropped = true;
      setTimeout(() => (this.#dropped = false));
    }
  };

  #cancel = (e: PointerEvent) => {
    if (this.#p && e.pointerId === this.#p.pointerId) this.cancel();
  };
}
