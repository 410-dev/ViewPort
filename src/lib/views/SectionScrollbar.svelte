<script lang="ts">
  /**
   * A draggable scrollbar drawn over the right edge of a view, replacing the native one.
   * Section starts are horizontal lines on its track; while dragging, a bubble names the
   * section at the top of the view (`label`). The thumb's centre sits on a section's line
   * when that section is at the top.
   */
  let {
    el,
    scrollTop,
    viewH,
    contentH,
    lines,
    label,
    inset = 0,
  }: {
    el: HTMLElement | undefined;
    scrollTop: number;
    viewH: number;
    contentH: number;
    /** Content offsets (px) where sections start. */
    lines: number[];
    /** The bubble text for a scroll position. */
    label: (top: number) => string;
    /** Space kept free at the top (a sticky header). */
    inset?: number;
  } = $props();

  const MIN_THUMB = 40;
  /** Lines closer than this on the track are drawn once. */
  const MIN_GAP = 3;

  let trackEl = $state<HTMLDivElement>();
  let dragging = $state(false);
  let grab = 0;

  const trackH = $derived(Math.max(0, viewH - inset - 8));
  const maxScroll = $derived(Math.max(0, contentH - viewH));
  const thumbH = $derived(contentH > 0 ? Math.min(trackH, Math.max(MIN_THUMB, (trackH * viewH) / contentH)) : trackH);
  const room = $derived(Math.max(1, trackH - thumbH));
  const thumbY = $derived(maxScroll > 0 ? (Math.min(scrollTop, maxScroll) / maxScroll) * room : 0);
  const marks = $derived.by(() => {
    if (maxScroll <= 0) return [];
    const out: number[] = [];
    let last = -Infinity;
    for (const top of lines) {
      const y = (Math.min(top, maxScroll) / maxScroll) * room + thumbH / 2;
      if (y - last < MIN_GAP) continue;
      out.push(y);
      last = y;
    }
    return out;
  });

  function scrollToThumb(y: number) {
    if (!el) return;
    el.scrollTop = (Math.min(room, Math.max(0, y)) / room) * maxScroll;
  }

  function down(e: PointerEvent) {
    if (e.button !== 0 || !trackEl) return;
    e.preventDefault();
    e.stopPropagation();
    const y = e.clientY - trackEl.getBoundingClientRect().top;
    // On the thumb: keep where it was grabbed. On the track: centre the thumb there.
    // Touch takes the thumb only, with some room around it (the CSS hit area).
    const slack = e.pointerType === "touch" ? 12 : 0;
    grab = y >= thumbY - slack && y <= thumbY + thumbH + slack ? Math.min(thumbH, Math.max(0, y - thumbY)) : thumbH / 2;
    scrollToThumb(y - grab);
    dragging = true;
    trackEl.setPointerCapture(e.pointerId);
  }

  function move(e: PointerEvent) {
    if (!dragging || !trackEl) return;
    scrollToThumb(e.clientY - trackEl.getBoundingClientRect().top - grab);
  }

  function up() {
    dragging = false;
  }

  const text = $derived(dragging ? label(scrollTop) : "");
</script>

{#if maxScroll > 0 && trackH > MIN_THUMB}
  <div
    class="track"
    class:dragging
    bind:this={trackEl}
    style:top="{inset + 4}px"
    style:height="{trackH}px"
    role="presentation"
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    onlostpointercapture={up}
  >
    {#each marks as y, i (i)}
      <span class="line" style:transform="translateY({y}px)"></span>
    {/each}
    <span class="thumb" style:height="{thumbH}px" style:transform="translateY({thumbY}px)"></span>
    {#if text}
      <span class="bubble" style:transform="translateY({Math.max(0, thumbY + thumbH / 2 - 15)}px)">{text}</span>
    {/if}
  </div>
{/if}

<style>
  .track {
    position: absolute;
    right: 2px;
    z-index: 3;
    width: 14px;
    touch-action: none;
    user-select: none;
    outline: none;
  }
  .line {
    position: absolute;
    top: 0;
    right: 3px;
    width: 8px;
    height: 1px;
    background: var(--border-strong);
    pointer-events: none;
  }
  .thumb {
    position: absolute;
    top: 0;
    right: 3px;
    width: 6px;
    border-radius: var(--radius-pill);
    background: var(--faint);
    opacity: 0.7;
    transition:
      width var(--t-fast) var(--ease),
      opacity var(--t-fast) var(--ease),
      background var(--t-fast) var(--ease);
  }
  .track:hover .thumb,
  .track.dragging .thumb {
    width: 10px;
    right: 2px;
    opacity: 1;
  }
  .track.dragging .thumb {
    background: var(--accent);
  }
  .bubble {
    position: absolute;
    top: 0;
    right: 22px;
    height: 30px;
    padding: 0 12px;
    border-radius: var(--radius-pill);
    background: var(--surface-3);
    box-shadow: var(--shadow);
    color: var(--fg);
    font-size: 13px;
    font-weight: 600;
    line-height: 30px;
    white-space: nowrap;
    max-width: 60vw;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
  /* Touch: a wider grip, always at full strength. */
  @media (hover: none) {
    /* Only the thumb takes touches: the track lies over the last column of tiles, and a
       swipe or tap starting there jumped the list instead of scrolling or opening. */
    .track {
      width: 24px;
      pointer-events: none;
    }
    .thumb {
      width: 8px;
      opacity: 1;
      pointer-events: auto;
    }
    .thumb::before {
      content: "";
      position: absolute;
      inset: -12px -8px -12px -14px;
    }
    .track.dragging .thumb {
      width: 12px;
    }
    .bubble {
      right: 30px;
    }
  }
</style>
