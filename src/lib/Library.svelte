<script lang="ts">
  import { ask, open } from "@tauri-apps/plugin-dialog";
  import { onDestroy } from "svelte";
  import { api, asCmdError, onScan, type BundleSummary, type SavedQuery, type ScanEvent, type Settings, type SourceRow } from "./api";
  import { i18n, t } from "./i18n.svelte";
  import { isTyping, matches } from "./keys";
  import RightPanel from "./panel/RightPanel.svelte";
  import { savePrefs, ui } from "./prefs.svelte";
  import { quoteParam } from "./query";
  import QueryModal from "./query/QueryModal.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import Sidebar from "./shell/Sidebar.svelte";
  import Toolbar from "./shell/Toolbar.svelte";
  import SourceDialog from "./SourceDialog.svelte";
  import StatusBar from "./StatusBar.svelte";
  import Icon from "./ui/Icon.svelte";
  import Viewer from "./Viewer.svelte";
  import { invalidate } from "./views/briefs.svelte";
  import { columnsFor, type SortSpec } from "./views/columns";
  import { VIEWS } from "./views";
  import { Selection } from "./views/selection.svelte";
  import type { FolderItem } from "./views/types";

  let {
    bundle,
    onclose,
    onbundle,
  }: { bundle: BundleSummary; onclose: () => void; onbundle: (b: BundleSummary) => void } = $props();

  let settings = $state<Settings | null>(null);
  let sources = $state<SourceRow[]>([]);
  let selected = $state<number | null>(null);
  let folder = $state("");
  let query = $state("");
  let sort = $state<SortSpec | null>(null);
  let ids = $state<number[]>([]);
  let elapsed = $state(0);
  let queryError = $state<string | null>(null);
  let subfolders = $state<{ name: string; count: number }[]>([]);
  let scans = $state<Record<number, ScanEvent>>({});
  let saved = $state<SavedQuery[]>([]);
  let resetKey = $state(0);

  let viewing = $state<{ list: number[]; index: number } | null>(null);
  let modal = $state<{ editing: SavedQuery | null } | null>(null);
  let showSettings = $state(false);
  let sourceDialog = $state<{ source: SourceRow | null } | null>(null);

  const selection = new Selection();
  const selectedIds = $derived([...selection.set]);
  const view = $derived(VIEWS.find((v) => v.key === ui().view_mode) ?? VIEWS[0]);
  const columns = $derived(columnsFor(ui().columns));

  // ---- data
  async function refreshSources() {
    sources = await api.listSources();
  }
  async function refreshSaved() {
    saved = await api.listSavedQueries(i18n.queryLocale).catch(() => []);
  }

  /** What is sent to the engine: plain words search names; header sorting is appended. */
  const effectiveQuery = $derived.by(() => {
    let q = query.trim();
    if (q && !/[()]/.test(q)) q = `name(contains:${quoteParam(q)})`;
    if (sort) q = `${q ? q + " AND " : ""}${sort.desc ? "sort desc" : "sort asc"}(${sort.field})`;
    return q;
  });

  let searchSeq = 0;
  async function runSearch(reset: boolean) {
    const seq = ++searchSeq;
    try {
      const r = await api.search(effectiveQuery, selected, selected === null ? null : folder, ui().recursive);
      if (seq !== searchSeq) return;
      ids = r.ids;
      elapsed = r.elapsed_ms;
      queryError = null;
      selection.retain(r.ids);
      if (reset) resetKey++;
    } catch (e) {
      if (seq === searchSeq) queryError = asCmdError(e).message;
    }
  }

  let lastSource: number | null = null;
  $effect(() => {
    if (selected !== lastSource) {
      lastSource = selected;
      folder = "";
    }
  });

  $effect(() => {
    const src = selected;
    const f = folder;
    subfolders = [];
    if (src !== null) api.childFolders(src, f).then((list) => (subfolders = list)).catch(() => {});
  });

  $effect(() => {
    effectiveQuery;
    selected;
    folder;
    ui().recursive;
    const timer = setTimeout(() => runSearch(true), ui().search_debounce_ms);
    return () => clearTimeout(timer);
  });

  const folders = $derived<FolderItem[]>(
    selected !== null && !query.trim() ? subfolders.map((s) => ({ ...s, path: folder ? `${folder}/${s.name}` : s.name })) : [],
  );

  // While scanning, refresh at most every 1.5 s so new files show up.
  let lastRefresh = 0;
  const unlisten = [
    onScan((e) => {
      scans[e.source_id] = e;
      const now = performance.now();
      if (e.done || now - lastRefresh > 1500) {
        lastRefresh = now;
        if (e.done) invalidate();
        runSearch(false);
        if (e.done && selected === e.source_id) api.childFolders(e.source_id, folder).then((l) => (subfolders = l)).catch(() => {});
      }
    }),
  ];
  onDestroy(() => unlisten.forEach((p) => p.then((f) => f())));

  $effect(() => {
    refreshSources();
    api.getSettings().then((s) => (settings = s));
  });
  $effect(() => {
    i18n.queryLocale;
    refreshSaved();
  });

  // ---- actions
  async function addFolder() {
    const path = await open({ title: t("side.addFolder"), directory: true });
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
    if (!(await ask(t("side.removeConfirm", { name: s.name }), { kind: "warning" }))) return;
    await api.removeSource(s.id);
    if (selected === s.id) selected = null;
    delete scans[s.id];
    await refreshSources();
    invalidate();
    runSearch(false);
  }

  async function deleteSaved(q: SavedQuery) {
    if (!(await ask(t("side.deleteSaved", { name: q.label }), { kind: "warning" }))) return;
    await api.deleteSavedQuery(q.id);
    refreshSaved();
  }

  function openViewer(id: number, list: number[]) {
    const index = list.indexOf(id);
    if (index >= 0) viewing = { list, index };
  }

  function toggleSort(field: string) {
    if (sort?.field !== field) sort = { field, desc: false };
    else if (!sort.desc) sort = { field, desc: true };
    else sort = null;
  }

  function setView(key: "grid" | "list" | "tree") {
    ui().view_mode = key;
    savePrefs();
  }

  let tileTimer: ReturnType<typeof setTimeout> | undefined;
  function setTile(px: number) {
    if (!settings) return;
    settings.grid.tile_px = px;
    clearTimeout(tileTimer);
    tileTimer = setTimeout(() => api.saveSettings($state.snapshot(settings!) as Settings), settings.general.autosave_debounce_ms);
  }

  async function close() {
    await api.closeBundle();
    onclose();
  }

  // ---- resizable columns
  function drag(e: PointerEvent, which: "sidebar" | "panel") {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const start = which === "sidebar" ? ui().sidebar_width : ui().panel_width;
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      if (which === "sidebar") ui().sidebar_width = Math.round(Math.min(420, Math.max(180, start + dx)));
      else ui().panel_width = Math.round(Math.min(720, Math.max(260, start - dx)));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      savePrefs();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // ---- keyboard
  let toolbar: Toolbar;
  function onKey(e: KeyboardEvent) {
    if (!settings || viewing || modal || showSettings || sourceDialog) return;
    const k = settings.keys;
    if (matches(e, k.focus_search)) toolbar?.focusSearch();
    else if (matches(e, k.query_editor)) modal = { editing: null };
    else if (isTyping(e)) return;
    else if (matches(e, k.toggle_panel)) {
      ui().panel_visible = !ui().panel_visible;
      savePrefs();
    } else if (matches(e, k.view_grid)) setView("grid");
    else if (matches(e, k.view_list)) setView("list");
    else if (matches(e, k.view_tree)) setView("tree");
    else if (e.key === "Backspace" && selected !== null && folder) folder = folder.split("/").slice(0, -1).join("/");
    else return;
    e.preventDefault();
  }

  const scope = $derived(selected === null ? null : (sources.find((s) => s.id === selected) ?? null));
</script>

<svelte:window onkeydown={onKey} />

<div
  class="app"
  style:--sidebar="{ui().sidebar_width}px"
  style:--panel="{ui().panel_visible ? ui().panel_width : 0}px"
  class:no-panel={!ui().panel_visible}
>
  <div class="sidebar">
    <Sidebar
      {bundle}
      {sources}
      {selected}
      {scans}
      {saved}
      activeQuery={query}
      onselect={(id) => (selected = id)}
      onaddfolder={addFolder}
      onaddnetwork={() => (sourceDialog = { source: null })}
      onrescan={(s) => api.scanSource(s.id)}
      onedit={(s) => (sourceDialog = { source: s })}
      onremove={removeSource}
      onrunsaved={(q) => (query = q.text)}
      oneditsaved={(q) => (modal = { editing: q })}
      ondeletesaved={deleteSaved}
      onsettings={() => (showSettings = true)}
      onclose={close}
      onsave={() => api.saveBundle().catch((e) => alert(asCmdError(e).message))}
    />
    <div class="resizer" role="separator" aria-orientation="vertical" onpointerdown={(e) => drag(e, "sidebar")}></div>
  </div>

  <main>
    <Toolbar
      bind:this={toolbar}
      bind:query
      {scope}
      {folder}
      {settings}
      error={queryError}
      viewKey={view.key}
      onfolder={(f) => (folder = f)}
      onview={setView}
      ontile={setTile}
      onadvanced={() => (modal = { editing: null })}
    />
    {#if queryError}
      <div class="qerror"><Icon name="alert" size={13} /><span>{queryError}</span></div>
    {/if}
    <div class="view">
      {#if sources.length === 0}
        <div class="empty">
          <Icon name="folderPlus" size={36} stroke={1.2} />
          <p>{t("lib.empty")}</p>
          <button class="btn primary" onclick={addFolder}><Icon name="plus" size={14} />{t("side.addFolder")}</button>
        </div>
      {:else if settings}
        {#if ids.length === 0 && folders.length === 0 && view.key !== "tree"}
          <div class="empty"><Icon name="search" size={30} stroke={1.3} /><p>{t("lib.noResults")}</p></div>
        {:else}
          <view.component
            {ids}
            {folders}
            {selection}
            {resetKey}
            tile={settings.grid.tile_px}
            showNames={settings.grid.show_names}
            overscan={settings.grid.overscan_rows}
            {columns}
            rowPx={ui().row_px}
            {sort}
            tree={{ sources: sources.map((s) => ({ id: s.id, name: s.name })), sourceId: selected, folder, query: effectiveQuery }}
            onsort={toggleSort}
            onopen={openViewer}
            onfolder={(p) => (folder = p)}
          />
        {/if}
      {/if}
    </div>
  </main>

  {#if ui().panel_visible}
    <div class="panel">
      <div class="resizer left" role="separator" aria-orientation="vertical" onpointerdown={(e) => drag(e, "panel")}></div>
      <RightPanel
        focus={selection.focus}
        selected={selectedIds}
        onopen={(id) => openViewer(id, ids.includes(id) ? ids : [id])}
        onchanged={() => {
          invalidate(selectedIds);
          runSearch(false);
        }}
      />
    </div>
  {/if}

  <div class="status"><StatusBar count={ids.length} {elapsed} selected={selection.set.size} /></div>
</div>

{#if modal}
  <QueryModal
    initial={modal.editing?.text ?? query}
    editing={modal.editing}
    onapply={(text) => {
      query = text;
      modal = null;
    }}
    onclose={() => (modal = null)}
    onsaved={refreshSaved}
  />
{/if}

{#if sourceDialog}
  <SourceDialog
    source={sourceDialog.source}
    onclose={() => (sourceDialog = null)}
    ondone={async (s) => {
      sourceDialog = null;
      await refreshSources();
      selected = s.id;
      runSearch(false);
    }}
  />
{/if}

{#if showSettings && settings}
  <SettingsPanel {settings} {bundle} {onbundle} onclose={() => (showSettings = false)} />
{/if}

{#if viewing && settings}
  <Viewer
    ids={viewing.list}
    bind:index={viewing.index}
    {settings}
    onclose={() => {
      const id = viewing?.list[viewing.index];
      viewing = null;
      if (id !== undefined) {
        selection.set.clear();
        selection.set.add(id);
        selection.focus = id;
      }
    }}
    onchanged={() => runSearch(false)}
  />
{/if}

<style>
  .app {
    display: grid;
    grid-template-columns: var(--sidebar) minmax(0, 1fr) var(--panel);
    grid-template-rows: minmax(0, 1fr) auto;
    height: 100vh;
  }
  .sidebar,
  .panel {
    position: relative;
    min-width: 0;
    min-height: 0;
  }
  .status {
    grid-column: 1 / -1;
  }
  main {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }
  .view {
    flex: 1;
    min-height: 0;
    position: relative;
  }
  .resizer {
    position: absolute;
    top: 0;
    bottom: 0;
    right: -4px;
    width: 8px;
    z-index: 5;
    cursor: col-resize;
  }
  .resizer.left {
    left: -4px;
    right: auto;
  }
  .resizer:hover,
  .resizer:active {
    background: linear-gradient(to right, transparent 3px, var(--accent-line) 3px, var(--accent-line) 5px, transparent 5px);
  }
  .qerror {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 16px 6px;
    padding: 6px 12px;
    border-radius: var(--radius-pill);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    color: var(--danger);
    font-size: 12px;
  }
  .qerror span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    height: 100%;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 12px;
    color: var(--faint);
  }
  .empty p {
    margin: 0;
    color: var(--muted);
  }
  @media (max-width: 760px) {
    .app {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: auto minmax(0, 1fr) auto;
    }
    .sidebar {
      max-height: 34vh;
    }
    .panel {
      display: none;
    }
    .resizer {
      display: none;
    }
  }
</style>
