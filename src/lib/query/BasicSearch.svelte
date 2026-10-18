<script lang="ts">
  import { api, type FieldInfo, type Vocabulary } from "../api";
  import { i18n, t } from "../i18n.svelte";
  import { quoteParam } from "../query";

  /** Form search. Produces query text for the same engine as the advanced editor. */
  let { onquery, onclose }: { onquery: (text: string) => void; onclose: () => void } = $props();

  let vocab = $state<Vocabulary | null>(null);
  let fields = $state<FieldInfo[]>([]);
  $effect(() => {
    api.queryVocabulary(i18n.queryLocale).then((v) => (vocab = v));
    api.listFields().then((f) => (fields = f)).catch(() => {});
  });

  let name = $state("");
  let kind = $state<"" | "image" | "video">("");
  let createdFrom = $state("");
  let createdTo = $state("");
  let minSizeMb = $state<number | null>(null);
  let maxSizeMb = $state<number | null>(null);
  let minDurSec = $state<number | null>(null);
  let maxDurSec = $state<number | null>(null);
  let tags = $state("");
  let minRating = $state<number | null>(null);
  let sortKey = $state("");
  let sortDesc = $state(false);

  const fname = (key: string) => vocab?.fields.find((f) => f.key === key)?.name ?? key;
  const opName = (op: string) => vocab?.ops.find((o) => o.op === op)?.name ?? op;
  const tagField = $derived(fields.find((f) => f.key === "x.tags"));
  const ratingField = $derived(fields.find((f) => f.key === "x.rating"));

  function build(): string {
    if (!vocab) return "";
    const parts: string[] = [];
    const call = (key: string, ...params: string[]) => `${fname(key)}(${params.join(":")})`;
    if (name.trim()) parts.push(call("name", opName("contains_any"), quoteParam(name.trim())));
    if (kind) parts.push(call("kind", vocab.kinds[kind === "image" ? 0 : 1]));
    if (createdFrom && createdTo) parts.push(call("created", createdFrom, createdTo, opName("between")));
    else if (createdFrom) parts.push(call("created", createdFrom, opName("ge")));
    else if (createdTo) parts.push(call("created", createdTo, opName("le")));
    if (minSizeMb != null) parts.push(call("size", `${minSizeMb}MB`, opName("ge")));
    if (maxSizeMb != null) parts.push(call("size", `${maxSizeMb}MB`, opName("le")));
    if (minDurSec != null) parts.push(call("duration", `${minDurSec}s`, opName("ge")));
    if (maxDurSec != null) parts.push(call("duration", `${maxDurSec}s`, opName("le")));
    const tagList = tags.split(",").map((x) => x.trim()).filter(Boolean);
    if (tagList.length && tagField) parts.push(call("x.tags", opName("contains_all"), ...tagList.map(quoteParam)));
    if (minRating != null && ratingField) parts.push(call("x.rating", String(minRating), opName("ge")));
    if (sortKey) parts.push(`${sortDesc ? vocab.sort_desc : vocab.sort_asc}(${fname(sortKey)})`);
    return parts.join(` ${vocab.and} `);
  }

  function reset() {
    name = tags = createdFrom = createdTo = sortKey = "";
    kind = "";
    minSizeMb = maxSizeMb = minDurSec = maxDurSec = minRating = null;
    onquery("");
  }
</script>

<form
  class="basic"
  onsubmit={(e) => {
    e.preventDefault();
    onquery(build());
    onclose();
  }}
>
  <label class="wide">
    <span>{t("basic.name")}</span>
    <input class="input" bind:value={name} />
  </label>
  <div class="field wide">
    <span>{t("basic.kind")}</span>
    <div class="seg sm">
      <button type="button" class:on={kind === ""} onclick={() => (kind = "")}>{t("basic.all")}</button>
      <button type="button" class:on={kind === "image"} onclick={() => (kind = "image")}>{t("basic.images")}</button>
      <button type="button" class:on={kind === "video"} onclick={() => (kind = "video")}>{t("basic.videos")}</button>
    </div>
  </div>
  <label class="wide">
    <span>{t("basic.created")}</span>
    <span class="pair"><input class="input" type="date" bind:value={createdFrom} /><i>–</i><input class="input" type="date" bind:value={createdTo} /></span>
  </label>
  <label>
    <span>{t("basic.sizeMb")}</span>
    <span class="pair"
      ><input class="input" type="number" min="0" step="any" bind:value={minSizeMb} /><i>–</i><input
        class="input"
        type="number"
        min="0"
        step="any"
        bind:value={maxSizeMb}
      /></span
    >
  </label>
  <label>
    <span>{t("basic.durationSec")}</span>
    <span class="pair"
      ><input class="input" type="number" min="0" bind:value={minDurSec} /><i>–</i><input class="input" type="number" min="0" bind:value={maxDurSec} /></span
    >
  </label>
  {#if tagField}
    <label>
      <span>{t("basic.tags")}</span>
      <input class="input" bind:value={tags} />
    </label>
  {/if}
  {#if ratingField && ratingField.ty.type === "ranged_int"}
    <label>
      <span>{t("basic.minRating")}</span>
      <input class="input" type="number" min={ratingField.ty.min} max={ratingField.ty.max} bind:value={minRating} />
    </label>
  {/if}
  <label class="wide">
    <span>{t("basic.sort")}</span>
    <span class="pair">
      <select class="input" bind:value={sortKey}>
        <option value="">{t("basic.defaultSort")}</option>
        {#each vocab?.fields ?? [] as f (f.key)}<option value={f.key}>{f.name}</option>{/each}
      </select>
      <span class="seg sm">
        <button type="button" class:on={!sortDesc} onclick={() => (sortDesc = false)}>{t("basic.asc")}</button>
        <button type="button" class:on={sortDesc} onclick={() => (sortDesc = true)}>{t("basic.desc")}</button>
      </span>
    </span>
  </label>
  <div class="buttons wide">
    <button type="button" class="btn ghost" onclick={reset}>{t("basic.reset")}</button>
    <button type="submit" class="btn primary">{t("basic.search")}</button>
  </div>
</form>

<style>
  .basic {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px 14px;
    padding: 16px;
  }
  .wide {
    grid-column: 1 / -1;
  }
  label,
  .field {
    display: grid;
    gap: 5px;
  }
  label > span:first-child,
  .field > span:first-child {
    color: var(--muted);
    font-size: 12px;
  }
  .pair {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .pair > .input {
    flex: 1;
    min-width: 0;
  }
  .pair i {
    color: var(--faint);
    font-style: normal;
  }
  .field .seg button,
  .seg {
    flex: 1;
  }
  .field .seg {
    display: flex;
  }
  .field .seg button {
    flex: 1;
  }
  .buttons {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding-top: 4px;
  }
</style>
