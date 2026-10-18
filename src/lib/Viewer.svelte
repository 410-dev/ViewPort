<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount } from "svelte";
  import { api, mediaUrl, type MediaBrief, type Settings } from "./api";
  import { isTyping, matches } from "./keys";
  import InfoPanel from "./InfoPanel.svelte";

  let {
    ids,
    index = $bindable(),
    settings,
    onclose,
    onchanged,
  }: { ids: number[]; index: number; settings: Settings; onclose: () => void; onchanged?: () => void } = $props();

  let showInfo = $state(false);

  const id = $derived(ids[index]);
  let brief = $state<MediaBrief | null>(null);
  let video = $state<HTMLVideoElement | null>(null);
  let fullscreen = $state(false);
  let hud = $state(true);
  let error = $state(false);

  const win = getCurrentWindow();

  // ---- navigation
  function go(delta: number) {
    if (ids.length === 0) return;
    index = (index + delta + ids.length) % ids.length;
  }

  // ---- brief + look-ahead whenever the current item changes
  const preloaded = new Map<number, HTMLImageElement>();
  let shownAt = performance.now();
  let lastId: number | undefined;

  $effect(() => {
    const current = id;
    if (current === undefined) return;
    error = false;
    if (lastId !== undefined && lastId !== current) {
      api.recordView(lastId, Math.round(performance.now() - shownAt)).catch(() => {});
    }
    lastId = current;
    shownAt = performance.now();

    const c = settings.cache;
    const ahead = Array.from({ length: c.prefetch_ahead }, (_, i) => ids[(index + i + 1) % ids.length]);
    const behind = Array.from({ length: c.prefetch_behind }, (_, i) => ids[(index - i - 1 + ids.length) % ids.length]);
    const around = [...new Set([...ahead, ...behind])].filter((x) => x !== current);

    api.mediaBrief([current, ...around]).then((bs) => {
      if (ids[index] !== current) return;
      brief = bs.find((b) => b.id === current) ?? null;
      // Decode neighbouring images in the webview too, so switching is instant.
      const keep = new Set<number>();
      for (const b of bs) {
        if (b.id === current || b.kind !== "image") continue;
        keep.add(b.id);
        if (!preloaded.has(b.id)) {
          const img = new Image();
          img.src = mediaUrl(b.id);
          img.decode().catch(() => {});
          preloaded.set(b.id, img);
        }
      }
      for (const k of preloaded.keys()) if (!keep.has(k)) preloaded.delete(k);
    });
    api.prefetch(around).catch(() => {});
  });

  // ---- video
  $effect(() => {
    if (!video) return;
    video.volume = settings.playback.volume;
    video.muted = settings.playback.muted;
  });

  function seek(seconds: number) {
    if (!video || !isFinite(video.duration)) return;
    video.currentTime = Math.min(Math.max(0, video.currentTime + seconds), video.duration);
  }

  function togglePlay() {
    if (!video) return;
    if (video.paused) video.play();
    else video.pause();
  }

  // ---- fullscreen (the window, so it also hides the OS chrome)
  async function toggleFullscreen(force?: boolean) {
    const next = force ?? !(await win.isFullscreen());
    await win.setFullscreen(next);
    fullscreen = next;
  }

  async function close() {
    if (fullscreen) await toggleFullscreen(false);
    if (lastId !== undefined) api.recordView(lastId, Math.round(performance.now() - shownAt)).catch(() => {});
    onclose();
  }

  // ---- keyboard
  function onKey(e: KeyboardEvent) {
    if (isTyping(e)) return;
    const k = settings.keys;
    const isVideo = brief?.kind === "video";
    let handled = true;
    // Seek bindings are checked first: they are the shifted variants of next/prev.
    if (matches(e, k.seek_forward)) isVideo ? seek(settings.playback.seek_step_secs) : go(1);
    else if (matches(e, k.seek_backward)) isVideo ? seek(-settings.playback.seek_step_secs) : go(-1);
    else if (matches(e, k.next)) go(1);
    else if (matches(e, k.prev)) go(-1);
    else if (matches(e, k.toggle_play)) togglePlay();
    else if (matches(e, k.toggle_fullscreen)) toggleFullscreen();
    else if (matches(e, k.toggle_info)) showInfo = !showInfo;
    else if (matches(e, k.close_viewer)) close();
    else handled = false;
    if (handled) {
      e.preventDefault();
      poke();
    }
  }

  // ---- touch / mouse swipe
  let down: { x: number; y: number; t: number } | null = null;
  function onPointerDown(e: PointerEvent) {
    down = { x: e.clientX, y: e.clientY, t: performance.now() };
  }
  function onPointerUp(e: PointerEvent) {
    if (!down) return;
    const dx = e.clientX - down.x;
    const dy = e.clientY - down.y;
    const fast = performance.now() - down.t < 600;
    down = null;
    if (fast && Math.abs(dx) > 50 && Math.abs(dx) > Math.abs(dy) * 1.5) go(dx < 0 ? 1 : -1);
    else poke();
  }

  // ---- HUD auto-hide
  let hudTimer: ReturnType<typeof setTimeout> | undefined;
  function poke() {
    hud = true;
    clearTimeout(hudTimer);
    hudTimer = setTimeout(() => (hud = false), 2000);
  }

  onMount(() => {
    window.addEventListener("keydown", onKey);
    poke();
  });
  onDestroy(() => {
    window.removeEventListener("keydown", onKey);
    clearTimeout(hudTimer);
  });
</script>

<div
  class="viewer"
  class:hide-cursor={!hud}
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  onpointerdown={onPointerDown}
  onpointerup={onPointerUp}
  onpointermove={poke}
>
  {#key id}
    {#if brief?.kind === "video"}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        src={mediaUrl(id)}
        autoplay
        loop
        playsinline
        controls={hud}
        onerror={() => (error = true)}
      ></video>
    {:else if brief}
      <img src={mediaUrl(id)} alt={brief.name} draggable="false" onerror={() => (error = true)} />
    {/if}
  {/key}

  {#if error}
    <div class="error">{brief?.name}: cannot be displayed</div>
  {/if}

  {#if showInfo && id !== undefined}
    <div class="side" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <InfoPanel ids={[id]} {onchanged} dark />
    </div>
  {/if}

  <div class="hud" class:visible={hud}>
    <span class="name">{brief?.name ?? ""}</span>
    <span class="pos">{index + 1} / {ids.length}</span>
    <button class="icon" onclick={() => (showInfo = !showInfo)} title="Info">ⓘ</button>
    <button class="icon" onclick={() => toggleFullscreen()} title="Fullscreen">⛶</button>
    <button class="icon" onclick={close} title="Close">✕</button>
  </div>
  <button class="nav prev" class:visible={hud} onclick={() => go(-1)} aria-label="Previous">‹</button>
  <button class="nav next" class:visible={hud} onclick={() => go(1)} aria-label="Next">›</button>
</div>

<style>
  .viewer {
    position: fixed;
    inset: 0;
    background: #000;
    display: grid;
    place-items: center;
    z-index: 10;
    outline: none;
    touch-action: none;
    user-select: none;
  }
  .hide-cursor {
    cursor: none;
  }
  img,
  video {
    max-width: 100%;
    max-height: 100%;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .error {
    position: absolute;
    color: #f87171;
  }
  .side {
    position: absolute;
    top: 44px;
    right: 0;
    bottom: 0;
    display: flex;
    z-index: 2;
  }
  .hud {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    background: linear-gradient(#000a, transparent);
    color: #eee;
    opacity: 0;
    transition: opacity 0.2s;
    pointer-events: none;
  }
  .hud.visible,
  .nav.visible {
    opacity: 1;
    pointer-events: auto;
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pos {
    font-variant-numeric: tabular-nums;
    opacity: 0.8;
  }
  .icon {
    background: transparent;
    border: 0;
    color: #eee;
    font-size: 16px;
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    background: #0006;
    border: 0;
    color: #fff;
    font-size: 32px;
    width: 48px;
    height: 72px;
    opacity: 0;
    transition: opacity 0.2s;
    pointer-events: none;
  }
  .prev {
    left: 8px;
  }
  .next {
    right: 8px;
  }
</style>
