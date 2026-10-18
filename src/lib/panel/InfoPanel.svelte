<script lang="ts">
  import { ask } from "@tauri-apps/plugin-dialog";
  import { api, asCmdError, type ExtValue, type FieldInfo, type FieldType, type MediaDetail } from "../api";
  import { fmtBytes, fmtDate, fmtDuration } from "../format";
  import { i18n, t, type Key } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";

  /** Metadata and editable extension fields (tags, rating, memo, custom) of the selection. */
  /** `compact` hides the file name (the preview above already shows it). */
  let { ids, onchanged, compact = false }: { ids: number[]; onchanged?: () => void; compact?: boolean } = $props();

  let fields = $state<FieldInfo[]>([]);
  let detail = $state<MediaDetail | null>(null);
  let error = $state("");

  const fieldName = (f: FieldInfo) => f.names.find(([l]) => l === i18n.locale)?.[1] ?? f.names[0]?.[1] ?? f.key;

  async function loadFields() {
    fields = await api.listFields().catch(() => []);
  }
  loadFields();

  $effect(() => {
    const list = ids;
    detail = null;
    if (list.length !== 1) return;
    const id = list[0];
    // Wait for the selection to settle before querying (fast arrow-key browsing).
    const timer = setTimeout(() => {
      api.mediaDetail(id).then((d) => {
        if (ids.length === 1 && ids[0] === id) detail = d;
      });
    }, 60);
    return () => clearTimeout(timer);
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

  let tagInput = $state<Record<string, string>>({});
  let suggestions = $state<Record<string, string[]>>({});
  async function suggest(key: string) {
    suggestions[key] = await api.suggestValues(key, tagInput[key] ?? "").catch(() => []);
  }
  function addTag(key: string) {
    const v = (tagInput[key] ?? "").trim();
    if (!v) return;
    tagInput[key] = "";
    run(() => api.addValue(ids, key, v));
  }

  function setScalar(f: FieldInfo, raw: string) {
    const ty = f.ty.type;
    if (raw.trim() === "") return run(() => api.setValues(ids, f.key, []));
    const v = ty === "text" ? raw : ty === "real" ? parseFloat(raw) : parseInt(raw, 10);
    if (typeof v === "number" && !isFinite(v)) return;
    run(() => api.setValues(ids, f.key, [v]));
  }

  let adding = $state(false);
  let newName = $state("");
  let newType = $state<"text" | "integer" | "real" | "enum" | "ranged_int">("text");
  let newMin = $state(0);
  let newMax = $state(5);
  const typeLabels: Record<string, Key> = {
    text: "ftype.text",
    integer: "ftype.integer",
    real: "ftype.real",
    enum: "ftype.enum",
    ranged_int: "ftype.ranged_int",
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
    if (!(await ask(t("info.deleteField", { name: fieldName(f) }), { kind: "warning" }))) return;
    await api.deleteField(f.key);
    await loadFields();
    onchanged?.();
  }
</script>

<div class="info">
  {#if ids.length === 0}
    <p class="muted center">{t("panel.nothing")}</p>
  {:else}
    {#if ids.length === 1 && detail}
      <section class="facts">
        {#if !compact}<h3 class="title" title={detail.rel_path}>{detail.name}</h3>{/if}
        <dl>
          <dt>{t("info.source")}</dt>
          <dd>{detail.source_name}</dd>
          <dt>{t("info.path")}</dt>
          <dd class="path">{detail.rel_path}</dd>
          <dt>{t("info.size")}</dt>
          <dd class="tabular">{fmtBytes(detail.size)}{detail.width ? ` · ${detail.width} × ${detail.height}` : ""}</dd>
          {#if detail.kind === "video"}
            <dt>{t("info.duration")}</dt>
            <dd class="tabular">{fmtDuration(detail.duration_ms) || "–"}</dd>
          {/if}
          <dt>{t("info.created")}</dt>
          <dd class="tabular">{fmtDate(detail.created_ms)}</dd>
          <dt>{t("info.modified")}</dt>
          <dd class="tabular">{fmtDate(detail.modified_ms)}</dd>
          <dt>{t("info.views")}</dt>
          <dd class="tabular">{detail.view_count}{detail.last_viewed_ms ? ` · ${fmtDate(detail.last_viewed_ms)}` : ""}</dd>
        </dl>
      </section>
    {:else if ids.length > 1}
      <section class="facts">
        <h3 class="title">{t("panel.selected", { n: ids.length.toLocaleString() })}</h3>
        <p class="muted small">{t("info.multiHint")}</p>
      </section>
    {/if}

    <section class="fields">
      {#each fields as f (f.key)}
        <div class="field">
          <div class="label">
            <span>{fieldName(f)}</span>
            {#if !f.builtin}
              <button class="icon-btn sm x" title={t("info.deleteFieldTitle")} onclick={() => deleteField(f)}><Icon name="x" size={12} /></button>
            {/if}
          </div>
          {#if f.ty.type === "enum"}
            {#if ids.length === 1 && values(f.key).length}
              <div class="chips">
                {#each values(f.key) as v (v)}
                  <span class="chip"
                    >{v}<button onclick={() => run(() => api.removeValue(ids, f.key, v))} aria-label="remove"
                      ><Icon name="x" size={11} /></button
                    ></span
                  >
                {/each}
              </div>
            {/if}
            <input
              class="input"
              list="sugg-{f.key}"
              placeholder={ids.length > 1 ? t("info.addToAll") : t("info.add")}
              bind:value={tagInput[f.key]}
              onfocus={() => suggest(f.key)}
              oninput={() => suggest(f.key)}
              onkeydown={(e) => e.key === "Enter" && addTag(f.key)}
            />
            <datalist id="sugg-{f.key}">
              {#each suggestions[f.key] ?? [] as s (s)}<option value={s}></option>{/each}
            </datalist>
            {#if ids.length > 1 && (tagInput[f.key] ?? "").trim()}
              <button class="btn sm ghost" onclick={() => run(() => api.removeValue(ids, f.key, tagInput[f.key].trim()))}
                >{t("info.removeFromAll")}</button
              >
            {/if}
          {:else if f.ty.type === "ranged_int"}
            {@const ty = f.ty}
            {@const cur = single(f.key) as number | undefined}
            {#if ty.max - ty.min <= 10}
              <div class="stars" role="radiogroup">
                {#each Array.from({ length: ty.max - ty.min }, (_, i) => ty.min + 1 + i) as n (n)}
                  <button
                    class="star"
                    class:on={cur !== undefined && n <= cur}
                    role="radio"
                    aria-checked={cur === n}
                    title={String(n)}
                    onclick={() => run(() => api.setValues(ids, f.key, cur === n ? [] : [n]))}><Icon name="star" size={17} /></button
                  >
                {/each}
              </div>
            {:else}
              <input class="input" type="number" min={ty.min} max={ty.max} value={cur ?? ""} onchange={(e) => setScalar(f, e.currentTarget.value)} />
            {/if}
          {:else if f.ty.type === "text"}
            {#if f.key === "x.memo"}
              <textarea class="input" rows="3" value={(single(f.key) as string) ?? ""} onchange={(e) => setScalar(f, e.currentTarget.value)}
              ></textarea>
            {:else}
              <input class="input" value={(single(f.key) as string) ?? ""} onchange={(e) => setScalar(f, e.currentTarget.value)} />
            {/if}
          {:else}
            <input
              class="input"
              type="number"
              step={f.ty.type === "real" ? "any" : "1"}
              value={single(f.key) ?? ""}
              onchange={(e) => setScalar(f, e.currentTarget.value)}
            />
          {/if}
        </div>
      {/each}

      {#if adding}
        <div class="new">
          <input class="input" placeholder={t("info.fieldName")} bind:value={newName} />
          <select class="input" bind:value={newType}>
            {#each Object.entries(typeLabels) as [v, l] (v)}<option value={v}>{t(l)}</option>{/each}
          </select>
          {#if newType === "ranged_int"}
            <div class="range">
              <label>{t("info.min")} <input class="input" type="number" bind:value={newMin} /></label>
              <label>{t("info.max")} <input class="input" type="number" bind:value={newMax} /></label>
            </div>
          {/if}
          <div class="row">
            <button class="btn sm ghost" onclick={() => (adding = false)}>{t("common.cancel")}</button>
            <button class="btn sm primary" onclick={createField}>{t("common.add")}</button>
          </div>
        </div>
      {:else}
        <button class="btn sm ghost add" onclick={() => (adding = true)}><Icon name="plus" size={13} />{t("info.addField")}</button>
      {/if}
    </section>
  {/if}
  {#if error}<p class="error">{error}</p>{/if}
</div>

<style>
  .info {
    height: 100%;
    overflow-y: auto;
    padding: 14px 14px 20px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .title {
    margin: 0 0 8px;
    font-size: 13px;
    font-weight: 600;
    word-break: break-all;
    user-select: text;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 5px 14px;
    margin: 0;
    font-size: 12px;
  }
  dt {
    color: var(--faint);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
    color: var(--fg-2);
    user-select: text;
  }
  .fields {
    display: grid;
    gap: 14px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }
  .field {
    display: grid;
    gap: 6px;
  }
  .label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 18px;
    color: var(--muted);
    font-size: 12px;
    font-weight: 500;
  }
  .x {
    opacity: 0;
  }
  .field:hover .x {
    opacity: 1;
  }
  .input {
    width: 100%;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    padding-right: 4px;
  }
  .chip button {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--faint);
  }
  .chip button:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .stars {
    display: flex;
    gap: 2px;
  }
  .star {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--faint);
  }
  .star:hover {
    background: var(--hover);
  }
  .star.on {
    color: #f5b301;
  }
  .star.on :global(path) {
    fill: currentColor;
  }
  .new {
    display: grid;
    gap: 6px;
  }
  .range {
    display: flex;
    gap: 6px;
  }
  .range label {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  .row {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .add {
    justify-self: start;
  }
  .muted {
    color: var(--muted);
  }
  .center {
    text-align: center;
    margin-top: 40px;
  }
  .small {
    font-size: 12px;
    margin: 0;
  }
  .error {
    color: var(--danger);
    margin: 0;
    font-size: 12px;
  }
</style>
