<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { api, platformApi, type BundleSummary, type Settings } from "./api";
  import Library, { type BrowseState } from "./Library.svelte";
  import { toggleMute } from "./audio.svelte";
  import { isTyping, matches } from "./keys";
  import { isMobile } from "./platform";
  import { prefs } from "./prefs.svelte";
  import Viewer from "./Viewer.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { fullscreenChanged, resetImmersive } from "./immersive.svelte";

  /**
   * Viewer split view: up to `windows.max_viewer_panes` panes side by side, each browsing
   * its own list. A new pane starts as an explorer, so it can show another source or
   * query; opening an item there turns it into a viewer. On mobile this replaces extra
   * windows; on desktop a pane can also be moved to its own window.
   */
  type Pane = {
    key: number;
    ids: number[];
    index: number;
    /** Showing an explorer (with this state) instead of the viewer. */
    browse: BrowseState | null;
  };

  let {
    panes: initial,
    settings,
    bundle: givenBundle,
    browseFrom,
    onclose,
    onchanged,
  }: {
    panes: { ids: number[]; index: number }[];
    settings: Settings;
    /** The open workspace; looked up when absent (viewer windows). */
    bundle?: BundleSummary;
    /** Where a new explorer pane starts. */
    browseFrom?: BrowseState;
    /** Called with the item the last active pane showed. */
    onclose: (lastId: number | undefined) => void;
    onchanged?: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let bundle = $state<BundleSummary | null>(givenBundle ?? null);
  // svelte-ignore state_referenced_locally
  if (!givenBundle) api.currentBundle().then((b) => (bundle = b)).catch(() => {});

  let nextKey = 0;
  let panes = $state<Pane[]>(untrack(() => initial.map((p) => ({ key: nextKey++, ...p, browse: null }))));
  let active = $state(0);
  let width = $state(0);
  let height = $state(0);

  const max = $derived(Math.min(3, Math.max(1, prefs.app?.windows.max_viewer_panes ?? 3)));
  const layout = $derived.by(() => {
    const l = prefs.app?.windows.split_layout ?? "auto";
    return l === "auto" ? (width >= height ? "columns" : "rows") : l;
  });

  /** Where a new explorer pane starts. */
  const start = (): BrowseState => ({ sourceId: null, folder: "", query: "", ...browseFrom });

  function split() {
    if (panes.length >= max) return;
    const p = panes[active];
    // The new pane starts as an explorer; until something is opened there it keeps the
    // active pane's list.
    panes.push({ key: nextKey++, ids: p.ids, index: p.index, browse: start() });
    active = panes.length - 1;
  }

  function remove(i: number) {
    if (panes.length === 1) return onclose(panes[0].ids[panes[0].index]);
    panes.splice(i, 1);
    active = Math.min(active, panes.length - 1);
  }

  /** Opened from a pane's explorer: the pane views that list. */
  function openIn(i: number, ids: number[], index: number, state: BrowseState) {
    const p = panes[i];
    p.ids = ids;
    p.index = index;
    p.browse = null;
    lastBrowse.set(p.key, state);
  }
  /** Where each pane's explorer was, to return there. */
  const lastBrowse = new Map<number, BrowseState>();

  async function pop(i: number) {
    const p = panes[i];
    await api.openWindow({ kind: "viewer", ids: p.ids, index: p.index }).catch(() => {});
    remove(i);
  }

  function onKey(e: KeyboardEvent) {
    if (isTyping(e)) return;
    const k = settings.keys;
    if (matches(e, k.toggle_mute)) toggleMute();
    else if (matches(e, k.split_view)) split();
    else if (matches(e, k.next_pane) && panes.length > 1) active = (active + 1) % panes.length;
    else return;
    e.preventDefault();
  }

  // Fullscreen entered any way (viewer button, OS control) turns immersive mode on.
  let unlistenResize: (() => void) | undefined;
  onMount(() => {
    window.addEventListener("keydown", onKey);
    // Mobile fullscreen hides the system bars (Viewer), which the window does not report.
    if (isMobile) return;
    try {
      const win = getCurrentWindow();
      const check = () => win.isFullscreen().then(fullscreenChanged).catch(() => {});
      check();
      win.onResized(check).then((f) => (unlistenResize = f));
    } catch {
      // Not running in Tauri.
    }
  });
  onDestroy(() => {
    window.removeEventListener("keydown", onKey);
    unlistenResize?.();
    resetImmersive();
    if (isMobile) platformApi.setSystemFullscreen(false).catch(() => {});
  });
</script>

<div
  class="shell"
  class:rows={layout === "rows"}
  bind:clientWidth={width}
  bind:clientHeight={height}
  style:--n={panes.length}
  role="dialog"
  aria-modal="true"
>
  {#each panes as p, i (p.key)}
    <div class="pane" class:browse={!!p.browse} onpointerdowncapture={() => (active = i)} role="presentation">
      {#if p.browse}
        {#if bundle}
          <Library
            {bundle}
            secondary
            initial={p.browse}
            onclose={() => remove(i)}
            onbundle={() => {}}
            embedded={{ active: i === active, onopen: (ids, index, state) => openIn(i, ids, index, state) }}
          />
        {/if}
      {:else}
        <Viewer
          ids={p.ids}
          bind:index={p.index}
          {settings}
          {onchanged}
          active={i === active}
          panes={panes.length}
          onactivate={() => (active = i)}
          onclose={() => remove(i)}
          onsplit={panes.length < max ? split : undefined}
          onpop={isMobile ? undefined : () => pop(i)}
          onbrowse={panes.length > 1 ? () => (p.browse = lastBrowse.get(p.key) ?? start()) : undefined}
        />
      {/if}
    </div>
  {/each}
</div>

<style>
  .shell {
    position: fixed;
    inset: 0;
    z-index: 20;
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    gap: 2px;
    background: #000;
  }
  .shell.rows {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: repeat(var(--n), minmax(0, 1fr));
  }
  .pane {
    position: relative;
    min-width: 0;
    min-height: 0;
  }
  /* An explorer pane: its fixed layers (drawer, sheets, dialogs) stay inside the pane. */
  .pane.browse {
    transform: translateZ(0);
    overflow: hidden;
    background: var(--bg);
    color: var(--fg);
  }
</style>
