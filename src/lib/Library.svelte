<script lang="ts">
  import { ask } from "@tauri-apps/plugin-dialog";
  import { pickFolder } from "./pick.svelte";
  import { onBack, PASS } from "./back.svelte";
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
  import ViewerShell from "./ViewerShell.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { isMobile } from "./platform";
  import { broadcast, onSync } from "./sync";
  import { invalidate } from "./views/briefs.svelte";
  import { columnsFor, type SortSpec } from "./views/columns";
  import { VIEWS } from "./views";
  import { Selection } from "./views/selection.svelte";
  import type { FolderItem } from "./views/types";

  let {
    bundle,
    onclose,
    onbundle,
    secondary = false,
    initial = null,
  }: {
    bundle: BundleSummary;
    onclose: () => void;
    onbundle: (b: BundleSummary) => void;
    /** An extra explorer window: "close" closes the window, not the bundle. */
    secondary?: boolean;
    initial?: { sourceId: number | null; folder: string; query: string } | null;
  } = $props();

  let settings = $state<Settings | null>(null);
  let sources = $state<SourceRow[]>([]);
  // svelte-ignore state_referenced_locally
  let selected = $state<number | null>(initial?.sourceId ?? null);
  // svelte-ignore state_referenced_locally
  let folder = $state(initial?.folder ?? "");
  // svelte-ignore state_referenced_locally
  let query = $state(initial?.query ?? "");
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

  // svelte-ignore state_referenced_locally
  let lastSource: number | null = selected;
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
  // Other windows on the same workspace changed something.
  unlisten.push(
    onSync("media", (changed) => {
      invalidate(changed);
      runSearch(false);
    }),
    onSync("sources", () => refreshSources()),
    onSync("saved", () => refreshSaved()),
    onSync("settings", () => api.getSettings().then((s) => (settings = s))),
  );
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
    const path = await pickFolder(t("side.addFolder"));
    if (!path) return;
    try {
      const s = await api.addLocalSource(path);
      await refreshSources();
      broadcast("sources");
      selected = s.id;
    } catch (e) {
      alert(asCmdError(e).message);
    }
  }

  async function removeSource(s: SourceRow) {
    if (!(await ask(t("side.removeConfirm", { name: s.name }), { kind: "warning" }))) return;
    await api.removeSource(s.id);
    broadcast("sources");
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
    broadcast("saved");
  }

  function openViewer(id: number, list: number[]) {
    const index = list.indexOf(id);
    if (index >= 0) viewing = { list, index };
  }

  /** Desktop: the item in its own viewer window (falls back to the in-window viewer on mobile). */
  function openViewerWindow(id: number, list: number[]) {
    if (isMobile) return openViewer(id, list);
    const index = list.indexOf(id);
    if (index >= 0) api.openWindow({ kind: "viewer", ids: list, index }).catch((e) => alert(asCmdError(e).message));
  }

  function openExplorerWindow() {
    api.openWindow({ kind: "library", sourceId: selected, folder, query }).catch((e) => alert(asCmdError(e).message));
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
    if (secondary) return getCurrentWindow().close();
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
    else if (matches(e, k.open_new_window) && selection.focus != null && !isTyping(e))
      openViewerWindow(selection.focus, ids.includes(selection.focus) ? ids : [selection.focus]);
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

  // ---- narrow screens (phones): the sidebar is a drawer, the panel a bottom sheet, and
  // a touch selection gets its own action bar.
  let compact = $state(false);
  $effect(() => {
    const mq = matchMedia("(max-width: 760px)");
    const update = () => (compact = mq.matches);
    update();
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  });
  let drawer = $state(false);
  $effect(() => {
    if (!compact) drawer = false;
  });
  function setPanel(on: boolean) {
    ui().panel_visible = on;
    savePrefs();
  }

  // Back button: open layers first (registered later, so they run first), then up a folder.
  $effect(() =>
    onBack(() => {
      if (selected !== null && folder) folder = folder.split("/").slice(0, -1).join("/");
      else return PASS;
    }),
  );
  $effect(() => {
    if (drawer) return onBack(() => (drawer = false));
  });
  $effect(() => {
    if (compact && ui().panel_visible) return onBack(() => setPanel(false));
  });
  $effect(() => {
    if (compact && selection.set.size > 0) return onBack(() => selection.clear());
  });
</script>

<svelte:window onkeydown={onKey} />

<div
  class="app"
  style:--sidebar="{ui().sidebar_width}px"
  style:--panel="{ui().panel_visible ? ui().panel_width : 0}px"
  class:no-panel={!ui().panel_visible}
>
  {#if drawer}
    <div class="drawer-scrim" role="presentation" onclick={() => (drawer = false)}></div>
  {/if}
  <!-- In the drawer, picking anything in the sidebar closes it. -->
  <div
    class="sidebar"
    class:open={drawer}
    role="presentation"
    onclick={(e) => {
      if (compact && (e.target as Element).closest("button.item")) drawer = false;
    }}
  >
    <Sidebar
      {bundle}
      {sources}
      {selected}
      {scans}
      {saved}
      activeQuery={query}
      {secondary}
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
      onnewwindow={isMobile ? undefined : openExplorerWindow}
      onmenu={compact ? () => (drawer = true) : undefined}
    />
    {#if compact && selection.set.size > 0}
      <div class="selbar">
        <button class="icon-btn" aria-label={t("sel.clear")} onclick={() => selection.clear()}><Icon name="x" size={16} /></button>
        <span class="grow tabular">{t("sel.count", { n: selection.set.size })}</span>
        <button class="btn sm ghost" onclick={() => selection.selectAll(ids)}><Icon name="checkSquare" size={14} />{t("sel.all")}</button>
        <button class="btn sm primary" onclick={() => setPanel(true)}><Icon name="info" size={14} />{t("sel.info")}</button>
      </div>
    {/if}
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
            onopenwindow={openViewerWindow}
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
        onopenwindow={isMobile ? undefined : (id) => openViewerWindow(id, ids.includes(id) ? ids : [id])}
        onclose={compact ? () => setPanel(false) : undefined}
        onchanged={() => {
          invalidate(selectedIds);
          broadcast("media", selectedIds);
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
    onsaved={() => {
      refreshSaved();
      broadcast("saved");
    }}
  />
{/if}

{#if sourceDialog}
  <SourceDialog
    source={sourceDialog.source}
    onclose={() => (sourceDialog = null)}
    ondone={async (s) => {
      sourceDialog = null;
      await refreshSources();
      broadcast("sources");
      selected = s.id;
      runSearch(false);
    }}
  />
{/if}

{#if showSettings && settings}
  <SettingsPanel {settings} {bundle} {onbundle} onclose={() => (showSettings = false)} />
{/if}

{#if viewing && settings}
  <ViewerShell
    panes={[{ ids: viewing.list, index: viewing.index }]}
    {settings}
    onclose={(id) => {
      viewing = null;
      if (id !== undefined) {
        selection.set.clear();
        // On touch a selection means selection mode; just remember where the viewer was.
        if (!selection.touch) selection.set.add(id);
        selection.focus = id;
      }
    }}
    onchanged={() => {
      broadcast("media");
      runSearch(false);
    }}
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
  .selbar {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 8px 6px;
    padding: 4px 6px;
    border-radius: var(--radius-pill);
    background: var(--accent-soft);
    color: var(--fg);
    font-weight: 500;
  }
  .drawer-scrim {
    position: fixed;
    inset: 0;
    z-index: 34;
    background: var(--scrim);
    animation: vp-fade var(--t-med) var(--ease);
  }
  @media (max-width: 760px) {
    .app {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(0, 1fr) auto;
    }
    .sidebar {
      position: fixed;
      top: 0;
      bottom: 0;
      left: 0;
      width: min(320px, 86vw);
      z-index: 35;
      transform: translateX(-102%);
      transition: transform var(--t-med) var(--ease);
    }
    .sidebar.open {
      transform: none;
      box-shadow: var(--shadow);
    }
    .panel {
      position: fixed;
      left: 0;
      right: 0;
      bottom: 0;
      height: 64%;
      z-index: 33;
      border-radius: var(--radius-lg) var(--radius-lg) 0 0;
      overflow: hidden;
      background: var(--surface);
      border-top: 1px solid var(--border);
      box-shadow: var(--shadow);
      animation: vp-sheet var(--t-med) var(--ease);
    }
    .resizer {
      display: none;
    }
  }
  @keyframes vp-sheet {
    from {
      transform: translateY(30%);
      opacity: 0;
    }
  }
</style>
