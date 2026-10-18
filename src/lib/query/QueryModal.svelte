<script lang="ts">
  import { untrack } from "svelte";
  import { api, asCmdError, type SavedQuery, type Vocabulary } from "../api";
  import { i18n, t, tx } from "../i18n.svelte";
  import { callAt, defaultOp, Lang, normalize, opAllowed, opArity, POSTFIX_OPS, quoteParam, splitParams } from "../query";
  import Icon from "../ui/Icon.svelte";
  import QueryInput from "./QueryInput.svelte";

  /**
   * Query command editor: the text at the top is the source of truth; the four columns
   * below build one condition at a time (condition type → parameters → operator →
   * connective) and write it into the text. Putting the caret on an existing condition
   * loads it into the columns for editing.
   */
  let {
    initial,
    editing = null,
    onapply,
    onclose,
    onsaved,
  }: {
    initial: string;
    editing?: SavedQuery | null;
    onapply: (text: string) => void;
    onclose: () => void;
    onsaved: () => void;
  } = $props();

  /** Values a multi-value operator accepts in the builder. */
  const MULTI_MAX = 4;
  const SORT_ASC = "sort_asc";
  const SORT_DESC = "sort_desc";

  let text = $state(untrack(() => initial));
  let vocab = $state<Vocabulary | null>(null);
  const lang = $derived(vocab ? new Lang(vocab) : null);
  $effect(() => {
    api.queryVocabulary(i18n.queryLocale).then((v) => (vocab = v));
  });

  // ---- validation
  let status = $state<{ ok: boolean; message: string; span: { start: number; end: number } | null }>({
    ok: true,
    message: "",
    span: null,
  });
  $effect(() => {
    const src = text;
    const timer = setTimeout(async () => {
      if (!src.trim()) {
        status = { ok: true, message: "", span: null };
        return;
      }
      const r = await api.checkQuery(src, i18n.queryLocale).catch(() => null);
      if (!r) return;
      if (r.status === "ok") status = { ok: true, message: t("query.valid"), span: null };
      else {
        const kind = r.error.kind as Record<string, unknown> & { code: string };
        const vars = Object.fromEntries(Object.entries(kind).map(([k, v]) => [k, String(v)]));
        status = { ok: false, message: tx(`qerr.${kind.code}`, kind.code, vars), span: r.span };
      }
    }, 180);
    return () => clearTimeout(timer);
  });

  // ---- builder state
  let fieldKey = $state<string | null>(null);
  let op = $state<string | null>(null);
  let params = $state<string[]>([]);
  let units = $state<string[]>([]);
  let connective = $state<"and" | "or">("and");
  let negate = $state(false);
  /** Span of the condition being edited in the text, if any. */
  let target = $state<{ start: number; end: number } | null>(null);
  let input: QueryInput;

  const field = $derived(vocab?.fields.find((f) => f.key === fieldKey) ?? null);
  const ty = $derived(field?.ty.type ?? null);
  const effectiveOp = $derived(op ?? (ty ? defaultOp(ty) : null));
  const isSort = $derived(op === SORT_ASC || op === SORT_DESC);
  const arity = $derived<[number, number]>(effectiveOp && !isSort ? opArity(effectiveOp, MULTI_MAX) : [0, 0]);
  const filled = $derived(params.filter((p) => p.trim() !== "").length);
  const required = $derived(Math.max(0, arity[0] - filled));
  const optional = $derived(Math.max(0, arity[1] - Math.max(filled, arity[0])));
  /** Input boxes to show: the filled ones plus one empty slot while more fit (at least the required count). */
  const slots = $derived(Math.min(arity[1], Math.max(arity[0], filled + (filled < arity[1] ? 1 : 0))));
  const complete = $derived(!!field && required === 0);

  const ops = $derived(vocab && ty ? vocab.ops.filter((o) => opAllowed(o.op, ty)) : []);

  function opLabel(o: string) {
    if (o === SORT_ASC) return t("qop.sort_asc");
    if (o === SORT_DESC) return t("qop.sort_desc");
    return (ty === "date" ? tx(`qop.date.${o}`) : "") || tx(`qop.${o}`, vocab?.ops.find((x) => x.op === o)?.name ?? o);
  }

  const UNITS: Record<string, string[]> = { size: ["KB", "MB", "GB"], duration: ["s", "m", "h"] };

  function pickField(key: string) {
    if (key === fieldKey) return;
    // A different condition type starts a new condition rather than rewriting the one in the text.
    target = null;
    negate = false;
    fieldKey = key;
    op = null;
    params = [];
    units = [];
  }

  function pickOp(o: string) {
    op = op === o ? null : o;
    const [, max] = op && op !== SORT_ASC && op !== SORT_DESC ? opArity(op, MULTI_MAX) : [0, 0];
    params = params.slice(0, max);
  }

  function reset() {
    fieldKey = null;
    op = null;
    params = [];
    units = [];
    negate = false;
    target = null;
  }

  function paramValue(i: number): string {
    const raw = (params[i] ?? "").trim();
    if (!raw) return "";
    if (ty && UNITS[ty] && /^[\d.]+$/.test(raw)) return raw + (units[i] ?? UNITS[ty][1]);
    return raw;
  }

  /** The condition the columns describe, as query text. */
  function build(): string | null {
    if (!field || !vocab || !complete) return null;
    if (isSort) return `${op === SORT_ASC ? vocab.sort_asc : vocab.sort_desc}(${field.name})`;
    const values = Array.from({ length: slots }, (_, i) => paramValue(i))
      .filter(Boolean)
      .map(quoteParam);
    const o = effectiveOp!;
    const name = vocab.ops.find((x) => x.op === o)?.name ?? o;
    // The default operator may be left out when values are given.
    const explicit = op !== null || values.length === 0;
    const parts = explicit ? (POSTFIX_OPS.has(o) ? [...values, name] : [name, ...values]) : values;
    const call = `${field.name}(${parts.join(":")})`;
    return negate ? `${vocab.not} ${call}` : call;
  }

  /** Writes the built condition into the text. Returns false if it is incomplete. */
  function commit(): boolean {
    const cond = build();
    if (!cond || !vocab) return false;
    if (target) {
      input.replace(target.start, target.end, cond);
      target = { start: target.start, end: target.start + cond.length };
    } else {
      const trimmed = text.replace(/\s+$/, "");
      const joiner = trimmed ? ` ${connective === "and" ? vocab.and : vocab.or} ` : "";
      const start = trimmed.length + joiner.length;
      text = trimmed + joiner + cond;
      target = { start, end: start + cond.length };
      queueMicrotask(() => input.focus());
    }
    return true;
  }

  function nextCondition() {
    commit();
    reset();
  }

  function apply() {
    if (complete && !target) commit();
    onapply(text);
  }

  /** Loads the condition under the caret into the columns. */
  function onCaret(caret: number) {
    if (!lang || !vocab) return;
    const call = callAt(lang, text, caret);
    if (!call) {
      target = null;
      return;
    }
    const before = text.slice(0, call.start).replace(/\s+$/, "");
    const notWord = [vocab.not, "not", "아님", "!"].find((w) => normalize(before).endsWith(normalize(w)));
    const start = notWord ? before.length - notWord.length : call.start;
    if (call.sort) {
      const f = lang.field(call.inner.trim());
      if (!f) return;
      fieldKey = f.key;
      op = lang.sortNames.has(normalize(call.name)) && normalize(call.name) === normalize(vocab.sort_desc) ? SORT_DESC : SORT_ASC;
      params = [];
    } else {
      const f = lang.field(call.name);
      if (!f) return;
      fieldKey = f.key;
      const ps = splitParams(call.inner);
      const opName = ps.find((p) => lang.opNames.has(normalize(p)));
      op = opName ? (vocab.ops.find((o) => normalize(o.name) === normalize(opName))?.op ?? null) : null;
      params = ps.filter((p) => p !== opName);
      units = [];
    }
    negate = !!notWord;
    target = { start, end: call.end };
  }

  // ---- saving
  let label = $state(untrack(() => editing?.label ?? ""));
  let saveError = $state("");
  let savedFlash = $state(false);
  async function save() {
    saveError = "";
    try {
      await api.saveQuery(editing?.id ?? null, label.trim(), text);
      savedFlash = true;
      setTimeout(() => (savedFlash = false), 1500);
      onsaved();
    } catch (e) {
      saveError = asCmdError(e).message;
    }
  }

  const typeLabel = $derived(isSort ? t("query.sortHint") : ty ? tx(`ftype.${ty}`, ty) : t("query.param"));

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog modal" role="dialog" aria-modal="true" aria-label={t("query.title")}>
    <header class="title"><h2>{t("query.title")}</h2></header>

    <div class="content">
      <div class="editor">
        <QueryInput
          bind:this={input}
          bind:value={text}
          {lang}
          {vocab}
          error={status.span}
          rows={5}
          placeholder={t("query.placeholder")}
          oncaret={onCaret}
          onsubmit={apply}
        />
      </div>

      <div class="state">
        {#if status.message}
          <span class:ok={status.ok} class:bad={!status.ok}>
            <Icon name={status.ok ? "check" : "alert"} size={14} />{status.message}
          </span>
        {/if}
        <span class="grow"></span>
        <span class="save">
          <input class="input sm" placeholder={t("query.saveAs")} bind:value={label} onkeydown={(e) => e.key === "Enter" && label.trim() && save()} />
          <button class="btn sm" disabled={!label.trim() || !status.ok} onclick={save}>
            <Icon name={savedFlash ? "check" : "bookmark"} size={13} />{editing ? t("common.update") : t("common.save")}
          </button>
        </span>
      </div>
      {#if saveError}<p class="bad small">{saveError}</p>{/if}

      <div class="labels">
        <span>{t("query.colList")}</span>
        <span>
          {t("query.colParams")}
          {#if field && !isSort}<em>{t("query.remaining", { req: required, opt: optional })}</em>{/if}
        </span>
        <span></span>
        <span>{t("query.colExtraLabel")}</span>
      </div>

      <div class="cols">
        <section class="col">
          <h3>{t("query.colKind")}</h3>
          <div class="items">
            {#each vocab?.fields ?? [] as f (f.key)}
              <button class="item" class:on={fieldKey === f.key} onclick={() => pickField(f.key)}>{f.name}</button>
            {/each}
          </div>
        </section>

        <section class="col">
          <h3>{typeLabel}</h3>
          <div class="items">
            {#if field && !isSort}
              {#each Array.from({ length: slots }, (_, i) => i) as i (i)}
                <div class="param">
                  {#if ty === "kind"}
                    <div class="seg sm">
                      {#each vocab?.kinds ?? [] as k (k)}
                        <button class:on={params[i] === k} onclick={() => (params[i] = params[i] === k ? "" : k)}>{k}</button>
                      {/each}
                    </div>
                  {:else if ty === "date"}
                    <input class="input" type="date" bind:value={params[i]} />
                  {:else if ty && UNITS[ty]}
                    <input class="input num" type="number" min="0" step="any" bind:value={params[i]} />
                    <select class="input unit" bind:value={units[i]}>
                      {#each UNITS[ty] as u (u)}<option value={u} selected={u === UNITS[ty][1]}>{u}</option>{/each}
                    </select>
                  {:else if ty === "integer" || ty === "real" || ty === "ranged_int"}
                    <input class="input" type="number" step={ty === "real" ? "any" : "1"} bind:value={params[i]} />
                  {:else}
                    <input class="input" bind:value={params[i]} list={field.ext ? `qm-sugg-${field.key}` : undefined} />
                  {/if}
                  {#if params[i]}
                    <button class="icon-btn sm" aria-label={t("common.remove")} onclick={() => params.splice(i, 1)}><Icon name="x" size={12} /></button>
                  {/if}
                </div>
              {/each}
              {#if slots === 0}<p class="hint">{t("query.noParams")}</p>{/if}
            {:else if isSort}
              <p class="hint">{t("query.sortParamHint")}</p>
            {:else}
              <p class="hint">{t("query.pickKind")}</p>
            {/if}
          </div>
        </section>

        <section class="col">
          <h3>{t("query.colOp")}</h3>
          <div class="items">
            {#if field}
              {#each ops as o (o.op)}
                <button class="item" class:on={effectiveOp === o.op && !isSort} class:implied={op === null && effectiveOp === o.op} onclick={() => pickOp(o.op)}
                  >{opLabel(o.op)}</button
                >
              {/each}
              <div class="divider"></div>
              <button class="item" class:on={op === SORT_ASC} onclick={() => pickOp(SORT_ASC)}><Icon name="sortAsc" size={14} />{opLabel(SORT_ASC)}</button>
              <button class="item" class:on={op === SORT_DESC} onclick={() => pickOp(SORT_DESC)}><Icon name="sortDesc" size={14} />{opLabel(SORT_DESC)}</button>
            {:else}
              <p class="hint">{t("query.pickKind")}</p>
            {/if}
          </div>
        </section>

        <section class="col">
          <h3>{t("query.colExtra")}</h3>
          <div class="items">
            <button class="item" class:on={connective === "and"} onclick={() => (connective = "and")}>{vocab?.and ?? "AND"}</button>
            <button class="item" class:on={connective === "or"} onclick={() => (connective = "or")}>{vocab?.or ?? "OR"}</button>
            <div class="divider"></div>
            <button class="item" class:on={negate} disabled={isSort} onclick={() => (negate = !negate)}>{t("query.negate", { not: vocab?.not ?? "NOT" })}</button>
            <p class="hint">{t("query.precedence", { and: vocab?.and ?? "AND", or: vocab?.or ?? "OR" })}</p>
            {#if target}
              <p class="hint edit"><Icon name="pencil" size={12} />{t("query.editingHint")}</p>
            {/if}
          </div>
        </section>
      </div>
    </div>

    <footer>
      <button class="btn wide-1" disabled={!complete} onclick={commit}>{t("query.finish")}</button>
      <button class="btn wide-2" disabled={!complete && !fieldKey} onclick={nextCondition}>{t("query.next")}</button>
      <button class="btn primary wide-3" disabled={!status.ok} onclick={apply}>{t("query.apply")}</button>
      <button class="btn wide-4" onclick={onclose}>{t("common.close")}</button>
    </footer>
  </div>
</div>

{#if field?.ext}
  {#await api.suggestValues(field.key, "") then list}
    <datalist id="qm-sugg-{field.key}">{#each list as s (s)}<option value={s}></option>{/each}</datalist>
  {/await}
{/if}

<style>
  .modal {
    width: min(1180px, 100%);
    height: min(860px, 100%);
  }
  .title {
    justify-content: center;
  }
  .title h2 {
    flex: none !important;
    text-align: center;
  }
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 18px 20px 0;
  }
  .editor {
    height: 168px;
    flex-shrink: 0;
  }
  .editor :global(.area) {
    height: 100%;
  }
  .editor :global(textarea) {
    height: 100%;
  }
  .state {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 30px;
    font-size: 12px;
  }
  .state > span:first-child {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding-left: 6px;
  }
  .ok {
    color: var(--ok);
  }
  .bad {
    color: var(--danger);
  }
  .small {
    margin: 0;
    font-size: 12px;
  }
  .save {
    display: flex;
    gap: 6px;
  }
  .input.sm {
    height: var(--h-control-sm);
    width: 200px;
    font-size: 12px;
  }
  .labels,
  .cols {
    display: grid;
    grid-template-columns: 1fr 1.1fr 1.1fr 1fr;
    gap: 12px;
  }
  .labels {
    color: var(--muted);
    font-size: 12px;
    padding: 0 4px;
  }
  .labels > span:nth-child(2) {
    grid-column: 2 / span 2;
  }
  .labels > span:nth-child(3) {
    display: none;
  }
  .labels em {
    font-style: normal;
    color: var(--faint);
    margin-left: 4px;
  }
  .cols {
    flex: 1;
    min-height: 0;
    padding-bottom: 14px;
  }
  .col {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    overflow: hidden;
  }
  .col h3 {
    margin: 0;
    height: 46px;
    display: grid;
    place-items: center;
    font-size: 13px;
    font-weight: 600;
    background: color-mix(in srgb, var(--surface-3) 55%, transparent);
    flex-shrink: 0;
  }
  .items {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 40px;
    padding: 0 18px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border);
    background: var(--surface);
    color: var(--fg-2);
    text-align: left;
    flex-shrink: 0;
    transition:
      background var(--t-fast) var(--ease),
      border-color var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .item:hover:not(:disabled) {
    background: var(--hover);
    color: var(--fg);
  }
  .item.on {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--syn-field);
    font-weight: 600;
  }
  .item.implied:not(:hover) {
    border-style: dashed;
  }
  .item:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .param {
    display: flex;
    gap: 6px;
    align-items: center;
    flex-shrink: 0;
  }
  .param .input {
    flex: 1;
    height: 40px;
    padding: 0 16px;
    background: var(--surface);
    border-color: var(--border);
  }
  .param .input:focus {
    border-color: var(--accent-line);
  }
  .param .seg {
    background: var(--surface);
  }
  .param .num {
    flex: 1;
  }
  .param .unit {
    flex: none;
    width: 76px;
  }
  .param .seg {
    flex: 1;
  }
  .param .seg button {
    flex: 1;
  }
  .divider {
    height: 1px;
    margin: 4px 8px;
    background: var(--border);
    flex-shrink: 0;
  }
  .hint {
    margin: 4px 6px;
    font-size: 12px;
    line-height: 1.55;
    color: var(--faint);
  }
  .hint.edit {
    display: flex;
    gap: 6px;
    align-items: center;
    color: var(--accent);
  }
  footer {
    display: grid !important;
    grid-template-columns: 1fr 1.1fr 0.62fr 0.48fr;
    gap: 12px !important;
    padding: 0 20px 18px !important;
    border-top: 0 !important;
  }
  footer .btn {
    height: 40px;
  }
  @media (max-width: 860px) {
    .labels {
      display: none;
    }
    .cols {
      grid-template-columns: 1fr 1fr;
      grid-auto-rows: minmax(200px, 1fr);
      overflow-y: auto;
    }
    footer {
      grid-template-columns: 1fr 1fr !important;
    }
  }
</style>
