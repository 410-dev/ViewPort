<script lang="ts">
  import { untrack } from "svelte";
  import { api, asCmdError, type SavedQuery, type Vocabulary } from "./api";
  import { i18n } from "./i18n.svelte";
  import { Lang, normalize, opAllowed, quoteParam, type Token } from "./query";

  let {
    value = $bindable(),
    onrun,
    onclose,
    onsaved,
    editing = null,
  }: {
    value: string;
    onrun: (text: string) => void;
    onclose: () => void;
    onsaved: () => void;
    editing?: SavedQuery | null;
  } = $props();

  type L = { ko: string; en: string };
  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);

  let vocab = $state<Vocabulary | null>(null);
  const lang = $derived(vocab ? new Lang(vocab) : null);
  $effect(() => {
    api.queryVocabulary(i18n.locale).then((v) => (vocab = v));
  });

  let ta: HTMLTextAreaElement;
  let hl: HTMLPreElement;
  let caret = $state(0);
  let error = $state<{ start: number; end: number; message: string } | null>(null);
  let normalized = $state("");

  // ---- validation (debounced)
  let checkTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const text = value;
    clearTimeout(checkTimer);
    checkTimer = setTimeout(async () => {
      if (!text.trim()) {
        error = null;
        normalized = "";
        return;
      }
      const r = await api.checkQuery(text, i18n.locale);
      if (r.status === "ok") {
        error = null;
        normalized = r.normalized;
      } else {
        error = { ...r.span, message: r.error.kind.code };
        normalized = "";
      }
    }, 200);
  });

  const tokens = $derived<Token[]>(lang ? lang.tokenize(value) : [{ kind: "text", text: value, start: 0 }]);

  // ---- completions
  type Item = { label: string; insert: string; detail?: string };
  let items = $state<Item[]>([]);
  let active = $state(0);
  let from = 0;
  let open = $state(false);
  let reqId = 0;

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
          ...vocab.fields.map((f) => ({ label: f.name, insert: f.name + "(", detail: f.ty.type })),
          { label: vocab.sort_asc, insert: vocab.sort_asc + "(", detail: "sort" },
          { label: vocab.sort_desc, insert: vocab.sort_desc + "(", detail: "sort" },
          { label: vocab.not, insert: vocab.not + " ", detail: "not" },
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
            : vocab.ops.filter((o) => opAllowed(o.op, field.ty.type)).map((o) => ({ label: o.name, insert: o.name + ":", detail: "op" }));
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
      caret = pos;
      // Keep suggesting only where the inserted text expects more (a name's '(' or an operator's ':').
      if (/[(:]$/.test(it.insert)) refreshCompletions();
    });
  }

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      open = false;
      onrun(value);
      return;
    }
    if (e.ctrlKey && e.key === " ") {
      e.preventDefault();
      refreshCompletions();
      return;
    }
    if (!open) {
      if (e.key === "Escape") onclose();
      return;
    }
    if (e.key === "ArrowDown") active = (active + 1) % items.length;
    else if (e.key === "ArrowUp") active = (active - 1 + items.length) % items.length;
    else if (e.key === "Enter" || e.key === "Tab") accept(items[active]);
    else if (e.key === "Escape") open = false;
    else return;
    e.preventDefault();
  }

  function syncCaret() {
    caret = ta.selectionStart;
    hl.scrollTop = ta.scrollTop;
    hl.scrollLeft = ta.scrollLeft;
  }

  // ---- saving
  // The parent re-creates this component per edited query, so the initial label is enough.
  let label = $state(untrack(() => editing?.label ?? ""));
  let saveError = $state("");
  async function save() {
    saveError = "";
    try {
      await api.saveQuery(editing?.id ?? null, label, value);
      onsaved();
    } catch (e) {
      saveError = asCmdError(e).message;
    }
  }

  function underline(t: Token) {
    if (!error) return false;
    const end = t.start + t.text.length;
    return error.end === error.start ? t.start === error.start : t.start < error.end && end > error.start;
  }
</script>

<div class="editor">
  <div class="area">
    <pre bind:this={hl} aria-hidden="true">{#each tokens as t, i (i)}<span class="t-{t.kind}" class:err={underline(t)}
          >{t.text}</span
        >{/each}{" "}</pre>
    <textarea
      bind:this={ta}
      bind:value
      spellcheck="false"
      rows="4"
      placeholder={lbl({ ko: "예: 생성 날짜(2026-10-18:이후) 그리고 유형(이미지) 그리고 내림차순 정렬(점수)", en: "e.g. created(2026-10-18:after) AND type(image) AND sort desc(rating)" })}
      oninput={() => {
        syncCaret();
        refreshCompletions();
      }}
      onclick={() => {
        syncCaret();
        refreshCompletions();
      }}
      onkeyup={(e) => {
        if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) {
          syncCaret();
          refreshCompletions();
        }
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

  <div class="status">
    {#if error}
      <span class="bad">{error.message}</span>
    {:else if normalized && normalize(normalized) !== normalize(value)}
      <button class="link" onclick={() => (value = normalized)} title={normalized}
        >{lbl({ ko: "정리된 형태로 바꾸기", en: "Normalize" })}</button
      >
    {/if}
    <span class="hint">Ctrl+Enter {lbl({ ko: "실행", en: "run" })} · Ctrl+Space {lbl({ ko: "자동 완성", en: "complete" })}</span>
  </div>

  <div class="actions">
    <input class="label" placeholder={lbl({ ko: "저장할 이름", en: "Label to save as" })} bind:value={label} />
    <button disabled={!label.trim() || !!error} onclick={save}
      >{editing ? lbl({ ko: "업데이트", en: "Update" }) : lbl({ ko: "저장", en: "Save" })}</button
    >
    <span class="grow"></span>
    <button onclick={onclose}>{lbl({ ko: "닫기", en: "Close" })}</button>
    <button class="primary" disabled={!!error} onclick={() => onrun(value)}>{lbl({ ko: "실행", en: "Run" })}</button>
  </div>
  {#if saveError}<p class="bad">{saveError}</p>{/if}
</div>

<style>
  .editor {
    display: grid;
    gap: 6px;
    padding: 10px 16px 4px;
  }
  .area {
    position: relative;
  }
  pre,
  textarea {
    font: 14px/1.6 ui-monospace, "D2Coding", "Cascadia Mono", monospace;
    padding: 8px 10px;
    margin: 0;
    border: 1px solid var(--border);
    border-radius: 6px;
    white-space: pre-wrap;
    word-break: break-all;
    box-sizing: border-box;
    width: 100%;
    min-height: 96px;
  }
  pre {
    position: absolute;
    inset: 0;
    overflow: hidden;
    color: var(--fg);
    background: var(--panel);
    pointer-events: none;
  }
  textarea {
    position: relative;
    resize: vertical;
    background: transparent;
    color: transparent;
    caret-color: var(--fg);
    outline: none;
  }
  textarea:focus {
    border-color: var(--accent);
  }
  .t-keyword {
    color: #c678dd;
    font-weight: 600;
  }
  .t-field {
    color: #61afef;
  }
  .t-sort {
    color: #d19a66;
    font-weight: 600;
  }
  .t-op {
    color: #e5c07b;
  }
  .t-value,
  .t-string {
    color: #98c379;
  }
  .t-punct {
    color: var(--muted);
  }
  .err {
    text-decoration: wavy underline var(--danger);
    text-underline-offset: 3px;
  }
  .menu {
    position: absolute;
    left: 10px;
    top: 100%;
    margin: 2px 0 0;
    padding: 4px;
    list-style: none;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    max-height: 240px;
    overflow-y: auto;
    min-width: 220px;
    z-index: 5;
    box-shadow: 0 8px 24px #0004;
  }
  .menu button {
    width: 100%;
    border: 0;
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 4px 8px;
    text-align: left;
  }
  .menu button.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .menu small {
    color: var(--muted);
  }
  .status {
    display: flex;
    gap: 12px;
    font-size: 12px;
    min-height: 18px;
  }
  .hint {
    margin-left: auto;
    color: var(--muted);
  }
  .bad {
    color: var(--danger);
    margin: 0;
    font-size: 12px;
  }
  .link {
    border: 0;
    background: transparent;
    color: var(--accent);
    padding: 0;
    font-size: 12px;
  }
  .actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .label {
    width: 200px;
  }
  .grow {
    flex: 1;
  }
</style>
