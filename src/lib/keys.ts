/** Key bindings use `KeyboardEvent.key` names with optional `Ctrl+`/`Alt+`/`Shift+`/`Meta+` prefixes. */

interface Binding {
  key: string;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
}

function parse(spec: string): Binding {
  // "Shift++" style is not needed; split on "+" but keep a trailing "+" key.
  const parts = spec.split("+");
  const key = parts.pop() || "+";
  const mods = new Set(parts.map((p) => p.toLowerCase()));
  return {
    key: key.length === 1 ? key.toLowerCase() : key,
    ctrl: mods.has("ctrl") || mods.has("control"),
    alt: mods.has("alt"),
    shift: mods.has("shift"),
    meta: mods.has("meta") || mods.has("cmd"),
  };
}

const cache = new Map<string, Binding>();

export function matches(e: KeyboardEvent, specs: string[] | undefined): boolean {
  if (!specs) return false;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  return specs.some((spec) => {
    let b = cache.get(spec);
    if (!b) cache.set(spec, (b = parse(spec)));
    return (
      b.key === key && b.ctrl === e.ctrlKey && b.alt === e.altKey && b.shift === e.shiftKey && b.meta === e.metaKey
    );
  });
}

/** True when the event comes from a text field, where keys must keep their normal meaning. */
export function isTyping(e: KeyboardEvent): boolean {
  const t = e.target as HTMLElement | null;
  return !!t && (t.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(t.tagName));
}
