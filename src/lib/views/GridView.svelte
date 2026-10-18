<script lang="ts">
  import { SvelteSet } from "svelte/reactivity";
  import { thumbUrl } from "../api";
  import { fmtDuration } from "../format";
  import Icon from "../ui/Icon.svelte";
  import { briefs, want } from "./briefs.svelte";
  import { arrowStep } from "./selection.svelte";
  import type { ViewProps } from "./types";

  let { ids, folders, selection, resetKey, tile, showNames, overscan, onopen, onfolder }: ViewProps = $props();

  const GAP = 10;
  const LABEL = 22;
  const PAD = 16;

  let el: HTMLDivElement;
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

  $effect(() => {
    resetKey;
    if (el) el.scrollTop = 0;
  });

  $effect(() => {
    want(visible.filter((v) => v.id !== undefined).map((v) => v.id!));
  });

  function scrollToIndex(index: number) {
    const row = Math.floor((index + folders.length) / cols);
    const top = row * rowH;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowH > el.scrollTop + height) el.scrollTop = top + rowH - height;
  }

  function onKey(e: KeyboardEvent) {
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      selection.selectAll(ids);
      return;
    }
    if (e.key === "Escape") return selection.clear();
    if (e.key === "Enter" && selection.focus != null) {
      e.preventDefault();
      return onopen(selection.focus, ids);
    }
    const step = arrowStep(e, cols, Math.max(1, Math.floor(height / rowH)));
    if (!step) return;
    e.preventDefault();
    const i = selection.move(Number.isFinite(step) ? step : step > 0 ? ids.length : -ids.length, ids, e.shiftKey);
    if (i >= 0) scrollToIndex(i);
  }

  let lastPointer = "mouse";
  function click(e: MouseEvent, index: number) {
    if (lastPointer === "touch") return onopen(ids[index], ids);
    selection.click(e, index, ids);
  }
</script>

<div
  class="grid"
  bind:this={el}
  bind:clientWidth={width}
  bind:clientHeight={height}
  onscroll={() => (scrollTop = el.scrollTop)}
  onkeydown={onKey}
  role="listbox"
  aria-multiselectable="true"
  tabindex="0"
>
  <div class="spacer" style:height="{Math.max(0, totalRows * rowH - GAP + PAD * 2)}px">
    {#each visible as v (v.folder ? `f:${v.folder.path}` : v.id)}
      {@const row = Math.floor(v.slot / cols)}
      {@const col = v.slot % cols}
      {#if v.folder}
        {@const f = v.folder}
        <button
          class="tile folder"
          style:width="{tileW}px"
          style:transform="translate({PAD + col * (tileW + GAP)}px, {PAD + row * rowH}px)"
          title={f.name}
          ondblclick={() => onfolder(f.path)}
          onkeydown={(e) => e.key === "Enter" && onfolder(f.path)}
        >
          <div class="frame" style:height="{tileW}px">
            <span class="folder-icon"><Icon name="folder" size={Math.max(28, tileW * 0.32)} stroke={1.4} /></span>
            <span class="count tabular">{f.count.toLocaleString()}</span>
          </div>
          {#if showNames}<div class="name">{f.name}</div>{/if}
        </button>
      {:else if v.id !== undefined}
        {@const id = v.id}
        {@const b = briefs.get(id)}
        {@const noThumb = b?.thumb === "unsupported" || b?.thumb === "failed" || broken.has(id)}
        <button
          class="tile"
          class:selected={selection.set.has(id)}
          class:focus={selection.focus === id}
          style:width="{tileW}px"
          style:transform="translate({PAD + col * (tileW + GAP)}px, {PAD + row * rowH}px)"
          title={b?.name}
          onpointerdown={(e) => (lastPointer = e.pointerType)}
          onclick={(e) => click(e, v.index)}
          ondblclick={() => onopen(id, ids)}
          tabindex="-1"
        >
          <div class="frame" style:height="{tileW}px">
            {#if noThumb}
              <span class="placeholder"><Icon name={b?.kind === "video" ? "film" : "image"} size={26} stroke={1.4} /></span>
            {:else}
              <img src={thumbUrl(id)} alt="" decoding="async" loading="lazy" draggable="false" onerror={() => broken.add(id)} />
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

<style>
  .grid {
    height: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    contain: strict;
    outline: none;
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
