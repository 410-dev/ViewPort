<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount } from "svelte";
  import { api, mediaUrl, onConvert, type ConvertEvent, type LoopSegment, type MediaBrief, type Settings } from "./api";
  import { t } from "./i18n.svelte";
  import { fraction, tasks } from "./tasks.svelte";
  import { fmtBytes } from "./format";
  import Icon from "./ui/Icon.svelte";
  import { isTyping, matches } from "./keys";
  import InfoPanel from "./panel/InfoPanel.svelte";

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

  // ---- playback compatibility / conversion
  let converting = $state<Record<number, ConvertEvent>>({});
  let convertError = $state<string | null>(null);
  let dismissed = $state<Set<number>>(new Set());
  let srcVersion = $state<Record<number, number>>({});
  let hasFfmpeg = $state(true);
  api.ffmpegStatus().then((s) => (hasFfmpeg = s.available)).catch(() => {});

  const unlistenConvert = onConvert((e) => {
    converting[e.id] = e;
    if (e.done) {
      if (e.error) convertError = e.error;
      else {
        srcVersion[e.id] = Date.now();
        if (brief?.id === e.id) brief = { ...brief, has_variant: true };
      }
      delete converting[e.id];
    }
  });

  const mode = $derived(settings.playback.fallback_mode);
  const showBanner = $derived(
    !!brief &&
      brief.kind === "video" &&
      mode !== "never" &&
      !brief.has_variant &&
      !brief.ignore_warning &&
      !dismissed.has(brief.id) &&
      !converting[brief.id] &&
      settings.playback.suggest_conversion &&
      (brief.compatible === false || error),
  );

  function convert(id: number) {
    convertError = null;
    converting[id] = { id, progress: 0, done: false, error: null };
    api.convertMedia([id]);
  }

  async function ignoreFile(id: number) {
    await api.setIgnoreWarning(id, true);
    if (brief?.id === id) brief = { ...brief, ignore_warning: true };
  }

  // Probe unprobed videos on open; in "always" mode convert anything without a copy.
  $effect(() => {
    const b = brief;
    if (!b || b.kind !== "video") return;
    if (b.compatible === null && hasFfmpeg) {
      api.probeMedia(b.id).then((nb) => {
        if (nb && brief?.id === nb.id) brief = nb;
      });
    }
    if (mode === "always" && !b.has_variant && !converting[b.id] && hasFfmpeg) convert(b.id);
  });

  const src = (id: number) => (srcVersion[id] ? `${mediaUrl(id)}?v=${srcVersion[id]}` : mediaUrl(id));


  // A slow read of the current file shows its progress in the HUD.
  const loadingTask = $derived(tasks.list.find((x) => x.kind === "read" && x.media_id === id && x.state === "running") ?? null);

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
    if (slideshow) {
      // In a slideshow, play/pause pauses the show (and the current video).
      ssPaused = !ssPaused;
      if (video) ssPaused ? video.pause() : video.play();
      return;
    }
    if (!video) return;
    if (video.paused) video.play();
    else video.pause();
  }

  // ---- loop segments
  let segments = $state<LoopSegment[]>([]);
  let activeSeg = $state<LoopSegment | null>(null);
  let showLoops = $state(false);
  let markA = $state<number | null>(null);
  let markB = $state<number | null>(null);
  let segLabel = $state("");
  let segError = $state("");

  $effect(() => {
    const b = brief;
    segments = [];
    activeSeg = null;
    markA = markB = null;
    if (!b || b.kind !== "video") return;
    api.loopSegments(b.id).then((list) => {
      if (brief?.id !== b.id) return;
      segments = list;
      activeSeg = list.find((x) => x.autoplay) ?? null;
      if (activeSeg && video) video.currentTime = activeSeg.start_ms / 1000;
    });
  });

  function playSegment(seg: LoopSegment | null) {
    activeSeg = seg;
    if (seg && video) {
      video.currentTime = seg.start_ms / 1000;
      video.play();
    }
  }

  async function saveSegment(seg?: LoopSegment) {
    segError = "";
    if (!brief) return;
    const toSave =
      seg ??
      ({
        id: 0,
        media_id: brief.id,
        label: segLabel.trim() || `${fmtTime((markA ?? 0) * 1000)}–${fmtTime((markB ?? 0) * 1000)}`,
        start_ms: Math.round((markA ?? 0) * 1000),
        end_ms: Math.round((markB ?? 0) * 1000),
        autoplay: false,
      } satisfies LoopSegment);
    try {
      await api.saveLoopSegment(toSave);
      segments = await api.loopSegments(brief.id);
      if (!seg) {
        markA = markB = null;
        segLabel = "";
      }
    } catch (e: any) {
      segError = e?.message ?? String(e);
    }
  }

  async function deleteSegment(seg: LoopSegment) {
    await api.deleteLoopSegment(seg.id);
    if (activeSeg?.id === seg.id) activeSeg = null;
    segments = segments.filter((x) => x.id !== seg.id);
  }

  function fmtTime(ms: number) {
    const s = Math.max(0, ms / 1000);
    const m = Math.floor(s / 60);
    return `${m}:${(s % 60).toFixed(1).padStart(4, "0")}`;
  }

  // ---- slideshow
  let slideshow = $state(false);
  let ssPaused = $state(false);
  let ssTimer: ReturnType<typeof setTimeout> | undefined;
  let plays = 0;
  let lastTime = 0;
  const ss = $derived(settings.slideshow);

  function toggleSlideshow() {
    slideshow = !slideshow;
    ssPaused = false;
    poke();
  }

  function advance() {
    if (!ss.wrap_around && index === ids.length - 1) {
      slideshow = false;
      return;
    }
    go(1);
  }

  // Images advance on a timer; videos advance from their playback events below.
  $effect(() => {
    clearTimeout(ssTimer);
    plays = 0;
    lastTime = 0;
    if (!slideshow || ssPaused || !brief || brief.id !== id) return;
    if (brief.kind === "image" || error) ssTimer = setTimeout(advance, ss.image_interval_secs * 1000);
  });

  function clipStart() {
    if (!video || !isFinite(video.duration)) return 0;
    const s = ss.clip_start_secs;
    return Math.max(0, s < 0 ? video.duration + s : Math.min(s, video.duration));
  }

  function onLoaded() {
    if (!video) return;
    if (activeSeg) video.currentTime = activeSeg.start_ms / 1000;
    else if (slideshow && ss.video_mode === "clip") video.currentTime = clipStart();
  }

  function onTime() {
    if (!video) return;
    const t = video.currentTime;
    if (activeSeg) {
      if (t >= activeSeg.end_ms / 1000 || t < activeSeg.start_ms / 1000 - 0.25) {
        video.currentTime = activeSeg.start_ms / 1000;
        plays++;
      }
    } else if (t + 0.5 < lastTime) {
      plays++; // the native loop wrapped around
    }
    lastTime = t;
    if (!slideshow || ssPaused) return;
    if (ss.video_mode === "play_count" && plays >= Math.max(1, ss.video_play_count)) advance();
    else if (ss.video_mode === "clip" && !activeSeg && t >= clipStart() + ss.clip_secs) advance();
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
    else if (matches(e, k.toggle_slideshow)) toggleSlideshow();
    else if (matches(e, k.toggle_loops) && isVideo) showLoops = !showLoops;
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
    clearTimeout(ssTimer);
    unlistenConvert.then((f) => f());
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
        src={src(id)}
        autoplay
        loop
        playsinline
        controls={hud}
        onloadedmetadata={onLoaded}
        ontimeupdate={onTime}
        onerror={() => (error = true)}
      ></video>
    {:else if brief}
      <img src={mediaUrl(id)} alt={brief.name} draggable="false" onerror={() => (error = true)} />
    {/if}
  {/key}

  {#if error && !(brief?.kind === "video")}
    <div class="error">{brief?.name}: {t("viewer.cannotDisplay")}</div>
  {/if}

  {#if brief && converting[brief.id]}
    <div class="banner" role="status">
      <span class="spinner"></span>
      <span>{t("viewer.converting")}</span>
      <span class="bar"><i style:width="{converting[brief.id].progress * 100}%"></i></span>
      <span class="tabular">{Math.round(converting[brief.id].progress * 100)}%</span>
      <button class="btn sm" onclick={() => api.cancelConvert(brief!.id)}>{t("common.cancel")}</button>
    </div>
  {:else if showBanner && brief}
    <div class="banner" role="alert" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      {#if hasFfmpeg}
        <span>{t("viewer.mayNotPlay")}</span>
        <button class="btn sm primary" onclick={() => convert(brief!.id)}>{t("viewer.convert")}</button>
      {:else}
        <span>{t("viewer.needFfmpeg")}</span>
      {/if}
      <button class="btn sm" onclick={() => ignoreFile(brief!.id)}>{t("viewer.ignore")}</button>
      <button class="icon-btn sm" aria-label={t("common.close")} onclick={() => (dismissed = new Set([...dismissed, brief!.id]))}><Icon name="x" size={13} /></button>
    </div>
  {/if}
  {#if convertError}
    <div class="banner err" role="alert">
      <span>{t("viewer.convertFailed")}: {convertError}</span>
      <button class="icon-btn sm" aria-label={t("common.close")} onclick={() => (convertError = null)}><Icon name="x" size={13} /></button>
    </div>
  {/if}

  {#if showLoops && brief?.kind === "video"}
    <div class="loops" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <div class="loops-head">
        <strong>{t("viewer.loops")}</strong>
        {#if activeSeg}<button class="btn sm" onclick={() => playSegment(null)}>{t("viewer.stopLoop")}</button>{/if}
      </div>
      <ul>
        {#each segments as seg (seg.id)}
          <li class:active={activeSeg?.id === seg.id}>
            <button class="seg" onclick={() => playSegment(seg)}>
              <span>{seg.label}</span><small>{fmtTime(seg.start_ms)}–{fmtTime(seg.end_ms)}</small>
            </button>
            <button
              class="auto"
              class:on={seg.autoplay}
              title={t("viewer.autoHint")}
              onclick={() => saveSegment({ ...seg, autoplay: !seg.autoplay })}>{t("viewer.auto")}</button
            >
            <button class="icon-btn sm" onclick={() => deleteSegment(seg)} aria-label={t("common.remove")}><Icon name="trash" size={13} /></button>
          </li>
        {/each}
      </ul>
      <div class="marks">
        <button class="btn sm" onclick={() => (markA = video?.currentTime ?? 0)}>A {markA != null ? fmtTime(markA * 1000) : ""}</button>
        <button class="btn sm" onclick={() => (markB = video?.currentTime ?? 0)}>B {markB != null ? fmtTime(markB * 1000) : ""}</button>
        <input class="input" placeholder={t("viewer.label")} bind:value={segLabel} />
        <button class="btn sm primary" disabled={markA == null || markB == null || markB <= markA} onclick={() => saveSegment()}
          >{t("common.save")}</button
        >
      </div>
      {#if segError}<p class="seg-err">{segError}</p>{/if}
    </div>
  {/if}

  {#if showInfo && id !== undefined}
    <div class="side" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <InfoPanel ids={[id]} {onchanged} />
    </div>
  {/if}

  <div class="hud" class:visible={hud}>
    <span class="name">{brief?.name ?? ""}</span>
    <span class="pos">{index + 1} / {ids.length}</span>
    {#if slideshow}
      <span class="ss">{ssPaused ? t("viewer.slideshowPaused") : t("viewer.slideshow")}</span>
    {/if}
    {#if loadingTask}
      {@const f = fraction(loadingTask)}
      <span class="load"><span class="spinner"></span><span class="tabular">{fmtBytes(loadingTask.done)} / {fmtBytes(loadingTask.total)}</span>
        <span class="bar"><i style:width="{(f ?? 0) * 100}%"></i></span></span>
    {/if}
    <span class="tools">
      <button class="hb" class:on={slideshow} onclick={toggleSlideshow} title={t("viewer.slideshow")}><Icon name={slideshow ? "stop" : "play"} size={15} /></button>
      {#if brief?.kind === "video"}
        <button class="hb" class:on={showLoops} onclick={() => (showLoops = !showLoops)} title={t("viewer.loops")}><Icon name="loop" size={15} /></button>
      {/if}
      <button class="hb" class:on={showInfo} onclick={() => (showInfo = !showInfo)} title={t("viewer.info")}><Icon name="info" size={15} /></button>
      <button class="hb" onclick={() => toggleFullscreen()} title={t("viewer.fullscreen")}><Icon name="maximize" size={15} /></button>
      <button class="hb" onclick={close} title={t("common.close")}><Icon name="x" size={15} /></button>
    </span>
  </div>
  <button class="nav prev" class:visible={hud} onclick={() => go(-1)} aria-label={t("viewer.prev")}><Icon name="chevronLeft" size={26} /></button>
  <button class="nav next" class:visible={hud} onclick={() => go(1)} aria-label={t("viewer.next")}><Icon name="chevronRight" size={26} /></button>
</div>

<style>
  .viewer {
    position: fixed;
    inset: 0;
    z-index: 20;
    display: grid;
    place-items: center;
    background: #000;
    color: #eee;
    outline: none;
    touch-action: none;
    user-select: none;
    /* The viewer is always dark. */
    --surface: #17191f;
    --surface-2: #20232b;
    --surface-3: #2a2e38;
    --hover: #2c303b;
    --border: #2b2f39;
    --fg: #ecedf1;
    --fg-2: #c8cbd4;
    --muted: #9095a3;
    --faint: #626877;
    --tile: #1d2028;
    color-scheme: dark;
  }
  .hide-cursor {
    cursor: none;
  }
  img,
  video {
    width: 100%;
    height: 100%;
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .error {
    position: absolute;
    color: var(--danger);
  }
  .banner {
    position: absolute;
    bottom: 76px;
    left: 50%;
    transform: translateX(-50%);
    z-index: 3;
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 10px;
    max-width: calc(100% - 32px);
    min-height: 44px;
    padding: 6px 8px 6px 18px;
    border-radius: var(--radius-pill);
    background: rgb(23 25 31 / 0.92);
    backdrop-filter: blur(10px);
    border: 1px solid var(--border);
    font-size: 13px;
  }
  .banner .bar {
    width: 120px;
  }
  .banner.err {
    bottom: 128px;
    border-color: color-mix(in srgb, var(--danger) 60%, transparent);
  }
  .ss {
    font-size: 12px;
    color: var(--accent);
  }
  .load {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--fg-2);
  }
  .load .bar {
    width: 90px;
  }
  .loops {
    position: absolute;
    left: 16px;
    bottom: 76px;
    z-index: 3;
    width: min(400px, calc(100% - 32px));
    display: grid;
    gap: 10px;
    padding: 14px;
    border-radius: var(--radius-lg);
    background: rgb(23 25 31 / 0.94);
    backdrop-filter: blur(10px);
    border: 1px solid var(--border);
    font-size: 13px;
  }
  .loops-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .loops ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    max-height: 220px;
    overflow-y: auto;
  }
  .loops li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .loops li.active .seg {
    color: var(--accent);
    background: var(--accent-soft);
  }
  .loops .seg {
    flex: 1;
    display: flex;
    justify-content: space-between;
    height: 30px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: inherit;
    text-align: left;
  }
  .loops .seg:hover {
    background: var(--hover);
  }
  .loops small {
    color: var(--muted);
  }
  .auto {
    height: 24px;
    padding: 0 10px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border);
    background: transparent;
    color: var(--muted);
    font-size: 11px;
  }
  .auto.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .marks {
    display: flex;
    gap: 6px;
  }
  .marks .input {
    flex: 1;
    height: var(--h-control-sm);
  }
  .seg-err {
    color: var(--danger);
    margin: 0;
  }
  .side {
    position: absolute;
    top: 56px;
    right: 12px;
    bottom: 12px;
    z-index: 2;
    width: 320px;
    border-radius: var(--radius-lg);
    background: rgb(23 25 31 / 0.94);
    backdrop-filter: blur(10px);
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .hud {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 14px;
    height: 56px;
    padding: 0 12px 0 20px;
    background: linear-gradient(rgb(0 0 0 / 0.66), transparent);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--t-med) var(--ease);
  }
  .hud.visible,
  .nav.visible {
    opacity: 1;
    pointer-events: auto;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pos {
    font-variant-numeric: tabular-nums;
    color: var(--muted);
    font-size: 12px;
  }
  .tools {
    display: flex;
    gap: 2px;
    padding: 3px;
    border-radius: var(--radius-pill);
    background: rgb(23 25 31 / 0.7);
    backdrop-filter: blur(8px);
  }
  .hb {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--fg-2);
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .hb:hover {
    background: var(--hover);
    color: #fff;
  }
  .hb.on {
    color: var(--accent);
  }
  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border: 0;
    border-radius: 50%;
    background: rgb(23 25 31 / 0.6);
    backdrop-filter: blur(6px);
    color: #fff;
    opacity: 0;
    pointer-events: none;
    transition:
      opacity var(--t-med) var(--ease),
      background var(--t-fast) var(--ease);
  }
  .nav:hover {
    background: rgb(40 44 54 / 0.85);
  }
  .prev {
    left: 16px;
  }
  .next {
    right: 16px;
  }
</style>
