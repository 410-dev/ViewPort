<script lang="ts" module>
  import type { SortSpec as Sort } from "./views/columns";
  /** What the library is browsing: restored on the next start, and used by split-view explorers. */
  export type BrowseState = { sourceId: number | null; folder: string; query: string; sort?: Sort | null };
</script>

<script lang="ts">
  import { fade, pop, slide } from "./ui/motion";
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
    type Sections,
    type Settings,
    type ShardStatus,
    type SourceRow,
  } from "./api";
  import { i18n, t } from "./i18n.svelte";
  import { isTyping, matches } from "./keys";
  import RightPanel from "./panel/RightPanel.svelte";
  import { prefs, savePrefs, ui } from "./prefs.svelte";
  import { quoteParam } from "./query";
  import QueryModal from "./query/QueryModal.svelte";
  import SettingsPanel from "./settings/SettingsPanel.svelte";
  import PeopleView from "./people/PeopleView.svelte";
  import ShardDialog from "./shard/ShardDialog.svelte";
  import ShardPanel from "./shard/ShardPanel.svelte";
  import Sidebar from "./shell/Sidebar.svelte";
  import Toolbar from "./shell/Toolbar.svelte";
  import SourceDialog from "./SourceDialog.svelte";
  import MoveDialog from "./MoveDialog.svelte";
  import StatusBar from "./StatusBar.svelte";
  import HeifBanner from "./HeifBanner.svelte";
  import CopyConflict from "./CopyConflict.svelte";
  import MirrorDialog from "./shard/MirrorDialog.svelte";
  import DedupDialog from "./dedup/DedupDialog.svelte";
  import ContextMenu, { type MenuItem } from "./ui/ContextMenu.svelte";
  import Icon from "./ui/Icon.svelte";
  import ViewerShell from "./ViewerShell.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { isMobile } from "./platform";
  import { broadcast, onSync } from "./sync";
  import { briefs, goneListeners, invalidate } from "./views/briefs.svelte";
  import { columnsFor, type SortSpec } from "./views/columns";
  import { VIEWS } from "./views";
  import { Selection, type FolderRef } from "./views/selection.svelte";
  import type { FolderItem } from "./views/types";
  import type { SectionMark } from "./views/scroll";

  let {
    bundle,
    onclose,
    onbundle,
    secondary = false,
    initial = null,
    embedded = null,
  }: {
    bundle: BundleSummary;
    onclose: () => void;
    onbundle: (b: BundleSummary) => void;
    /** An extra explorer window: "close" closes the window, not the bundle. */
    secondary?: boolean;
    initial?: BrowseState | null;
    /** An explorer inside a viewer split pane: opening items hands them to the pane. */
    embedded?: { active: boolean; onopen: (ids: number[], index: number, state: BrowseState) => void } | null;
  } = $props();

  /** Only the main window's library is restored on the next start. */
  // svelte-ignore state_referenced_locally
  const persist = !secondary && !embedded;

  let settings = $state<Settings | null>(null);
  let sources = $state<SourceRow[]>([]);
  // svelte-ignore state_referenced_locally
  let selected = $state<number | null>(initial?.sourceId ?? null);
  // svelte-ignore state_referenced_locally
  let folder = $state(initial?.folder ?? "");
  // svelte-ignore state_referenced_locally
  let query = $state(initial?.query ?? "");
  // svelte-ignore state_referenced_locally
  let sort = $state<SortSpec | null>(initial?.sort ?? null);
  /** False until the last browsing state is restored, so nothing is searched twice. */
  // svelte-ignore state_referenced_locally
  let restored = $state(!persist || !!initial);
  let ids = $state<number[]>([]);
  let elapsed = $state(0);
  let queryError = $state<string | null>(null);
  let subfolders = $state<{ name: string; count: number }[]>([]);
  let scans = $state<Record<number, ScanEvent>>({});
  let saved = $state<SavedQuery[]>([]);
  let resetKey = $state(0);
  /** Set with the results it names, so a view never applies one listing's position to another's items. */
  let scrollKey = $state("");
  let sections = $state<Sections>({ kind: "none", marks: [] });
  const sectionMarks = $derived<SectionMark[]>(sections.marks.map((m) => ({ index: m.index, label: sectionLabel(sections.kind, m.key) })));

  let viewing = $state<{ list: number[]; index: number } | null>(null);
  let modal = $state<{ editing: SavedQuery | null } | null>(null);
  let showSettings = $state(false);
  let showPeople = $state(false);
  let showDedup = $state(false);
  let sourceDialog = $state<{ source: SourceRow | null } | null>(null);
  let shardDialog = $state(false);
  /** Source id of the shard being managed (or unlocked). */
  let shardPanel = $state<number | null>(null);
  let shards = $state<Record<number, ShardStatus>>({});

  /** Sources whose storage is not connected (struck through, items hidden). */
  let offline = $state<number[]>([]);
  /** Copying between sources is offered only while the bundle has a shard. */
  const hasShard = $derived(sources.some((s) => s.driver === "shard"));
  // svelte-ignore state_referenced_locally
  let localPanel = $state(false);
  /** Panel visibility: a device preference, but per pane for split-view explorers. */
  const panelVisible = $derived(embedded ? localPanel : ui().panel_visible);

  const selection = new Selection();
  const selectedIds = $derived([...selection.set]);
  const view = $derived(VIEWS.find((v) => v.key === ui().view_mode) ?? VIEWS[0]);
  const columns = $derived(columnsFor(ui().columns));
  /** The source whose mirrors are being set up. */
  let mirroring = $state<SourceRow | null>(null);
  /** Bumped when a new image decoder arrives: views remount and drop thumbnails they gave up on. */
  let decoderEpoch = $state(0);

  /** A section key as shown on the scrollbar: days as "2026년 10월 24일", folders as paths. */
  function sectionLabel(kind: Sections["kind"], key: string): string {
    if (kind === "day") {
      if (!key) return t("lib.noDate");
      return new Date(Number(key) * 86_400_000).toLocaleDateString(i18n.locale, { timeZone: "UTC", year: "numeric", month: "long", day: "numeric" });
    }
    return key || "–";
  }

  // ---- data
  async function refreshSources() {
    sources = await api.listSources();
    refreshShards();
    refreshOffline();
  }
  async function refreshOffline() {
    const list = await api.offlineSources().catch(() => offline);
    if (list.join() === offline.join()) return;
    offline = list;
    invalidate();
    runSearch(false);
  }
  // Drives come and go: check on focus and every `ui.source_check_secs`.
  $effect(() => {
    const secs = ui().source_check_secs;
    const timer = secs > 0 ? setInterval(refreshOffline, secs * 1000) : undefined;
    window.addEventListener("focus", refreshOffline);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", refreshOffline);
    };
  });

  // ---- last browsing state (main window only)
  // svelte-ignore state_referenced_locally
  if (!restored)
    api
      .lastView(bundle.path)
      .then((v) => {
        const last = v as BrowseState | null;
        if (last && typeof last === "object") {
          selected = last.sourceId ?? null;
          lastSource = selected;
          folder = last.folder ?? "";
          query = last.query ?? "";
          sort = last.sort ?? null;
        }
      })
      .catch(() => {})
      .finally(() => (restored = true));
  function browseState(): BrowseState {
    return { sourceId: selected, folder, query, sort: sort ? { ...sort } : null };
  }
  $effect(() => {
    if (!persist || !restored) return;
    const state = browseState();
    const timer = setTimeout(() => api.saveLastView(bundle.path, state).catch(() => {}), 400);
    return () => clearTimeout(timer);
  });
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
  /** Selected and scrolled to once a search lists it (`revealInLibrary`). */
  let revealAfterSearch: number | null = null;
  async function runSearch(reset: boolean) {
    const seq = ++searchSeq;
    const key = `${selected}|${folder}|${ui().recursive}|${effectiveQuery}`;
    try {
      const r = await api.search(effectiveQuery, selected, selected === null ? null : folder, ui().recursive);
      if (seq !== searchSeq) return;
      ids = r.ids;
      sections = r.sections;
      scrollKey = key;
      elapsed = r.elapsed_ms;
      queryError = null;
      selection.retain(r.ids);
      if (reset) resetKey++;
      const reveal = revealAfterSearch;
      if (reveal != null && r.ids.includes(reveal)) {
        revealAfterSearch = null;
        selection.set.clear();
        selection.set.add(reveal);
        selection.focus = reveal;
        selection.revealing = reveal;
      }
    } catch (e) {
      if (seq === searchSeq) queryError = asCmdError(e).message;
    }
  }

  /** Goes to item `id`'s folder (no search, no sorting) and selects it there. */
  async function revealInLibrary(id: number) {
    const b = (await api.mediaBrief([id]).catch(() => []))[0];
    if (!b) return;
    showPeople = false;
    revealAfterSearch = id;
    const same = selected === b.source_id && folder === b.folder && !query && !sort;
    selected = b.source_id;
    lastSource = b.source_id;
    folder = b.folder;
    query = "";
    sort = null;
    // Otherwise the changes above start a search.
    if (same) runSearch(false);
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

  // Image analysis works through what is shown first (desktop; phones do not analyse).
  $effect(() => {
    const shown = ids;
    if (isMobile || embedded) return;
    const timer = setTimeout(() => api.mlPrioritize(shown.slice(0, prefs.app?.ml.priority_items ?? 500)).catch(() => {}), ui().search_debounce_ms);
    return () => clearTimeout(timer);
  });

  $effect(() => {
    effectiveQuery;
    selected;
    folder;
    ui().recursive;
    if (!restored) return;
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
  // Items found gone (failed to load, file missing): drop them from this and other windows.
  const onGone = (gone: number[]) => {
    broadcast("media", gone);
    runSearch(false);
  };
  goneListeners.add(onGone);
  onDestroy(() => {
    unlisten.forEach((p) => p.then((f) => f()));
    goneListeners.delete(onGone);
  });

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
    if (!list || index < 0) return;
    if (embedded) embedded.onopen(list, index, browseState());
    else viewing = { list, index };
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
    if (embedded) return onclose();
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
    if (embedded && !embedded.active) return;
    if (!settings || viewing || modal || savingCurrent || copying || showSettings || showPeople || showDedup || sourceDialog || shardDialog || shardPanel !== null) return;
    const k = settings.keys;
    if (matches(e, k.focus_search)) toolbar?.focusSearch();
    else if (matches(e, k.query_editor)) modal = { editing: null };
    else if (matches(e, k.open_new_window) && selection.focus != null && !isTyping(e))
      openViewerWindow(selection.focus, ids.includes(selection.focus) ? ids : [selection.focus]);
    else if (isTyping(e)) return;
    else if (matches(e, k.toggle_panel)) setPanel(!panelVisible);
    else if (matches(e, k.view_grid)) setView("grid");
    else if (matches(e, k.view_list)) setView("list");
    else if (matches(e, k.view_tree)) setView("tree");
    else if (e.key === "Backspace" && selected !== null && folder) folder = folder.split("/").slice(0, -1).join("/");
    else return;
    e.preventDefault();
  }

  const scope = $derived(selected === null ? null : (sources.find((s) => s.id === selected) ?? null));

  // ---- narrow screens (phones): the sidebar is a drawer, the panel a bottom sheet, and
  // a touch selection gets its own action bar.
  // Measured on the library itself, which may be a split-view pane.
  let rootWidth = $state(0);
  const compact = $derived(rootWidth > 0 ? rootWidth <= 760 : matchMedia("(max-width: 760px)").matches);
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
    {
      label: t("sel.goToFolder"),
      icon: "folder",
      action: () => {
        const id = selection.focus != null && selection.set.has(selection.focus) ? selection.focus : [...selection.set][0];
        if (id != null) revealInLibrary(id);
      },
    },
    ...(selection.set.size === 1 ? [{ label: t("sel.rename"), icon: "pencil" as const, action: () => startRename([...selection.set][0]) }] : []),
    { label: t("sel.move"), icon: "folder", action: () => startMove([...selection.set]) },
    ...(hasShard ? [{ label: t("sel.copyTo"), icon: "copy" as const, action: () => startCopy([...selection.set], []) }] : []),
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

  // ---- copy to another source: into a shard, or out of one (only while the bundle has a shard)
  type FolderPick = { sourceId: number; path: string };
  let copying = $state<{ ids: number[]; folders: FolderPick[]; targets: { id: number; name: string }[] } | null>(null);
  $effect(() => {
    if (copying) return onBack(() => (copying = null));
  });

  /** Items of shards may go anywhere else; anything else only into a shard. */
  async function startCopy(list: number[], folders: FolderPick[]) {
    const origin = new Set([...(await briefsOf(list)).map((b) => b.source_id), ...folders.map((f) => f.sourceId)]);
    const isShard = (id: number) => sources.find((s) => s.id === id)?.driver === "shard";
    const fromShards = [...origin].every(isShard);
    const targets = sources
      .filter((s) => !origin.has(s.id) && s.enabled && !offline.includes(s.id) && (fromShards || s.driver === "shard"))
      .map((s) => ({ id: s.id, name: s.name }));
    if (targets.length === 0) return alert(t("copy.noTarget"));
    copying = { ids: list, folders, targets };
  }

  async function commitCopy(dest: string, to: number) {
    const c = copying;
    copying = null;
    if (!c) return;
    // Runs in the background (status bar); the target's rescan brings the copies in.
    api.copyToSource(c.ids, c.folders, to, dest).catch((e) => alert(asCmdError(e).message));
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

  /** Goes to the folder holding folder `f` (no search, no sorting) and selects `f` there. */
  function revealFolder(f: FolderRef) {
    showPeople = false;
    selected = f.sourceId;
    lastSource = f.sourceId;
    folder = parentOf(f.path);
    query = "";
    sort = null;
    selection.selectFolder(f);
  }

  function folderAction(run: (f: FolderRef) => void) {
    return () => {
      if (selection.folder) run(selection.folder);
    };
  }

  /** Several folders selected: only what applies to all of them at once. */
  const foldersMenuItems = $derived<MenuItem[]>([
    { label: t("sel.info"), icon: "info", action: () => setPanel(true) },
    { label: t("sel.export"), icon: "download", action: () => exportFolders([...selection.folders]) },
    ...(hasShard
      ? [{ label: t("sel.copyTo"), icon: "copy" as const, action: () => startCopy([], selection.folders.map((f) => ({ sourceId: f.sourceId, path: f.path }))) }]
      : []),
    { label: t("sel.delete"), icon: "trash", danger: true, action: () => (confirmFolderDelete = [...selection.folders]) },
  ]);

  const folderMenuItems = $derived<MenuItem[]>([
    { label: t("folder.open"), icon: "folder", action: folderAction(openFolder) },
    { label: t("sel.goToFolder"), icon: "arrowUp", action: folderAction(revealFolder) },
    { label: t("sel.info"), icon: "info", action: () => setPanel(true) },
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
    { label: t("sel.export"), icon: "download", action: folderAction(exportFolder) },
    ...(hasShard ? [{ label: t("sel.copyTo"), icon: "copy" as const, action: folderAction((f) => startCopy([], [{ sourceId: f.sourceId, path: f.path }])) }] : []),
    { label: t("sel.delete"), icon: "trash", danger: true, action: folderAction((f) => (confirmFolderDelete = [f])) },
  ]);

  let confirmFolderDelete = $state<FolderRef[] | null>(null);
  $effect(() => {
    if (confirmFolderDelete) return onBack(() => (confirmFolderDelete = null));
  });

  async function deleteFolderConfirmed() {
    const list = confirmFolderDelete;
    if (!list) return;
    deleting = true;
    try {
      const failed: string[] = [];
      for (const f of list) failed.push(...(await api.deleteFolder(f.sourceId, f.path)).failed);
      afterFolderChange();
      if (failed.length) alert(`${t("del.failed", { n: failed.length })}\n\n${failed.slice(0, 5).join("\n")}`);
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

  async function exportFolder(f: FolderRef) {
    return exportFolders([f]);
  }

  async function exportFolders(list: FolderRef[]) {
    const dest = await pickFolder(t("panel.exportTo"));
    if (!dest) return;
    for (const f of list) api.exportFolder(f.sourceId, f.path, dest).catch((e) => alert(asCmdError(e).message));
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
    if (embedded) return void (localPanel = on);
    ui().panel_visible = on;
    savePrefs();
  }

  // ---- save the current search
  /** The query as it would be saved: plain words become a name search, the header sort is kept. */
  const currentText = $derived.by(() => {
    let q = query.trim();
    if (q && !/[()]/.test(q)) q = `name(contains:${quoteParam(q)})`;
    if (sort) q = `${q ? q + " AND " : ""}${sort.desc ? "sort desc" : "sort asc"}(${sort.field})`;
    return q;
  });
  const canSaveCurrent = $derived(!!query.trim() && !queryError && !saved.some((q) => q.text === query));
  let savingCurrent = $state<{ label: string; error: string } | null>(null);
  $effect(() => {
    if (savingCurrent) return onBack(() => (savingCurrent = null));
  });
  async function commitSaveCurrent() {
    if (!savingCurrent) return;
    try {
      const id = await api.saveQuery(null, savingCurrent.label.trim(), currentText);
      savingCurrent = null;
      await refreshSaved();
      broadcast("saved");
      // Show the saved search as the active one (its text is the normalized form).
      const q = saved.find((x) => x.id === id);
      if (q) {
        sort = null;
        query = q.text;
      }
    } catch (e) {
      if (savingCurrent) savingCurrent.error = asCmdError(e).message;
    }
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
    if (compact && panelVisible) return onBack(() => setPanel(false));
  });
  $effect(() => {
    if (compact && (selection.set.size > 0 || selection.folders.length > 0)) return onBack(() => selection.clear());
  });
</script>

<svelte:window onkeydown={onKey} />

<div class="lib-root" class:embedded={!!embedded} bind:clientWidth={rootWidth}>
  <div
    class="app"
    class:compact
    style:--sidebar="{ui().sidebar_width}px"
    style:--panel="{panelVisible ? ui().panel_width : 0}px"
    class:no-panel={!panelVisible}
  >
    {#if drawer}
      <div class="drawer-scrim" role="presentation" transition:fade onclick={() => (drawer = false)}></div>
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
        {offline}
        cansavecurrent={canSaveCurrent}
        closeLabel={embedded ? t("side.closeSplit") : undefined}
        onsavecurrent={() => (savingCurrent = { label: "", error: "" })}
        {secondary}
        {shards}
        onselect={selectSource}
        onaddfolder={addFolder}
        onaddnetwork={() => (sourceDialog = { source: null })}
        onaddshard={() => (shardDialog = true)}
        onrescan={(s) => api.scanSource(s.id)}
        onedit={(s) => (s.driver === "shard" ? (shardPanel = s.id) : (sourceDialog = { source: s }))}
        onremove={removeSource}
      oncopy={hasShard ? (s) => startCopy([], [{ sourceId: s.id, path: "" }]) : undefined}
      onmirror={embedded ? undefined : (s) => (mirroring = s)}
        onrunsaved={(q) => (query = q.text)}
        oneditsaved={(q) => (modal = { editing: q })}
        ondeletesaved={deleteSaved}
        onsettings={() => (showSettings = true)}
        onpeople={embedded ? undefined : () => (showPeople = true)}
        ondedup={embedded ? undefined : () => (showDedup = true)}
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
        <div class="selbar" transition:slide={{ y: -8 }}>
          <button class="icon-btn" aria-label={t("sel.clear")} onclick={() => selection.clear()}><Icon name="x" size={16} /></button>
          <span class="grow tabular">{t("sel.count", { n: selection.set.size })}</span>
          <button class="btn sm ghost" onclick={() => selection.selectAll(ids)}><Icon name="checkSquare" size={14} />{t("sel.all")}</button>
          <button class="btn sm primary" onclick={() => setPanel(true)}><Icon name="info" size={14} />{t("sel.info")}</button>
          <button class="btn sm danger" onclick={() => (confirmDelete = [...selection.set])}><Icon name="trash" size={14} />{t("sel.delete")}</button>
        </div>
      {:else if compact && selection.folders.length > 0}
        <div class="selbar" transition:slide={{ y: -8 }}>
          <button class="icon-btn" aria-label={t("sel.clear")} onclick={() => selection.clear()}><Icon name="x" size={16} /></button>
          <span class="grow tabular">{t("sel.folders", { n: selection.folders.length })}</span>
          <button class="btn sm primary" onclick={() => setPanel(true)}><Icon name="info" size={14} />{t("sel.info")}</button>
          <button class="btn sm danger" onclick={() => (confirmFolderDelete = [...selection.folders])}><Icon name="trash" size={14} />{t("sel.delete")}</button>
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
            {#key decoderEpoch}
            <view.component
              {ids}
              {folders}
              {selection}
              {resetKey}
              {scrollKey}
              sections={sectionMarks}
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
            {/key}
          {/if}
        {/if}
      </div>
    </main>

    {#if panelVisible}
      <div class="panel" out:slide={{ y: compact ? 80 : 0, duration: compact ? undefined : 0 }}>
        <div class="resizer left" role="separator" aria-orientation="vertical" onpointerdown={(e) => drag(e, "panel")}></div>
        <RightPanel
          focus={selection.focus}
          selected={selectedIds}
          folders={selection.folders}
          onopen={(id) => openViewer(id, ids.includes(id) ? ids : [id])}
          onopenwindow={isMobile ? undefined : (id) => openViewerWindow(id, ids.includes(id) ? ids : [id])}
          onclose={compact ? () => setPanel(false) : undefined}
          onchanged={() => {
            // Folder edits may have reached every item inside.
            const changed = selection.folders.length ? undefined : selectedIds;
            invalidate(changed);
            broadcast("media", changed);
            runSearch(false);
          }}
        />
      </div>
    {/if}

    <div class="status">
      <HeifBanner
        oninstalled={() => {
          // Thumbnails that could not be made before get another try.
          invalidate();
          broadcast("media");
          decoderEpoch++;
        }}
      />
      <StatusBar count={ids.length} {elapsed} selected={selection.set.size} />
    </div>
  </div>
</div>

{#if savingCurrent}
  <div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (savingCurrent = null)}>
    <div out:pop class="dialog confirm" role="dialog" aria-modal="true" aria-labelledby="savecur-title">
      <header><h2 id="savecur-title">{t("side.saveCurrent.title")}</h2></header>
      <div class="body form">
        <p class="query-text">{currentText}</p>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input"
          placeholder={t("side.saveCurrent.name")}
          bind:value={savingCurrent.label}
          autofocus
          onkeydown={(e) => e.key === "Enter" && savingCurrent?.label.trim() && commitSaveCurrent()}
        />
        {#if savingCurrent.error}<p class="err">{savingCurrent.error}</p>{/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" onclick={() => (savingCurrent = null)}>{t("common.cancel")}</button>
        <button class="btn primary" disabled={!savingCurrent.label.trim()} onclick={commitSaveCurrent}>{t("common.save")}</button>
      </footer>
    </div>
  </div>
{/if}

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

{#if selection.folderMenu && selection.folders.length}
  <ContextMenu
    x={selection.folderMenu.x}
    y={selection.folderMenu.y}
    items={selection.folder ? folderMenuItems : foldersMenuItems}
    onclose={() => (selection.folderMenu = null)}
  />
{/if}

{#if selection.menu && selection.set.size > 0}
  <ContextMenu x={selection.menu.x} y={selection.menu.y} items={menuItems} onclose={() => (selection.menu = null)} />
{/if}

{#if renaming}
  <div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (renaming = null)}>
    <div out:pop class="dialog confirm" role="dialog" aria-modal="true" aria-labelledby="ren-title">
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

{#if copying}
  <MoveDialog
    sourceId={copying.targets[0].id}
    sourceName={copying.targets[0].name}
    start=""
    targets={copying.targets}
    title={t("copy.title")}
    action={t("copy.here")}
    onpick={commitCopy}
    onclose={() => (copying = null)}
  />
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
  <div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !deleting && (confirmDelete = null)}>
    <div out:pop class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="del-title">
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
  {@const list = confirmFolderDelete}
  {@const n = list.reduce((a, f) => a + f.count, 0).toLocaleString()}
  <div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !deleting && (confirmFolderDelete = null)}>
    <div out:pop class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="fdel-title">
      <header><h2 id="fdel-title">{t("folder.delTitle")}</h2></header>
      <div class="body"><p class="text">
          {list.length === 1 ? t("folder.delBody", { name: baseName(list[0].path), n }) : t("folder.delBodyMany", { folders: list.length, n })}
        </p></div>
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

{#if !embedded}
  <CopyConflict />
{/if}

{#if mirroring}
  <MirrorDialog source={mirroring} {sources} onclose={() => (mirroring = null)} />
{/if}

{#if showDedup}
  <DedupDialog {sources} initial={selected} onchange={() => runSearch(false)} onclose={() => (showDedup = false)} />
{/if}

{#if showPeople}
  <PeopleView
    hidden={viewing != null}
    onopen={openViewer}
    onreveal={revealInLibrary}
    onclose={() => (showPeople = false)}
    onshow={(q) => {
      // A person's photos: everything with their name, in every source.
      selected = null;
      lastSource = null;
      folder = "";
      sort = null;
      query = q;
      showPeople = false;
    }}
  />
{/if}

{#if viewing && settings}
  <ViewerShell
    panes={[{ ids: viewing.list, index: viewing.index }]}
    {settings}
    {bundle}
    browseFrom={browseState()}
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
  .lib-root {
    height: 100vh;
  }
  .lib-root.embedded {
    height: 100%;
  }
  .app {
    display: grid;
    grid-template-columns: var(--sidebar) minmax(0, 1fr) var(--panel);
    grid-template-rows: minmax(0, 1fr) auto;
    height: 100%;
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
  .confirm .query-text {
    margin: 0;
    font-family: var(--font-editor);
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
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
  .app.compact {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) auto;
  }
  .compact .sidebar {
    position: fixed;
    top: 0;
    bottom: 0;
    left: 0;
    width: min(320px, 86%);
    z-index: 35;
    transform: translateX(-102%);
    transition: transform var(--t-med) var(--ease);
  }
  .compact .sidebar.open {
    transform: none;
    box-shadow: var(--shadow);
  }
  .compact .panel {
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
  .compact .resizer {
    display: none;
  }
  @keyframes vp-sheet {
    from {
      transform: translateY(30%);
      opacity: 0;
    }
  }
</style>
