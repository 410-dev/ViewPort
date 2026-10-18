import type { FieldType, Vocabulary } from "./api";

/** Mirrors `vp_query::lang::normalize`: lowercase, whitespace removed. */
export const normalize = (s: string) => s.replace(/\s+/g, "").toLowerCase();

export type TokenKind = "keyword" | "field" | "sort" | "op" | "value" | "string" | "punct" | "space" | "text";
export interface Token {
  kind: TokenKind;
  text: string;
  start: number;
}

const OPS_FOR: Record<string, (t: FieldType["type"]) => boolean> = {
  eq: () => true,
  ne: () => true,
  contains_any: () => true,
  contains_none: () => true,
  exists: () => true,
  not_exists: () => true,
  gt: (t) => numeric(t),
  ge: (t) => numeric(t),
  lt: (t) => numeric(t),
  le: (t) => numeric(t),
  between: (t) => numeric(t),
  contains_all: (t) => t === "text" || t === "enum",
  starts_with: (t) => t === "text",
  ends_with: (t) => t === "text",
  glob: (t) => t === "text",
};

function numeric(t: FieldType["type"]) {
  return ["integer", "real", "date", "size", "duration", "ranged_int"].includes(t);
}

export const opAllowed = (op: string, t: FieldType["type"]) => OPS_FOR[op]?.(t) ?? false;

export class Lang {
  keywords = new Set<string>();
  sortNames = new Set<string>();
  opNames = new Set<string>();

  constructor(public vocab: Vocabulary) {
    for (const k of [vocab.and, vocab.or, vocab.not, "and", "or", "not", "그리고", "혹은", "또는", "및", "아님"]) {
      this.keywords.add(normalize(k));
    }
    for (const s of [vocab.sort_asc, vocab.sort_desc, "정렬", "오름차순", "내림차순", "sort", "asc", "desc"]) {
      this.sortNames.add(normalize(s));
    }
    for (const o of vocab.ops) this.opNames.add(normalize(o.name));
  }

  field(name: string) {
    const n = normalize(name);
    return this.vocab.fields.find((f) => f.aliases.some((a) => normalize(a) === n) || normalize(f.name) === n);
  }

  /** Splits text into tokens for highlighting. Never fails; unknown input becomes `text`. */
  tokenize(src: string): Token[] {
    const out: Token[] = [];
    let i = 0;
    let depth = 0; // depth inside a call's parentheses
    const callStack: boolean[] = [];
    const push = (kind: TokenKind, end: number) => {
      out.push({ kind, text: src.slice(i, end), start: i });
      i = end;
    };
    while (i < src.length) {
      const c = src[i];
      if (/\s/.test(c)) {
        let j = i;
        while (j < src.length && /\s/.test(src[j])) j++;
        push("space", j);
      } else if (c === '"') {
        let j = i + 1;
        while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
        push("string", Math.min(src.length, j + 1));
      } else if (c === "(" || c === ")" || c === ":") {
        if (c === "(") callStack.push(false);
        if (c === ")") {
          if (callStack.pop()) depth--;
        }
        push("punct", i + 1);
      } else if (depth > 0) {
        // Inside a call: parameters until ':' or ')'.
        let j = i;
        while (j < src.length && !":()\"".includes(src[j])) j++;
        const raw = src.slice(i, j);
        const trimmed = raw.trimEnd();
        push(this.opNames.has(normalize(trimmed)) ? "op" : "value", i + trimmed.length);
        if (trimmed.length < raw.length) push("space", j);
      } else {
        // Top level: a run of words. If it ends with '(', it is a call name.
        let j = i;
        while (j < src.length && !"():\"".includes(src[j])) j++;
        const segment = src.slice(i, j);
        if (src[j] === "(") {
          // Leading keywords stay keywords; the rest is the name.
          const words = segment.match(/\S+|\s+/g) ?? [];
          let k = 0;
          while (k < words.length && (/^\s+$/.test(words[k]) || this.keywords.has(normalize(words[k])))) {
            push(/^\s+$/.test(words[k]) ? "space" : "keyword", i + words[k].length);
            k++;
          }
          const name = src.slice(i, j);
          push(this.sortNames.has(normalize(name)) ? "sort" : "field", j);
          push("punct", j + 1);
          callStack.push(true);
          depth++;
        } else {
          for (const w of segment.match(/\S+|\s+/g) ?? []) {
            push(/^\s+$/.test(w) ? "space" : this.keywords.has(normalize(w)) ? "keyword" : "text", i + w.length);
          }
        }
      }
    }
    return out;
  }

  /** What the caret is inside of, for completions. */
  context(src: string, caret: number): Context {
    const pre = src.slice(0, caret);
    const stack: { pos: number; call: string | null }[] = [];
    let inQuote = false;
    let segStart = 0;
    for (let i = 0; i < pre.length; i++) {
      const c = pre[i];
      if (inQuote) {
        if (c === "\\") i++;
        else if (c === '"') inQuote = false;
        continue;
      }
      if (c === '"') inQuote = true;
      else if (c === "(") {
        const inCall = stack.length > 0 && stack[stack.length - 1].call !== null;
        const words = pre.slice(segStart, i).trim().split(/\s+/).filter(Boolean);
        while (words.length && this.keywords.has(normalize(words[0]))) words.shift();
        const name = words.join(" ");
        stack.push({ pos: i, call: !inCall && name ? name : null });
        segStart = i + 1;
      } else if (c === ")") {
        stack.pop();
        segStart = i + 1;
      }
    }
    if (inQuote) return { kind: "none" };
    const top = stack[stack.length - 1];
    if (top?.call) {
      const inside = pre.slice(top.pos + 1);
      const lastColon = inside.lastIndexOf(":");
      const prefix = inside.slice(lastColon + 1).trimStart();
      const params = inside.slice(0, Math.max(0, lastColon)).split(":").map((p) => p.trim()).filter(Boolean);
      return { kind: "param", call: top.call, prefix, params, from: caret - prefix.length };
    }
    const m = /[^\s()]*$/.exec(pre)!;
    const prefix = m[0];
    const before = pre.slice(0, caret - prefix.length).trimEnd();
    const afterCall = before.endsWith(")");
    return { kind: afterCall ? "connective" : "start", prefix, from: caret - prefix.length };
  }
}

export type Context =
  | { kind: "none" }
  | { kind: "start" | "connective"; prefix: string; from: number }
  | { kind: "param"; call: string; prefix: string; params: string[]; from: number };

export function quoteParam(v: string): string {
  return /[():"\\]/.test(v) || v.trim() !== v ? `"${v.replace(/["\\]/g, (c) => "\\" + c)}"` : v;
}
