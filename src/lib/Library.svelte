<script lang="ts">
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import { onDestroy } from "svelte";
  import { api, asCmdError, onScan, onThumbs, type BundleSummary, type ScanEvent, type Settings, type SourceRow } from "./api";
  import Grid from "./Grid.svelte";
  import Viewer from "./Viewer.svelte";
  import SettingsPanel from "./Settings.svelte";
  import { t } from "./i18n.svelte";

  let { bundle, onclose }: { bundle: BundleSummary; onclose: () => void } = $props();

  let sources = $state<SourceRow[]>([]);
  let selected = $state<number | null>(null);
  let query = $state("");
  let ids = $state<number[]>([]);
  let elapsed = $state(0);
  let queryError = $state<{ message: string; start: number; end: number } | null>(null);
  let scans = $state<Record<number, ScanEvent>>({});
  let thumbsDone = $state<number | null>(null);
  let settings = $state<Settings | null>(null);
  let resetKey = $state(0);
  let viewing = $state<number | null>(null);
  let showSettings = $state(false);

  async function refreshSources() {
    sources = await api.listSources();
  }

  async function runSearch(reset: boolean) {
    try {
      const r = await api.search(query, selected);
      ids = r.ids;
      elapsed = r.elapsed_ms;
      queryError = null;
      if (reset) resetKey++;
    } catch (e) {
      const err = asCmdError(e);
      queryError =
        err.code === "query"
          ? { message: err.message, start: err.detail.span.start, end: err.detail.span.end }
          : { message: err.message, start: 0, end: 0 };
    }
  }

  // Re-run the search while typing, after a short pause.
  let debounce: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    query;
    selected;
    clearTimeout(debounce);
    debounce = setTimeout(() => runSearch(true), 250);
  });

  // While scanning, refresh results at most every 1.5 s so new files show up.
  let lastRefresh = 0;
  const unlisten = [
    onScan((e) => {
      scans[e.source_id] = e;
      const now = performance.now();
      if (e.done || now - lastRefresh > 1500) {
        lastRefresh = now;
        runSearch(false);
      }
    }),
    onThumbs((e) => (thumbsDone = e.finished ? null : e.done)),
  ];
  onDestroy(() => unlisten.forEach((p) => p.then((f) => f())));

  $effect(() => {
    refreshSources();
    api.getSettings().then((s) => (settings = s));
  });

  async function addFolder() {
    const path = await open({ title: t("lib.addFolder"), directory: true });
    if (typeof path !== "string") return;
    try {
      const s = await api.addLocalSource(path);
      await refreshSources();
      selected = s.id;
    } catch (e) {
      alert(asCmdError(e).message);
    }
  }

  async function removeSource(s: SourceRow) {
    if (!(await ask(t("lib.removeConfirm", { name: s.name }), { kind: "warning" }))) return;
    await api.removeSource(s.id);
    if (selected === s.id) selected = null;
    delete scans[s.id];
    await refreshSources();
    runSearch(false);
  }

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  function setTile(px: number) {
    if (!settings) return;
    settings.grid.tile_px = px;
    clearTimeout(saveTimer);
    saveTimer = setTimeout(() => api.saveSettings($state.snapshot(settings!)), settings.general.autosave_debounce_ms);
  }

  async function close() {
    await api.closeBundle();
    onclose();
  }

  const status = $derived.by(() => {
    const running = Object.values(scans).find((s) => !s.done);
    if (running) return t("lib.scanning", { files: running.files });
    if (thumbsDone != null) return t("lib.thumbs", { done: thumbsDone });
    const last = Object.values(scans).at(-1);
    if (last?.error) return last.error;
    if (last) return t("lib.scanDone", { added: last.added, updated: last.updated, missing: last.missing ?? 0 });
    return "";
  });
</script>

<div class="layout">
  <aside>
    <div class="bundle">
      <strong title={bundle.path}>{bundle.name}</strong>
      {#if bundle.mode === "manual_save_only"}
        <span class="warn">{t("lib.manualSave")}</span>
      {/if}
    </div>

    <h2>{t("lib.sources")}</h2>
    <nav>
      <button class="src" class:active={selected === null} onclick={() => (selected = null)}>{t("lib.all")}</button>
      {#each sources as s (s.id)}
        <div class="src-row">
          <button class="src" class:active={selected === s.id} onclick={() => (selected = s.id)} title={s.root}>
            {s.name}
            {#if scans[s.id] && !scans[s.id].done}<span class="spin">⟳</span>{/if}
          </button>
          <button class="icon" title={t("lib.rescan")} onclick={() => api.scanSource(s.id)}>⟳</button>
          <button class="icon" title={t("lib.remove")} onclick={() => removeSource(s)}>✕</button>
        </div>
      {/each}
    </nav>
    <button onclick={addFolder}>+ {t("lib.addFolder")}</button>

    <div class="grow"></div>
    {#if bundle.mode === "manual_save_only"}
      <button onclick={() => api.saveBundle().catch((e) => alert(asCmdError(e).message))}>{t("lib.save")}</button>
    {/if}
    <button class="ghost" onclick={() => (showSettings = true)}>{t("lib.settings")}</button>
    <button class="ghost" onclick={close}>{t("lib.close")}</button>
  </aside>

  <main>
    <div class="toolbar">
      <input
        class="query"
        class:invalid={queryError}
        bind:value={query}
        placeholder={t("lib.searchPlaceholder")}
        spellcheck="false"
      />
      {#if settings}
        <label class="size" title={t("lib.tileSize")}>
          <input
            type="range"
            min={settings.grid.min_tile_px}
            max={settings.grid.max_tile_px}
            value={settings.grid.tile_px}
            oninput={(e) => setTile(+e.currentTarget.value)}
          />
        </label>
      {/if}
    </div>
    {#if queryError}
      <div class="query-error">
        <code
          >{query.slice(0, queryError.start)}<mark>{query.slice(queryError.start, queryError.end) || " "}</mark
          >{query.slice(queryError.end)}</code
        >
        <span>{t("err.query")}: {queryError.message}</span>
      </div>
    {/if}
    <div class="meta">
      <span>{t("lib.count", { n: ids.length.toLocaleString() })} · {elapsed.toFixed(1)} ms</span>
      <span>{status}</span>
    </div>

    <div class="content">
      {#if ids.length === 0}
        <p class="empty">{sources.length === 0 ? t("lib.empty") : t("lib.noResults")}</p>
      {:else if settings}
        <Grid
          {ids}
          {resetKey}
          tile={settings.grid.tile_px}
          showNames={settings.grid.show_names}
          overscan={settings.grid.overscan_rows}
          onopen={(i) => (viewing = i)}
        />
      {/if}
    </div>
  </main>
</div>

{#if showSettings && settings}
  <SettingsPanel {settings} onclose={() => (showSettings = false)} />
{/if}

{#if viewing !== null && settings && ids.length > 0}
  <Viewer {ids} bind:index={viewing} {settings} onclose={() => (viewing = null)} />
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    height: 100vh;
  }
  aside {
    border-right: 1px solid var(--border);
    background: var(--panel);
    padding: 16px 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
  }
  .bundle {
    display: grid;
    gap: 4px;
    padding: 0 4px 8px;
  }
  .bundle strong {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .warn {
    font-size: 12px;
    color: var(--warn);
  }
  h2 {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    margin: 8px 4px 0;
  }
  nav {
    display: grid;
    gap: 2px;
  }
  .src-row {
    display: flex;
    align-items: center;
  }
  .src {
    flex: 1;
    min-width: 0;
    text-align: left;
    border: 0;
    background: transparent;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .src.active {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .icon {
    border: 0;
    background: transparent;
    padding: 4px 6px;
    color: var(--muted);
    opacity: 0;
  }
  .src-row:hover .icon {
    opacity: 1;
  }
  .spin {
    display: inline-block;
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .grow {
    flex: 1;
  }
  .ghost {
    border: 0;
    background: transparent;
    color: var(--muted);
  }
  main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .toolbar {
    display: flex;
    gap: 12px;
    align-items: center;
    padding: 12px 16px 0;
  }
  .query {
    flex: 1;
    min-width: 0;
    font: 14px ui-monospace, monospace;
  }
  .query.invalid {
    border-color: var(--danger);
  }
  .size input {
    width: 120px;
  }
  .query-error {
    margin: 6px 16px 0;
    display: grid;
    gap: 2px;
    font-size: 12px;
    color: var(--danger);
  }
  .query-error code {
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--fg);
  }
  mark {
    background: var(--danger);
    color: #fff;
  }
  .meta {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 6px 16px 8px;
    font-size: 12px;
    color: var(--muted);
  }
  .content {
    flex: 1;
    min-height: 0;
  }
  .empty {
    color: var(--muted);
    text-align: center;
    margin-top: 80px;
  }
  @media (max-width: 640px) {
    .layout {
      grid-template-columns: 1fr;
      grid-template-rows: auto 1fr;
    }
    aside {
      border-right: 0;
      border-bottom: 1px solid var(--border);
      max-height: 40vh;
      overflow-y: auto;
    }
  }
</style>
