<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount } from "svelte";
  import { api, mediaUrl, onConvert, type ConvertEvent, type LoopSegment, type MediaBrief, type Settings } from "./api";
  import { i18n } from "./i18n.svelte";
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

  // ---- playback compatibility / conversion
  type L = { ko: string; en: string };
  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);
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
    <div class="error">{brief?.name}: {lbl({ ko: "표시할 수 없습니다", en: "cannot be displayed" })}</div>
  {/if}

  {#if brief && converting[brief.id]}
    <div class="banner" role="status">
      <span>{lbl({ ko: "고호환 포맷으로 변환 중…", en: "Converting to a compatible format…" })}</span>
      <progress max="1" value={converting[brief.id].progress}></progress>
      <span>{Math.round(converting[brief.id].progress * 100)}%</span>
      <button onclick={() => api.cancelConvert(brief!.id)}>{lbl({ ko: "취소", en: "Cancel" })}</button>
    </div>
  {:else if showBanner && brief}
    <div class="banner" role="alert" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      {#if hasFfmpeg}
        <span>{lbl({ ko: "이 영상은 재생되지 않을 수 있습니다. 고호환 포맷(MP4)으로 변환할까요?", en: "This video may not play. Convert it to a compatible format (MP4)?" })}</span>
        <button class="primary" onclick={() => convert(brief!.id)}>{lbl({ ko: "변환", en: "Convert" })}</button>
      {:else}
        <span>{lbl({ ko: "이 영상을 재생하려면 FFmpeg가 필요합니다. 설정에서 경로를 지정하세요.", en: "FFmpeg is needed to play this video. Set its path in Settings." })}</span>
      {/if}
      <button onclick={() => ignoreFile(brief!.id)}>{lbl({ ko: "이 파일을 무시 목록에 추가", en: "Add this file to the ignore list" })}</button>
      <button class="icon" aria-label="Dismiss" onclick={() => (dismissed = new Set([...dismissed, brief!.id]))}>✕</button>
    </div>
  {/if}
  {#if convertError}
    <div class="banner err" role="alert">
      <span>{lbl({ ko: "변환 실패", en: "Conversion failed" })}: {convertError}</span>
      <button class="icon" onclick={() => (convertError = null)}>✕</button>
    </div>
  {/if}

  {#if showLoops && brief?.kind === "video"}
    <div class="loops" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <div class="loops-head">
        <strong>{lbl({ ko: "구간 반복", en: "Loop segments" })}</strong>
        {#if activeSeg}<button onclick={() => playSegment(null)}>{lbl({ ko: "반복 해제", en: "Stop looping" })}</button>{/if}
      </div>
      <ul>
        {#each segments as seg (seg.id)}
          <li class:active={activeSeg?.id === seg.id}>
            <button class="seg" onclick={() => playSegment(seg)}>
              <span>{seg.label}</span><small>{fmtTime(seg.start_ms)}–{fmtTime(seg.end_ms)}</small>
            </button>
            <label title={lbl({ ko: "열 때 바로 이 구간 재생", en: "Start here when opened" })}>
              <input type="checkbox" checked={seg.autoplay} onchange={(e) => saveSegment({ ...seg, autoplay: e.currentTarget.checked })} />
              {lbl({ ko: "자동", en: "Auto" })}
            </label>
            <button class="icon" onclick={() => deleteSegment(seg)} aria-label="delete">✕</button>
          </li>
        {/each}
      </ul>
      <div class="marks">
        <button onclick={() => (markA = video?.currentTime ?? 0)}>A {markA != null ? fmtTime(markA * 1000) : ""}</button>
        <button onclick={() => (markB = video?.currentTime ?? 0)}>B {markB != null ? fmtTime(markB * 1000) : ""}</button>
        <input placeholder={lbl({ ko: "이름", en: "Label" })} bind:value={segLabel} />
        <button class="primary" disabled={markA == null || markB == null || markB <= markA} onclick={() => saveSegment()}
          >{lbl({ ko: "저장", en: "Save" })}</button
        >
      </div>
      {#if segError}<p class="seg-err">{segError}</p>{/if}
    </div>
  {/if}

  {#if showInfo && id !== undefined}
    <div class="side" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <InfoPanel ids={[id]} {onchanged} dark />
    </div>
  {/if}

  <div class="hud" class:visible={hud}>
    <span class="name">{brief?.name ?? ""}</span>
    <span class="pos">{index + 1} / {ids.length}</span>
    {#if slideshow}
      <span class="ss">{ssPaused ? lbl({ ko: "슬라이드쇼 일시정지", en: "Slideshow paused" }) : lbl({ ko: "슬라이드쇼", en: "Slideshow" })}</span>
    {/if}
    <button class="icon" onclick={toggleSlideshow} title={lbl({ ko: "슬라이드쇼", en: "Slideshow" })}>{slideshow ? "⏹" : "▶"}</button>
    {#if brief?.kind === "video"}
      <button class="icon" class:on={showLoops} onclick={() => (showLoops = !showLoops)} title={lbl({ ko: "구간 반복", en: "Loop segments" })}>⟲</button>
    {/if}
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
  .banner {
    position: absolute;
    bottom: 72px;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 10px;
    background: #1d1d1df2;
    color: #eee;
    border: 1px solid #444;
    border-radius: 8px;
    padding: 8px 12px;
    font-size: 13px;
    max-width: calc(100% - 32px);
    flex-wrap: wrap;
    z-index: 3;
  }
  .banner.err {
    border-color: #f87171;
    bottom: 120px;
  }
  .banner button {
    background: #2a2a2a;
    border-color: #555;
    color: #eee;
  }
  .banner button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .ss {
    font-size: 12px;
    color: #9cf;
  }
  .icon.on {
    color: #60a5fa;
  }
  .loops {
    position: absolute;
    left: 16px;
    bottom: 72px;
    width: min(380px, calc(100% - 32px));
    background: #161616f0;
    border: 1px solid #333;
    border-radius: 8px;
    color: #eee;
    padding: 10px;
    font-size: 13px;
    display: grid;
    gap: 8px;
    z-index: 3;
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
    max-height: 200px;
    overflow-y: auto;
  }
  .loops li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .loops li.active .seg {
    color: #60a5fa;
  }
  .loops .seg {
    flex: 1;
    display: flex;
    justify-content: space-between;
    background: transparent;
    border: 0;
    color: inherit;
    text-align: left;
  }
  .loops small {
    color: #aaa;
  }
  .loops label {
    display: flex;
    align-items: center;
    gap: 2px;
    font-size: 12px;
  }
  .marks {
    display: flex;
    gap: 6px;
  }
  .marks input {
    flex: 1;
    min-width: 0;
    background: #222;
    color: #eee;
    border: 1px solid #444;
    border-radius: 6px;
    padding: 4px 6px;
  }
  .loops button {
    background: #2a2a2a;
    border-color: #555;
    color: #eee;
  }
  .loops button.primary {
    background: var(--accent);
    border-color: var(--accent);
  }
  .seg-err {
    color: #f87171;
    margin: 0;
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
