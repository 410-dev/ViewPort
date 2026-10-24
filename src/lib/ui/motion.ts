import { cubicOut } from "svelte/easing";
import type { TransitionConfig } from "svelte/transition";

/**
 * Open/close motion for overlays, timed by the design tokens in `app.css` (`--t-pop`).
 * Dialogs and menus already animate in with CSS (`vp-pop`), so they take these as `out:`;
 * overlays without a CSS entrance use them as `transition:`.
 */
function token(name: string, fallback: number): number {
  if (typeof document === "undefined") return fallback;
  const v = parseFloat(getComputedStyle(document.documentElement).getPropertyValue(name));
  return Number.isFinite(v) ? v : fallback;
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
