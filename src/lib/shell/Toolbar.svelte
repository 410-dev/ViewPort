<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import { onBack } from "../back.svelte";
  import type { Settings, SourceRow } from "../api";
  import { t } from "../i18n.svelte";
  import { savePrefs, ui } from "../prefs.svelte";
  import BasicSearch from "../query/BasicSearch.svelte";
  import Dropdown from "../ui/Dropdown.svelte";
  import Icon from "../ui/Icon.svelte";
  import { VIEWS } from "../views";
  import { COLUMNS, type SortSpec } from "../views/columns";

  let {
    query = $bindable(),
    scope,
    folder,
    settings,
    error,
    viewKey,
    sort,
    onsetsort,
    onfolder,
    onview,
    ontile,
    onadvanced,
    onnewwindow,
    onmenu,
    panelVisible,
    onpanel,
  }: {
    query: string;
    scope: SourceRow | null;
    folder: string;
    settings: Settings | null;
    error: string | null;
    viewKey: string;
    /** Sort applied on top of the query, shared by every view (the list headers set it too). */
    sort: SortSpec | null;
    onsetsort: (sort: SortSpec | null) => void;
    onfolder: (path: string) => void;
    onview: (key: "grid" | "list" | "tree") => void;
    ontile: (px: number) => void;
    onadvanced: () => void;
    /** Opens another explorer window here; absent on mobile. */
    onnewwindow?: () => void;
    /** Narrow screens: opens the sidebar drawer; the breadcrumbs move to a second row. */
    onmenu?: () => void;
    panelVisible: boolean;
    /** Shows or hides the side panel of this explorer. */
    onpanel: () => void;
  } = $props();

  let search: HTMLInputElement;
  let basic = $state(false);
  $effect(() => {
    if (basic) return onBack(() => (basic = false));
  });

  export function focusSearch() {
    search?.focus();
    search?.select();
  }

  const parts = $derived(folder ? folder.split("/") : []);

  const SORTS = COLUMNS.filter((c) => c.sort);
  let sortBtn = $state<HTMLButtonElement>();
  let sortOpen = $state(false);
  const sortItems = $derived([
    { label: t("basic.defaultSort") },
    ...SORTS.map((c) => ({
      label: t(c.label),
      detail: sort?.field === c.sort ? t(sort!.desc ? "basic.desc" : "basic.asc") : undefined,
    })),
  ]);
  const sortIndex = $derived(sort ? SORTS.findIndex((c) => c.sort === sort!.field) + 1 : 0);

  /** The same field again flips the direction; another field starts ascending. */
  function pickSort(i: number) {
    sortOpen = false;
    const field = i > 0 ? SORTS[i - 1].sort! : null;
    if (!field) onsetsort(null);
    else onsetsort({ field, desc: sort?.field === field ? !sort.desc : false });
  }
</script>

<div class="toolbar" class:compact={!!onmenu}>
  {#if onmenu}
    <button class="icon-btn menu" title={t("tb.menu")} aria-label={t("tb.menu")} onclick={onmenu}><Icon name="menu" size={18} /></button>
  {/if}
  <div class="crumbs">
    <button class="icon-btn" disabled={!scope || !folder} title={t("tb.up")} onclick={() => onfolder(parts.slice(0, -1).join("/"))}>
      <Icon name="arrowUp" size={15} />
    </button>
    {#if scope}
      <button class="crumb" class:cur={!folder} onclick={() => onfolder("")}>{scope.name}</button>
      {#each parts as part, i (i)}
        <Icon name="chevronRight" size={13} />
        <button class="crumb" class:cur={i === parts.length - 1} onclick={() => onfolder(parts.slice(0, i + 1).join("/"))}>{part}</button>
      {/each}
    {:else}
      <span class="crumb cur static">{t("side.all")}</span>
    {/if}
  </div>

  <div class="search input" class:invalid={!!error}>
    <Icon name="search" size={15} />
    <input
      bind:this={search}
      bind:value={query}
      placeholder={t("tb.searchPlaceholder")}
      spellcheck="false"
      onkeydown={(e) => {
        if (e.key === "Escape") {
          if (query) query = "";
          else search.blur();
        }
      }}
    />
    {#if query}
      <button class="icon-btn sm" title={t("tb.clear")} onclick={() => (query = "")}><Icon name="x" size={13} /></button>
    {/if}
    <button class="icon-btn sm" class:on={basic} title={t("tb.basic")} onclick={() => (basic = !basic)}><Icon name="filter" size={14} /></button>
    <button class="icon-btn sm" title={t("tb.advanced")} onclick={onadvanced}><Icon name="code" size={15} /></button>
  </div>

  <div class="right">
    {#if scope}
      <button
        class="icon-btn"
        class:on={ui().recursive}
        title={t("tb.recursive")}
        onclick={() => {
          ui().recursive = !ui().recursive;
          savePrefs();
        }}><Icon name="layers" size={16} /></button
      >
    {/if}
    <button
      bind:this={sortBtn}
      class="icon-btn"
      class:on={!!sort}
      title={t("basic.sort")}
      aria-label={t("basic.sort")}
      aria-expanded={sortOpen}
      onclick={() => (sortOpen = !sortOpen)}><Icon name={sort?.desc ? "sortDesc" : "sortAsc"} size={16} /></button
    >
    {#if sort}
      <button
        class="icon-btn"
        title={t(sort.desc ? "basic.desc" : "basic.asc")}
        aria-label={t(sort.desc ? "basic.desc" : "basic.asc")}
        onclick={() => sort && onsetsort({ ...sort, desc: !sort.desc })}><Icon name={sort.desc ? "arrowDown" : "arrowUp"} size={15} /></button
      >
    {/if}
    {#if sortOpen && sortBtn}
      <Dropdown anchor={sortBtn} items={sortItems} selected={sortIndex} onpick={pickSort} onclose={() => (sortOpen = false)} />
    {/if}
    {#if viewKey === "grid" && settings && !onmenu}
      <input
        class="tile"
        type="range"
        title={t("tb.tileSize")}
        min={settings.grid.min_tile_px}
        max={settings.grid.max_tile_px}
        value={settings.grid.tile_px}
        oninput={(e) => ontile(+e.currentTarget.value)}
      />
    {/if}
    <div class="seg icons" role="tablist" aria-label={t("tb.view")}>
      {#each VIEWS as v (v.key)}
        <button role="tab" aria-selected={viewKey === v.key} class:on={viewKey === v.key} title={t(v.label)} onclick={() => onview(v.key)}>
          <Icon name={v.icon} size={15} />
        </button>
      {/each}
    </div>
    {#if onnewwindow}
      <button class="icon-btn" title={t("tb.newWindow")} onclick={onnewwindow}><Icon name="windowPlus" size={16} /></button>
    {/if}
    <button
      class="icon-btn"
      class:on={panelVisible}
      title={t("tb.panel")}
      onclick={onpanel}><Icon name="panel" size={16} /></button
    >
  </div>
</div>

{#if basic}
  <div class="pop-scrim" role="presentation" transition:fade onclick={() => (basic = false)}></div>
  <div class="pop" transition:pop={{ base: "translateX(-50%)" }}>
    <BasicSearch onquery={(text) => (query = text)} onclose={() => (basic = false)} />
  </div>
{/if}

<style>
  .toolbar {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 52px;
    padding: 0 12px 0 10px;
    flex-shrink: 0;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
    flex: 0 1 auto;
    max-width: 38%;
    color: var(--faint);
    overflow: hidden;
  }
  .crumb {
    height: 28px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 200px;
    flex-shrink: 1;
    min-width: 0;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .crumb:hover:not(.static) {
    background: var(--hover);
    color: var(--fg);
  }
  .crumb.cur {
    color: var(--fg);
    font-weight: 600;
  }
  .crumb.static {
    display: inline-flex;
    align-items: center;
    cursor: default;
  }
  .search {
    flex: 1;
    min-width: 160px;
    display: flex;
    align-items: center;
    gap: 6px;
    height: 34px;
    padding: 0 4px 0 12px;
    color: var(--muted);
  }
  .search input {
    flex: 1;
    min-width: 0;
    height: 100%;
    border: 0;
    outline: none;
    background: transparent;
    color: var(--fg);
    font: inherit;
    user-select: text;
  }
  .search input::placeholder {
    color: var(--faint);
  }
  .search.invalid {
    border-color: color-mix(in srgb, var(--danger) 60%, transparent);
  }
  .right {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .toolbar.compact {
    flex-wrap: wrap;
    height: auto;
    gap: 6px 8px;
    padding: 8px 8px 4px;
  }
  /* Two rows: menu + search, then breadcrumbs + view controls. */
  .compact .search {
    min-width: 0;
    flex: 1 1 calc(100% - 50px);
    height: var(--h-control);
  }
  .compact .crumbs {
    order: 3;
    flex: 1 1 0;
    max-width: none;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .compact .right {
    order: 4;
    gap: 2px;
    /* Many view controls on a narrow phone wrap instead of running off-screen. */
    flex-wrap: wrap;
    justify-content: flex-end;
    min-width: 0;
  }
  .tile {
    width: 96px;
    accent-color: var(--accent);
  }
  .pop-scrim {
    position: fixed;
    inset: 0;
    z-index: 30;
  }
  .pop {
    position: fixed;
    top: 50px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 31;
    width: min(520px, calc(100vw - 24px));
    max-height: calc(100dvh - 66px);
    overflow-y: auto;
    overscroll-behavior: contain;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
</style>
