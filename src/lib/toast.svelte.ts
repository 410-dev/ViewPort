/**
 * Short "done" messages: pills stacked at the bottom of the window (`ui/Toasts.svelte`),
 * each leaving after `ui.toast_ms` or when swiped away. Only the window that did the work
 * shows its message.
 */
import { i18n, t } from "./i18n.svelte";
import { prefs } from "./prefs.svelte";
import type { IconName } from "./ui/Icon.svelte";

export interface Toast {
  id: number;
  text: string;
  icon: IconName;
}

export const toasts = $state<{ list: Toast[] }>({ list: [] });

let next = 1;
const timers = new Map<number, { left: number; started: number; timer?: ReturnType<typeof setTimeout> }>();

/** Shows `text` in a new pill. */
export function toast(text: string, icon: IconName = "check") {
  const ui = prefs.app?.ui;
  const id = next++;
  toasts.list.push({ id, text, icon });
  const max = Math.max(1, ui?.toast_max || 4);
  while (toasts.list.length > max) dismiss(toasts.list[0].id);
  timers.set(id, { left: Math.max(500, ui?.toast_ms || 3500), started: 0 });
  resume(id);
  return id;
}

export function dismiss(id: number) {
  const tm = timers.get(id);
  if (tm) clearTimeout(tm.timer);
  timers.delete(id);
  const i = toasts.list.findIndex((x) => x.id === id);
  if (i >= 0) toasts.list.splice(i, 1);
}

/** Held (pressed or hovered): the message does not leave by itself meanwhile. */
export function pause(id: number) {
  const tm = timers.get(id);
  if (!tm?.timer) return;
  clearTimeout(tm.timer);
  tm.timer = undefined;
  tm.left = Math.max(800, tm.left - (performance.now() - tm.started));
}

export function resume(id: number) {
  const tm = timers.get(id);
  if (!tm || tm.timer) return;
  tm.started = performance.now();
  tm.timer = setTimeout(() => dismiss(id), tm.left);
}

/** Korean subject particle for `word`: 이 after a final consonant, 가 otherwise. */
function josa(word: string) {
  const c = word.trim().charCodeAt(word.trim().length - 1);
  if (c >= 0xac00 && c <= 0xd7a3) return (c - 0xac00) % 28 ? "이" : "가";
  return "(이)가";
}

/**
 * "`what` is done": for one item, `n` items, or `n` folders.
 * `what` is the action, e.g. `t("toast.add", { field, value })`.
 */
export function done(what: string, n = 1, folders = false) {
  const josaVar = i18n.locale === "ko" ? josa(what) : "";
  const vars = { what, josa: josaVar, n: n.toLocaleString() };
  toast(folders ? t("toast.doneFolders", vars) : n > 1 ? t("toast.doneMany", vars) : t("toast.done", vars));
}
