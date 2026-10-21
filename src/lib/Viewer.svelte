<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount } from "svelte";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import {
    api,
    displayUrl,
    mediaUrl,
    thumbUrl,
    platformApi,
    onConvert,
    FAVORITE_FIELD,
    type ConvertEvent,
    type LoopSegment,
    type MediaBrief,
    type Settings,
  } from "./api";
  import { invalidate } from "./views/briefs.svelte";
  import { broadcast } from "./sync";
  import { t } from "./i18n.svelte";
  import { fraction, tasks } from "./tasks.svelte";
  import { fmtBytes } from "./format";
  import Icon from "./ui/Icon.svelte";
  import { isTyping, matches } from "./keys";
  import InfoPanel from "./panel/InfoPanel.svelte";
  import { audible, audio, claimFocus, setMuted, setVolume } from "./audio.svelte";
  import VolumeControl from "./ui/VolumeControl.svelte";
  import { prefs } from "./prefs.svelte";
  import { fullscreenChanged, immersive } from "./immersive.svelte";
  import { ZoomPan } from "./zoom.svelte";
  import { onBack } from "./back.svelte";
  import { isMobile } from "./platform";
  import { reportBroken } from "./views/briefs.svelte";

  /**
   * One viewer pane. `ViewerShell` lays out up to `windows.max_viewer_panes` of them;
   * only the `active` pane takes keyboard input (and, with `audio.focus_only`, plays sound).
   */
  let {
    ids,
    index = $bindable(),
    settings,
    onclose,
    onchanged,
    active = true,
    panes = 1,
    onactivate,
    onsplit,
    onpop,
    onbrowse,
  }: {
    ids: number[];
    index: number;
    settings: Settings;
    onclose: () => void;
    onchanged?: () => void;
    active?: boolean;
    /** Panes in this window. */
    panes?: number;
    onactivate?: () => void;
    /** Adds a pane; absent when no more fit. */
    onsplit?: () => void;
    /** Moves this pane to its own window; absent on mobile. */
    onpop?: () => void;
    /** Shows an explorer in this pane to pick other items (split view). */
    onbrowse?: () => void;
  } = $props();

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
  // Without FFmpeg the OS may still probe videos (Android), but nothing can convert.
  let canProbe = $state(true);
  api
    .ffmpegStatus()
    .then((s) => {
      hasFfmpeg = s.available;
      canProbe = s.available || s.os_media;
    })
    .catch(() => {});

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
    if (b.compatible === null && canProbe) {
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
  /** Briefs of the current item and its neighbours, so the swipe strip can draw them. */
  const known = new SvelteMap<number, MediaBrief>();

  // ---- favorite: the toolbar heart, and on touch screens a swipe up (on) or down (off)
  /** Favorites changed in this viewer, ahead of the briefs. */
  const favs = new SvelteMap<number, boolean>();
  const isFav = $derived(favs.get(id) ?? known.get(id)?.favorite ?? (brief?.id === id ? brief.favorite : false));
  /** The heart shown briefly after a change (true = on). */
  let favFlash = $state<boolean | null>(null);
  let favTimer: ReturnType<typeof setTimeout> | undefined;

  function setFav(on: boolean) {
    const target = id;
    if (target === undefined) return;
    favFlash = on;
    clearTimeout(favTimer);
    favTimer = setTimeout(() => (favFlash = null), 700);
    if (isFav === on) return;
    favs.set(target, on);
    api
      .setValues([target], FAVORITE_FIELD, on ? [1] : [])
      .then(() => {
        invalidate([target]);
        broadcast("media", [target]);
        onchanged?.();
      })
      .catch(() => favs.delete(target));
  }

  // Huge images show a display copy. Zooming in past its pixels loads a copy twice as
  // large, and so on up to the original, so a 200-megapixel photo is never decoded whole
  // unless the zoom really needs it.
  const displayMax = () => prefs.app?.viewer.display_max_px ?? 4096;
  /**
   * Edge of the copy shown before zooming: no more than the screen needs (its longer edge
   * in device pixels, rounded up to `viewer.fit_screen_step_px`). Sliding in a picture
   * several times the screen's size means decoding and rasterizing it on the fly, which
   * shows as black or torn tiles. The window's size, not the pane's, keeps the URL (and
   * so the decoded copy) the same in split view and after rotating.
   */
  const defaultTier = () => {
    const max = displayMax();
    const v = prefs.app?.viewer;
    if (max <= 0 || !(v?.fit_screen ?? true)) return max;
    const step = Math.max(64, v?.fit_screen_step_px ?? 512);
    const screen = Math.ceil((Math.max(innerWidth, innerHeight) * devicePixelRatio) / step) * step;
    return Math.min(max, screen);
  };
  /** Edge in px of the copy shown per id (absent: `displayMax`; 0: the original). */
  const tiers = new SvelteMap<number, number>();
  const upgrading = new Set<number>();
  /** Items whose picture has loaded; until then the thumbnail stands in. */
  const loaded = new SvelteSet<number>();
  /** Largest copy the backend makes (`media::display_key`); beyond it, the original. */
  const MAX_COPY_PX = 16383;
  /** Formats without transparency: a thumbnail underneath never shows through. */
  const opaque = (b: MediaBrief) => ["jpg", "jpeg", "heic", "heif", "hif"].includes(b.ext.toLowerCase());
  const imageSrc = (b: MediaBrief) => {
    const tier = tiers.get(b.id);
    if (tier === undefined) return displayUrl(b, defaultTier());
    return tier === 0 ? mediaUrl(b.id) : mediaUrl(b.id, tier);
  };
  // A file that will not load may be gone: check, so searches stop listing it.
  $effect(() => {
    if (error && id !== undefined) reportBroken(id);
  });
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
    // The direct neighbours always: the swipe strip draws them.
    const n = ids.length;
    const around = [...new Set([ids[(index + 1) % n], ids[(index - 1 + n) % n], ...ahead, ...behind])].filter((x) => x !== current);

    api.mediaBrief([current, ...around]).then((bs) => {
      for (const b of bs) known.set(b.id, b);
      if (ids[index] !== current) return;
      brief = bs.find((b) => b.id === current) ?? null;
      // Decode neighbouring images in the webview too, so switching is instant.
      const keep = new Set<number>();
      for (const b of bs) {
        if (b.id === current || b.kind !== "image") continue;
        keep.add(b.id);
        if (!preloaded.has(b.id)) {
          const img = new Image();
          img.src = imageSrc(b);
          img.decode().catch(() => {});
          preloaded.set(b.id, img);
        }
      }
      for (const k of preloaded.keys()) if (!keep.has(k)) preloaded.delete(k);
    });
    api.prefetch(around).catch(() => {});
  });

  // ---- video
  // Master audio: one volume and mute for every window and pane.
  const wantMuted = $derived(!audible(active));
  $effect(() => {
    if (!video) return;
    video.volume = audio.volume;
    video.muted = wantMuted;
  });

  /** The native controls changed volume or mute: make that the master state. */
  function onVolumeChange() {
    if (!video) return;
    if (Math.abs(video.volume - audio.volume) > 0.01) setVolume(video.volume);
    if (video.muted !== wantMuted) {
      if (!video.muted) {
        onactivate?.();
        claimFocus();
        setMuted(false);
      } else setMuted(true);
    }
  }

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
    if (isMobile) {
      // Mobile: the app always fills the screen; fullscreen hides the system bars.
      const next = force ?? !fullscreen;
      await platformApi.setSystemFullscreen(next).catch(() => {});
      fullscreen = next;
      fullscreenChanged(next);
      return;
    }
    const next = force ?? !(await win.isFullscreen());
    await win.setFullscreen(next);
    fullscreen = next;
    fullscreenChanged(next);
  }

  async function close() {
    if (fullscreen && panes <= 1) await toggleFullscreen(false);
    if (lastId !== undefined) api.recordView(lastId, Math.round(performance.now() - shownAt)).catch(() => {});
    onclose();
  }

  // ---- keyboard
  // A single Escape closes the viewer (the panic key is a fast double press). Bundles saved
  // while Escape was a single-press panic key still have that version's default; upgrade it.
  const closeKeys = $derived(
    settings.keys.close_viewer.join() === "Backspace,q" ? ["Escape", "Backspace", "q"] : settings.keys.close_viewer,
  );

  function onKey(e: KeyboardEvent) {
    if (!active || isTyping(e)) return;
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
    else if (matches(e, closeKeys)) close();
    else if (matches(e, k.toggle_immersive)) immersive.on = !immersive.on;
    else if (matches(e, k.zoom_in) && zoomable) zoom.zoomBy(step());
    else if (matches(e, k.zoom_out) && zoomable) zoom.zoomBy(1 / step());
    else if (matches(e, k.zoom_reset) && zoomable) zoom.reset();
    else handled = false;
    if (handled) {
      e.preventDefault();
      poke();
    }
  }

  // ---- zoom, pan and gestures (images)
  // Fit-to-screen is scale 1. Zoomed: dragging pans. Not zoomed: a fast horizontal swipe
  // changes item, and on touch screens a tap goes to the next one. Two fingers pinch.
  const zoom = new ZoomPan(() => prefs.app?.viewer.max_zoom ?? 8);
  const step = () => Math.max(1.01, prefs.app?.viewer.zoom_step ?? 1.25);
  // `brief` lags behind `id` until it loads; a slid-in neighbour is already known.
  const zoomable = $derived((known.get(id)?.kind ?? (brief?.id === id ? brief.kind : null)) === "image" && !error);
  let stageEl = $state<HTMLDivElement | null>(null);
  let stageW = $state(0);
  let stageH = $state(0);
  $effect(() => {
    zoom.stage = { w: stageW, h: stageH };
  });
  // Natural image sizes by id: a neighbour's image is already loaded when it slides in.
  const naturals = new Map<number, { w: number; h: number }>();
  $effect(() => {
    const current = id;
    zoom.reset();
    zoom.natural = naturals.get(current) ?? { w: 0, h: 0 };
  });

  // Zoomed past the shown copy's own pixels: load the next size, swap once decoded.
  $effect(() => {
    const current = id;
    const n = zoom.natural;
    const scale = zoom.scale;
    if (!zoomable || !n.w || upgrading.has(current)) return;
    const tier = tiers.get(current) ?? defaultTier();
    // Copies come out within a few percent of their size; a smaller picture is the whole image.
    if (tier <= 0 || Math.max(n.w, n.h) < tier * 0.9) return;
    const fit = Math.min(stageW / n.w, stageH / n.h);
    if (fit * scale * devicePixelRatio <= 1.05) return;
    const next = tier * 2 > MAX_COPY_PX ? 0 : tier * 2;
    upgrading.add(current);
    const img = new Image();
    img.src = next ? mediaUrl(current, next) : mediaUrl(current);
    img
      .decode()
      .then(() => tiers.set(current, next))
      .catch(() => {})
      .finally(() => upgrading.delete(current));
  });

  /** Event position relative to the stage center. */
  function local(x: number, y: number) {
    const r = stageEl!.getBoundingClientRect();
    return { x: x - r.left - r.width / 2, y: y - r.top - r.height / 2 };
  }

  const pointers = new Map<number, { x: number; y: number }>();
  let gesture: { x: number; y: number; t: number; type: string; moved: boolean; pinched: boolean; swipe: boolean } | null = null;
  /** Vertical drag offset while swiping for favorite (touch). */
  let pull = $state(0);
  const favSwipe = (g: { type: string } | null) => g?.type === "touch" && (prefs.app?.viewer.swipe_favorite ?? true);
  let pinch: { dist: number; scale: number } | null = null;
  const distance = () => {
    const [a, b] = [...pointers.values()];
    return Math.hypot(a.x - b.x, a.y - b.y);
  };

  // Back button: leave immersive mode first, then close this pane.
  $effect(() => {
    if (active)
      return onBack(() => {
        if (immersive.on) immersive.on = false;
        else if (showInfo) showInfo = false;
        else close();
      });
  });

  // ---- swipe strip: the previous and next items sit beside the current one and the strip
  // follows the finger; on release it slides to the neighbour or springs back.
  let swipe = $state(0);
  /** Whole device pixels: a fractional offset blurs the picture and shows tile seams. */
  const snap = (v: number) => Math.round(v * devicePixelRatio) / devicePixelRatio;
  let sliding = $state(false);
  let slideTimer: ReturnType<typeof setTimeout> | undefined;
  let slideDone: (() => void) | null = null;
  let lock: "h" | "v" | null = null;

  const strip = $derived.by(() => {
    const n = ids.length;
    if (n === 0 || id === undefined) return [];
    const out = [{ id, pos: 0 }];
    if (n === 1) return out;
    const next = ids[(index + 1) % n];
    const prev = ids[(index - 1 + n) % n];
    // Two items: the other one is on whichever side the finger pulls it from.
    if (next === prev) out.push({ id: next, pos: swipe > 0 ? -1 : 1 });
    else out.push({ id: prev, pos: -1 }, { id: next, pos: 1 });
    return out;
  });

  const slideMs = () => (prefs.app?.ui.reduce_motion ? 0 : (prefs.app?.viewer.swipe_ms ?? 220));

  /** Slides to the neighbour in `dir` (1 next, -1 previous) or back to the current one (0). */
  function settle(dir: number) {
    finishSlide();
    if (ids.length < 2) dir = 0;
    const ms = slideMs();
    if (ms <= 0 || stageW === 0) {
      swipe = 0;
      if (dir) go(dir);
      return;
    }
    sliding = true;
    swipe = -dir * stageW;
    slideDone = () => {
      sliding = false;
      swipe = 0;
      if (dir) go(dir);
    };
    slideTimer = setTimeout(finishSlide, ms);
  }

  /** Ends a running slide at once (on a new touch, or when its time is up). */
  function finishSlide() {
    clearTimeout(slideTimer);
    const done = slideDone;
    slideDone = null;
    done?.();
  }

  // A tap waits out the double-tap interval, so a double tap zooms without first moving on.
  let tapTimer: ReturnType<typeof setTimeout> | undefined;
  let lastPointerType = "mouse";

  function onTap(e: PointerEvent) {
    if (tapTimer !== undefined) {
      clearTimeout(tapTimer);
      tapTimer = undefined;
      toggleZoomAt(e.clientX, e.clientY);
      return;
    }
    tapTimer = setTimeout(() => {
      tapTimer = undefined;
      if (!zoom.zoomed && (prefs.app?.viewer.tap_to_advance ?? true)) settle(1);
    }, prefs.app?.viewer.double_tap_ms ?? 280);
  }

  function toggleZoomAt(x: number, y: number) {
    if (!zoomable) return;
    if (zoom.zoomed) return zoom.reset();
    const p = local(x, y);
    zoom.zoomTo(prefs.app?.viewer.double_click_zoom ?? 2.5, p.x, p.y);
  }

  function onPointerDown(e: PointerEvent) {
    lastPointerType = e.pointerType;
    if (e.pointerType === "touch") poke();
    finishSlide();
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    // Capturing over a video would take drags away from its native seek bar.
    if (zoomable) (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    if (pointers.size === 1) {
      lock = null;
      // A drag starting on a video's control bar belongs to the controls, not to swiping.
      const bottom = stageEl!.getBoundingClientRect().bottom;
      const onControls = brief?.kind === "video" && showHud && e.clientY > bottom - 80;
      gesture = {
        x: e.clientX,
        y: e.clientY,
        t: performance.now(),
        type: e.pointerType,
        moved: false,
        pinched: false,
        swipe: !onControls,
      };
    } else if (pointers.size === 2 && zoomable) {
      pinch = { dist: distance(), scale: zoom.scale };
      if (gesture) gesture.pinched = true;
      swipe = 0;
    }
  }

  function onPointerMove(e: PointerEvent) {
    poke();
    const prev = pointers.get(e.pointerId);
    if (!prev) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (gesture && Math.hypot(e.clientX - gesture.x, e.clientY - gesture.y) > 8) gesture.moved = true;
    if (pinch && pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      const mid = local((a.x + b.x) / 2, (a.y + b.y) / 2);
      zoom.zoomTo((pinch.scale * distance()) / Math.max(1, pinch.dist), mid.x, mid.y);
    } else if (pointers.size === 1 && zoom.zoomed) {
      zoom.panBy(e.clientX - prev.x, e.clientY - prev.y);
    } else if (pointers.size === 1 && gesture?.swipe && !gesture.pinched) {
      const dx = e.clientX - gesture.x;
      const dy = e.clientY - gesture.y;
      if (!lock && gesture.moved) lock = Math.abs(dx) > Math.abs(dy) ? "h" : "v";
      const vertical = lock === "v" && favSwipe(gesture);
      if ((lock === "h" && ids.length > 1) || vertical) {
        // Videos are not captured on pointerdown (their seek bar needs drags); capture now.
        const el = e.currentTarget as HTMLElement;
        if (!el.hasPointerCapture(e.pointerId)) el.setPointerCapture(e.pointerId);
      }
      if (lock === "h" && ids.length > 1) swipe = snap(dx);
      // The item follows a vertical drag at a third of the distance.
      else if (vertical) pull = snap(dy / 3);
    }
  }

  function onPointerUp(e: PointerEvent) {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size > 0 || !gesture) return;
    const g = gesture;
    gesture = null;
    const dx = e.clientX - g.x;
    const dt = Math.max(1, performance.now() - g.t);
    if (pull !== 0 || (lock === "v" && favSwipe(g))) {
      pull = 0;
      const dy = e.clientY - g.y;
      // Far enough (the same fraction as sideways swipes), or flicked.
      const far = Math.abs(dy) > (prefs.app?.viewer.swipe_threshold ?? 0.2) * stageH;
      const flick = Math.abs(dy) > 30 && Math.abs(dy) / dt > 0.5;
      if (lock === "v" && e.type !== "pointercancel" && (far || flick)) setFav(dy < 0);
      return;
    }
    if (lock === "h" && e.type !== "pointercancel") {
      // Far enough, or flicked: slide on; otherwise spring back.
      const far = Math.abs(dx) > (prefs.app?.viewer.swipe_threshold ?? 0.2) * stageW;
      const flick = Math.abs(dx) > 30 && Math.abs(dx) / dt > 0.5;
      return settle(far || flick ? (dx < 0 ? 1 : -1) : 0);
    }
    if (swipe !== 0) return settle(0);
    if (g.pinched) return;
    // Taps: touch only (a mouse double click comes as `dblclick`). Zoomed, a single tap
    // does nothing and a double tap zooms back out.
    if (g.type === "touch" && zoomable && !g.moved && dt < 600) onTap(e);
    else poke();
  }

  function onDoubleClick(e: MouseEvent) {
    // Touch double taps are recognised in `onTap`.
    if (lastPointerType !== "touch") toggleZoomAt(e.clientX, e.clientY);
  }

  function onWheel(e: WheelEvent) {
    if (!zoomable || !(prefs.app?.viewer.wheel_zoom ?? true)) return;
    e.preventDefault();
    const p = local(e.clientX, e.clientY);
    zoom.zoomBy(Math.pow(step(), -e.deltaY / 100), p.x, p.y);
  }

  // Non-passive, so zooming does not also scroll anything.
  $effect(() => {
    const el = stageEl;
    if (!el) return;
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  });

  // Immersive mode hides every control.
  const showHud = $derived(hud && !immersive.on);

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
    clearTimeout(tapTimer);
    clearTimeout(slideTimer);
  });
</script>

<div
  class="viewer"
  class:hide-cursor={!showHud}
  class:active={active && panes > 1}
  role="dialog"
  aria-modal="true"
  tabindex="-1"
  onpointerdown={() => onactivate?.()}
  onpointermove={poke}
>
  <div
    class="stage"
    class:zoomed={zoom.zoomed}
    bind:this={stageEl}
    bind:clientWidth={stageW}
    bind:clientHeight={stageH}
    role="presentation"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    ondblclick={onDoubleClick}
  >
    <!-- Keyed by id, so an image sliding in keeps its element: no reload, no flash. -->
    <div class="strip" class:sliding style:transform="translate({swipe}px, {pull}px)" style:--slide-ms="{slideMs()}ms">
      {#each strip as s (s.id)}
        {@const b = known.get(s.id) ?? (brief?.id === s.id ? brief : null)}
        <div class="slot" style:transform="translateX({s.pos * 100}%)">
          {#if s.pos === 0 && b?.kind === "video"}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              bind:this={video}
              src={src(s.id)}
              poster={thumbUrl(s.id)}
              autoplay
              loop
              playsinline
              controls={showHud && swipe === 0}
              onloadedmetadata={onLoaded}
              onvolumechange={onVolumeChange}
              ontimeupdate={onTime}
              onerror={() => (error = true)}
            ></video>
          {:else if b?.kind === "nonmedia"}
            <div class="nomedia"><Icon name="file" size={48} stroke={1.2} /><span>{b.name}</span><small>{t("panel.noPreview")}</small></div>
          {:else if b}
            <!-- The thumbnail stands in until the picture loads, and under an opaque neighbour
                 while it slides in, so tiles not yet drawn show it instead of black. -->
            {#if b.kind === "image" && (!loaded.has(s.id) || ((s.pos !== 0 || sliding) && opaque(b)))}
              <img class="ph" src={thumbUrl(s.id)} alt="" draggable="false" style:transform={s.pos === 0 ? zoom.transform : null} />
            {/if}
            <!-- Neighbouring videos show their thumbnail until they become current. -->
            <img
              src={b.kind === "video" ? thumbUrl(s.id) : imageSrc(b)}
              alt={b.name}
              draggable="false"
              style:transform={s.pos === 0 ? zoom.transform : null}
              onload={(e) => {
                if (b.kind !== "image") return;
                loaded.add(s.id);
                const img = e.currentTarget as HTMLImageElement;
                const size = { w: img.naturalWidth, h: img.naturalHeight };
                naturals.set(s.id, size);
                if (s.id === id) zoom.natural = size;
              }}
              onerror={() => s.id === id && (error = true)}
            />
          {/if}
        </div>
      {/each}
    </div>
  </div>

  {#if error && !(brief?.kind === "video")}
    <div class="error">{brief?.name}: {t("viewer.cannotDisplay")}</div>
  {/if}

  {#if immersive.on}
    <!-- Immersive: no banners. -->
  {:else if brief && converting[brief.id]}
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
        <span>{canProbe ? t("viewer.unplayableHere") : t("viewer.needFfmpeg")}</span>
      {/if}
      <button class="btn sm" onclick={() => ignoreFile(brief!.id)}>{t("viewer.ignore")}</button>
      <button class="icon-btn sm" aria-label={t("common.close")} onclick={() => (dismissed = new Set([...dismissed, brief!.id]))}><Icon name="x" size={13} /></button>
    </div>
  {/if}
  {#if favFlash !== null}
    {#key favFlash}
      <div class="fav-flash" class:on={favFlash} aria-live="polite">
        <Icon name="heart" size={52} stroke={1.6} /><span>{t(favFlash ? "viewer.favOn" : "viewer.favOff")}</span>
      </div>
    {/key}
  {/if}
  {#if convertError && !immersive.on}
    <div class="banner err" role="alert">
      <span>{t("viewer.convertFailed")}: {convertError}</span>
      <button class="icon-btn sm" aria-label={t("common.close")} onclick={() => (convertError = null)}><Icon name="x" size={13} /></button>
    </div>
  {/if}

  {#if showLoops && brief?.kind === "video" && !immersive.on}
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

  {#if showInfo && id !== undefined && !immersive.on}
    <!-- Tapping beside the panel closes it, and never reaches the picture or the arrows. -->
    <div
      class="side-scrim"
      role="presentation"
      onpointerdown={(e) => e.stopPropagation()}
      onpointerup={(e) => e.stopPropagation()}
      onclick={() => (showInfo = false)}
    ></div>
    <div class="side" role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <InfoPanel ids={[id]} {onchanged} />
    </div>
  {/if}

  <div class="hud" class:visible={showHud}>
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
      <button class="hb" onclick={close} title={t("common.close")}><Icon name="x" size={15} /></button>
    </span>
  </div>
  <!-- Everything but "close" sits at the bottom, above a video's own control bar. -->
  <div
    class="dock"
    class:visible={showHud}
    class:above-controls={brief?.kind === "video"}
    role="presentation"
    onpointerdown={(e) => e.stopPropagation()}
    onpointerup={(e) => e.stopPropagation()}
  >
    <span class="tools vol">
      <VolumeControl dark />
    </span>
    <span class="tools">
      <button class="hb" class:on={slideshow} onclick={toggleSlideshow} title={t("viewer.slideshow")}><Icon name={slideshow ? "stop" : "play"} size={15} /></button>
      {#if brief?.kind === "video"}
        <button class="hb" class:on={showLoops} onclick={() => (showLoops = !showLoops)} title={t("viewer.loops")}><Icon name="loop" size={15} /></button>
      {/if}
      <button class="hb fav" class:on={isFav} aria-pressed={isFav} onclick={() => setFav(!isFav)} title={t("viewer.favorite")}
        ><Icon name="heart" size={15} /></button
      >
      <button class="hb" class:on={showInfo} onclick={() => (showInfo = !showInfo)} title={t("viewer.info")}><Icon name="info" size={15} /></button>
      {#if onbrowse}
        <button class="hb" onclick={onbrowse} title={t("viewer.browse")}><Icon name="search" size={15} /></button>
      {/if}
      {#if onsplit}
        <button class="hb" onclick={onsplit} title={t("viewer.split")}><Icon name="columns" size={15} /></button>
      {/if}
      {#if onpop}
        <button class="hb" onclick={onpop} title={t("viewer.popOut")}><Icon name="external" size={15} /></button>
      {/if}
      <button class="hb" onclick={() => (immersive.on = true)} title={t("viewer.immersive")}><Icon name="eyeOff" size={15} /></button>
      <button class="hb" onclick={() => toggleFullscreen()} title={t("viewer.fullscreen")}><Icon name="maximize" size={15} /></button>
    </span>
  </div>
  <button class="nav prev" class:visible={showHud} onclick={() => go(-1)} aria-label={t("viewer.prev")}><Icon name="chevronLeft" size={26} /></button>
  <button class="nav next" class:visible={showHud} onclick={() => go(1)} aria-label={t("viewer.next")}><Icon name="chevronRight" size={26} /></button>
</div>

<style>
  .viewer {
    position: absolute;
    inset: 0;
    overflow: hidden;
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
    /* Bottom tool dock; banners and panels stack above it. */
    --dock-bottom: calc(16px + env(safe-area-inset-bottom, 0px));
    --above-dock: calc(var(--dock-bottom) + 56px);
  }
  .viewer:has(.dock.above-controls) {
    --dock-bottom: calc(64px + env(safe-area-inset-bottom, 0px));
  }
  .viewer.active::after {
    content: "";
    position: absolute;
    inset: 0;
    border: 2px solid var(--accent-line);
    pointer-events: none;
    z-index: 4;
  }
  .hide-cursor {
    cursor: none;
  }
  /* The media always fits the pane (long images included); zoom is a transform on top. */
  .stage {
    position: absolute;
    inset: 0;
    overflow: hidden;
    touch-action: none;
  }
  .stage.zoomed {
    cursor: grab;
  }
  .stage.zoomed:active {
    cursor: grabbing;
  }
  img,
  video {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  img {
    transform-origin: center;
    will-change: transform;
  }
  .strip,
  .slot {
    position: absolute;
    inset: 0;
  }
  .strip {
    will-change: transform;
  }
  .strip.sliding {
    transition: transform var(--slide-ms) cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .slot {
    overflow: hidden;
  }
  .error {
    position: absolute;
    color: var(--danger);
  }
  .nomedia {
    display: grid;
    justify-items: center;
    gap: 10px;
    color: var(--muted);
    text-align: center;
    padding: 24px;
  }
  .nomedia span {
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .banner {
    position: absolute;
    bottom: var(--above-dock);
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
    bottom: calc(var(--above-dock) + 52px);
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
    bottom: var(--above-dock);
    z-index: 3;
    width: min(400px, calc(100% - 32px));
    display: grid;
    /* minmax: an input's intrinsic width must not widen the panel past the screen. */
    grid-template-columns: minmax(0, 1fr);
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
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
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
  .loops .seg span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .loops small {
    flex-shrink: 0;
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
  /* A, B, name, save: the name takes what is left and the row always fits. */
  .marks {
    display: grid;
    grid-template-columns: auto auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 6px;
  }
  .marks .btn {
    white-space: nowrap;
  }
  .marks .input {
    width: 100%;
    height: var(--h-control-sm);
  }
  .seg-err {
    color: var(--danger);
    margin: 0;
  }
  .side-scrim {
    position: absolute;
    inset: 0;
    z-index: 2;
  }
  .side {
    position: absolute;
    top: 56px;
    right: 12px;
    bottom: var(--above-dock);
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
  .dock {
    position: absolute;
    left: 50%;
    bottom: var(--dock-bottom);
    transform: translateX(-50%);
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: calc(100% - 24px);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--t-med) var(--ease);
  }
  .hud.visible,
  .dock.visible,
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
  .hb.fav.on {
    color: #ff4d6d;
  }
  .hb.fav.on :global(path),
  .fav-flash.on :global(path) {
    fill: currentColor;
  }
  /* Feedback for a favorite change: a heart that pops and fades in the middle. */
  .fav-flash {
    position: absolute;
    left: 50%;
    top: 50%;
    z-index: 4;
    display: grid;
    place-items: center;
    width: 96px;
    height: 96px;
    margin: -48px 0 0 -48px;
    border-radius: 50%;
    background: rgb(0 0 0 / 0.45);
    color: #fff;
    pointer-events: none;
    animation: fav-pop 700ms var(--ease) forwards;
  }
  .fav-flash.on {
    color: #ff4d6d;
  }
  .fav-flash span {
    position: absolute;
    bottom: -26px;
    font-size: 12px;
    color: #fff;
    white-space: nowrap;
  }
  @keyframes fav-pop {
    0% {
      transform: scale(0.6);
      opacity: 0;
    }
    20% {
      transform: scale(1.05);
      opacity: 1;
    }
    75% {
      opacity: 1;
    }
    100% {
      transform: scale(1);
      opacity: 0;
    }
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
  @media (max-width: 600px) {
    .hud {
      gap: 8px;
      padding: 0 6px 0 12px;
    }
    .dock {
      gap: 4px;
    }
    .tools.vol {
      display: none;
    }
    .hb {
      width: 36px;
      height: 36px;
    }
    .tools {
      gap: 0;
      padding: 2px;
    }
  }
  .next {
    right: 16px;
  }
</style>
