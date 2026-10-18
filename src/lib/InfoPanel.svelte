<script lang="ts">
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, asCmdError, type ExtValue, type FieldInfo, type FieldType, type MediaDetail } from "./api";
  import { i18n } from "./i18n.svelte";

  let {
    ids,
    onchanged,
    dark = false,
  }: { ids: number[]; onchanged?: () => void; dark?: boolean } = $props();

  type L = { ko: string; en: string };
  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);

  let fields = $state<FieldInfo[]>([]);
  let detail = $state<MediaDetail | null>(null);
  let error = $state("");

  const fieldName = (f: FieldInfo) =>
    f.names.find(([l]) => l === i18n.locale)?.[1] ?? f.names[0]?.[1] ?? f.key;

  async function loadFields() {
    fields = await api.listFields();
  }
  loadFields();

  $effect(() => {
    const list = ids;
    detail = null;
    if (list.length === 1) {
      const id = list[0];
      api.mediaDetail(id).then((d) => {
        if (ids.length === 1 && ids[0] === id) detail = d;
      });
    }
  });

  async function reload() {
    if (ids.length === 1) detail = await api.mediaDetail(ids[0]);
    onchanged?.();
  }

  async function run(f: () => Promise<unknown>) {
    error = "";
    try {
      await f();
      await reload();
    } catch (e) {
      error = asCmdError(e).message;
    }
  }

  const values = (key: string): ExtValue[] => detail?.values[key] ?? [];
  const single = (key: string) => values(key)[0];

  // ---- enum (tags) input with suggestions
  let tagInput = $state<Record<string, string>>({});
  let suggestions = $state<Record<string, string[]>>({});
  async function suggest(key: string) {
    suggestions[key] = await api.suggestValues(key, tagInput[key] ?? "");
  }
  function addTag(key: string) {
    const v = (tagInput[key] ?? "").trim();
    if (!v) return;
    tagInput[key] = "";
    run(() => api.addValue(ids, key, v));
  }

  function setScalar(f: FieldInfo, raw: string) {
    const t = f.ty.type;
    if (raw.trim() === "") return run(() => api.setValues(ids, f.key, []));
    const v = t === "text" ? raw : t === "real" ? parseFloat(raw) : parseInt(raw, 10);
    if (typeof v === "number" && !isFinite(v)) return;
    run(() => api.setValues(ids, f.key, [v]));
  }

  // ---- new field
  let adding = $state(false);
  let newName = $state("");
  let newType = $state<"text" | "integer" | "real" | "enum" | "ranged_int">("text");
  let newMin = $state(0);
  let newMax = $state(5);
  const typeLabels: Record<string, L> = {
    text: { ko: "문자열", en: "Text" },
    integer: { ko: "정수", en: "Integer" },
    real: { ko: "실수", en: "Number" },
    enum: { ko: "열거형 (여러 값)", en: "Labels (multiple)" },
    ranged_int: { ko: "범위 정수 (점수)", en: "Ranged integer (rating)" },
  };
  async function createField() {
    const name = newName.trim();
    if (!name) return;
    const ty: FieldType = newType === "ranged_int" ? { type: "ranged_int", min: newMin, max: newMax } : { type: newType };
    error = "";
    try {
      await api.createField([[i18n.locale, name]], [], ty);
      newName = "";
      adding = false;
      await loadFields();
    } catch (e) {
      error = asCmdError(e).message;
    }
  }
  async function deleteField(f: FieldInfo) {
    const msg = lbl({ ko: `'${fieldName(f)}' 필드와 모든 값을 삭제할까요?`, en: `Delete field '${fieldName(f)}' and all its values?` });
    if (!(await ask(msg, { kind: "warning" }))) return;
    await api.deleteField(f.key);
    await loadFields();
    onchanged?.();
  }

  const fmtBytes = (b: number) =>
    b >= 1024 ** 3 ? `${(b / 1024 ** 3).toFixed(2)} GB` : b >= 1024 ** 2 ? `${(b / 1024 ** 2).toFixed(1)} MB` : `${Math.ceil(b / 1024)} KB`;
  const fmtDate = (ms: number | null) => (ms == null ? "–" : new Date(ms).toLocaleString(i18n.locale));
  const fmtDur = (ms: number | null) => {
    if (ms == null) return "–";
    const s = Math.round(ms / 1000);
    const h = Math.floor(s / 3600);
    const m = Math.floor((s % 3600) / 60);
    return `${h ? h + ":" : ""}${String(m).padStart(h ? 2 : 1, "0")}:${String(s % 60).padStart(2, "0")}`;
  };
</script>

<aside class="info" class:dark>
  {#if ids.length === 0}
    <p class="muted">{lbl({ ko: "항목을 선택하세요", en: "Select an item" })}</p>
  {:else}
    {#if ids.length === 1 && detail}
      <h3 class="title" title={detail.rel_path}>{detail.name}</h3>
      <dl>
        <dt>{lbl({ ko: "소스", en: "Source" })}</dt>
        <dd>{detail.source_name}</dd>
        <dt>{lbl({ ko: "경로", en: "Path" })}</dt>
        <dd class="path">{detail.rel_path}</dd>
        <dt>{lbl({ ko: "크기", en: "Size" })}</dt>
        <dd>{fmtBytes(detail.size)}{detail.width ? ` · ${detail.width}×${detail.height}` : ""}</dd>
        {#if detail.kind === "video"}
          <dt>{lbl({ ko: "길이", en: "Duration" })}</dt>
          <dd>{fmtDur(detail.duration_ms)}</dd>
        {/if}
        <dt>{lbl({ ko: "생성", en: "Created" })}</dt>
        <dd>{fmtDate(detail.created_ms)}</dd>
        <dt>{lbl({ ko: "변경", en: "Modified" })}</dt>
        <dd>{fmtDate(detail.modified_ms)}</dd>
        <dt>{lbl({ ko: "조회", en: "Views" })}</dt>
        <dd>{detail.view_count}{detail.last_viewed_ms ? ` · ${fmtDate(detail.last_viewed_ms)}` : ""}</dd>
      </dl>
    {:else if ids.length > 1}
      <h3 class="title">{lbl({ ko: `${ids.length}개 선택됨`, en: `${ids.length} selected` })}</h3>
      <p class="muted small">
        {lbl({ ko: "변경 사항은 선택한 모든 항목에 적용됩니다.", en: "Changes apply to every selected item." })}
      </p>
    {/if}

    {#each fields as f (f.key)}
      <section class="field">
        <div class="label">
          <span>{fieldName(f)}</span>
          {#if !f.builtin}
            <button class="x" title={lbl({ ko: "필드 삭제", en: "Delete field" })} onclick={() => deleteField(f)}>✕</button>
          {/if}
        </div>
        {#if f.ty.type === "enum"}
          {#if ids.length === 1}
            <div class="chips">
              {#each values(f.key) as v (v)}
                <span class="chip"
                  >{v}<button onclick={() => run(() => api.removeValue(ids, f.key, v))} aria-label="remove">×</button></span
                >
              {/each}
            </div>
          {/if}
          <input
            list="sugg-{f.key}"
            placeholder={ids.length > 1 ? lbl({ ko: "모두에 추가…", en: "Add to all…" }) : lbl({ ko: "추가…", en: "Add…" })}
            bind:value={tagInput[f.key]}
            onfocus={() => suggest(f.key)}
            oninput={() => suggest(f.key)}
            onkeydown={(e) => e.key === "Enter" && addTag(f.key)}
          />
          <datalist id="sugg-{f.key}">
            {#each suggestions[f.key] ?? [] as s (s)}<option value={s}></option>{/each}
          </datalist>
          {#if ids.length > 1 && (tagInput[f.key] ?? "").trim()}
            <button class="small-btn" onclick={() => run(() => api.removeValue(ids, f.key, tagInput[f.key].trim()))}
              >{lbl({ ko: "모두에서 제거", en: "Remove from all" })}</button
            >
          {/if}
        {:else if f.ty.type === "ranged_int"}
          {@const ty = f.ty}
          {@const cur = single(f.key) as number | undefined}
          {#if ty.max - ty.min <= 10}
            <div class="stars" role="radiogroup">
              {#each Array.from({ length: ty.max - ty.min + 1 }, (_, i) => ty.min + i) as n (n)}
                <button
                  class="star"
                  class:on={cur !== undefined && n <= cur && n > ty.min}
                  class:zero={n === ty.min}
                  role="radio"
                  aria-checked={cur === n}
                  title={String(n)}
                  onclick={() => run(() => api.setValues(ids, f.key, cur === n ? [] : [n]))}
                  >{n === ty.min ? "⊘" : "★"}</button
                >
              {/each}
            </div>
          {:else}
            <input
              type="number"
              min={ty.min}
              max={ty.max}
              value={cur ?? ""}
              onchange={(e) => setScalar(f, e.currentTarget.value)}
            />
          {/if}
        {:else if f.ty.type === "text"}
          {#if f.key === "x.memo"}
            <textarea rows="3" value={(single(f.key) as string) ?? ""} onchange={(e) => setScalar(f, e.currentTarget.value)}
            ></textarea>
          {:else}
            <input value={(single(f.key) as string) ?? ""} onchange={(e) => setScalar(f, e.currentTarget.value)} />
          {/if}
        {:else}
          <input
            type="number"
            step={f.ty.type === "real" ? "any" : "1"}
            value={single(f.key) ?? ""}
            onchange={(e) => setScalar(f, e.currentTarget.value)}
          />
        {/if}
      </section>
    {/each}

    {#if adding}
      <section class="new">
        <input placeholder={lbl({ ko: "필드 이름", en: "Field name" })} bind:value={newName} />
        <select bind:value={newType}>
          {#each Object.entries(typeLabels) as [v, l] (v)}<option value={v}>{lbl(l)}</option>{/each}
        </select>
        {#if newType === "ranged_int"}
          <div class="range">
            <label>{lbl({ ko: "최소", en: "Min" })} <input type="number" bind:value={newMin} /></label>
            <label>{lbl({ ko: "최대", en: "Max" })} <input type="number" bind:value={newMax} /></label>
          </div>
        {/if}
        <div class="row">
          <button onclick={() => (adding = false)}>{lbl({ ko: "취소", en: "Cancel" })}</button>
          <button class="primary" onclick={createField}>{lbl({ ko: "추가", en: "Add" })}</button>
        </div>
      </section>
    {:else}
      <button class="add" onclick={() => (adding = true)}>+ {lbl({ ko: "확장 조건 추가", en: "Add field" })}</button>
    {/if}
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</aside>

<style>
  .info {
    width: 280px;
    flex-shrink: 0;
    box-sizing: border-box;
    padding: 12px 14px;
    overflow-y: auto;
    background: var(--panel);
    border-left: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 13px;
  }
  .info.dark {
    --panel: #161616f0;
    --border: #333;
    --fg: #eee;
    --muted: #aaa;
    --bg: #222;
    --tile: #2a2a2a;
    color: var(--fg);
  }
  .title {
    margin: 0;
    font-size: 14px;
    word-break: break-all;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 3px 10px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .path {
    font-size: 12px;
  }
  .field {
    display: grid;
    gap: 4px;
  }
  .label {
    display: flex;
    justify-content: space-between;
    color: var(--muted);
    font-size: 12px;
  }
  .x {
    border: 0;
    background: transparent;
    padding: 0 2px;
    color: var(--muted);
    opacity: 0;
  }
  .field:hover .x {
    opacity: 1;
  }
  input,
  textarea,
  select {
    font: inherit;
    color: inherit;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 5px 8px;
    width: 100%;
    box-sizing: border-box;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    background: var(--tile);
    border-radius: 999px;
    padding: 1px 4px 1px 9px;
    display: inline-flex;
    align-items: center;
    gap: 2px;
  }
  .chip button {
    border: 0;
    background: transparent;
    padding: 0 4px;
    color: var(--muted);
  }
  .stars {
    display: flex;
    gap: 2px;
  }
  .star {
    border: 0;
    background: transparent;
    padding: 2px 3px;
    font-size: 18px;
    color: var(--muted);
    opacity: 0.4;
  }
  .star.on {
    color: #f5b301;
    opacity: 1;
  }
  .star.zero {
    font-size: 14px;
  }
  .new {
    display: grid;
    gap: 6px;
  }
  .range {
    display: flex;
    gap: 6px;
  }
  .row {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .add,
  .small-btn {
    border-style: dashed;
    background: transparent;
  }
  .muted {
    color: var(--muted);
  }
  .small {
    font-size: 12px;
    margin: 0;
  }
  .error {
    color: var(--danger, #f87171);
    margin: 0;
  }
</style>
