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
  import { groups } from "./groups.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { fullscreenChanged, resetImmersive } from "./immersive.svelte";

  /**
   * Viewer split view: up to `windows.max_viewer_panes` panes side by side, each browsing
   * its own list. A new pane starts as an explorer, so it can show another source or
   * query; opening an item there turns it into a viewer. On mobile this replaces extra
   * windows; on desktop a pane can also be moved to its own window.
   *
   * Dividers between panes are dragged to resize them. A pane dragged past
   * `switch_at_percent` of the window enters switching mode: it takes `switch_main_percent`,
   * the others fold to a strip showing the edge of their (full-size) picture, and tapping a
   * strip swaps the two, sliding one aside as the other comes in. Dragged nearly to the end,
   * so that one of the two panes keeps less than `100 - close_at_percent` percent of their
   * length, that pane closes instead. The "…" button on a divider opens its menu: swap the two panes, equal sizes,
   * turn that part the other way, close either side. Panes stay in `panes` (and in the DOM)
   * where they were added and are placed absolutely from `tiles`, so a swap or a turn moves
   * no element: a playing video keeps playing.
   */
  type Pane = {
    key: number;
    ids: number[];
    index: number;
    /** Showing an explorer (with this state) instead of the viewer. */
    browse: BrowseState | null;
    /** The temporary group the list is (by id), if any. */
    group?: string | null;
    /** The explorer the viewer was opened from in this pane, to go back to when it closes. */
    back?: BrowseState | null;
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
    panes: { ids: number[]; index: number; group?: string | null }[];
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

  /**
   * The layout: tiles along the split direction `dir`. A tile holds one pane, or two
   * stacked across `dir` (one long pane beside two short ones). Panes are named by their
   * `key`, so closing one renumbers nothing here. `size` is the tile's share of the split
   * length (the tiles' sizes sum to 1); `inner` the shares of its panes across it.
   */
  type Tile = { tid: number; size: number; keys: number[]; inner: number[] };
  type Dir = "columns" | "rows";
  let nextTid = 0;
  let tiles = $state<Tile[]>(untrack(() => panes.map((p) => ({ tid: nextTid++, size: 1 / panes.length, keys: [p.key], inner: [1] }))));
  /** Chosen when the split opens (by the window's shape when `auto`); it stays until changed
   * from a divider menu, however the window turns. */
  let dir = $state<Dir | null>(null);
  let active = $state(0);
  let width = $state(0);
  let height = $state(0);

  /** Switching mode: the large tile (`tid`), or null. */
  let main = $state<number | null>(null);
  /** The large tile before the last switch: the one sliding aside. */
  let prevMain = $state<number | null>(null);
  let animating = $state(false);
  /**
   * While a divider is dragged: each pane's size (by key) when the drag started. Panes keep
   * that layout and are scaled to their slot instead of being laid out again every frame,
   * which made the WebView decode and rasterize large pictures over and over (flicker).
   */
  let frozen = $state<Map<number, { w: number; h: number }> | null>(null);
  /** The panes (keys) a divider drag would close if let go now. */
  let closing = $state<number[]>([]);
  /** Panes on their way out: kept at their size, clipped by their shrinking slot, against
   * the far side (`end`) along `rows` (a vertical shrink). */
  let leaving = $state<{ keys: number[]; size: Map<number, { w: number; h: number }>; end: boolean; rows: boolean } | null>(null);
  /** The divider (by id) whose menu is open. */
  let menuAt = $state<string | null>(null);
  const inners: Record<number, HTMLDivElement> = {};
  const paneEls: Record<number, HTMLDivElement> = {};
  let shell = $state<HTMLDivElement>();
  let animTimer: ReturnType<typeof setTimeout> | undefined;

  const win = $derived(prefs.app?.windows);
  const switchAt = $derived((win?.switch_at_percent ?? 90) / 100);
  const mainShare = $derived((win?.switch_main_percent ?? 95) / 100);
  /** A pane closes once a drag leaves it less than this share of the two panes' length. */
  const closeBelow = $derived(1 - (win?.close_at_percent ?? 98) / 100);
  const animMs = $derived(reduced() ? 0 : (win?.switch_anim_ms ?? 280));

  const max = $derived(Math.min(3, Math.max(1, prefs.app?.windows.max_viewer_panes ?? 3)));

  /** The direction a split opening now takes. */
  function autoDir(): Dir {
    const l = prefs.app?.windows.split_layout ?? "auto";
    return l === "columns" || l === "rows" ? l : width >= height ? "columns" : "rows";
  }
  // Opened already split: the direction is chosen once the window's size is known.
  $effect(() => {
    if (dir === null && width > 0 && height > 0) untrack(() => (dir = autoDir()));
  });

  const cols = $derived((dir ?? "columns") === "columns");
  const DIVIDER = 4;
  /** Length shared by the tiles (without the dividers). */
  const avail = $derived(Math.max(1, (cols ? width : height) - DIVIDER * (tiles.length - 1)));

  const indexOf = (key: number) => panes.findIndex((p) => p.key === key);
  const tileOf = (key: number) => tiles.findIndex((t) => t.keys.includes(key));
  /** Pane indices in the order they are shown. */
  const order = $derived(tiles.flatMap((t) => t.keys.map(indexOf)).filter((i) => i >= 0));

  type Rect = { x: number; y: number; w: number; h: number };
  /** A divider: between tiles `k` and `k + 1` (`root`), or between the two panes of tile
   * `k`. `h` is a horizontal line (panes above and below it). `at` is where its menu button
   * sits along it (%): beside a stacked tile, off the point where the other line meets it.
   * `folded`: inside a tile folded to a strip (switching mode). */
  type Divider = { id: string; root: boolean; k: number; rect: Rect; h: boolean; at: number; folded: boolean };
  const geo = $derived.by(() => {
    const across = cols ? height : width;
    const box = (a: number, c: number, la: number, lc: number): Rect =>
      cols ? { x: a, y: c, w: la, h: lc } : { x: c, y: a, w: lc, h: la };
    const rects = new Map<number, Rect>();
    const dividers: Divider[] = [];
    let at = 0;
    tiles.forEach((t, k) => {
      const len = t.size * avail;
      if (t.keys.length === 1) rects.set(t.keys[0], box(at, 0, len, across));
      else {
        const room = Math.max(1, across - DIVIDER);
        let c = 0;
        t.keys.forEach((key, j) => {
          const l = (t.inner[j] ?? 0.5) * room;
          rects.set(key, box(at, c, len, l));
          c += l;
          if (j === 0) {
            dividers.push({ id: `i${t.tid}`, root: false, k, rect: box(at, c, len, DIVIDER), h: cols, at: 50, folded: main !== null && t.tid !== main });
            c += DIVIDER;
          }
        });
      }
      at += len;
      if (k < tiles.length - 1) {
        const stacked = [t, tiles[k + 1]].find((x) => x.keys.length > 1);
        dividers.push({ id: `r${k}`, root: true, k, rect: box(at, 0, DIVIDER, across), h: !cols, at: stacked ? (stacked.inner[0] ?? 0.5) * 50 : 50, folded: false });
        at += DIVIDER;
      }
    });
    return { rects, dividers };
  });

  function equalSizes() {
    for (const t of tiles) t.size = 1 / tiles.length;
    main = prevMain = null;
  }

  /** Equal sizes on both sides of divider `d` (the tiles, or the two stacked panes). */
  function equalAt(d: Divider) {
    if (d.root) equalSizes();
    else tiles[d.k].inner = [0.5, 0.5];
  }

  /** Turns the size transition on, in the styles, before anything changes size. */
  async function animate() {
    animating = true;
    clearTimeout(animTimer);
    animTimer = setTimeout(() => (animating = false), animMs + 40);
    await tick();
    void shell?.offsetHeight;
  }

  /** Switching mode with tile `tid` large. */
  async function switchTo(tid: number) {
    if (tiles.length < 2) return;
    const rest = (1 - mainShare) / (tiles.length - 1);
    // The transition must be on before any pane changes size. Set in the same update,
    // the WebView could restyle a pane in between (the tapped strip is removed then) and
    // that pane jumped while the other grew from the wrong edge.
    await animate();
    prevMain = main ?? tid;
    main = tid;
    const t = tiles.find((x) => x.tid === tid);
    if (t && !t.keys.includes(panes[active]?.key)) active = indexOf(t.keys[0]);
    for (const x of tiles) x.size = x.tid === tid ? mainShare : rest;
  }

  const measure = (keys: number[]) =>
    new Map(
      keys.map((key) => {
        const r = inners[key]?.getBoundingClientRect();
        return [key, { w: r?.width ?? 0, h: r?.height ?? 0 }];
      }),
    );

  /**
   * Drags divider `d`. Between tiles, let go past `switchAt` switching mode starts; nearly at
   * the end, the smaller tile closes (both panes of a stacked tile). Between the two panes of
   * a tile, it only resizes them or closes one.
   */
  function drag(e: PointerEvent, d: Divider) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    if (leaving) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    e.preventDefault();
    e.stopPropagation();
    menuAt = null;
    if (d.root) main = prevMain = null;
    frozen = measure(panes.map((p) => p.key));
    animating = false;
    const t = tiles[d.k];
    const [ta, tb] = [tiles[d.k], tiles[d.k + 1]];
    // The shares on both sides of the divider.
    const get = (j: 0 | 1) => (d.root ? (j ? tb : ta).size : t.inner[j]);
    const set = (j: 0 | 1, v: number) => {
      if (d.root) (j ? tb : ta).size = v;
      else t.inner[j] = v;
    };
    const keysOf = (j: 0 | 1) => (d.root ? (j ? tb : ta).keys : [t.keys[j]]);
    const room = d.root ? avail : Math.max(1, (cols ? height : width) - DIVIDER);
    const along = (ev: PointerEvent) => (d.h ? ev.clientY : ev.clientX);
    const from = along(e);
    const a0 = get(0);
    const pair = a0 + get(1);
    let gone: 0 | 1 | null = null;
    const move = (ev: PointerEvent) => {
      const x = Math.min(pair, Math.max(0, a0 + (along(ev) - from) / room));
      set(0, x);
      set(1, pair - x);
      gone = x < pair * closeBelow ? 0 : pair - x < pair * closeBelow ? 1 : null;
      closing = gone === null ? [] : [...keysOf(gone)];
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      frozen = null;
      closing = [];
      if (gone !== null) return void collapse(d, gone);
      if (!d.root) return;
      const big = tiles.find((x) => x.size > switchAt);
      if (big) switchTo(big.tid);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  /** Side `j` of divider `d` shrinks away into it, the other side taking its length, and closes. */
  async function collapse(d: Divider, j: 0 | 1) {
    if (leaving) return;
    const t = tiles[d.k];
    const keys = [...(d.root ? tiles[d.k + j].keys : [t.keys[j]])];
    // Held at its size and against the far side, so it looks pushed off rather than squeezed.
    leaving = { keys, size: measure(keys), end: j === 0, rows: d.h };
    if (d.root) main = prevMain = null;
    await animate();
    if (d.root) {
      const [gone, into] = [tiles[d.k + j], tiles[d.k + 1 - j]];
      into.size += gone.size;
      gone.size = 0;
    } else {
      t.inner[1 - j] = 1;
      t.inner[j] = 0;
    }
    setTimeout(() => {
      leaving = null;
      // Removing one renumbers the panes after it: look each up again.
      for (const key of keys) {
        const i = indexOf(key);
        if (i >= 0) remove(i);
      }
    }, animMs);
  }

  /** Runs `change`, then slides each pane from where it was to where it is now. */
  async function slide(change: () => void) {
    const before = new Map(panes.map((p) => [p.key, paneEls[p.key]?.getBoundingClientRect()]));
    change();
    await tick();
    if (!animMs) return;
    for (const [key, was] of before) {
      const el = paneEls[key];
      if (!el || !was) continue;
      const now = el.getBoundingClientRect();
      if (Math.abs(was.left - now.left) < 1 && Math.abs(was.top - now.top) < 1) continue;
      el.style.transition = "none";
      el.style.transform = `translate(${was.left - now.left}px, ${was.top - now.top}px)`;
      void el.offsetWidth;
      el.style.transition = `transform ${animMs}ms cubic-bezier(0.3, 0, 0.1, 1)`;
      el.style.transform = "";
      setTimeout(() => (el.style.transition = ""), animMs + 40);
    }
  }

  /** Swaps what is on both sides of divider `d` (a stacked tile moves as one). */
  function swap(d: Divider) {
    menuAt = null;
    slide(() => {
      if (d.root) [tiles[d.k], tiles[d.k + 1]] = [tiles[d.k + 1], tiles[d.k]];
      else {
        const t = tiles[d.k];
        t.keys = [t.keys[1], t.keys[0]];
        t.inner = [t.inner[1], t.inner[0]];
      }
    });
  }

  /**
   * Turns the panes on both sides of divider `d` the other way. Between two tiles that are
   * the whole split, the split turns (a stacked tile with it). Between two of three single
   * tiles, those two stack across and the other one stays long. Between two stacked panes,
   * they go back in line with the rest.
   */
  async function turn(d: Divider) {
    menuAt = null;
    await animate();
    if (!d.root) {
      const t = tiles[d.k];
      tiles.splice(d.k, 1, ...t.keys.map((key, j) => ({ tid: nextTid++, size: t.size * (t.inner[j] ?? 0.5), keys: [key], inner: [1] })));
    } else if (tiles.length === 2) {
      dir = cols ? "rows" : "columns";
    } else {
      const [a, b] = [tiles[d.k], tiles[d.k + 1]];
      const size = a.size + b.size;
      tiles.splice(d.k, 2, { tid: nextTid++, size, keys: [...a.keys, ...b.keys], inner: [a.size / size, b.size / size] });
    }
    main = prevMain = null;
  }

  /**
   * Switching mode: every pane is drawn at the large length, clipped by its slot and held
   * against the side facing the large tile, so a folding tile looks pushed aside.
   */
  function anchor(key: number): "start" | "end" | null {
    if (main === null) return null;
    const at = tileOf(key);
    const m = tiles.findIndex((t) => t.tid === main);
    if (tiles[at]?.tid !== main) return at > m ? "start" : "end";
    const p = tiles.findIndex((t) => t.tid === prevMain);
    return p >= 0 && p > at ? "end" : "start";
  }

  /** Scale of a frozen pane in its current slot (never enlarged: the rest stays black). */
  function frozenScale(key: number) {
    const f = frozen?.get(key);
    const r = geo.rects.get(key);
    if (!f?.w || !f.h || !r) return 1;
    return Math.min(1, r.w / f.w, r.h / f.h);
  }

  /** Where a new explorer pane starts. */
  const start = (): BrowseState => ({ sourceId: null, folder: "", query: "", ...browseFrom });

  function split() {
    if (panes.length >= max) return;
    const p = panes[active];
    // A split opening takes its direction from the window now.
    if (panes.length === 1) dir = autoDir();
    // The new pane starts as an explorer; until something is opened there it keeps the
    // active pane's list.
    const key = nextKey++;
    panes.push({ key, ids: p.ids, index: p.index, group: p.group, browse: start() });
    tiles.push({ tid: nextTid++, size: 0, keys: [key], inner: [1] });
    active = panes.length - 1;
    menuAt = null;
    equalSizes();
  }

  /** Closes pane `i`; the others keep their proportions. */
  function remove(i: number) {
    if (panes.length === 1) return onclose(panes[0].ids[panes[0].index]);
    const key = panes[i].key;
    const activeKey = panes[active]?.key;
    panes.splice(i, 1);
    const at = tileOf(key);
    const t = tiles[at];
    if (t && t.keys.length > 1) {
      t.keys = t.keys.filter((k) => k !== key);
      t.inner = [1];
    } else if (t) {
      if (main === t.tid) main = prevMain = null;
      if (prevMain === t.tid) prevMain = null;
      tiles.splice(at, 1);
      const sum = tiles.reduce((n, x) => n + x.size, 0);
      for (const x of tiles) x.size = sum > 0 ? x.size / sum : 1 / tiles.length;
    }
    // A stacked tile left alone becomes the split, turned its way.
    if (tiles.length === 1 && tiles[0].keys.length === 2) {
      const only = tiles[0];
      dir = cols ? "rows" : "columns";
      tiles = only.keys.map((k, j) => ({ tid: nextTid++, size: only.inner[j] ?? 0.5, keys: [k], inner: [1] }));
      main = prevMain = null;
    }
    const still = indexOf(activeKey);
    active = still >= 0 ? still : (order[0] ?? 0);
    if (main !== null) {
      // Switching mode goes on with the tiles left.
      if (tiles.length < 2) equalSizes();
      else for (const x of tiles) x.size = x.tid === main ? mainShare : (1 - mainShare) / (tiles.length - 1);
    }
    menuAt = null;
  }

  /** Closes the whole viewer (the close button): back to the library, at pane `i`'s item. */
  function closeAll(i: number) {
    const p = panes[i];
    onclose(p.browse ? undefined : p.ids[p.index]);
  }

  /** Item `id` was deleted (or, with `only`, taken out of that temporary group): every pane
   * (of that group) drops it and shows the item after it (or the one before, at the end).
   * A pane left with nothing closes. */
  function dropItem(id: number, only?: string) {
    for (let i = panes.length - 1; i >= 0; i--) {
      const p = panes[i];
      if (only !== undefined && p.group !== only) continue;
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
  function openIn(i: number, ids: number[], index: number, state: BrowseState, group: string | null) {
    const p = panes[i];
    p.back = { ...state, group };
    p.ids = ids;
    p.index = index;
    p.group = group;
    p.browse = null;
  }

  /**
   * A viewer pane closed (close button, back, pull down): with other panes open, the pane
   * goes back to its explorer at the item it showed, and the split stays. The only pane
   * closes the whole viewer.
   */
  function closeViewer(i: number) {
    const p = panes[i];
    if (panes.length === 1) return closeAll(i);
    const back = p.back ?? { ...start(), group: p.group ?? null };
    p.browse = { ...back, reveal: p.ids[p.index] ?? null };
    pulled = 0;
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
    else if (matches(e, k.next_pane) && panes.length > 1) active = order[(order.indexOf(active) + 1) % order.length];
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
    {@const side = anchor(p.key)}
    {@const r = geo.rects.get(p.key)}
    {@const out = leaving?.keys.includes(p.key) ? leaving : null}
    {@const held = out?.size.get(p.key)}
    {@const fz = frozen?.get(p.key)}
    <div
      class="pane"
      bind:this={paneEls[p.key]}
      class:closing={closing.includes(p.key)}
      class:ax-rows={out ? out.rows : !cols}
      style:left="{r?.x ?? 0}px"
      style:top="{r?.y ?? 0}px"
      style:width="{r?.w ?? 0}px"
      style:height="{r?.h ?? 0}px"
      onpointerdowncapture={() => (active = i)}
      role="presentation"
    >
      <div
        class="inner"
        bind:this={inners[p.key]}
        class:browse={!!p.browse}
        class:a-start={side === "start"}
        class:a-end={side === "end"}
        class:leaving={!!out}
        class:end={!!out?.end}
        class:frozen={!!fz?.w}
        style:--big="{avail * mainShare}px"
        style:--lw="{held?.w ?? 0}px"
        style:--lh="{held?.h ?? 0}px"
        style:--fw="{fz?.w ?? 0}px"
        style:--fh="{fz?.h ?? 0}px"
        style:--fs={frozenScale(p.key)}
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
              embedded={{ active: i === active, onopen: (ids, index, state, group) => openIn(i, ids, index, state, group) }}
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
            onclose={() => closeViewer(i)}
            onsplit={panes.length < max ? split : undefined}
            onpop={isMobile ? undefined : () => pop(i)}
            onpull={panes.length === 1 ? (v) => (pulled = v) : undefined}
            onremoved={dropItem}
            group={p.group && groups.get(p.group)
              ? {
                  name: groups.get(p.group)!.name,
                  onremove: (id) => {
                    groups.removeItems(p.group!, [id]);
                    dropItem(id, p.group!);
                  },
                }
              : null}
          />
        {/if}
      </div>
      {#if main !== null && tiles[tileOf(p.key)] && tiles[tileOf(p.key)].tid !== main}
        <!-- A folded tile: a tap swaps it in. -->
        <button class="peek" class:end={side === "end"} aria-label={t("viewer.switchPane")} onclick={() => switchTo(tiles[tileOf(p.key)].tid)}></button>
      {/if}
    </div>
  {/each}
  {#each geo.dividers as d (d.id)}
    <div
      class="divider"
      class:h={d.h}
      class:open={menuAt === d.id}
      class:folded={d.folded}
      style:--at="{d.at}%"
      style:left="{d.rect.x}px"
      style:top="{d.rect.y}px"
      style:width="{d.rect.w}px"
      style:height="{d.rect.h}px"
      role="separator"
      aria-orientation={d.h ? "horizontal" : "vertical"}
      onpointerdown={(e) => drag(e, d)}
      ondblclick={() => animate().then(() => equalAt(d))}
    >
      <button
        class="more"
        class:on={menuAt === d.id}
        aria-label={t("viewer.paneMenu")}
        aria-expanded={menuAt === d.id}
        onpointerdown={(e) => e.stopPropagation()}
        ondblclick={(e) => e.stopPropagation()}
        onclick={() => (menuAt = menuAt === d.id ? null : d.id)}
      >
        <Icon name="dots" size={16} />
      </button>
      {#if menuAt === d.id}
        <div class="menu-at">
        <div
          class="menu"
          role="menu"
          tabindex="-1"
          transition:popIn={{ y: -6 }}
          onpointerdown={(e) => e.stopPropagation()}
          use:dismiss={{ onclose: () => (menuAt = null), keep: () => [shell?.querySelector(".more.on")] }}
        >
          <button class="btn sm ghost" role="menuitem" onclick={() => swap(d)}>
            <Icon name="swap" size={14} />{t("viewer.swapPanes")}
          </button>
          <button class="btn sm ghost" role="menuitem" onclick={() => turn(d)}>
            <!-- Panes side by side turn into a stack (세로), a stack into panes side by side (가로). -->
            <Icon name={d.h ? "columns" : "rows"} size={14} />{t(d.h ? "viewer.turnColumns" : "viewer.turnRows")}
          </button>
          <button
            class="btn sm ghost"
            role="menuitem"
            onclick={() => {
              menuAt = null;
              animate().then(() => equalAt(d));
            }}
          >
            <Icon name={d.h ? "rows" : "columns"} size={14} />{t("viewer.equalPanes")}
          </button>
          <button class="btn sm ghost" role="menuitem" onclick={() => ((menuAt = null), collapse(d, 0))}>
            <Icon name="x" size={14} />{t(d.h ? "viewer.closeTop" : "viewer.closeLeft")}
          </button>
          <button class="btn sm ghost" role="menuitem" onclick={() => ((menuAt = null), collapse(d, 1))}>
            <Icon name="x" size={14} />{t(d.h ? "viewer.closeBottom" : "viewer.closeRight")}
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
    overflow: hidden;
    background: #000;
  }
  /* Panes and dividers are placed from the layout (ViewerShell `geo`). */
  .pane {
    position: absolute;
    overflow: hidden;
  }
  .shell.animating .pane,
  .shell.animating .divider {
    transition-property: left, top, width, height;
    transition-duration: var(--anim);
    transition-timing-function: cubic-bezier(0.3, 0, 0.1, 1);
  }
  .inner {
    position: absolute;
    inset: 0;
  }
  /* Switching mode: drawn at the large size, against the side facing the large pane. */
  .inner.a-start,
  .inner.a-end {
    width: var(--big);
  }
  .inner.a-start {
    right: auto;
  }
  .inner.a-end {
    left: auto;
  }
  .ax-rows .inner.a-start,
  .ax-rows .inner.a-end {
    width: auto;
    height: var(--big);
  }
  .ax-rows .inner.a-start {
    right: 0;
    bottom: auto;
  }
  .ax-rows .inner.a-end {
    left: 0;
    top: auto;
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
  .ax-rows .peek {
    background: linear-gradient(to bottom, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
  }
  .ax-rows .peek.end {
    background: linear-gradient(to top, rgb(0 0 0 / 0.55), rgb(0 0 0 / 0.25));
  }
  .peek:hover {
    background: rgb(255 255 255 / 0.08);
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
  .ax-rows .inner.leaving.end {
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
    position: absolute;
    z-index: 40;
    background: #000;
    cursor: col-resize;
    touch-action: none;
  }
  .divider.h {
    cursor: row-resize;
  }
  /* Its menu goes over the other dividers. */
  .divider.open {
    z-index: 45;
  }
  /* Inside a folded tile: only the line shows. */
  .divider.folded {
    pointer-events: none;
  }
  .divider.folded .more {
    display: none;
  }
  /* A wider grip than the visible line (a finger needs more). */
  .divider::before {
    content: "";
    position: absolute;
    inset: 0 -6px;
  }
  .divider.h::before {
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
  .divider.h::after {
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
    top: var(--at, 50%);
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
  .divider.h .more {
    left: var(--at, 50%);
    top: 50%;
    width: 34px;
    height: 22px;
  }
  .divider.h .more :global(svg) {
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
    top: var(--at, 50%);
    transform: translate(-50%, 30px);
  }
  .divider.h .menu-at {
    left: var(--at, 50%);
    top: 50%;
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
    .divider.h .more {
      width: 44px;
      height: 28px;
    }
    .divider::before {
      inset: 0 -12px;
    }
    .divider.h::before {
      inset: -12px 0;
    }
  }
</style>
