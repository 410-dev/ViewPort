<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import { onBack } from "../back.svelte";
  import { untrack } from "svelte";
  import { api, asCmdError, type SavedQuery, type Vocabulary } from "../api";
  import { i18n, t, tx } from "../i18n.svelte";
  import { callAt, defaultOp, Lang, normalize, opAllowed, opArity, POSTFIX_OPS, quoteParam, splitParams } from "../query";
  import Icon from "../ui/Icon.svelte";
  import QueryInput from "./QueryInput.svelte";
  import Combobox from "../ui/Combobox.svelte";
  import Select from "../ui/Select.svelte";

  /**
   * Query command editor: the text at the top is the source of truth; the four columns
   * below build one condition at a time (condition type → parameters → operator →
   * connective) and write it into the text. Putting the caret on an existing condition
   * loads it into the columns for editing. On phones the columns become slides, one step
   * at a time: condition → value → operator → connective, then back to the first.
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

  $effect(() => onBack(() => (mobile && step > 0 ? go(step - 1) : onclose())));

  // ---- phone layout: one column per slide
  const narrow = matchMedia("(max-width: 640px)");
  let mobile = $state(narrow.matches);
  $effect(() => {
    const on = () => (mobile = narrow.matches);
    narrow.addEventListener("change", on);
    return () => narrow.removeEventListener("change", on);
  });
  const STEPS = ["query.stepField", "query.stepValue", "query.stepOp", "query.stepJoin"] as const;
  let step = $state(0);
  let track = $state<HTMLElement>();
  let settle: ReturnType<typeof setTimeout> | undefined;

  function go(i: number) {
    step = Math.max(0, Math.min(STEPS.length - 1, i));
    track?.scrollTo({ left: step * track.clientWidth, behavior: "smooth" });
  }

  /** A swipe moves between slides too; the step follows where it comes to rest. */
  function onTrackScroll() {
    clearTimeout(settle);
    settle = setTimeout(() => {
      if (track && track.clientWidth) step = Math.round(track.scrollLeft / track.clientWidth);
    }, 90);
  }

  /** Sources the `source` condition can name: the ones in use. */
  let sourceNames = $state<string[]>([]);
  $effect(() => {
    api.listSources().then((l) => (sourceNames = l.filter((x) => x.enabled).map((x) => x.name)), () => {});
  });

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

  // An item is in one source: "contains all" of several never matches.
  const ops = $derived(vocab && ty ? vocab.ops.filter((o) => opAllowed(o.op, ty) && !(fieldKey === "source" && o.op === "contains_all")) : []);

  function opLabel(o: string) {
    if (o === SORT_ASC) return t("qop.sort_asc");
    if (o === SORT_DESC) return t("qop.sort_desc");
    return (ty === "date" ? tx(`qop.date.${o}`) : "") || tx(`qop.${o}`, vocab?.ops.find((x) => x.op === o)?.name ?? o);
  }

  const UNITS: Record<string, string[]> = { size: ["KB", "MB", "GB"], duration: ["s", "m", "h"] };

  // ---- live writing
  // Every change in the columns rewrites the condition being built (the draft) in the text
  // right away, complete or not. `target` is the draft's span; `appendAt` is set when the
  // draft was appended to the end, so the joiner (AND/OR) before it follows the connective.
  let appendAt = $state<number | null>(null);

  function pickField(key: string) {
    if (mobile) go(1);
    if (key === fieldKey) return;
    fieldKey = key;
    op = null;
    params = [];
    units = [];
    live();
  }

  function pickOp(o: string) {
    op = op === o ? null : o;
    const [, max] = op && op !== SORT_ASC && op !== SORT_DESC ? opArity(op, MULTI_MAX) : [0, 0];
    params = params.slice(0, max);
    live();
  }

  function setParam(i: number, v: string) {
    params[i] = v;
    live();
  }

  function removeParam(i: number) {
    params.splice(i, 1);
    live();
  }

  function setConnective(c: "and" | "or") {
    connective = c;
    live();
  }

  /** Picks the sources the condition names; extra picks push out the oldest past the operator's limit. */
  function toggleSource(name: string) {
    const kept = params.filter((p) => p.trim() !== "");
    let next = kept.includes(name) ? kept.filter((p) => p !== name) : [...kept, name];
    if (next.length > arity[1]) next = next.slice(next.length - arity[1]);
    params = next;
    live();
  }

  /** Phones: the connective ends this condition and starts the next with it. */
  function joinNext(c: "and" | "or") {
    connective = c;
    nextCondition();
    go(0);
  }

  function finishStep() {
    finish();
    if (mobile) go(0);
  }

  function resetBuilder() {
    fieldKey = null;
    op = null;
    params = [];
    units = [];
    negate = false;
  }

  function paramValue(i: number): string {
    const raw = (params[i] ?? "").trim();
    if (!raw) return "";
    if (ty && UNITS[ty] && /^[\d.]+$/.test(raw)) return raw + (units[i] ?? UNITS[ty][1]);
    return raw;
  }

  /** The condition the columns describe, as query text; may be incomplete (`이름()`). */
  function draftText(): string | null {
    if (!field || !vocab) return null;
    if (isSort) return `${op === SORT_ASC ? vocab.sort_asc : vocab.sort_desc}(${field.name})`;
    const values = params.map((_, i) => paramValue(i)).filter(Boolean).map(quoteParam);
    const o = effectiveOp!;
    const name = vocab.ops.find((x) => x.op === o)?.name ?? o;
    // The default operator is left out unless chosen explicitly.
    const parts = op !== null ? (POSTFIX_OPS.has(o) ? [...values, name] : [name, ...values]) : values;
    const call = `${field.name}(${parts.join(":")})`;
    return negate ? `${vocab.not} ${call}` : call;
  }

  /** Writes the draft into the text. */
  function live() {
    if (!vocab) return;
    const cond = draftText() ?? "";
    if (target && appendAt === null) {
      // Editing a condition already in the text, in place.
      text = text.slice(0, target.start) + cond + text.slice(target.end);
      target = { start: target.start, end: target.start + cond.length };
      return;
    }
    if (!target && !cond) return;
    const head = target ? text.slice(0, appendAt!) : text.replace(/\s+$/, "");
    const tail = target ? text.slice(target.end) : "";
    const joiner = head.trim() ? ` ${connective === "and" ? vocab.and : vocab.or} ` : "";
    appendAt = head.length;
    text = head + joiner + cond + tail;
    target = { start: head.length + joiner.length, end: head.length + joiner.length + cond.length };
  }

  /** Ends the draft: it stays in the text and the columns clear for a new condition. */
  function finish() {
    target = null;
    appendAt = null;
    resetBuilder();
    // Phones would raise the keyboard over the slides.
    if (!mobile) queueMicrotask(() => input.focus());
  }

  /** Ends the draft and starts the next one right away, joined with the chosen connective. */
  function nextCondition() {
    finish();
    if (text.trim()) live2Joiner();
  }

  /** Writes just the joiner for the next condition (the draft that follows replaces it). */
  function live2Joiner() {
    if (!vocab) return;
    const head = text.replace(/\s+$/, "");
    const joiner = ` ${connective === "and" ? vocab.and : vocab.or} `;
    appendAt = head.length;
    text = head + joiner;
    target = { start: text.length, end: text.length };
  }

  /** A joiner with no condition after it is dropped before applying. */
  function dropEmptyDraft() {
    if (target && appendAt !== null && target.start === target.end) {
      text = text.slice(0, appendAt) + text.slice(target.end);
      target = null;
      appendAt = null;
    }
  }

  function apply() {
    dropEmptyDraft();
    onapply(text);
  }

  /** Loads the condition under the caret into the columns. */
  function onCaret(caret: number) {
    if (!lang || !vocab) return;
    const call = callAt(lang, text, caret);
    appendAt = null;
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

<div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div out:pop class="dialog modal" role="dialog" aria-modal="true" aria-label={t("query.title")}>
    <header class="title">
      <h2>{t("query.title")}</h2>
      {#if mobile}<button class="icon-btn close" onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>{/if}
    </header>

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

      {#if mobile}
        <div class="steps" role="tablist">
          {#each STEPS as key, i (key)}
            <button role="tab" aria-selected={step === i} aria-label={t(key)} title={t(key)} class:on={step === i} class:done={i === 0 ? !!field : i === 1 ? filled > 0 : i === 2 ? op !== null : false} onclick={() => go(i)}>
              <span class="n">{i + 1}</span><span class="label">{t(key)}</span>
            </button>
          {/each}
        </div>
      {/if}

      <div class="cols" class:carousel={mobile} bind:this={track} onscroll={mobile ? onTrackScroll : undefined}>
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
            {#if field && !isSort && field.key === "source" && arity[1] > 0}
              <p class="hint">{t("query.pickSources")}</p>
              {#each [...sourceNames, ...params.filter((p) => p.trim() && !sourceNames.includes(p))] as name (name)}
                <button class="item" class:on={params.includes(name)} onclick={() => toggleSource(name)}>
                  <Icon name={params.includes(name) ? "check" : "folder"} size={14} /><span class="label">{name}</span>
                </button>
              {:else}
                <p class="hint">{t("query.noSources")}</p>
              {/each}
            {:else if field && !isSort}
              {#each Array.from({ length: slots }, (_, i) => i) as i (i)}
                <div class="param">
                  {#if ty === "kind"}
                    <div class="seg sm">
                      {#each vocab?.kinds ?? [] as k (k)}
                        <button class:on={params[i] === k} onclick={() => setParam(i, params[i] === k ? "" : k)}>{k}</button>
                      {/each}
                    </div>
                  {:else if ty === "toggle"}
                    <div class="seg sm">
                      {#each [vocab?.yes ?? "yes", vocab?.no ?? "no"] as k (k)}
                        <button class:on={params[i] === k} onclick={() => setParam(i, params[i] === k ? "" : k)}>{k}</button>
                      {/each}
                    </div>
                  {:else if ty === "date"}
                    <input class="input" type="date" value={params[i] ?? ""} oninput={(e) => setParam(i, e.currentTarget.value)} />
                  {:else if ty && UNITS[ty]}
                    <input class="input num" type="number" min="0" step="any" value={params[i] ?? ""} oninput={(e) => setParam(i, e.currentTarget.value)} />
                    <Select
                      class="unit"
                      value={units[i] ?? UNITS[ty][1]}
                      options={UNITS[ty].map((u) => ({ value: u, label: u }))}
                      onchange={(v) => {
                        units[i] = v;
                        live();
                      }}
                    />
                  {:else if ty === "integer" || ty === "real" || ty === "ranged_int"}
                    <input
                      class="input"
                      type="number"
                      step={ty === "real" ? "any" : "1"}
                      value={params[i] ?? ""}
                      oninput={(e) => setParam(i, e.currentTarget.value)}
                    />
                  {:else}
                    {#if field.ext}
                      {@const key = field.key}
                      <Combobox value={params[i] ?? ""} suggest={(prefix, limit) => api.suggestValues(key, prefix, limit)} oninput={(v) => setParam(i, v)} />
                    {:else}
                      <input class="input" value={params[i] ?? ""} oninput={(e) => setParam(i, e.currentTarget.value)} />
                    {/if}
                  {/if}
                  {#if params[i]}
                    <button class="icon-btn sm" aria-label={t("common.remove")} onclick={() => removeParam(i)}><Icon name="x" size={12} /></button>
                  {/if}
                </div>
              {/each}
              {#if slots === 0}<p class="hint">{t("query.noParams")}</p>{/if}
            {:else if isSort}
              <p class="hint">{t("query.sortParamHint")}</p>
            {:else}
              <p class="hint">{mobile ? t("query.pickFirst") : t("query.pickKind")}</p>
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
              <p class="hint">{mobile ? t("query.pickFirst") : t("query.pickKind")}</p>
            {/if}
          </div>
        </section>

        <section class="col">
          <h3>{t("query.colExtra")}</h3>
          <div class="items">
            {#if mobile}
              <p class="hint">{t("query.joinHint")}</p>
              {#each [["and", vocab?.and ?? "AND"], ["or", vocab?.or ?? "OR"]] as [c, word] (c)}
                <button class="item join" disabled={!!target && !complete} onclick={() => joinNext(c as "and" | "or")}>
                  <Icon name="plus" size={14} />{t("query.joinNext", { word })}
                </button>
              {/each}
            {:else}
              <button class="item" class:on={connective === "and"} onclick={() => setConnective("and")}>{vocab?.and ?? "AND"}</button>
              <button class="item" class:on={connective === "or"} onclick={() => setConnective("or")}>{vocab?.or ?? "OR"}</button>
            {/if}
            <div class="divider"></div>
            <button class="item" class:on={negate} disabled={isSort} onclick={() => {
                negate = !negate;
                live();
              }}>{t("query.negate", { not: vocab?.not ?? "NOT" })}</button>
            <p class="hint">{t("query.precedence", { and: vocab?.and ?? "AND", or: vocab?.or ?? "OR" })}</p>
            {#if target && appendAt === null}
              <p class="hint edit"><Icon name="pencil" size={12} />{t("query.editingHint")}</p>
            {/if}
          </div>
        </section>
      </div>
    </div>

    {#if mobile}
      <footer class="phone">
        <button class="btn" disabled={step === 0} onclick={() => go(step - 1)}><Icon name="chevronLeft" size={14} />{t("query.prevStep")}</button>
        {#if step < STEPS.length - 1}
          <button class="btn" disabled={step === 0 && !field} onclick={() => go(step + 1)}>{t("query.nextStep")}<Icon name="chevronRight" size={14} /></button>
        {:else}
          <button class="btn" disabled={!target || !complete} onclick={finishStep}>{t("query.finish")}</button>
        {/if}
        <button class="btn primary" disabled={!status.ok} onclick={apply}>{t("query.apply")}</button>
      </footer>
    {:else}
      <footer>
        <button class="btn wide-1" disabled={!target || !complete} onclick={finish}>{t("query.finish")}</button>
        <button class="btn wide-2" disabled={!!target && !complete} onclick={nextCondition}>{t("query.next")}</button>
        <button class="btn primary wide-3" disabled={!status.ok} onclick={apply}>{t("query.apply")}</button>
        <button class="btn wide-4" onclick={onclose}>{t("common.close")}</button>
      </footer>
    {/if}
  </div>
</div>


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
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.1fr) minmax(0, 1.1fr) minmax(0, 1fr);
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
    min-width: 0;
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
  .param :global(.unit) {
    flex: none;
    width: 84px;
    height: 40px;
    background: var(--surface);
    border-color: var(--border);
  }
  .param :global(input.input) {
    flex: 1;
    min-width: 0;
    height: 40px;
    padding: 0 16px;
    background: var(--surface);
    border-color: var(--border);
  }
  .param .seg {
    flex: 1;
  }
  .param .seg button {
    flex: 1;
    min-width: 0;
    overflow: hidden;
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
    grid-template-columns: minmax(0, 1fr) minmax(0, 1.1fr) minmax(0, 0.62fr) minmax(0, 0.48fr);
    gap: 12px !important;
    padding: 0 20px 18px !important;
    border-top: 0 !important;
  }
  /* Labels wrap instead of pushing past narrow columns. */
  footer .btn {
    min-width: 0;
    min-height: 40px;
    height: auto;
    white-space: normal;
    line-height: 1.2;
  }
  @media (max-width: 860px) {
    .labels {
      display: none;
    }
    .cols {
      grid-template-columns: repeat(2, minmax(0, 1fr));
      grid-auto-rows: minmax(200px, 1fr);
      overflow-y: auto;
    }
    footer {
      grid-template-columns: repeat(2, minmax(0, 1fr)) !important;
    }
  }
  .item .label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title {
    position: relative;
  }
  .title .close {
    position: absolute;
    right: 10px;
  }
  /* Phones: the four columns are slides of one track, a step bar above them. */
  .steps {
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }
  /* The current step is named; the others show their number. */
  .steps button:not(.on) .label {
    display: none;
  }
  .steps button:not(.on) {
    flex: 0 0 40px;
  }
  .steps button.on {
    flex: 1;
  }
  .steps button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 5px;
    min-width: 0;
    padding: 0 6px;
    height: 34px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--faint);
    font-size: 12px;
    white-space: nowrap;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .steps .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .steps .n {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    border: 1px solid currentColor;
    font-size: 10px;
    font-weight: 600;
  }
  .steps button.done {
    color: var(--fg-2);
  }
  .steps button.done .n {
    background: var(--accent-soft);
    border-color: var(--accent-line);
  }
  .steps button.on {
    background: var(--accent-soft);
    color: var(--fg);
    font-weight: 600;
  }
  .steps button.on .n {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-fg);
  }
  .cols.carousel {
    display: flex;
    gap: 0;
    overflow-x: auto;
    overflow-y: hidden;
    scroll-snap-type: x mandatory;
    overscroll-behavior-x: contain;
    scrollbar-width: none;
    margin: 0 -20px;
    padding: 0 0 12px;
  }
  .cols.carousel::-webkit-scrollbar {
    display: none;
  }
  .cols.carousel > .col {
    flex: 0 0 100%;
    scroll-snap-align: start;
    scroll-snap-stop: always;
    background: transparent;
    border-radius: 0;
    padding: 0 20px;
  }
  .cols.carousel .col h3 {
    display: none;
  }
  .cols.carousel .items {
    padding: 4px 0;
  }
  .cols.carousel .item {
    min-height: 46px;
  }
  .item.join {
    justify-content: center;
    font-weight: 600;
  }
  footer.phone {
    grid-template-columns: minmax(0, 0.8fr) minmax(0, 1fr) minmax(0, 1fr) !important;
    padding-top: 10px !important;
    border-top: 1px solid var(--border) !important;
  }
  footer.phone .btn {
    gap: 4px;
  }
  /* Phones: a shorter editor, and the status message gets its own line above saving. */
  @media (max-width: 600px) {
    .editor {
      height: 112px;
    }
    .state {
      flex-wrap: wrap;
    }
    .state > span:first-child {
      flex-basis: 100%;
    }
    .state .grow {
      display: none;
    }
    .save {
      flex: 1;
    }
    .save .input {
      flex: 1;
      min-width: 0;
    }
  }
</style>
