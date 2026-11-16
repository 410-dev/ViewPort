<script lang="ts">
  import { t } from "../i18n.svelte";
  import { untrack } from "svelte";
  import { briefs, want } from "./briefs.svelte";
  import Cells from "./Cells.svelte";
  import HeaderRow from "./HeaderRow.svelte";
  import { recallScroll, rememberScroll, sectionAt } from "./scroll";
  import SectionScrollbar from "./SectionScrollbar.svelte";
  import { arrowStep } from "./selection.svelte";
  import { Sweep } from "./sweep.svelte";
  import type { ViewProps } from "./types";

  let { ids, folders, selection, resetKey, scrollKey, sections, overscan, columns, rowPx, sort, onsort, onopenwindow, onopen, onfolder }: ViewProps = $props();

  const HEAD = 33;
  let el = $state<HTMLDivElement>() as HTMLDivElement;
  let height = $state(0);
  let scrollTop = $state(0);

  const count = $derived(folders.length + ids.length);
  const over = $derived(overscan * 4);
  const start = $derived(Math.max(0, Math.floor(scrollTop / rowPx) - over));
  const end = $derived(Math.min(count, Math.ceil((scrollTop + height) / rowPx) + over));
  const rows = $derived.by(() => {
    const out: { slot: number; folder?: (typeof folders)[number]; id?: number; index: number }[] = [];
    for (let slot = start; slot < end; slot++) {
      if (slot < folders.length) out.push({ slot, folder: folders[slot], index: -1 });
      else out.push({ slot, id: ids[slot - folders.length], index: slot - folders.length });
    }
    return out;
  });

  const contentH = $derived(HEAD + count * rowPx + 16);

  // New results or a remount: back to where this listing was.
  $effect(() => {
    resetKey;
    const top = recallScroll(scrollKey) * rowPx;
    if (!el || height === 0) return;
    untrack(() => {
      el.scrollTop = top;
      scrollTop = el.scrollTop;
    });
  });

  function onScroll() {
    scrollTop = el.scrollTop;
    rememberScroll(scrollKey, scrollTop / rowPx);
  }

  const lines = $derived(sections.map((s) => (folders.length + s.index) * rowPx));
  function label(top: number): string {
    const slot = Math.min(count - 1, Math.floor(top / rowPx));
    const index = slot - folders.length;
    if (index < 0) return folders[slot]?.name ?? "";
    return sectionAt(sections, index) ?? `${(index + 1).toLocaleString()} / ${ids.length.toLocaleString()}`;
  }
  $effect(() => want(rows.filter((r) => r.id !== undefined).map((r) => r.id!)));

  /** Item `index`'s row is wholly in view. */
  function shown(index: number) {
    const top = (index + folders.length) * rowPx;
    return top >= el.scrollTop && top + rowPx <= el.scrollTop + height - HEAD;
  }

  function scrollToIndex(index: number, centre = false) {
    const top = (index + folders.length) * rowPx;
    const view = height - HEAD;
    if (centre) el.scrollTop = Math.max(0, top - (view - rowPx) / 2);
    else if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowPx > el.scrollTop + view) el.scrollTop = top + rowPx - view;
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

  // ---- drag-selecting a run of rows (see `sweep.svelte.ts`)
  /** Wide columns scroll sideways: then a sideways drag is that, not a sweep. */
  let wide = $state(false);
  const sweep = new Sweep({
    el: () => el,
    indexAt: (_x, y) => {
      if (!ids.length) return -1;
      const r = el.getBoundingClientRect();
      const slot = Math.floor((y - r.top + el.scrollTop - HEAD - 4) / rowPx);
      return Math.min(ids.length - 1, Math.max(0, slot - folders.length));
    },
    ids: () => ids,
    selection: () => selection,
    reorderHoldMs: () => null,
    sideways: () => !wide,
    inset: HEAD,
  });
  $effect(() => {
    if (!el) return;
    const block = (e: TouchEvent) => {
      if (sweep.blocksScroll) e.preventDefault();
    };
    el.addEventListener("touchmove", block, { passive: false });
    return () => {
      el.removeEventListener("touchmove", block);
      sweep.cancel();
    };
  });

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      return selection.selectAll(ids);
    }
    if (e.key === "Escape") return selection.clear();
    if (e.key === "Enter" && !e.ctrlKey && !e.metaKey && selection.focus != null) {
      e.preventDefault();
      return onopen(selection.focus, ids);
    }
    const step = arrowStep(e, 1, Math.max(1, Math.floor((height - HEAD) / rowPx)));
    if (!step) return;
    e.preventDefault();
    const i = selection.move(Number.isFinite(step) ? step : step > 0 ? ids.length : -ids.length, ids, e.shiftKey);
    if (i >= 0) scrollToIndex(i);
  }
</script>

<div class="wrap">
<div
  class="list"
  bind:this={el}
  bind:clientHeight={height}
  onscroll={onScroll}
  onkeydown={onKey}
  class:sweeping={sweep.active}
  class:pan-y={!wide}
  onpointerdowncapture={() => (wide = el.scrollWidth > el.clientWidth + 1)}
  role="grid"
  aria-multiselectable="true"
  tabindex="0"
>
  <HeaderRow {columns} {sort} {onsort} />
  <div class="spacer" style:height="{count * rowPx + 8}px">
    {#each rows as r (r.folder ? `f:${r.folder.path}` : r.id)}
      {#if r.folder}
        {@const f = r.folder}
        <button
          class="row"
          class:selected={selection.isFolder(f.sourceId, f.path)}
          style:height="{rowPx}px"
          style:transform="translateY({r.slot * rowPx}px)"
          onpointerdown={(e) => selection.pointer(e)}
          onclick={(e) => selection.clickFolder(e, f, folders) || onfolder(f.path)}
          oncontextmenu={(e) => selection.holdFolder(e, f)}
          ondblclick={() => onfolder(f.path)}
          onkeydown={(e) => e.key === "Enter" && onfolder(f.path)}
          tabindex="-1"
        >
          <Cells {columns} name={f.name} icon="folder" extra={t("lib.items", { n: f.count.toLocaleString() })} />
        </button>
      {:else if r.id !== undefined}
        {@const id = r.id}
        <button
          class="row"
          class:selected={selection.set.has(id)}
          class:focus={selection.focus === id}
          style:height="{rowPx}px"
          style:transform="translateY({r.slot * rowPx}px)"
          onpointerdown={(e) => {
            selection.pointer(e);
            sweep.down(e, r.index);
          }}
          onclick={(e) => !sweep.dropped && selection.activate(e, r.index, ids, onopen)}
          oncontextmenu={(e) => {
            if (sweep.active) return e.preventDefault();
            sweep.held();
            selection.hold(e, r.index, ids);
          }}
          ondblclick={() => onopen(id, ids)}
          onauxclick={(e) => e.button === 1 && onopenwindow?.(id, ids)}
          onmousedown={(e) => e.button === 1 && e.preventDefault()}
          tabindex="-1"
        >
          <Cells {columns} {id} brief={briefs.get(id) ?? null} />
        </button>
      {/if}
    {/each}
  </div>
</div>
<SectionScrollbar {el} {scrollTop} viewH={height} {contentH} {lines} {label} inset={HEAD} />
</div>

<style>
  .wrap {
    position: relative;
    height: 100%;
  }
  .list {
    height: 100%;
    overflow: auto;
    outline: none;
    contain: strict;
    /* The vertical bar is `SectionScrollbar`; wide columns keep the horizontal one. */
    scrollbar-width: auto;
    scrollbar-color: auto;
  }
  .list::-webkit-scrollbar {
    width: 0;
  }
  /* A sideways drag is a sweep (drag-select) unless the columns scroll sideways. */
  .list.pan-y {
    touch-action: pan-y;
  }
  .sweeping {
    user-select: none;
    -webkit-user-select: none;
  }
  .spacer {
    position: relative;
    margin: 4px 8px;
  }
  .row {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    text-align: left;
    will-change: transform;
    transition: background var(--t-fast) var(--ease);
  }
  .row:hover {
    background: var(--surface-2);
  }
  .row.selected {
    background: var(--accent-soft);
  }
  .row.focus {
    box-shadow: inset 0 0 0 1px var(--accent-line);
  }
</style>
