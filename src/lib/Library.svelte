<script lang="ts">
  import { ask } from "@tauri-apps/plugin-dialog";
  import { pickFolder } from "./pick.svelte";
  import { onBack, PASS } from "./back.svelte";
  import { onDestroy } from "svelte";
  import {
    api,
    asCmdError,
    onScan,
    type BundleSummary,
    type MediaBrief,
    type SavedQuery,
    type ScanEvent,
    type Settings,
    type ShardStatus,
    type SourceRow,
  } from "./api";
  import { i18n, t } from "./i18n.svelte";
  import { isTyping, matches } from "./keys";
  import RightPanel from "./panel/RightPanel.svelte";
  import { savePrefs, ui } from "./prefs.svelte";
  import { quoteParam } from "./query";
  import QueryModal from "./query/QueryModal.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import ShardDialog from "./shard/ShardDialog.svelte";
  import ShardPanel from "./shard/ShardPanel.svelte";
  import Sidebar from "./shell/Sidebar.svelte";
  import Toolbar from "./shell/Toolbar.svelte";
  import SourceDialog from "./SourceDialog.svelte";
  import MoveDialog from "./MoveDialog.svelte";
  import StatusBar from "./StatusBar.svelte";
  import ContextMenu, { type MenuItem } from "./ui/ContextMenu.svelte";
  import Icon from "./ui/Icon.svelte";
  import ViewerShell from "./ViewerShell.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { isMobile } from "./platform";
  import { broadcast, onSync } from "./sync";
  import { briefs, invalidate } from "./views/briefs.svelte";
  import { columnsFor, type SortSpec } from "./views/columns";
  import { VIEWS } from "./views";
  import { Selection, type FolderRef } from "./views/selection.svelte";
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
  let shardDialog = $state(false);
  /** Source id of the shard being managed (or unlocked). */
  let shardPanel = $state<number | null>(null);
  let shards = $state<Record<number, ShardStatus>>({});

  const selection = new Selection();
  const selectedIds = $derived([...selection.set]);
  const view = $derived(VIEWS.find((v) => v.key === ui().view_mode) ?? VIEWS[0]);
  const columns = $derived(columnsFor(ui().columns));

  // ---- data
  async function refreshSources() {
    sources = await api.listSources();
    refreshShards();
  }
  async function refreshShards() {
    if (!sources.some((s) => s.driver === "shard")) {
      shards = {};
      return;
    }
    const list = await api.shardStatus().catch(() => []);
    shards = Object.fromEntries(list.map((s) => [s.source_id, s]));
  }
  /** A locked shard opens its panel (to unlock it) when chosen. */
  function selectSource(id: number | null) {
    selected = id;
    const st = id === null ? undefined : shards[id];
    if (st && st.found && st.encrypted && !st.unlocked) shardPanel = id;
  }
  async function refreshSaved() {
    saved = await api.listSavedQueries(i18n.queryLocale).catch(() => []);
  }

  /** What is sent to the engine: plain words search names; header sorting is appended. */
  const effectiveQuery = $derived.by(() => {
    let q = query.trim();
    if (q && !/[()]/.test(q)) q = `name(contains:${quoteParam(q)})`;
    if (ui().media_only) q = q ? `(${q}) AND NOT type(nonmedia)` : "NOT type(nonmedia)";
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
    selected !== null && !query.trim()
      ? subfolders.map((s) => ({ ...s, sourceId: selected!, path: folder ? `${folder}/${s.name}` : s.name }))
      : [],
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
        if (e.done && shards[e.source_id]) refreshShards();
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
    const shardPath = s.driver === "shard" ? shards[s.id]?.path : undefined;
    const deleteData = !!shardPath && (await ask(t("side.removeShardData", { path: shardPath }), { kind: "warning" }));
    try {
      await api.removeSource(s.id, deleteData);
    } catch (e) {
      alert(asCmdError(e).message);
    }
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

  /** The viewer shows media only: non-media files are left out, and opening one shows its panel. */
  function viewable(id: number, list: number[]): number[] | null {
    if (briefs.get(id)?.kind === "nonmedia") {
      setPanel(true);
      return null;
    }
    return list.filter((x) => x === id || briefs.get(x)?.kind !== "nonmedia");
  }

  function openViewer(id: number, all: number[]) {
    const list = viewable(id, all);
    const index = list ? list.indexOf(id) : -1;
    if (list && index >= 0) viewing = { list, index };
  }

  /** Desktop: the item in its own viewer window (falls back to the in-window viewer on mobile). */
  function openViewerWindow(id: number, all: number[]) {
    if (isMobile) return openViewer(id, all);
    const list = viewable(id, all);
    if (!list) return;
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
    if (!settings || viewing || modal || showSettings || sourceDialog || shardDialog || shardPanel !== null) return;
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
  // ---- selection actions (selection bar, context menu)
  let confirmDelete = $state<number[] | null>(null);
  let deleting = $state(false);
  $effect(() => {
    if (confirmDelete) return onBack(() => (confirmDelete = null));
  });

  const menuItems = $derived<MenuItem[]>([
    { label: t("sel.info"), icon: "info", action: () => setPanel(true) },
    ...(selection.set.size === 1 ? [{ label: t("sel.rename"), icon: "pencil" as const, action: () => startRename([...selection.set][0]) }] : []),
    { label: t("sel.move"), icon: "folder", action: () => startMove([...selection.set]) },
    { label: t("sel.export"), icon: "download", action: () => exportSelected([...selection.set]) },
    { label: t("sel.all"), icon: "checkSquare", action: () => selection.selectAll(ids) },
    { label: t("sel.clear"), icon: "x", action: () => selection.clear() },
    { label: t("sel.delete"), icon: "trash", danger: true, action: () => (confirmDelete = [...selection.set]) },
  ]);

  // ---- rename / move (within the item's own source; shards included)
  /** Renaming an item (`id`) or a folder (`folder`). */
  let renaming = $state<{ id?: number; folder?: FolderRef; name: string; error: string } | null>(null);
  let moving = $state<{ ids: number[]; folder?: FolderRef; sourceId: number; start: string } | null>(null);
  $effect(() => {
    if (renaming) return onBack(() => (renaming = null));
  });

  async function briefsOf(list: number[]): Promise<MediaBrief[]> {
    const missing = list.filter((id) => !briefs.has(id));
    if (missing.length) for (const b of await api.mediaBrief(missing)) briefs.set(b.id, b);
    return list.map((id) => briefs.get(id)).filter((b): b is MediaBrief => !!b);
  }

  function relocateError(e: unknown): string {
    const err = asCmdError(e);
    return err.code === "exists" ? t("rename.exists") : err.code === "unsupported" ? t("rename.unsupported") : err.message;
  }

  async function startRename(id: number) {
    const [b] = await briefsOf([id]);
    if (b) renaming = { id, name: b.name, error: "" };
  }

  async function commitRename() {
    if (!renaming) return;
    const { id, folder: f, name } = renaming;
    try {
      if (f) {
        const r = await api.renameFolder(f.sourceId, f.path, name);
        renaming = null;
        afterFolderChange();
        if (r.failed.length) alert(`${t("move.failed", { n: r.failed.length })}\n\n${r.failed.slice(0, 5).join("\n")}`);
        return;
      }
      if (id === undefined) return;
      await api.renameMedia(id, name);
      renaming = null;
      afterRelocate([id]);
    } catch (e) {
      if (renaming) renaming.error = relocateError(e);
    }
  }

  async function startMove(list: number[]) {
    const bs = await briefsOf(list);
    const src = new Set(bs.map((b) => b.source_id));
    if (src.size !== 1) return alert(t("move.mixed"));
    moving = { ids: list, sourceId: bs[0].source_id, start: bs[0].folder };
  }

  async function commitMove(target: string) {
    if (!moving) return;
    const { ids: list, folder: f } = moving;
    moving = null;
    if (f) {
      try {
        const r = await api.moveFolder(f.sourceId, f.path, target);
        afterFolderChange();
        if (r.failed.length) alert(`${t("move.failed", { n: r.failed.length })}\n\n${r.failed.slice(0, 5).join("\n")}`);
      } catch (e) {
        alert(relocateError(e));
      }
      return;
    }
    try {
      const r = await api.moveMediaTo(list, target);
      afterRelocate(list);
      if (r.failed.length) alert(`${t("move.failed", { n: r.failed.length })}\n\n${r.failed.slice(0, 5).join("\n")}`);
    } catch (e) {
      alert(relocateError(e));
    }
  }

  function afterRelocate(list: number[]) {
    invalidate(list);
    broadcast("media", list);
    runSearch(false);
    if (selected !== null) api.childFolders(selected, folder).then((l) => (subfolders = l)).catch(() => {});
  }

  // ---- folder actions (a folder's context menu)
  const parentOf = (path: string) => path.split("/").slice(0, -1).join("/");
  const baseName = (path: string) => path.split("/").at(-1) ?? path;

  /** Shows a folder of any source (the tree lists every source). */
  function openFolder(f: FolderRef) {
    if (selected !== f.sourceId) {
      selected = f.sourceId;
      lastSource = f.sourceId;
    }
    folder = f.path;
    selection.clear();
  }

  function folderAction(run: (f: FolderRef) => void) {
    return () => {
      if (selection.folder) run(selection.folder);
    };
  }

  const folderMenuItems = $derived<MenuItem[]>([
    { label: t("folder.open"), icon: "folder", action: folderAction(openFolder) },
    ...(isMobile
      ? []
      : [
          {
            label: t("folder.openWindow"),
            icon: "external" as const,
            action: folderAction((f) =>
              api.openWindow({ kind: "library", sourceId: f.sourceId, folder: f.path, query: "" }).catch((e) => alert(asCmdError(e).message)),
            ),
          },
        ]),
    { label: t("sel.rename"), icon: "pencil", action: folderAction((f) => (renaming = { folder: f, name: baseName(f.path), error: "" })) },
    { label: t("sel.move"), icon: "folder", action: folderAction((f) => (moving = { ids: [], folder: f, sourceId: f.sourceId, start: parentOf(f.path) })) },
    { label: t("sel.delete"), icon: "trash", danger: true, action: folderAction((f) => (confirmFolderDelete = f)) },
  ]);

  let confirmFolderDelete = $state<FolderRef | null>(null);
  $effect(() => {
    if (confirmFolderDelete) return onBack(() => (confirmFolderDelete = null));
  });

  async function deleteFolderConfirmed() {
    const f = confirmFolderDelete;
    if (!f) return;
    deleting = true;
    try {
      const r = await api.deleteFolder(f.sourceId, f.path);
      afterFolderChange();
      if (r.failed.length) alert(`${t("del.failed", { n: r.failed.length })}\n\n${r.failed.slice(0, 5).join("\n")}`);
    } catch (e) {
      alert(asCmdError(e).message);
    } finally {
      deleting = false;
      confirmFolderDelete = null;
    }
  }

  /** Paths changed under a folder: everything cached about paths is stale. */
  function afterFolderChange() {
    selection.clear();
    invalidate();
    broadcast("media");
    runSearch(false);
    if (selected !== null) api.childFolders(selected, folder).then((l) => (subfolders = l)).catch(() => {});
    resetKey++;
  }

  async function exportSelected(list: number[]) {
    const dest = await pickFolder(t("panel.exportTo"));
    if (dest) api.exportMedia(list, dest).catch((e) => alert(asCmdError(e).message));
  }

  async function deleteConfirmed() {
    if (!confirmDelete) return;
    const list = confirmDelete;
    deleting = true;
    try {
      const r = await api.deleteMedia(list);
      selection.clear();
      broadcast("media", list);
      runSearch(false);
      if (r.failed.length) alert(`${t("del.failed", { n: r.failed.length })}\n\n${r.failed.slice(0, 5).join("\n")}`);
    } catch (e) {
      alert(asCmdError(e).message);
    } finally {
      deleting = false;
      confirmDelete = null;
    }
  }

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
      {shards}
      onselect={selectSource}
      onaddfolder={addFolder}
      onaddnetwork={() => (sourceDialog = { source: null })}
      onaddshard={() => (shardDialog = true)}
      onrescan={(s) => api.scanSource(s.id)}
      onedit={(s) => (s.driver === "shard" ? (shardPanel = s.id) : (sourceDialog = { source: s }))}
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
      {sort}
      onsetsort={(s) => (sort = s)}
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
        <button class="btn sm danger" onclick={() => (confirmDelete = [...selection.set])}><Icon name="trash" size={14} />{t("sel.delete")}</button>
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
            folderPreview={settings.grid.folder_preview ?? 2}
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

{#if selection.folderMenu && selection.folder}
  <ContextMenu x={selection.folderMenu.x} y={selection.folderMenu.y} items={folderMenuItems} onclose={() => (selection.folderMenu = null)} />
{/if}

{#if selection.menu && selection.set.size > 0}
  <ContextMenu x={selection.menu.x} y={selection.menu.y} items={menuItems} onclose={() => (selection.menu = null)} />
{/if}

{#if renaming}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (renaming = null)}>
    <div class="dialog confirm" role="dialog" aria-modal="true" aria-labelledby="ren-title">
      <header><h2 id="ren-title">{t("rename.title")}</h2></header>
      <div class="body form">
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input"
          bind:value={renaming.name}
          autofocus
          onfocus={(e) => {
            const dot = renaming?.folder ? -1 : e.currentTarget.value.lastIndexOf(".");
            e.currentTarget.setSelectionRange(0, dot > 0 ? dot : e.currentTarget.value.length);
          }}
          onkeydown={(e) => e.key === "Enter" && renaming?.name.trim() && commitRename()}
        />
        {#if renaming.error}<p class="err">{renaming.error}</p>{/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" onclick={() => (renaming = null)}>{t("common.cancel")}</button>
        <button class="btn primary" disabled={!renaming.name.trim()} onclick={commitRename}>{t("common.save")}</button>
      </footer>
    </div>
  </div>
{/if}

{#if moving}
  <MoveDialog
    sourceId={moving.sourceId}
    sourceName={sources.find((s) => s.id === moving!.sourceId)?.name ?? ""}
    start={moving.start}
    exclude={moving.folder?.path ?? null}
    onpick={commitMove}
    onclose={() => (moving = null)}
  />
{/if}

{#if confirmDelete}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !deleting && (confirmDelete = null)}>
    <div class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="del-title">
      <header><h2 id="del-title">{t("del.title")}</h2></header>
      <div class="body"><p class="text">{t("del.body", { n: confirmDelete.length.toLocaleString() })}</p></div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" disabled={deleting} onclick={() => (confirmDelete = null)}>{t("common.cancel")}</button>
        <button class="btn danger" disabled={deleting} onclick={deleteConfirmed}>
          {#if deleting}<span class="spinner"></span>{:else}<Icon name="trash" size={14} />{/if}{t("del.confirm")}
        </button>
      </footer>
    </div>
  </div>
{/if}

{#if confirmFolderDelete}
  {@const f = confirmFolderDelete}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !deleting && (confirmFolderDelete = null)}>
    <div class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="fdel-title">
      <header><h2 id="fdel-title">{t("folder.delTitle")}</h2></header>
      <div class="body"><p class="text">{t("folder.delBody", { name: baseName(f.path), n: f.count.toLocaleString() })}</p></div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" disabled={deleting} onclick={() => (confirmFolderDelete = null)}>{t("common.cancel")}</button>
        <button class="btn danger" disabled={deleting} onclick={deleteFolderConfirmed}>
          {#if deleting}<span class="spinner"></span>{:else}<Icon name="trash" size={14} />{/if}{t("del.confirm")}
        </button>
      </footer>
    </div>
  </div>
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

{#if shardDialog}
  <ShardDialog
    {bundle}
    {sources}
    onclose={() => (shardDialog = false)}
    ondone={async (s) => {
      shardDialog = false;
      await refreshSources();
      broadcast("sources");
      selected = s.id;
      runSearch(false);
    }}
  />
{/if}

{#if shardPanel !== null}
  {@const src = sources.find((s) => s.id === shardPanel)}
  {#if src}
    <ShardPanel
      source={src}
      status={shards[src.id] ?? null}
      {bundle}
      {sources}
      onclose={() => (shardPanel = null)}
      onchanged={async () => {
        await refreshSources();
        broadcast("sources");
        invalidate();
        runSearch(false);
      }}
    />
  {/if}
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
  .confirm {
    width: min(420px, 100%);
  }
  .confirm .err {
    margin: 0;
    color: var(--danger);
    font-size: 13px;
  }
  .confirm .text {
    margin: 0;
    line-height: 1.5;
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
