/**
 * UI strings. Each language is one dictionary file in `./i18n/`; to add a language,
 * add a file that satisfies `Dict` and register it in `LOCALES`. Missing entries fall
 * back to English. Query keywords are localized by the backend (`vp_query::lang`).
 */
import en from "./i18n/en";
import ko, { type Key } from "./i18n/ko";

export type { Key };
export type Dict = Record<Key, string>;

export const LOCALES: { code: string; name: string; dict: Dict }[] = [
  { code: "ko", name: "한국어", dict: ko },
  { code: "en", name: "English", dict: en },
];

/** Languages the query engine understands. */
export const QUERY_LOCALES = ["ko", "en"];

function systemLocale(): string {
  const lang = (typeof navigator !== "undefined" ? navigator.language : "en").toLowerCase();
  return LOCALES.find((l) => lang.startsWith(l.code))?.code ?? "en";
}

export function resolveLocale(code: string): string {
  return !code || code === "auto" ? systemLocale() : LOCALES.some((l) => l.code === code) ? code : "en";
}

export const i18n = $state({ locale: systemLocale(), queryLocale: systemLocale() });

export function t(key: Key, vars: Record<string, string | number | null | undefined> = {}): string {
  const dict = LOCALES.find((l) => l.code === i18n.locale)?.dict ?? en;
  const s = dict[key] ?? en[key] ?? key;
  return s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? ""));
}

/** Like `t`, for keys built at runtime (e.g. `task.kind.${kind}`); unknown keys give `fallback`. */
export function tx(key: string, fallback = "", vars: Record<string, string | number> = {}): string {
  const dict = (LOCALES.find((l) => l.code === i18n.locale)?.dict ?? en) as Record<string, string>;
  const s = dict[key] ?? (en as Record<string, string>)[key];
  return s == null ? fallback : s.replace(/\{(\w+)\}/g, (_, k) => String(vars[k] ?? ""));
}
