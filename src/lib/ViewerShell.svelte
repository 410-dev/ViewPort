<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { api, platformApi, type BundleSummary, type Settings } from "./api";
  import Library, { type BrowseState, type Listing } from "./Library.svelte";
  import { toggleMute } from "./audio.svelte";
  import { isTyping, matches } from "./keys";
  import { isMobile } from "./platform";
  import { prefs } from "./prefs.svelte";
  import { t } from "./i18n.svelte";
  import Viewer from "./Viewer.svelte";
  import { pop as popIn, reduced, zoom } from "./ui/motion";
  import { dismiss } from "./ui/dismiss";
  import { onBack } from "./back.svelte";
  import Icon from "./ui/Icon.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { fullscreenChanged, resetImmersive } from "./immersive.svelte";

  /**
   * Viewer split view: up to `windows.max_viewer_panes` panes side by side, each browsing
   * its own list. A new pane starts as an explorer, so it can show another source or
   * query; opening an item there turns it into a viewer. On mobile this replaces extra
   * windows; on desktop a pane can also be moved to its own window.
   *
   * Dividers between panes are dragged to resize them; dragged so far that one of the two
   * panes keeps less than `100 - switch_at_percent` percent of their length, that pane
   * closes. The "…" button on a divider opens its menu: swap the two panes, equal sizes,
   * close either side. Panes stay in `panes` (and in the DOM) where they were added and
   * are placed by CSS `order` from `slots`, so a swap moves no element: a playing video
   * keeps playing.
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
    browseSeed,
    onclose,
    onchanged,
  }: {
    panes: { ids: number[]; index: number }[];
    settings: Settings;
    /** The open workspace; looked up when absent (viewer windows). */
    bundle?: BundleSummary;
    /** Where a new explorer pane starts. */
    browseFrom?: BrowseState;
    /** The opener's listing for `browseFrom`, so a new explorer need not search again. */
    browseSeed?: Listing;
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
  /** Pane indices in the order they are shown. */
  let slots = $state<number[]>(untrack(() => initial.map((_, i) => i)));
  let active = $state(0);
  let width = $state(0);
  let height = $state(0);

  /** Share of the split length per pane (sums to 1), by pane index. */
  let sizes = $state<number[]>(untrack(() => initial.map(() => 1 / initial.length)));
  let animating = $state(false);
  /**
   * While a divider is dragged: each pane's size when the drag started. Panes keep that
   * layout and are scaled to their slot instead of being laid out again every frame, which
   * made the WebView decode and rasterize large pictures over and over (flicker).
   */
  let frozen = $state<{ w: number; h: number }[] | null>(null);
  /** The pane a divider drag would close if let go now. */
  let closing = $state<number | null>(null);
  /** A pane on its way out: kept at this size, clipped by its shrinking slot. */
  let leaving = $state<{ i: number; w: number; h: number; end: boolean } | null>(null);
  /** The divider (by slot) whose menu is open. */
  let menuAt = $state<number | null>(null);
  const inners: HTMLDivElement[] = [];
  const paneEls: HTMLDivElement[] = [];
  let shell = $state<HTMLDivElement>();
  let animTimer: ReturnType<typeof setTimeout> | undefined;

  const win = $derived(prefs.app?.windows);
  /** A pane closes once a drag leaves it less than this share of the two panes' length. */
  const closeBelow = $derived(1 - (win?.switch_at_percent ?? 90) / 100);
  const animMs = $derived(reduced() ? 0 : (win?.switch_anim_ms ?? 280));

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
  }

  /** Turns the size transition on, in the styles, before anything changes size. */
  async function animate() {
    animating = true;
    clearTimeout(animTimer);
    animTimer = setTimeout(() => (animating = false), animMs + 40);
    await tick();
    void shell?.offsetHeight;
  }

  /** Drags the divider after slot `k`; let go past the threshold, the smaller pane closes. */
  function drag(e: PointerEvent, k: number) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (leaving) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    e.preventDefault();
    e.stopPropagation();
    menuAt = null;
    frozen = panes.map((_, j) => {
      const r = inners[j]?.getBoundingClientRect();
      return { w: r?.width ?? 0, h: r?.height ?? 0 };
    });
    animating = false;
    const [a, b] = [slots[k], slots[k + 1]];
    const from = cols ? e.clientX : e.clientY;
    const a0 = sizes[a];
    const pair = a0 + sizes[b];
    const move = (ev: PointerEvent) => {
      const x = Math.min(pair, Math.max(0, a0 + ((cols ? ev.clientX : ev.clientY) - from) / avail));
      sizes[a] = x;
      sizes[b] = pair - x;
      closing = x < pair * closeBelow ? a : pair - x < pair * closeBelow ? b : null;
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      frozen = null;
      const gone = closing;
      closing = null;
      if (gone !== null) collapse(gone, gone === a ? b : a);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  /** Pane `i` shrinks away into the divider, its neighbour `into` taking its length, and closes. */
  async function collapse(i: number, into: number) {
    if (leaving) return;
    const r = inners[i]?.getBoundingClientRect();
    // Held at its size and against the far side, so it looks pushed off rather than squeezed.
    leaving = { i, w: r?.width ?? 0, h: r?.height ?? 0, end: slots.indexOf(i) < slots.indexOf(into) };
    await animate();
    sizes[into] += sizes[i];
    sizes[i] = 0;
    setTimeout(() => {
      leaving = null;
      remove(i);
    }, animMs);
  }

  /** Swaps the panes on both sides of divider `k`, sliding each into the other's place. */
  async function swap(k: number) {
    menuAt = null;
    const moved = [slots[k], slots[k + 1]];
    const before = moved.map((i) => paneEls[i]?.getBoundingClientRect());
    [slots[k], slots[k + 1]] = [slots[k + 1], slots[k]];
    await tick();
    if (!animMs) return;
    moved.forEach((i, n) => {
      const el = paneEls[i];
      const was = before[n];
      if (!el || !was) return;
      const now = el.getBoundingClientRect();
      el.style.transition = "none";
      el.style.transform = `translate(${was.left - now.left}px, ${was.top - now.top}px)`;
      void el.offsetWidth;
      el.style.transition = `transform ${animMs}ms cubic-bezier(0.3, 0, 0.1, 1)`;
      el.style.transform = "";
      setTimeout(() => (el.style.transition = ""), animMs + 40);
    });
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
    slots.push(panes.length - 1);
    active = panes.length - 1;
    menuAt = null;
    equalSizes();
  }

  /** Closes pane `i`; the others keep their proportions. */
  function remove(i: number) {
    if (panes.length === 1) return onclose(panes[0].ids[panes[0].index]);
    panes.splice(i, 1);
    slots = slots.filter((x) => x !== i).map((x) => (x > i ? x - 1 : x));
    const rest = sizes.filter((_, j) => j !== i);
    const sum = rest.reduce((a, b) => a + b, 0);
    sizes = rest.map((x) => (sum > 0 ? x / sum : 1 / rest.length));
    if (active === i) active = slots[0];
    else if (active > i) active--;
    menuAt = null;
  }

  /** Closes the whole viewer (the close button): back to the library, at pane `i`'s item. */
  function closeAll(i: number) {
    const p = panes[i];
    onclose(p.browse ? undefined : p.ids[p.index]);
  }

  /** Item `id` was deleted: every pane drops it and shows the item after it (or the one
   * before, at the end). A pane left with nothing closes. */
  function dropItem(id: number) {
    for (let i = panes.length - 1; i >= 0; i--) {
      const p = panes[i];
      const at = p.ids.indexOf(id);
      if (at < 0) continue;
      const ids = p.ids.filter((x) => x !== id);
      if (!ids.length) {
        if (panes.length === 1) return onclose(undefined);
        remove(i);
        continue;
      }
      p.ids = ids;
      if (p.index > at || p.index >= ids.length) p.index = Math.min(Math.max(0, p.index - (p.index > at ? 1 : 0)), ids.length - 1);
    }
  }

  /** Opened from a pane's explorer: the pane views that list. */
  function openIn(i: number, ids: number[], index: number) {
    const p = panes[i];
    p.ids = ids;
    p.index = index;
    p.browse = null;
  }

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
    else if (matches(e, k.next_pane) && panes.length > 1) active = slots[(slots.indexOf(active) + 1) % slots.length];
    else return;
    e.preventDefault();
  }

  $effect(() => {
    if (menuAt !== null) return onBack(() => (menuAt = null));
  });

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
  bind:this={shell}
  transition:zoom
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
    {@const slot = slots.indexOf(i)}
    <div
      class="pane"
      bind:this={paneEls[i]}
      class:closing={closing === i}
      style:order={slot * 2}
      style:flex-grow={sizes[i] ?? 1}
      onpointerdowncapture={() => (active = i)}
      role="presentation"
    >
      <div
        class="inner"
        bind:this={inners[i]}
        class:browse={!!p.browse}
        class:leaving={leaving?.i === i}
        class:end={leaving?.i === i && leaving.end}
        class:frozen={!!frozen?.[i]?.w}
        style:--lw="{leaving?.w ?? 0}px"
        style:--lh="{leaving?.h ?? 0}px"
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
              seed={browseSeed}
              onclose={() => remove(i)}
              onbundle={() => {}}
              embedded={{ active: i === active, onopen: (ids, index) => openIn(i, ids, index) }}
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
            onclose={() => closeAll(i)}
            onsplit={panes.length < max ? split : undefined}
            onpop={isMobile ? undefined : () => pop(i)}
            onpull={(v) => (pulled = v)}
            onremoved={dropItem}
          />
        {/if}
      </div>
    </div>
  {/each}
  {#each slots.slice(1) as _, k (k)}
    <div
      class="divider"
      style:order={k * 2 + 1}
      role="separator"
      aria-orientation={cols ? "vertical" : "horizontal"}
      onpointerdown={(e) => drag(e, k)}
      ondblclick={equalSizes}
    >
      <button
        class="more"
        class:on={menuAt === k}
        aria-label={t("viewer.paneMenu")}
        aria-expanded={menuAt === k}
        onpointerdown={(e) => e.stopPropagation()}
        ondblclick={(e) => e.stopPropagation()}
        onclick={() => (menuAt = menuAt === k ? null : k)}
      >
        <Icon name="dots" size={16} />
      </button>
      {#if menuAt === k}
        <div class="menu-at">
        <div
          class="menu"
          role="menu"
          tabindex="-1"
          transition:popIn={{ y: -6 }}
          onpointerdown={(e) => e.stopPropagation()}
          use:dismiss={{ onclose: () => (menuAt = null), keep: () => [shell?.querySelector(".more.on")] }}
        >
          <button class="btn sm ghost" role="menuitem" onclick={() => swap(k)}>
            <Icon name="swap" size={14} />{t("viewer.swapPanes")}
          </button>
          <button
            class="btn sm ghost"
            role="menuitem"
            onclick={() => {
              menuAt = null;
              animate().then(equalSizes);
            }}
          >
            <Icon name={cols ? "columns" : "rows"} size={14} />{t("viewer.equalPanes")}
          </button>
          <button class="btn sm ghost" role="menuitem" onclick={() => ((menuAt = null), collapse(slots[k], slots[k + 1]))}>
            <Icon name="x" size={14} />{t(cols ? "viewer.closeLeft" : "viewer.closeTop")}
          </button>
          <button class="btn sm ghost" role="menuitem" onclick={() => ((menuAt = null), collapse(slots[k + 1], slots[k]))}>
            <Icon name="x" size={14} />{t(cols ? "viewer.closeRight" : "viewer.closeBottom")}
          </button>
        </div>
        </div>
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
  /* Closing: held at its size against the far side while its slot shrinks. */
  .inner.leaving {
    width: var(--lw);
    height: var(--lh);
    right: auto;
    bottom: auto;
    transition: opacity var(--anim) ease-in;
    opacity: 0.3;
  }
  .inner.leaving.end {
    left: auto;
    right: 0;
  }
  .rows .inner.leaving.end {
    top: auto;
    bottom: 0;
    left: 0;
  }
  /* A drag let go now would close this pane. */
  .pane.closing::after {
    content: "";
    position: absolute;
    inset: 0;
    z-index: 30;
    background: rgb(0 0 0 / 0.55);
    pointer-events: none;
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
  /* The divider's menu button, on the middle of the line. */
  .more {
    position: absolute;
    z-index: 2;
    left: 50%;
    top: 50%;
    width: 22px;
    height: 34px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 1px solid #2b2f39;
    border-radius: var(--radius-pill);
    background: #14161c;
    color: #c8ccd6;
    cursor: pointer;
    transform: translate(-50%, -50%);
    opacity: 0.75;
    transition:
      opacity var(--t-fast) var(--ease),
      background var(--t-fast) var(--ease);
  }
  .more :global(svg) {
    transform: rotate(90deg);
  }
  .rows .more {
    width: 34px;
    height: 22px;
  }
  .rows .more :global(svg) {
    transform: none;
  }
  .more:hover,
  .more.on {
    opacity: 1;
    background: #1d2029;
  }
  .menu-at {
    position: absolute;
    z-index: 3;
    left: 50%;
    top: 50%;
    transform: translate(-50%, 30px);
  }
  .rows .menu-at {
    transform: translate(-50%, 16px);
  }
  .menu {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 2px;
    padding: 6px;
    min-width: max-content;
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    background: var(--surface);
    color: var(--fg);
    box-shadow: var(--shadow);
    cursor: default;
  }
  .menu .btn {
    justify-content: flex-start;
  }
  @media (pointer: coarse) {
    .more {
      width: 28px;
      height: 44px;
    }
    .rows .more {
      width: 44px;
      height: 28px;
    }
    .divider::before {
      inset: 0 -12px;
    }
    .rows > .divider::before {
      inset: -12px 0;
    }
  }
</style>
