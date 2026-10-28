import { cubicOut } from "svelte/easing";
import type { TransitionConfig } from "svelte/transition";

/**
 * Open/close motion for overlays, timed by the design tokens in `app.css` (`--t-pop`).
 * Dialogs and menus already animate in with CSS (`vp-pop`), so they take these as `out:`;
 * overlays without a CSS entrance use them as `transition:`.
 */
function token(name: string, fallback: number): number {
  if (typeof document === "undefined") return fallback;
  return ms(getComputedStyle(document.documentElement).getPropertyValue(name)) ?? fallback;
}

/**
 * A CSS time in milliseconds. The production CSS minifier rewrites `220ms` as `.22s`;
 * read as a bare number that made every transition last 0.22 ms in release builds.
 */
export function ms(css: string): number | null {
  const m = /^\s*(-?[\d.]+)(ms|s)?\s*$/i.exec(css);
  if (!m) return null;
  const v = parseFloat(m[1]);
  if (!Number.isFinite(v)) return null;
  return m[2]?.toLowerCase() === "s" ? v * 1000 : v;
}

/** The app's own "reduce motion" setting (`ui.reduce_motion`, set on the root by `prefs`). */
const reduced = () => typeof document !== "undefined" && document.documentElement.dataset.motion === "reduce";

/** Fades. */
export function fade(_node: Element, { duration }: { duration?: number } = {}): TransitionConfig {
  return { duration: reduced() ? 0 : (duration ?? token("--t-pop", 220)), easing: cubicOut, css: (t) => `opacity: ${t}` };
}

/**
 * Fades with a small lift and scale (dialogs, menus, popovers). `base` is the element's
 * own transform, kept underneath (a centered banner's `translateX(-50%)`).
 */
export function pop(_node: Element, { base = "", y = 10, duration }: { base?: string; y?: number; duration?: number } = {}): TransitionConfig {
  return {
    duration: reduced() ? 0 : (duration ?? token("--t-pop", 220)),
    easing: cubicOut,
    css: (t, u) => `opacity: ${t}; transform: ${base} translateY(${u * y}px) scale(${0.94 + 0.06 * t})`,
  };
}

/** Slides in from (and out to) an edge by `x`/`y` px (side panels, sheets, drawers). */
export function slide(_node: Element, { x = 0, y = 0, duration }: { x?: number; y?: number; duration?: number } = {}): TransitionConfig {
  return {
    duration: reduced() ? 0 : (duration ?? token("--t-pop", 220) * 1.2),
    easing: cubicOut,
    css: (t, u) => `opacity: ${Math.min(1, t * 1.5)}; transform: translate(${u * x}px, ${u * y}px)`,
  };
}

/**
 * Opens (and closes) in place by growing from no height, pushing what follows down
 * smoothly (menus and sections that open inside a list rather than over it).
 */
export function expand(node: Element, { duration }: { duration?: number } = {}): TransitionConfig {
  const st = getComputedStyle(node);
  const px = (v: string) => parseFloat(v) || 0;
  const [h, pt, pb, mt, mb] = [st.height, st.paddingTop, st.paddingBottom, st.marginTop, st.marginBottom].map(px);
  return {
    duration: reduced() ? 0 : (duration ?? token("--t-pop", 220)),
    easing: cubicOut,
    css: (t) =>
      `overflow: hidden; opacity: ${Math.min(1, t * 1.4)}; height: ${t * h}px; padding-top: ${t * pt}px; padding-bottom: ${t * pb}px;` +
      ` margin-top: ${t * mt}px; margin-bottom: ${t * mb}px; transform: translateY(${(t - 1) * 6}px)`,
  };
}

/**
 * Slides all the way in from (and out to) the nearest screen `side` (side panels and
 * sheets that sit against an edge), fading only at the start.
 */
export function edge(node: Element, { side = "right", duration }: { side?: "left" | "right" | "bottom"; duration?: number } = {}): TransitionConfig {
  const r = node.getBoundingClientRect();
  const dist =
    side === "right" ? window.innerWidth - r.left : side === "left" ? r.right : window.innerHeight - r.top;
  const [x, y] = side === "bottom" ? [0, dist] : [side === "left" ? -dist : dist, 0];
  return {
    duration: reduced() ? 0 : (duration ?? token("--t-pop", 220) * 1.4),
    easing: cubicOut,
    css: (t, u) => `opacity: ${Math.min(1, t * 3)}; transform: translate(${u * x}px, ${u * y}px)`,
  };
}

/** Grows into place from a little smaller, fading (a full-screen view opening over the library). */
export function zoom(_node: Element, { from = 0.9, duration }: { from?: number; duration?: number } = {}): TransitionConfig {
  return {
    duration: reduced() ? 0 : (duration ?? token("--t-pop", 220) * 1.2),
    easing: cubicOut,
    css: (t, u) => `opacity: ${t}; transform: scale(${from + (1 - from) * t}); transform-origin: center`,
  };
}
