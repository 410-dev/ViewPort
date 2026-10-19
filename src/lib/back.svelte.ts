/**
 * The Android back button (and gesture) closes the topmost open layer: the viewer, a
 * dialog, a drawer, a dropdown, immersive mode. A layer registers a handler while it is
 * open; the most recently registered one runs first. A handler that returns `PASS` did
 * nothing and lets the next one try. With nothing left, the app exits.
 *
 * Use in a component: `$effect(() => onBack(close))`, or conditionally
 * `$effect(() => { if (open) return onBack(() => (open = false)); })`.
 */
import { exit, onBackButtonPress } from "@tauri-apps/api/app";
import { isAndroid } from "./platform";

type Handler = () => unknown;

/** Returned by a handler that had nothing to do. */
export const PASS = Symbol("pass");

const stack: Handler[] = [];

export function onBack(handler: Handler): () => void {
  stack.push(handler);
  return () => {
    const i = stack.lastIndexOf(handler);
    if (i >= 0) stack.splice(i, 1);
  };
}

/** Takes over the back button (Android). Call once per window. */
export function startBackButton() {
  if (!isAndroid) return;
  onBackButtonPress(() => {
    for (let i = stack.length - 1; i >= 0; i--) {
      if (stack[i]() !== PASS) return;
    }
    exit(0).catch(() => {});
  }).catch(() => {});
}
