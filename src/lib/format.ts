import { i18n } from "./i18n.svelte";

const KB = 1024;
const MB = KB * 1024;
const GB = MB * 1024;

export function fmtBytes(b: number | null | undefined): string {
  if (b == null) return "–";
  if (b >= GB) return `${(b / GB).toFixed(2)} GB`;
  if (b >= MB) return `${(b / MB).toFixed(1)} MB`;
  if (b >= KB) return `${Math.ceil(b / KB)} KB`;
  return `${b} B`;
}

export function fmtDate(ms: number | null | undefined, withTime = true): string {
  if (ms == null) return "–";
  const d = new Date(ms);
  return withTime
    ? d.toLocaleString(i18n.locale, { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit", hourCycle: "h23" })
    : d.toLocaleDateString(i18n.locale);
}

export function fmtDuration(ms: number | null | undefined): string {
  if (ms == null) return "";
  const s = Math.round(ms / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return `${h ? h + ":" : ""}${String(m).padStart(h ? 2 : 1, "0")}:${String(s % 60).padStart(2, "0")}`;
}

export function fmtCount(n: number): string {
  return n.toLocaleString(i18n.locale);
}
