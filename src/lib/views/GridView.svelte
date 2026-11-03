<script lang="ts">
  import { untrack } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { thumbUrl } from "../api";
  import { fmtDuration } from "../format";
  import Icon from "../ui/Icon.svelte";
  import { briefs, reportBroken, want } from "./briefs.svelte";
  import { folderSamples, sampleKey, wantFolders } from "./folders.svelte";
  import { recallScroll, rememberScroll, sectionAt } from "./scroll";
  import SectionScrollbar from "./SectionScrollbar.svelte";
  import { arrowStep } from "./selection.svelte";
  import type { ViewProps } from "./types";

  let { ids: listed, folders, selection, resetKey, scrollKey, sections, tile, showNames, folderPreview, overscan, onopenwindow, onopen, onfolder, reorder }: ViewProps = $props();

  const GAP = 10;
  const LABEL = 22;
  const PAD = 16;
  /** Movement (px) that makes a press a scroll or a drag rather than a hold. */
  const SLOP = 8;
  /** Within this distance (px) of the top or bottom, a dragged item scrolls the grid. */
  const EDGE = 56;

  // ---- reordering (temporary groups): hold an item, then drag it to its new place
  /** A press that may turn into a drag. */
  let press: {
    id: number;
    from: number;
    x: number;
    y: number;
    pointerId: number;
    armed: boolean;
    timer: ReturnType<typeof setTimeout>;
    /** The selection before the press (a touch hold selects and opens the menu: undone if it drags). */
    before: number[];
  } | null = null;
  /** The item being dragged: where the pointer is (grid content coordinates) and where it would land. */
  let dragging = $state<{ id: number; from: number; to: number; x: number; y: number; grabX: number; grabY: number } | null>(null);
  /** Swallows the click that ends a drag. */
  let dropped = false;
  let edgeFrame = 0;
  let pointerY = 0;

  /** The order shown: while dragging, the dragged item sits where it would land. */
  const ids = $derived.by(() => {
    if (!dragging) return listed;
    const next = listed.filter((x) => x !== dragging!.id);
    next.splice(dragging.to, 0, dragging.id);
    return next;
  });

  function startPress(e: PointerEvent, id: number, index: number) {
    selection.pointer(e);
    if (!reorder || !e.isPrimary || (e.pointerType === "mouse" && e.button !== 0)) return;
    endPress();
    press = {
      id,
      from: index,
      x: e.clientX,
      y: e.clientY,
      pointerId: e.pointerId,
      armed: false,
      before: [...selection.set],
      timer: setTimeout(() => press && (press.armed = true), reorder.holdMs),
    };
    window.addEventListener("pointermove", onPressMove);
    window.addEventListener("pointerup", onPressUp);
    window.addEventListener("pointercancel", onPressCancel);
  }

  /** Content coordinates of a pointer position. */
  function contentPoint(x: number, y: number) {
    const r = el.getBoundingClientRect();
    return { x: x - r.left, y: y - r.top + el.scrollTop };
  }

  /** The position in the list for a point in content coordinates. */
  function slotAt(x: number, y: number) {
    const col = Math.min(cols - 1, Math.max(0, Math.floor((x - PAD + GAP / 2) / (tileW + GAP))));
    const row = Math.max(0, Math.floor((y - PAD + GAP / 2) / rowH));
    return Math.min(listed.length - 1, Math.max(0, row * cols + col - folders.length));
  }

  function onPressMove(e: PointerEvent) {
    if (!press || e.pointerId !== press.pointerId) return;
    pointerY = e.clientY;
    if (!dragging) {
      const moved = Math.hypot(e.clientX - press.x, e.clientY - press.y) > SLOP;
      if (!moved) return;
      if (!press.armed) return endPress();
      // A drag: whatever the hold did (selected the item, opened the menu) is undone.
      selection.menu = null;
      selection.set.clear();
      for (const x of press.before) selection.set.add(x);
      const p = contentPoint(press.x, press.y);
      const slot = press.from + folders.length;
      dragging = {
        id: press.id,
        from: press.from,
        to: press.from,
        x: p.x,
        y: p.y,
        grabX: p.x - (PAD + (slot % cols) * (tileW + GAP)),
        grabY: p.y - (PAD + Math.floor(slot / cols) * rowH),
      };
      edgeFrame = requestAnimationFrame(edgeScroll);
    }
    const p = contentPoint(e.clientX, e.clientY);
    dragging.x = p.x;
    dragging.y = p.y;
    dragging.to = slotAt(p.x - dragging.grabX + tileW / 2, p.y - dragging.grabY + tileW / 2);
  }

  /** Near the top or bottom while dragging, the grid scrolls (faster the closer). */
  function edgeScroll() {
    if (!dragging) return;
    const r = el.getBoundingClientRect();
    const up = pointerY - r.top;
    const down = r.bottom - pointerY;
    const step = up < EDGE ? -(EDGE - up) / 4 : down < EDGE ? (EDGE - down) / 4 : 0;
    if (step) {
      const before = el.scrollTop;
      el.scrollTop += step;
      const d = el.scrollTop - before;
      if (d) {
        dragging.y += d;
        dragging.to = slotAt(dragging.x - dragging.grabX + tileW / 2, dragging.y - dragging.grabY + tileW / 2);
      }
    }
    edgeFrame = requestAnimationFrame(edgeScroll);
  }

  function onPressUp(e: PointerEvent) {
    if (!press || e.pointerId !== press.pointerId) return;
    const d = dragging;
    endPress();
    if (!d) return;
    dropped = true;
    setTimeout(() => (dropped = false));
    if (d.to !== d.from) reorder?.onmove(d.id, d.to);
  }

  function onPressCancel(e: PointerEvent) {
    if (press && e.pointerId === press.pointerId) endPress();
  }

  function endPress() {
    if (press) clearTimeout(press.timer);
    press = null;
    dragging = null;
    cancelAnimationFrame(edgeFrame);
    window.removeEventListener("pointermove", onPressMove);
    window.removeEventListener("pointerup", onPressUp);
    window.removeEventListener("pointercancel", onPressCancel);
  }

  // A held item keeps the finger from scrolling the grid (the move becomes the drag).
  $effect(() => {
    if (!el) return;
    const block = (e: TouchEvent) => {
      if (press?.armed) e.preventDefault();
    };
    el.addEventListener("touchmove", block, { passive: false });
    return () => {
      el.removeEventListener("touchmove", block);
      endPress();
    };
  });

  let el = $state<HTMLDivElement>() as HTMLDivElement;
  let width = $state(0);
  let height = $state(0);
  let scrollTop = $state(0);
  const broken = new SvelteSet<number>();

  // Folders come first in the layout, then files.
  const count = $derived(folders.length + ids.length);
  const inner = $derived(Math.max(0, width - PAD * 2));
  const cols = $derived(Math.max(1, Math.floor((inner + GAP) / (tile + GAP))));
  const tileW = $derived(inner > 0 ? (inner - GAP * (cols - 1)) / cols : tile);
  const rowH = $derived(tileW + (showNames ? LABEL : 0) + GAP);
  const totalRows = $derived(Math.ceil(count / cols));
  const startRow = $derived(Math.max(0, Math.floor(scrollTop / rowH) - overscan));
  const endRow = $derived(Math.min(totalRows, Math.ceil((scrollTop + height) / rowH) + overscan));
  const visible = $derived.by(() => {
    const out: { slot: number; folder?: (typeof folders)[number]; id?: number; index: number }[] = [];
    for (let slot = startRow * cols; slot < Math.min(count, endRow * cols); slot++) {
      if (slot < folders.length) out.push({ slot, folder: folders[slot], index: -1 });
      else out.push({ slot, id: ids[slot - folders.length], index: slot - folders.length });
    }
    return out;
  });

  const contentH = $derived(Math.max(0, totalRows * rowH - GAP + PAD * 2));

  // New results, a remount or a new tile size: back to where this listing was.
  $effect(() => {
    resetKey;
    const key = scrollKey;
    if (!el || width === 0) return;
    const top = (recallScroll(key) / cols) * rowH;
    untrack(() => {
      el.scrollTop = top;
      scrollTop = el.scrollTop;
    });
  });

  function onScroll() {
    scrollTop = el.scrollTop;
    rememberScroll(scrollKey, (scrollTop / rowH) * cols);
  }

  const lines = $derived(sections.map((s) => PAD + Math.floor((folders.length + s.index) / cols) * rowH));
  function label(top: number): string {
    const slot = Math.min(count - 1, Math.floor(Math.max(0, top - PAD) / rowH) * cols);
    const index = slot - folders.length;
    if (index < 0) return folders[slot]?.name ?? "";
    return sectionAt(sections, index) ?? `${(index + 1).toLocaleString()} / ${ids.length.toLocaleString()}`;
  }

  $effect(() => {
    want(visible.filter((v) => v.id !== undefined).map((v) => v.id!));
  });

  const sampleCount = $derived(Math.max(0, Math.min(4, folderPreview)) ** 2);
  $effect(() => {
    wantFolders(visible.flatMap((v) => (v.folder ? [v.folder] : [])), sampleCount);
  });

  /** Item `index`'s row is wholly in view. */
  function shown(index: number) {
    const top = Math.floor((index + folders.length) / cols) * rowH;
    return top >= el.scrollTop && top + rowH <= el.scrollTop + height;
  }

  function scrollToIndex(index: number, centre = false) {
    const row = Math.floor((index + folders.length) / cols);
    const top = row * rowH;
    if (centre) el.scrollTop = Math.max(0, top - (height - rowH) / 2);
    else if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowH > el.scrollTop + height) el.scrollTop = top + rowH - height;
  }

  // Declared after the reset above, so a revealed item wins over scrolling to the top.
  $effect(() => {
    const id = selection.revealing;
    if (id == null || !el || height === 0) return;
    const i = ids.indexOf(id);
    if (i < 0) return;
    scrollToIndex(i, !shown(i));
    scrollTop = el.scrollTop;
    selection.revealing = null;
  });

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selection.selectAll(ids);
      return;
    }
    if (e.key === "Escape") return selection.clear();
    if (e.key === "Enter" && !e.ctrlKey && !e.metaKey && selection.focus != null) {
      e.preventDefault();
      return onopen(selection.focus, ids);
    }
    const step = arrowStep(e, cols, Math.max(1, Math.floor(height / rowH)));
    if (!step) return;
    e.preventDefault();
    const i = selection.move(Number.isFinite(step) ? step : step > 0 ? ids.length : -ids.length, ids, e.shiftKey);
    if (i >= 0) scrollToIndex(i);
  }

</script>

<div class="wrap">
<div
  class="grid"
  bind:this={el}
  bind:clientWidth={width}
  bind:clientHeight={height}
  onscroll={onScroll}
  onkeydown={onKey}
  class:reordering={!!dragging}
  role="listbox"
  aria-multiselectable="true"
  tabindex="0"
>
  <div class="spacer" style:height="{contentH}px">
    {#each visible as v (v.folder ? `f:${v.folder.path}` : v.id)}
      {@const row = Math.floor(v.slot / cols)}
      {@const col = v.slot % cols}
      {#if v.folder}
        {@const f = v.folder}
        {@const samples = sampleCount ? folderSamples.get(sampleKey(f.sourceId, f.path, sampleCount)) : undefined}
        <button
          class="tile folder"
          class:selected={selection.isFolder(f.sourceId, f.path)}
          style:width="{tileW}px"
          style:transform="translate({PAD + col * (tileW + GAP)}px, {PAD + row * rowH}px)"
          title={f.name}
          onpointerdown={(e) => selection.pointer(e)}
          onclick={(e) => selection.clickFolder(e, f, folders) || onfolder(f.path)}
          oncontextmenu={(e) => selection.holdFolder(e, f)}
          ondblclick={() => onfolder(f.path)}
          onkeydown={(e) => e.key === "Enter" && onfolder(f.path)}
        >
          <div class="frame" style:height="{tileW}px">
            {#if samples?.length}
              <!-- A folder drawn as its tab and pocket, holding thumbnails from inside. -->
              <span class="art">
                <span class="tab"></span>
                <span class="pocket" style:--n={Math.sqrt(sampleCount)}>
                  {#each { length: sampleCount } as _, i (i)}
                    {@const sid = samples[i]}
                    <span class="cell">
                      {#if sid !== undefined && !broken.has(sid)}
                        <img src={thumbUrl(sid)} alt="" decoding="async" loading="lazy" draggable="false" onerror={() => broken.add(sid)} />
                      {/if}
                    </span>
                  {/each}
                </span>
              </span>
            {:else}
              <span class="folder-icon"><Icon name="folder" size={Math.max(28, tileW * 0.32)} stroke={1.4} /></span>
            {/if}
            <span class="count tabular">{f.count.toLocaleString()}</span>
          </div>
          {#if showNames}<div class="name">{f.name}</div>{/if}
        </button>
      {:else if v.id !== undefined}
        {@const id = v.id}
        {@const b = briefs.get(id)}
        {@const noThumb = b?.thumb === "unsupported" || b?.thumb === "failed" || broken.has(id)}
        {@const lifted = dragging?.id === id}
        <button
          class="tile"
          class:selected={selection.set.has(id)}
          class:focus={selection.focus === id}
          class:lifted
          style:width="{tileW}px"
          style:transform={lifted && dragging
            ? `translate(${dragging.x - dragging.grabX}px, ${dragging.y - dragging.grabY}px) scale(1.06)`
            : `translate(${PAD + col * (tileW + GAP)}px, ${PAD + row * rowH}px)`}
          title={b?.name}
          onpointerdown={(e) => startPress(e, id, v.index)}
          onclick={(e) => !dropped && selection.activate(e, v.index, ids, onopen)}
          oncontextmenu={(e) => (dragging ? e.preventDefault() : selection.hold(e, v.index, ids))}
          ondblclick={() => onopen(id, ids)}
          onauxclick={(e) => e.button === 1 && onopenwindow?.(id, ids)}
          onmousedown={(e) => e.button === 1 && e.preventDefault()}
          tabindex="-1"
        >
          <div class="frame" style:height="{tileW}px">
            {#if noThumb}
              <span class="placeholder"><Icon name={b?.kind === "video" ? "film" : b?.kind === "nonmedia" ? "file" : "image"} size={26} stroke={1.4} /></span>
            {:else}
              <img
                src={thumbUrl(id)}
                alt=""
                decoding="async"
                loading="lazy"
                draggable="false"
                onerror={() => {
                  broken.add(id);
                  reportBroken(id);
                }}
              />
            {/if}
            {#if selection.set.has(id)}
              <span class="check"><Icon name="check" size={13} stroke={3} /></span>
            {/if}
            {#if b?.favorite}
              <span class="fav-badge"><Icon name="heart" size={13} /></span>
            {/if}
            {#if b?.kind === "video"}
              <span class="badge tabular">{fmtDuration(b.duration_ms) || "VIDEO"}</span>
            {/if}
          </div>
          {#if showNames}<div class="name">{b?.name ?? ""}</div>{/if}
        </button>
      {/if}
    {/each}
  </div>
</div>
<SectionScrollbar {el} {scrollTop} viewH={height} {contentH} {lines} {label} />
</div>

<style>
  .wrap {
    position: relative;
    height: 100%;
  }
  .grid {
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    contain: strict;
    outline: none;
    /* Replaced by `SectionScrollbar`. */
    scrollbar-width: none;
  }
  .grid::-webkit-scrollbar {
    display: none;
  }
  .spacer {
    position: relative;
  }
  .tile {
    position: absolute;
    top: 0;
    left: 0;
    padding: 0;
    border: 0;
    background: transparent;
    text-align: left;
    will-change: transform;
    contain: layout style;
  }
  /* Reordering: the other tiles slide to their new places; the dragged one follows the pointer. */
  .reordering {
    user-select: none;
    -webkit-user-select: none;
  }
  .reordering .tile:not(.lifted) {
    transition: transform var(--t-fast) var(--ease);
  }
  .tile.lifted {
    z-index: 5;
    pointer-events: none;
  }
  .tile.lifted .frame {
    box-shadow: var(--shadow);
  }
  .frame {
    position: relative;
    display: grid;
    place-items: center;
    border-radius: var(--radius-md);
    background: var(--tile);
    overflow: hidden;
    transition: box-shadow var(--t-fast) var(--ease);
  }
  .tile:hover .frame {
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .tile:hover img {
    filter: brightness(1.06);
  }
  .tile.selected .frame {
    box-shadow:
      inset 0 0 0 2px var(--accent),
      0 0 0 3px var(--accent-soft);
  }
  /* Drawn over the image so a selection is visible on any thumbnail. */
  .tile.selected .frame::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: inherit;
    box-shadow: inset 0 0 0 3px var(--accent);
    pointer-events: none;
  }
  .check {
    position: absolute;
    top: 6px;
    left: 6px;
    z-index: 1;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-fg);
  }
  .tile.focus:not(.selected) .frame {
    box-shadow: inset 0 0 0 1px var(--accent-line);
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
  }
  .placeholder,
  .folder-icon {
    color: var(--faint);
    display: grid;
    place-items: center;
  }
  .folder .frame {
    background: var(--surface-2);
  }
  .folder:hover .frame {
    background: var(--hover);
  }
  .folder-icon {
    color: var(--accent);
    opacity: 0.85;
  }
  .count {
    position: absolute;
    right: 8px;
    bottom: 7px;
    font-size: 11px;
    color: var(--muted);
  }
  .art {
    position: absolute;
    inset: 12% 9% 15%;
  }
  .tab {
    position: absolute;
    top: 0;
    left: 0;
    width: 42%;
    height: 14%;
    border-radius: 7px 7px 0 0;
    background: color-mix(in srgb, var(--accent) 55%, var(--surface-2));
  }
  .pocket {
    position: absolute;
    inset: 8% 0 0;
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    grid-template-rows: repeat(var(--n), minmax(0, 1fr));
    gap: 4%;
    padding: 5%;
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--accent) 38%, var(--surface-2));
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.25);
  }
  .cell {
    min-width: 0;
    min-height: 0;
    overflow: hidden;
    border-radius: 4px;
    background: color-mix(in srgb, var(--surface-2) 70%, transparent);
  }
  .cell img {
    object-fit: cover;
  }
  .folder:has(.art) .count {
    right: 6px;
    bottom: 5px;
    padding: 0 6px;
    border-radius: var(--radius-pill);
    background: var(--surface-2);
  }
  .fav-badge {
    position: absolute;
    top: 6px;
    right: 6px;
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    background: rgb(0 0 0 / 0.5);
    color: #ff4d6d;
  }
  .fav-badge :global(path) {
    fill: currentColor;
  }
  .badge {
    position: absolute;
    right: 6px;
    bottom: 6px;
    height: 18px;
    padding: 0 7px;
    border-radius: var(--radius-pill);
    background: rgb(0 0 0 / 0.62);
    color: #fff;
    font-size: 11px;
    line-height: 18px;
  }
  .name {
    height: 22px;
    line-height: 22px;
    padding: 0 2px;
    font-size: 12px;
    color: var(--fg-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tile.selected .name {
    color: var(--accent);
  }
</style>
