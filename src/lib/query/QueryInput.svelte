<script lang="ts">
  import { api, type Vocabulary } from "../api";
  import { t, tx } from "../i18n.svelte";
  import { normalize, opAllowed, quoteParam, type Lang, type Token } from "../query";

  /**
   * Query text area with syntax highlighting, error underline and completions.
   * Highlighting is a `<pre>` mirrored under a transparent textarea.
   */
  let {
    value = $bindable(),
    lang,
    vocab,
    error = null,
    rows = 4,
    placeholder = "",
    oncaret,
    onsubmit,
  }: {
    value: string;
    lang: Lang | null;
    vocab: Vocabulary | null;
    error?: { start: number; end: number } | null;
    rows?: number;
    placeholder?: string;
    oncaret?: (caret: number) => void;
    onsubmit?: () => void;
  } = $props();

  let ta: HTMLTextAreaElement;
  let hl: HTMLPreElement;
  let caret = $state(0);

  const tokens = $derived<Token[]>(lang ? lang.tokenize(value) : [{ kind: "text", text: value, start: 0 }]);

  type Item = { label: string; insert: string; detail?: string };
  let items = $state<Item[]>([]);
  let active = $state(0);
  let from = 0;
  let open = $state(false);
  let reqId = 0;

  export function focus() {
    ta?.focus();
  }

  /** Replaces `start..end` with `text` and puts the caret after it. */
  export function replace(start: number, end: number, text: string) {
    value = value.slice(0, start) + text + value.slice(end);
    const pos = start + text.length;
    queueMicrotask(() => {
      ta.focus();
      ta.setSelectionRange(pos, pos);
      syncCaret();
    });
  }

  async function refreshCompletions() {
    if (!lang || !vocab) return;
    const ctx = lang.context(value, caret);
    const id = ++reqId;
    let list: Item[] = [];
    const starts = (s: string, p: string) => normalize(s).startsWith(normalize(p));
    if (ctx.kind === "start" || ctx.kind === "connective") {
      from = ctx.from;
      if (ctx.kind === "connective") {
        list = [vocab.and, vocab.or].map((k) => ({ label: k, insert: k + " " }));
      } else {
        list = [
          ...vocab.fields.map((f) => ({ label: f.name, insert: f.name + "(", detail: tx(`ftype.${f.ty.type}`) })),
          { label: vocab.sort_asc, insert: vocab.sort_asc + "(", detail: t("query.sort") },
          { label: vocab.sort_desc, insert: vocab.sort_desc + "(", detail: t("query.sort") },
          { label: vocab.not, insert: vocab.not + " ", detail: t("query.not") },
        ];
      }
      list = list.filter((it) => starts(it.label, ctx.prefix));
    } else if (ctx.kind === "param") {
      from = ctx.from;
      if (lang.sortNames.has(normalize(ctx.call))) {
        list = vocab.fields.map((f) => ({ label: f.name, insert: f.name + ")" }));
      } else {
        const field = lang.field(ctx.call);
        if (field) {
          const hasOp = ctx.params.some((p) => lang.opNames.has(normalize(p)));
          const ops = hasOp
            ? []
            : vocab.ops
                .filter((o) => opAllowed(o.op, field.ty.type))
                .map((o) => ({ label: o.name, insert: o.name + (ctx.params.length ? ")" : ":"), detail: t("query.operator") }));
          let values: Item[] = [];
          if (field.ty.type === "kind") values = vocab.kinds.map((k) => ({ label: k, insert: k }));
          else if (field.ext && (field.ty.type === "enum" || field.ty.type === "text")) {
            const s = await api.suggestValues(field.key, ctx.prefix).catch(() => [] as string[]);
            values = s.map((v) => ({ label: v, insert: quoteParam(v) }));
          }
          list = [...values, ...ops];
        }
      }
      list = list.filter((it) => starts(it.label, ctx.prefix));
    }
    if (id !== reqId) return;
    items = list.slice(0, 40);
    active = 0;
    open = items.length > 0 && document.activeElement === ta;
  }

  function accept(it: Item) {
    const before = value.slice(0, from);
    const after = value.slice(caret);
    value = before + it.insert + after;
    const pos = before.length + it.insert.length;
    open = false;
    queueMicrotask(() => {
      ta.focus();
      ta.setSelectionRange(pos, pos);
      syncCaret();
      if (/[(:]$/.test(it.insert)) refreshCompletions();
    });
  }

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      open = false;
      onsubmit?.();
      return;
    }
    if (e.ctrlKey && e.key === " ") {
      e.preventDefault();
      refreshCompletions();
      return;
    }
    if (!open) return;
    if (e.key === "ArrowDown") active = (active + 1) % items.length;
    else if (e.key === "ArrowUp") active = (active - 1 + items.length) % items.length;
    else if (e.key === "Enter" || e.key === "Tab") accept(items[active]);
    else if (e.key === "Escape") {
      open = false;
      e.stopPropagation();
    } else return;
    e.preventDefault();
  }

  function syncCaret() {
    caret = ta.selectionStart;
    hl.scrollTop = ta.scrollTop;
    hl.scrollLeft = ta.scrollLeft;
    oncaret?.(caret);
  }

  function underline(tk: Token) {
    if (!error) return false;
    const end = tk.start + tk.text.length;
    return error.end === error.start ? tk.start === error.start : tk.start < error.end && end > error.start;
  }
</script>

<div class="area">
  <pre bind:this={hl} aria-hidden="true">{#each tokens as tk, i (i)}<span class="t-{tk.kind}" class:err={underline(tk)}>{tk.text}</span
      >{/each}{" "}</pre>
  <textarea
    bind:this={ta}
    bind:value
    spellcheck="false"
    {rows}
    {placeholder}
    oninput={() => {
      syncCaret();
      refreshCompletions();
    }}
    onclick={() => {
      syncCaret();
      refreshCompletions();
    }}
    onkeyup={(e) => {
      if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(e.key) && !open) syncCaret();
    }}
    onscroll={syncCaret}
    onkeydown={onKey}
    onblur={() => setTimeout(() => (open = false), 150)}
  ></textarea>
  {#if open}
    <ul class="menu" role="listbox">
      {#each items as it, i (it.label + i)}
        <li>
          <button
            class:active={i === active}
            role="option"
            aria-selected={i === active}
            onmousedown={(e) => {
              e.preventDefault();
              accept(it);
            }}
          >
            <span>{it.label}</span>{#if it.detail}<small>{it.detail}</small>{/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .area {
    position: relative;
  }
  pre,
  textarea {
    font-family: var(--font-editor);
    font-size: calc(var(--font-size) + 3px);
    line-height: 1.7;
    padding: 16px 20px;
    margin: 0;
    border: 1px solid transparent;
    border-radius: var(--radius-lg);
    white-space: pre-wrap;
    word-break: break-word;
    box-sizing: border-box;
    width: 100%;
    min-height: 100%;
    letter-spacing: 0;
  }
  pre {
    position: absolute;
    inset: 0;
    overflow: hidden;
    color: var(--fg);
    background: var(--surface-2);
    pointer-events: none;
  }
  textarea {
    position: relative;
    display: block;
    resize: none;
    background: transparent;
    color: transparent;
    caret-color: var(--fg);
    outline: none;
    user-select: text;
    transition: border-color var(--t-fast) var(--ease);
  }
  textarea:focus {
    border-color: var(--accent-line);
  }
  textarea::placeholder {
    color: var(--faint);
  }
  .t-keyword {
    color: var(--syn-keyword);
    font-weight: 600;
  }
  .t-field {
    color: var(--syn-field);
    font-weight: 600;
  }
  .t-sort {
    color: var(--syn-sort);
    font-weight: 600;
  }
  .t-op {
    color: var(--syn-op);
  }
  .t-value,
  .t-string {
    color: var(--syn-value);
  }
  .t-punct {
    color: var(--faint);
  }
  .err {
    text-decoration: wavy underline var(--danger);
    text-underline-offset: 4px;
  }
  .menu {
    position: absolute;
    left: 16px;
    top: calc(100% + 4px);
    z-index: 5;
    min-width: 240px;
    max-height: 260px;
    margin: 0;
    padding: 4px;
    list-style: none;
    overflow-y: auto;
    border-radius: var(--radius-md);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
  .menu button {
    width: 100%;
    display: flex;
    justify-content: space-between;
    gap: 16px;
    height: 30px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    text-align: left;
  }
  .menu button.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .menu small {
    color: var(--faint);
  }
</style>
