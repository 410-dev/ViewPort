<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { api, platformApi, type BundleSummary, type Settings } from "./api";
  import Library, { type BrowseState } from "./Library.svelte";
  import { toggleMute } from "./audio.svelte";
  import { isTyping, matches } from "./keys";
  import { isMobile } from "./platform";
  import { prefs } from "./prefs.svelte";
  import { t } from "./i18n.svelte";
  import Viewer from "./Viewer.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { fullscreenChanged, resetImmersive } from "./immersive.svelte";

  /**
   * Viewer split view: up to `windows.max_viewer_panes` panes side by side, each browsing
   * its own list. A new pane starts as an explorer, so it can show another source or
   * query; opening an item there turns it into a viewer. On mobile this replaces extra
   * windows; on desktop a pane can also be moved to its own window.
   *
   * Dividers between panes are dragged to resize them. A pane dragged past
   * `switch_at_percent` enters switching mode: it takes `switch_main_percent`, the others
   * fold to a strip showing the edge of their (full-size) picture, and tapping a strip
   * swaps the two, sliding one aside as the other comes in.
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

  /** Share of the split length per pane (sums to 1). */
  let sizes = $state<number[]>(untrack(() => initial.map(() => 1 / initial.length)));
  /** Switching mode: the large pane, or null. */
  let main = $state<number | null>(null);
  /** The large pane before the last switch: the one sliding aside. */
  let prevMain = $state<number | null>(null);
  let animating = $state(false);
  /**
   * While a divider is dragged: each pane's size when the drag started. Panes keep that
   * layout and are scaled to their slot instead of being laid out again every frame, which
   * made the WebView decode and rasterize large pictures over and over (flicker).
   */
  let frozen = $state<{ w: number; h: number }[] | null>(null);
  const inners: HTMLDivElement[] = [];
  let animTimer: ReturnType<typeof setTimeout> | undefined;

  const win = $derived(prefs.app?.windows);
  const switchAt = $derived((win?.switch_at_percent ?? 90) / 100);
  const mainShare = $derived((win?.switch_main_percent ?? 95) / 100);
  const animMs = $derived(win?.switch_anim_ms ?? 280);

  const max = $derived(Math.min(3, Math.max(1, prefs.app?.windows.max_viewer_panes ?? 3)));
  const layout = $derived.by(() => {
    const l = prefs.app?.windows.split_layout ?? "auto";
    return l === "auto" ? (width >= height ? "columns" : "rows") : l;
  });

  const cols = $derived(layout === "columns");
  const DIVIDER = 4;
  /** Length shared by the panes (without the dividers). */
  const avail = $derived(Math.max(1, (cols ? width : height) - DIVIDER * (panes.length - 1)));

  function equalSizes() {
    sizes = panes.map(() => 1 / panes.length);
    main = prevMain = null;
  }

  function animate() {
    animating = true;
    clearTimeout(animTimer);
    animTimer = setTimeout(() => (animating = false), animMs + 40);
  }

  /** Switching mode with pane `i` large. */
  function switchTo(i: number) {
    if (panes.length < 2) return;
    const rest = (1 - mainShare) / (panes.length - 1);
    prevMain = main ?? i;
    animate();
    main = i;
    active = i;
    sizes = panes.map((_, j) => (j === i ? mainShare : rest));
  }

  /** Drags the divider after pane `i`; past the threshold, switching mode starts. */
  function drag(e: PointerEvent, i: number) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    e.preventDefault();
    e.stopPropagation();
    frozen = panes.map((_, j) => {
      const r = inners[j]?.getBoundingClientRect();
      return { w: r?.width ?? 0, h: r?.height ?? 0 };
    });
    main = prevMain = null;
    animating = false;
    const from = cols ? e.clientX : e.clientY;
    const a0 = sizes[i];
    const pair = a0 + sizes[i + 1];
    const min = Math.min(0.02, pair / 2);
    const move = (ev: PointerEvent) => {
      const a = Math.min(pair - min, Math.max(min, a0 + ((cols ? ev.clientX : ev.clientY) - from) / avail));
      sizes[i] = a;
      sizes[i + 1] = pair - a;
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      frozen = null;
      const big = sizes.findIndex((x) => x > switchAt);
      if (big >= 0) switchTo(big);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  /**
   * Switching mode: every pane is drawn at the large size, clipped by its slot and held
   * against the side facing the large pane, so a folding pane looks pushed aside.
   */
  function anchor(i: number): "start" | "end" | null {
    if (main === null) return null;
    if (i !== main) return i > main ? "start" : "end";
    return prevMain !== null && prevMain > i ? "end" : "start";
  }

  /** Scale of a frozen pane in its current slot (never enlarged: the rest stays black). */
  function frozenScale(i: number) {
    const f = frozen?.[i];
    if (!f?.w || !f.h) return 1;
    const slot = (sizes[i] ?? 0) * avail;
    const [w, h] = cols ? [slot, height] : [width, slot];
    return Math.min(1, w / f.w, h / f.h);
  }

  /** Where a new explorer pane starts. */
  const start = (): BrowseState => ({ sourceId: null, folder: "", query: "", ...browseFrom });

  function split() {
    if (panes.length >= max) return;
    const p = panes[active];
    // The new pane starts as an explorer; until something is opened there it keeps the
    // active pane's list.
    panes.push({ key: nextKey++, ids: p.ids, index: p.index, browse: start() });
    active = panes.length - 1;
    equalSizes();
  }

  function remove(i: number) {
    if (panes.length === 1) return onclose(panes[0].ids[panes[0].index]);
    panes.splice(i, 1);
    active = Math.min(active, panes.length - 1);
    equalSizes();
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
    clearTimeout(animTimer);
    resetImmersive();
    if (isMobile) platformApi.setSystemFullscreen(false).catch(() => {});
  });
  /** Pull-to-close progress of the (single) pane: the library shows through as it grows. */
  let pulled = $state(0);
</script>

<div
  class="shell"
  class:rows={!cols}
  class:animating
  bind:clientWidth={width}
  bind:clientHeight={height}
  style:--anim="{animMs}ms"
  style:background={pulled > 0 ? "transparent" : undefined}
  role="dialog"
  aria-modal="true"
>
  {#each panes as p, i (p.key)}
    {@const side = anchor(i)}
    {#if i > 0}
      <div
        class="divider"
        role="separator"
        aria-orientation={cols ? "vertical" : "horizontal"}
        onpointerdown={(e) => drag(e, i - 1)}
        ondblclick={equalSizes}
      ></div>
    {/if}
    <div class="pane" style:flex-grow={sizes[i] ?? 1} onpointerdowncapture={() => (active = i)} role="presentation">
      <div
        class="inner"
        bind:this={inners[i]}
        class:browse={!!p.browse}
        class:start={side === "start"}
        class:end={side === "end"}
        class:frozen={!!frozen?.[i]?.w}
        style:--big="{avail * mainShare}px"
        style:--fw="{frozen?.[i]?.w ?? 0}px"
        style:--fh="{frozen?.[i]?.h ?? 0}px"
        style:--fs={frozenScale(i)}
      >
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
            onpull={(v) => (pulled = v)}
          />
        {/if}
      </div>
      {#if main !== null && i !== main}
        <!-- A folded pane: a tap swaps it in. -->
        <button class="peek" class:end={side === "end"} aria-label={t("viewer.switchPane")} onclick={() => switchTo(i)}></button>
      {/if}
    </div>
  {/each}
</div>

<style>
  .shell {
    position: fixed;
    inset: 0;
    z-index: 20;
    display: flex;
    background: #000;
  }
  .shell.rows {
    flex-direction: column;
  }
  .pane {
    position: relative;
    flex: 1 1 0px;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .shell.animating .pane {
    transition: flex-grow var(--anim) cubic-bezier(0.3, 0, 0.1, 1);
  }
  .inner {
    position: absolute;
    inset: 0;
  }
  /* Switching mode: drawn at the large size, against the side facing the large pane. */
  .inner.start,
  .inner.end {
    width: var(--big);
  }
  .inner.start {
    right: auto;
  }
  .inner.end {
    left: auto;
  }
  .rows .inner.start,
  .rows .inner.end {
    width: auto;
    height: var(--big);
  }
  .rows .inner.start {
    right: 0;
    bottom: auto;
  }
  .rows .inner.end {
    left: 0;
    top: auto;
  }
  /* An explorer pane: its fixed layers (drawer, sheets, dialogs) stay inside the pane. */
  .inner.browse {
    transform: translateZ(0);
    overflow: hidden;
    background: var(--bg);
    color: var(--fg);
  }
  /* Dragging a divider: the size from the drag start, scaled into the slot. */
  .inner.frozen {
    inset: auto;
    left: 50%;
    top: 50%;
    width: var(--fw);
    height: var(--fh);
    transform: translate(-50%, -50%) scale(var(--fs));
    will-change: transform;
  }
  .peek {
    position: absolute;
    inset: 0;
    z-index: 30;
    padding: 0;
    border: 0;
    background: linear-gradient(to right, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
    cursor: pointer;
    transition: background var(--t-fast) var(--ease);
  }
  .peek.end {
    background: linear-gradient(to left, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
  }
  .rows .peek {
    background: linear-gradient(to bottom, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
  }
  .rows .peek.end {
    background: linear-gradient(to top, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
  }
  .peek:hover {
    background: rgb(255 255 255 / 0.08);
  }
  .divider {
    position: relative;
    z-index: 40;
    flex: 0 0 4px;
    background: #000;
    cursor: col-resize;
    touch-action: none;
  }
  .rows > .divider {
    cursor: row-resize;
  }
  /* A wider grip than the visible line (a finger needs more). */
  .divider::before {
    content: "";
    position: absolute;
    inset: 0 -6px;
  }
  .rows > .divider::before {
    inset: -6px 0;
  }
  .divider::after {
    content: "";
    position: absolute;
    inset: 0 1px;
    border-radius: 1px;
    background: #2b2f39;
    transition: background var(--t-fast) var(--ease);
  }
  .rows > .divider::after {
    inset: 1px 0;
  }
  .divider:hover::after,
  .divider:active::after {
    background: var(--accent-line);
  }
  @media (pointer: coarse) {
    .divider::before {
      inset: 0 -12px;
    }
    .rows > .divider::before {
      inset: -12px 0;
    }
  }
</style>
