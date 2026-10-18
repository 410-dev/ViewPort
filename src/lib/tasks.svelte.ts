/**
 * Mirror of the backend task registry (see `src-tauri/src/tasks.rs`). Finished tasks are
 * kept for `progress.keep_finished_secs` so the status bar can show what just happened.
 */
import { api, onTask, type TaskInfo } from "./api";
import { prefs } from "./prefs.svelte";

export const tasks = $state<{ list: TaskInfo[] }>({ list: [] });

const timers = new Map<number, ReturnType<typeof setTimeout>>();

/** Kinds that come and go constantly; they vanish as soon as they finish (unless they failed). */
const EPHEMERAL = new Set(["read", "thumbs_visible"]);

function upsert(t: TaskInfo) {
  const i = tasks.list.findIndex((x) => x.id === t.id);
  if (i >= 0) tasks.list[i] = t;
  else tasks.list.push(t);
  if (t.state !== "running" && !timers.has(t.id)) {
    const keep = EPHEMERAL.has(t.kind) && t.state !== "failed" ? 0 : (prefs.app?.progress.keep_finished_secs ?? 5) * 1000;
    timers.set(
      t.id,
      setTimeout(() => {
        timers.delete(t.id);
        tasks.list = tasks.list.filter((x) => x.id !== t.id);
      }, keep),
    );
  }
}

let started = false;
export function startTaskTracking() {
  if (started) return;
  started = true;
  onTask(upsert);
  api.listTasks().then((list) => list.forEach(upsert)).catch(() => {});
}

export function running(): TaskInfo[] {
  return tasks.list.filter((t) => t.state === "running");
}

/** 0..1, or null when the total is unknown. */
export function fraction(t: TaskInfo): number | null {
  if (t.total == null || t.total === 0) return t.state === "running" ? null : 1;
  return Math.min(1, t.done / t.total);
}
