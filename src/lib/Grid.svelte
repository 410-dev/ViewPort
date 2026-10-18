<script lang="ts">
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { api, thumbUrl, type MediaBrief } from "./api";

  let {
    ids,
    tile,
    showNames,
    overscan,
    resetKey,
  }: { ids: number[]; tile: number; showNames: boolean; overscan: number; resetKey: unknown } = $props();

  const GAP = 6;
  const LABEL = 20;
  const CACHE_LIMIT = 20_000;

  let el: HTMLDivElement;
  let width = $state(0);
  let height = $state(0);
  let scrollTop = $state(0);
  const briefs = new SvelteMap<number, MediaBrief>();
  const broken = new SvelteSet<number>();

  const cols = $derived(Math.max(1, Math.floor((width + GAP) / (tile + GAP))));
  const tileW = $derived(width > 0 ? (width - GAP * (cols - 1)) / cols : tile);
  const rowH = $derived(tileW + (showNames ? LABEL : 0) + GAP);
  const totalRows = $derived(Math.ceil(ids.length / cols));
  const startRow = $derived(Math.max(0, Math.floor(scrollTop / rowH) - overscan));
  const endRow = $derived(Math.min(totalRows, Math.ceil((scrollTop + height) / rowH) + overscan));
  const visible = $derived(
    ids.slice(startRow * cols, endRow * cols).map((id, i) => ({ id, index: startRow * cols + i })),
  );

  $effect(() => {
    resetKey;
    if (el) el.scrollTop = 0;
  });

  // Fetch names/metadata only for what is on screen, shortly after scrolling settles.
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const missing = visible.map((v) => v.id).filter((id) => !briefs.has(id));
    if (missing.length === 0) return;
    clearTimeout(timer);
    timer = setTimeout(async () => {
      if (briefs.size > CACHE_LIMIT) briefs.clear();
      for (const b of await api.mediaBrief(missing)) briefs.set(b.id, b);
    }, 40);
  });

  function duration(ms: number | null) {
    if (ms == null) return "";
    const s = Math.round(ms / 1000);
    const m = Math.floor(s / 60);
    return `${m}:${String(s % 60).padStart(2, "0")}`;
  }
</script>

<div
  class="grid"
  bind:this={el}
  bind:clientWidth={width}
  bind:clientHeight={height}
  onscroll={() => (scrollTop = el.scrollTop)}
>
  <div class="spacer" style:height="{Math.max(0, totalRows * rowH - GAP)}px">
    {#each visible as { id, index } (id)}
      {@const b = briefs.get(id)}
      {@const row = Math.floor(index / cols)}
      {@const col = index % cols}
      {@const noThumb = b?.thumb === "unsupported" || b?.thumb === "failed" || broken.has(id)}
      <div
        class="tile"
        style:width="{tileW}px"
        style:transform="translate({col * (tileW + GAP)}px, {row * rowH}px)"
        title={b?.name}
      >
        <div class="frame" style:height="{tileW}px">
          {#if noThumb}
            <div class="placeholder">{b?.kind === "video" ? "▶" : "◻"}</div>
          {:else}
            <img src={thumbUrl(id)} alt="" decoding="async" draggable="false" onerror={() => broken.add(id)} />
          {/if}
          {#if b?.kind === "video"}
            <span class="badge">{duration(b.duration_ms) || "VIDEO"}</span>
          {/if}
        </div>
        {#if showNames}
          <div class="name">{b?.name ?? ""}</div>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .grid {
    overflow-y: auto;
    overflow-x: hidden;
    height: 100%;
    padding: 0 16px 16px;
    box-sizing: border-box;
    contain: strict;
  }
  .spacer {
    position: relative;
  }
  .tile {
    position: absolute;
    top: 0;
    left: 0;
    will-change: transform;
  }
  .frame {
    position: relative;
    background: var(--tile);
    border-radius: 6px;
    overflow: hidden;
    display: grid;
    place-items: center;
  }
  img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    user-select: none;
  }
  .placeholder {
    font-size: 28px;
    color: var(--muted);
  }
  .badge {
    position: absolute;
    right: 6px;
    bottom: 6px;
    background: #000a;
    color: #fff;
    font-size: 11px;
    padding: 1px 5px;
    border-radius: 4px;
  }
  .name {
    height: 20px;
    line-height: 20px;
    font-size: 12px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
