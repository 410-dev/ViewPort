<script lang="ts">
  import { api, type FieldInfo, type Vocabulary } from "./api";
  import { i18n } from "./i18n.svelte";
  import { quoteParam } from "./query";

  /** Simple form search. Produces query text for the same engine as the advanced editor. */
  let { onquery }: { onquery: (text: string) => void } = $props();

  type L = { ko: string; en: string };
  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);

  let vocab = $state<Vocabulary | null>(null);
  let fields = $state<FieldInfo[]>([]);
  $effect(() => {
    api.queryVocabulary(i18n.locale).then((v) => (vocab = v));
    api.listFields().then((f) => (fields = f));
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
  let folder = $state("");
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
    const tagList = tags.split(",").map((t) => t.trim()).filter(Boolean);
    if (tagList.length && tagField) parts.push(call("x.tags", opName("contains_all"), ...tagList.map(quoteParam)));
    if (minRating != null && ratingField) parts.push(call("x.rating", String(minRating), opName("ge")));
    if (folder.trim()) parts.push(call("folder", opName("contains_any"), quoteParam(folder.trim())));
    if (sortKey) parts.push(`${sortDesc ? vocab.sort_desc : vocab.sort_asc}(${fname(sortKey)})`);
    return parts.join(` ${vocab.and} `);
  }

  function reset() {
    name = folder = tags = createdFrom = createdTo = sortKey = "";
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
  }}
>
  <label>
    <span>{lbl({ ko: "이름", en: "Name" })}</span>
    <input bind:value={name} />
  </label>
  <label>
    <span>{lbl({ ko: "유형", en: "Type" })}</span>
    <select bind:value={kind}>
      <option value="">{lbl({ ko: "전체", en: "All" })}</option>
      <option value="image">{lbl({ ko: "이미지", en: "Images" })}</option>
      <option value="video">{lbl({ ko: "영상", en: "Videos" })}</option>
    </select>
  </label>
  <label>
    <span>{lbl({ ko: "생성 날짜", en: "Created" })}</span>
    <span class="pair"><input type="date" bind:value={createdFrom} />–<input type="date" bind:value={createdTo} /></span>
  </label>
  <label>
    <span>{lbl({ ko: "크기 (MB)", en: "Size (MB)" })}</span>
    <span class="pair"
      ><input type="number" min="0" step="any" bind:value={minSizeMb} />–<input type="number" min="0" step="any" bind:value={maxSizeMb} /></span
    >
  </label>
  <label>
    <span>{lbl({ ko: "길이 (초)", en: "Duration (s)" })}</span>
    <span class="pair"
      ><input type="number" min="0" bind:value={minDurSec} />–<input type="number" min="0" bind:value={maxDurSec} /></span
    >
  </label>
  {#if tagField}
    <label>
      <span>{lbl({ ko: "태그 (쉼표, 모두 포함)", en: "Tags (comma, all of)" })}</span>
      <input bind:value={tags} />
    </label>
  {/if}
  {#if ratingField && ratingField.ty.type === "ranged_int"}
    <label>
      <span>{lbl({ ko: "최소 점수", en: "Min rating" })}</span>
      <input type="number" min={ratingField.ty.min} max={ratingField.ty.max} bind:value={minRating} />
    </label>
  {/if}
  <label>
    <span>{lbl({ ko: "폴더", en: "Folder" })}</span>
    <input bind:value={folder} />
  </label>
  <label>
    <span>{lbl({ ko: "정렬", en: "Sort" })}</span>
    <span class="pair">
      <select bind:value={sortKey}>
        <option value="">{lbl({ ko: "기본", en: "Default" })}</option>
        {#each vocab?.fields ?? [] as f (f.key)}<option value={f.key}>{f.name}</option>{/each}
      </select>
      <label class="check"><input type="checkbox" bind:checked={sortDesc} />{lbl({ ko: "내림차순", en: "Desc" })}</label>
    </span>
  </label>
  <div class="buttons">
    <button type="button" onclick={reset}>{lbl({ ko: "초기화", en: "Reset" })}</button>
    <button type="submit" class="primary">{lbl({ ko: "검색", en: "Search" })}</button>
  </div>
</form>

<style>
  .basic {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: 8px 16px;
    padding: 10px 16px 4px;
    font-size: 13px;
  }
  label {
    display: grid;
    gap: 3px;
  }
  label > span:first-child {
    color: var(--muted);
    font-size: 12px;
  }
  .pair {
    display: flex;
    gap: 4px;
    align-items: center;
  }
  .pair > :global(input),
  .pair > select {
    flex: 1;
    min-width: 0;
  }
  input,
  select {
    font: inherit;
    color: inherit;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 5px 8px;
    min-width: 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
    white-space: nowrap;
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: end;
    justify-content: flex-end;
  }
</style>
