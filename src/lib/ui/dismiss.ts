/**
 * Closes a small overlay (popover, panel, menu) on a press anywhere outside it.
 * `keep` lists elements that count as inside (the button that toggles it); `ignore` lets
 * the owner pass over a press (a video's control bar).
 */
export interface DismissOptions {
  onclose: () => void;
  keep?: () => (Element | null | undefined)[];
  ignore?: (e: PointerEvent) => boolean;
}

export function dismiss(node: HTMLElement, options: DismissOptions) {
  let opts = options;
  const down = (e: PointerEvent) => {
    const t = e.target as Node | null;
    if (!t || node.contains(t) || opts.keep?.().some((k) => k?.contains(t)) || opts.ignore?.(e)) return;
    opts.onclose();
  };
  // Listen after the press that opened it has finished.
  const timer = setTimeout(() => window.addEventListener("pointerdown", down, true));
  return {
    update(next: DismissOptions) {
      opts = next;
    },
    destroy() {
      clearTimeout(timer);
      window.removeEventListener("pointerdown", down, true);
    },
  };
}
