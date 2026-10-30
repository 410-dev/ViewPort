<script lang="ts">
  import { dismiss } from "./ui/dismiss";
  import { edge, fade, pop } from "./ui/motion";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy, onMount, untrack } from "svelte";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import {
    api,
    displayUrl,
    mediaUrl,
    thumbUrl,
    platformApi,
    onConvert,
    asCmdError,
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
  import Editor from "./edit/Editor.svelte";
  import { bumpFile } from "./edit/versions.svelte";
  import { audible, audio, claimFocus, setMuted, setVolume } from "./audio.svelte";
  import VolumeControl from "./ui/VolumeControl.svelte";
  import { prefs, savePrefs } from "./prefs.svelte";
  import { fullscreenChanged, immersive } from "./immersive.svelte";
  import { ZoomPan } from "./zoom.svelte";
  import { onBack } from "./back.svelte";
  import { isMobile } from "./platform";
  import { reportBroken } from "./views/briefs.svelte";
  import { heading, lookahead } from "./lookahead";
  import { briefs as libraryBriefs } from "./views/briefs.svelte";

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
    onpull,
    onremoved,
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
    /** How far a pull-to-close drag has gone (0 none .. 1 smallest), so the shell can fade. */
    onpull?: (progress: number) => void;
    /** The item `id` was deleted: the shell drops it from every pane. */
    onremoved?: (id: number) => void;
  } = $props();

  let showInfo = $state(false);

  const id = $derived(ids[index]);
  let brief = $state<MediaBrief | null>(null);
  /** `brief` once it is the current item's: until its details arrive, the previous one's
   * must not drive the controls (a video's dock over a picture, saving a loop to it). */
  const cur = $derived(brief?.id === id ? brief : null);
  const curKind = $derived(cur?.kind);
  let video = $state<HTMLVideoElement | null>(null);
  /** The current video is paused (the dock's play/pause button). */
  let paused = $state(false);
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
  const probing = new Set<number>();
  $effect(() => {
    const b = brief;
    if (!b || b.kind !== "video") return;
    if (b.compatible === null && canProbe && !probing.has(b.id)) {
      probing.add(b.id);
      api
        .probeMedia(b.id)
        .then((nb) => {
          if (nb && brief?.id === nb.id) brief = nb;
        })
        .catch(() => {})
        .finally(() => probing.delete(b.id));
    }
    if (mode === "always" && !b.has_variant && !converting[b.id] && hasFfmpeg) convert(b.id);
  });

  const src = (id: number) => {
    const url = mediaUrl(id);
    return again(`m:${id}`, srcVersion[id] ? `${url}${url.includes("?") ? "&" : "?"}v=${srcVersion[id]}` : url);
  };

  // ---- failed loads. A request can fail while the app is in the background (Android
  // pauses the WebView), which left a broken picture on return: retry a few times
  // (`viewer.load_retries`), and once more when the app is shown again. Keys are
  // `m:<id>` for the picture or video, `t:<id>` for its thumbnail.
  /** Times each element was requested again: part of its URL, so a change reloads it. */
  const attempts = new SvelteMap<string, number>();
  /** Retries spent since the last success, return to the app or visit. */
  const tries = new Map<string, number>();
  /** Elements whose last load failed. */
  const failed = new Set<string>();
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
  function again(key: string, url: string) {
    const n = attempts.get(key);
    return n ? `${url}${url.includes("?") ? "&" : "?"}r=${n}` : url;
  }
  function reload(key: string) {
    attempts.set(key, (attempts.get(key) ?? 0) + 1);
  }
  /** Element `key` failed; true when the viewer gives up on it. */
  function loadFailed(key: string): boolean {
    failed.add(key);
    // In the background every request may fail: try again when the app is back.
    if (document.hidden || retryTimers.has(key)) return false;
    const v = prefs.app?.viewer;
    const n = tries.get(key) ?? 0;
    if (n >= (v?.load_retries ?? 3)) return true;
    tries.set(key, n + 1);
    retryTimers.set(
      key,
      setTimeout(() => {
        retryTimers.delete(key);
        reload(key);
      }, v?.load_retry_ms ?? 700),
    );
    return false;
  }
  function loadDone(key: string) {
    failed.delete(key);
    tries.delete(key);
  }
  /** Gives element `key` a fresh set of retries, starting now, if it had failed. */
  function retryFailed(key: string) {
    if (!failed.has(key)) return;
    clearTimeout(retryTimers.get(key));
    retryTimers.delete(key);
    tries.delete(key);
    reload(key);
  }
  function onVisibility() {
    if (document.hidden) return;
    for (const key of [...failed]) retryFailed(key);
    if (id !== undefined && failed.has(`m:${id}`)) error = false;
  }


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

  // ---- save the video frame on screen as a picture (next to the video), delete the item
  let savingFrame = $state(false);
  /** A short message over the dock (frame saved, or why it was not). */
  let notice = $state<{ text: string; err: boolean } | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | undefined;
  function showNotice(text: string, err = false) {
    notice = { text, err };
    clearTimeout(noticeTimer);
    noticeTimer = setTimeout(() => (notice = null), err ? 5000 : 2200);
  }

  function saveFrame() {
    const target = id;
    if (target === undefined || cur?.kind !== "video" || !video || savingFrame) return;
    savingFrame = true;
    api
      .saveVideoFrame(target, Math.round(video.currentTime * 1000))
      .then(() => {
        showNotice(t("viewer.frameSaved"));
        broadcast("media");
        onchanged?.();
      })
      .catch((e) => showNotice(`${t("viewer.frameFailed")}: ${asCmdError(e).message}`, true))
      .finally(() => (savingFrame = false));
  }

  // ---- editing
  let editing = $state(false);
  let canEditVideo = $state(false);
  api
    .editSupport()
    .then((s) => (canEditVideo = s.video))
    .catch(() => {});
  /** Picture formats the editor writes back (`vp_edit::Format`). */
  const EDITABLE_IMAGES = new Set(["jpg", "jpeg", "jpe", "jfif", "png", "webp", "bmp", "tif", "tiff"]);

  function openEditor() {
    const b = cur;
    if (!b) return;
    if (b.kind === "video" ? !canEditVideo : b.kind !== "image" || !EDITABLE_IMAGES.has(b.ext.toLowerCase())) {
      showNotice(b.kind === "video" ? t("edit.noVideo") : t("edit.unsupported"), true);
      return;
    }
    video?.pause();
    if (slideshow) toggleSlideshow();
    editing = true;
  }

  function edited(result: number, mode: "copy" | "overwrite") {
    editing = false;
    if (mode === "copy") {
      showNotice(t("edit.savedCopy"));
      broadcast("media");
    } else {
      bumpFile(result);
      invalidate([result]);
      tiers.delete(result);
      broadcast("edited", [result]);
      broadcast("media", [result]);
      api.mediaBrief([result]).then((bs) => {
        for (const b of bs) {
          known.set(b.id, b);
          if (brief?.id === b.id) brief = b;
        }
      });
      showNotice(t("edit.saved"));
    }
    onchanged?.();
  }

  let confirmDelete = $state<number | null>(null);
  let deleting = $state(false);
  $effect(() => {
    if (confirmDelete !== null) return onBack(() => !deleting && (confirmDelete = null));
  });

  async function deleteConfirmed() {
    const target = confirmDelete;
    if (target === null) return;
    deleting = true;
    try {
      const r = await api.deleteMedia([target]);
      if (r.failed.length) showNotice(`${t("del.failed", { n: r.failed.length })} ${r.failed[0]}`, true);
      else {
        broadcast("media", [target]);
        onchanged?.();
        onremoved?.(target);
      }
    } catch (e) {
      showNotice(asCmdError(e).message, true);
    } finally {
      deleting = false;
      confirmDelete = null;
    }
  }

  // Huge images show a display copy. Zooming in past its pixels loads a copy twice as
  // large, and so on up to the original, so a 200-megapixel photo is never decoded whole
  // unless the zoom really needs it.
  const displayMax = () => prefs.app?.viewer.display_max_px ?? 4096;
  /**
   * Edge of the copy shown before zooming: the size the picture is actually drawn at when
   * fitted to the screen (in device pixels, rounded up to `viewer.fit_screen_step_px`).
   * A picture larger than that is drawn scaled down, and the WebView decodes and
   * rasterizes it again for that scale while it slides in, which shows as black or torn
   * tiles. The window's size, not the pane's, and the fit in either orientation keep the
   * URL (and so the decoded copy) the same in split view and after rotating.
   */
  const defaultTier = (b: MediaBrief | undefined) => {
    const max = displayMax();
    const v = prefs.app?.viewer;
    if (max <= 0 || !(v?.fit_screen ?? true)) return max;
    const step = Math.max(64, v?.fit_screen_step_px ?? 512);
    const W = innerWidth * devicePixelRatio;
    const H = innerHeight * devicePixelRatio;
    let need = Math.max(W, H);
    if (b?.width && b.height) {
      const fit = Math.max(Math.min(W / b.width, H / b.height), Math.min(H / b.width, W / b.height));
      need = Math.max(b.width, b.height) * Math.min(1, fit);
    }
    return Math.min(max, Math.ceil(need / step) * step);
  };
  /** Edge in px of the copy shown per id (absent: `displayMax`; 0: the original). */
  const tiers = new SvelteMap<number, number>();
  const upgrading = new Set<number>();
  /** Items whose larger copy failed to load (out of memory, usually): not tried again. */
  const upgradeFailed = new Set<number>();
  /** Items whose picture has loaded; until then the thumbnail stands in. */
  const loaded = new SvelteSet<number>();
  /** Largest copy the backend makes (`media::display_key`); beyond it, the original. */
  const MAX_COPY_PX = 16383;
  const imageSrc = (b: MediaBrief) => {
    const tier = tiers.get(b.id);
    const url = tier === undefined ? displayUrl(b, defaultTier(b)) : tier === 0 ? mediaUrl(b.id) : mediaUrl(b.id, tier);
    return again(`m:${b.id}`, url);
  };
  // A file that will not load may be gone: check, so searches stop listing it.
  $effect(() => {
    if (error && id !== undefined) reportBroken(id);
  });
  let shownAt = performance.now();
  let lastId: number | undefined;
  /** Where the viewer has been heading (-1..1), for the predictive look-ahead. */
  let toward = 0;
  let lastIndex = -1;
  /** Bumped when the window settles at a new size (a foldable opening): the look-ahead
   * is made again for copies of the new screen size. */
  let screenRev = $state(0);
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  /**
   * Logs a picture the viewer waited for too long (`log.slow_view_ms`): how long until
   * its bytes arrived, and until it was decoded. The backend logs where its part went.
   */
  function noteSlow(target: number, img: HTMLImageElement) {
    const limit = prefs.app?.log.slow_view_ms ?? 400;
    if (limit <= 0) return;
    const since = shownAt;
    const arrived = performance.now() - since;
    img
      .decode()
      .catch(() => {})
      .finally(() => {
        const total = performance.now() - since;
        if (total < limit) return;
        const detail = `bytes ${Math.round(arrived)}ms, decoded ${Math.round(total - arrived)}ms later, ${img.naturalWidth}x${img.naturalHeight}, ${img.src.includes("fit=") ? "display copy" : "original"}`;
        api.logSlowView(target, Math.round(total), detail).catch(() => {});
      });
  }
  function onResize() {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => screenRev++, 300);
  }

  // Runs when the current item, the list or the screen size changes. What it reads besides
  // (the briefs it fills in, `brief` it sets) must not run it again: each lookup would
  // start another, and every new `brief` restarted the video's loop segment.
  $effect(() => {
    const current = id;
    screenRev;
    ids;
    index;
    settings.cache;
    if (current === undefined) return;
    untrack(() => visit(current));
  });

  /** Shows item `current`: its brief, look-ahead and preloads. */
  function visit(current: number) {
    error = false;
    retryFailed(`m:${current}`);
    retryFailed(`t:${current}`);
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

    // Metadata a window wider than the preloads ahead of time (`viewer.brief_window`), so
    // each move preloads its new neighbour at once — a rolling window — instead of waiting
    // for a lookup (which on a busy database can take seconds).
    const wide = Math.max(prefs.app?.viewer.brief_window ?? 24, c.prefetch_ahead, c.prefetch_behind);
    const span = [current, ...around];
    for (let k = 1; k <= wide && k < n; k++) span.push(ids[(index + k) % n], ids[(((index - k) % n) + n) % n]);
    if (known.size > 20 * wide + 1000) known.clear();
    for (const x of span) {
      const cached = libraryBriefs.get(x);
      if (!known.has(x) && cached) known.set(x, cached);
    }
    const preload = () => {
      if (ids[index] !== current) return;
      const b = known.get(current);
      if (b) brief = b;
      // Decode neighbouring images in the webview too, so switching is instant.
      const keep = new Set<number>();
      for (const x of around) {
        const nb = known.get(x);
        if (!nb || nb.kind !== "image") continue;
        keep.add(x);
        if (!preloaded.has(x)) {
          const img = new Image();
          img.src = imageSrc(nb);
          img.decode().catch(() => {});
          preloaded.set(x, img);
        }
      }
      for (const k of preloaded.keys()) if (!keep.has(k)) preloaded.delete(k);
    };
    preload();
    // The current item is always looked up again (its state may have changed); the rest
    // only when not known yet.
    const missing = [...new Set(span)].filter((x) => x === current || !known.has(x));
    api.mediaBrief(missing).then((bs) => {
      for (const b of bs) known.set(b.id, b);
      preload();
    });
    // The backend's look-ahead, shaped by the scheduler (`lookahead.ts`).
    const hw = prefs.app?.hardware;
    if (lastIndex >= 0 && lastIndex !== index) {
      let move = index - lastIndex;
      if (Math.abs(move) > n / 2) move -= Math.sign(move) * n;
      toward = heading(toward, move, hw?.predict_decay ?? 0.6);
    }
    lastIndex = index;
    const minSide = Math.min(0.5, Math.max(0, hw?.predict_min_side ?? 0.15));
    const plan = lookahead(ids, index, hw?.scheduler ?? "predictive", {
      ahead: c.prefetch_ahead,
      behind: c.prefetch_behind,
      max: hw?.prefetch_max_items ?? 400,
      forward: Math.min(1 - minSide, Math.max(minSide, (1 + toward) / 2)),
    });
    api.prefetchPlan(plan, { width: innerWidth * devicePixelRatio, height: innerHeight * devicePixelRatio }).catch(() => {});
  }

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

  /** Frame duration for frame stepping: measured from presented frames where the WebView
   * reports them (`requestVideoFrameCallback`), else assumed 30 fps. */
  let frameSecs = 1 / 30;
  function measureFrames(v: HTMLVideoElement) {
    if (!("requestVideoFrameCallback" in v)) return;
    let last = -1;
    const tick = (_: number, m: VideoFrameCallbackMetadata) => {
      const d = m.mediaTime - last;
      if (last >= 0 && d > 0.004 && d < 0.2) frameSecs = d;
      last = m.mediaTime;
      if (v.isConnected) v.requestVideoFrameCallback(tick);
    };
    v.requestVideoFrameCallback(tick);
  }

  function stepFrame(dir: 1 | -1) {
    if (!video || !isFinite(video.duration)) return;
    video.pause();
    video.currentTime = Math.min(Math.max(0, video.currentTime + dir * frameSecs), video.duration);
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

  /** The current video's id. Its brief is replaced often (lookups, probing, conversion);
   * the segments are loaded, and the autoplay one started, once per video. */
  const videoId = $derived(brief?.kind === "video" ? brief.id : null);
  $effect(() => {
    const vid = videoId;
    segments = [];
    activeSeg = null;
    markA = markB = null;
    if (vid === null) return;
    api.loopSegments(vid).then((list) => {
      if (videoId !== vid) return;
      segments = list;
      activeSeg = list.find((x) => x.autoplay) ?? null;
      if (activeSeg && video && video.readyState > 0) seekToSegment(activeSeg);
    });
  });

  /** A segment the video can play: starts before its end (one saved on another copy of the
   * file may not). Others are ignored, or the video would seek forever. */
  function usable(seg: LoopSegment) {
    if (!video || !isFinite(video.duration)) return true;
    return seg.start_ms / 1000 < video.duration - 0.05 && seg.end_ms > seg.start_ms;
  }

  function seekToSegment(seg: LoopSegment) {
    if (video && usable(seg)) video.currentTime = seg.start_ms / 1000;
  }

  function playSegment(seg: LoopSegment | null) {
    activeSeg = seg;
    if (seg && video) {
      seekToSegment(seg);
      video.play();
    }
  }

  async function saveSegment(seg?: LoopSegment) {
    segError = "";
    if (!brief || videoId !== id) return;
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
    const kind = curKind;
    if (!slideshow || ssPaused || !kind) return;
    if (kind === "image" || error) ssTimer = setTimeout(advance, ss.image_interval_secs * 1000);
  });

  function clipStart() {
    if (!video || !isFinite(video.duration)) return 0;
    const s = ss.clip_start_secs;
    return Math.max(0, s < 0 ? video.duration + s : Math.min(s, video.duration));
  }

  function onLoaded() {
    if (!video) return;
    if (activeSeg) seekToSegment(activeSeg);
    else if (slideshow && ss.video_mode === "clip") video.currentTime = clipStart();
  }

  function onTime() {
    if (!video) return;
    const t = video.currentTime;
    if (activeSeg && usable(activeSeg)) {
      // Not while a seek is under way: the position it reports may still be the old one.
      if (!video.seeking && (t >= activeSeg.end_ms / 1000 || t < activeSeg.start_ms / 1000 - 0.25)) {
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
    if (!active || isTyping(e) || editing) return;
    if (confirmDelete !== null) {
      if (e.key === "Escape" && !deleting) confirmDelete = null;
      return;
    }
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
    else if (matches(e, k.save_frame ?? []) && isVideo) saveFrame();
    else if (matches(e, k.delete_item ?? []) && id !== undefined) confirmDelete = id;
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
  let dockEl = $state<HTMLDivElement | null>(null);

  /** A press on a video's own control bar (seeking while the loop panel is open). */
  function onVideoControls(e: PointerEvent) {
    if (!video || e.target !== video || !video.controls) return false;
    return e.clientY > video.getBoundingClientRect().bottom - 72;
  }
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
    if (!zoomable || !n.w || upgrading.has(current) || upgradeFailed.has(current)) return;
    const tier = tiers.get(current) ?? defaultTier(known.get(current) ?? (brief?.id === current ? brief : undefined));
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
      .catch(() => upgradeFailed.add(current))
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
  // ---- pull to close (phones): past the favorite swipe's distance, the item shrinks as it
  // follows the finger down, and letting go past `pull_close_threshold` closes the viewer.
  const pullClose = (g: { type: string } | null) =>
    g?.type === "touch" && isMobile && panes <= 1 && (prefs.app?.viewer.pull_close ?? true);
  /** Scale of the item while pulled down to close (1 = not shrinking). */
  let shrink = $state(1);
  const favDistance = () => (prefs.app?.viewer.swipe_threshold ?? 0.2) * stageH;
  const closeDistance = () => Math.max((prefs.app?.viewer.pull_close_threshold ?? 0.4) * stageH, favDistance() + 40);
  const minShrink = () => Math.min(0.95, Math.max(0.05, prefs.app?.viewer.pull_close_min_scale ?? 0.4));
  /** Where the pull-to-close shrinking starts: the favorite swipe's distance, if it is on. */
  const shrinkFrom = (g: { type: string } | null) => (favSwipe(g) ? favDistance() : 0);
  $effect(() => onpull?.((1 - shrink) / (1 - minShrink())));
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
        else if (showLoops) showLoops = false;
        else if (slideshow) slideshow = false;
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

  /**
   * The current item once it has stayed put for two slide durations: its thumbnail
   * underlay goes then, not the moment the slide ends, when the WebView may still be
   * drawing the picture's tiles.
   */
  let rested = $state<number | undefined>(undefined);
  $effect(() => {
    const current = id;
    rested = undefined;
    const timer = setTimeout(() => (rested = current), slideMs() * 2);
    return () => clearTimeout(timer);
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

  function cancelTap() {
    clearTimeout(tapTimer);
    tapTimer = undefined;
  }

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
      const onControls = cur?.kind === "video" && showHud && e.clientY > bottom - 80;
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
      cancelTap();
      // A drag that became a pinch is not a swipe, favorite or pull any more.
      swipe = 0;
      pull = 0;
      shrink = 1;
      lock = null;
    }
  }

  function onPointerMove(e: PointerEvent) {
    poke();
    const prev = pointers.get(e.pointerId);
    if (!prev) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (gesture && !gesture.moved && Math.hypot(e.clientX - gesture.x, e.clientY - gesture.y) > 8) {
      gesture.moved = true;
      cancelTap();
    }
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
      const vertical = lock === "v" && (favSwipe(gesture) || pullClose(gesture));
      if ((lock === "h" && ids.length > 1) || vertical) {
        // Videos are not captured on pointerdown (their seek bar needs drags); capture now.
        const el = e.currentTarget as HTMLElement;
        if (!el.hasPointerCapture(e.pointerId)) el.setPointerCapture(e.pointerId);
      }
      if (lock === "h" && ids.length > 1) swipe = snap(dx);
      else if (vertical) {
        const from = shrinkFrom(gesture);
        if (pullClose(gesture) && dy > from) {
          // Pulling to close: the item follows the finger and shrinks with the distance.
          pull = snap(from / 3 + (dy - from));
          shrink = Math.max(minShrink(), 1 - (dy - from) / Math.max(1, stageH));
        } else {
          // The item follows a vertical drag at a third of the distance.
          shrink = 1;
          pull = favSwipe(gesture) ? snap(dy / 3) : 0;
        }
      }
    }
  }

  function onPointerUp(e: PointerEvent) {
    pointers.delete(e.pointerId);
    if (pointers.size < 2) pinch = null;
    if (pointers.size > 0 || !gesture) return;
    const g = gesture;
    gesture = null;
    if (g.pinched) {
      pull = 0;
      shrink = 1;
      lock = null;
      if (swipe !== 0) settle(0);
      return;
    }
    const dx = e.clientX - g.x;
    const dt = Math.max(1, performance.now() - g.t);
    if (pull !== 0 || (lock === "v" && (favSwipe(g) || pullClose(g)))) {
      pull = 0;
      shrink = 1;
      const dy = e.clientY - g.y;
      if (lock === "v" && e.type !== "pointercancel" && pullClose(g) && dy > closeDistance()) {
        close();
        return;
      }
      if (!favSwipe(g)) return;
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

  // Pinned video controls: while the rest hides, the transport moves down to where the
  // timeline was, then to the centre (and back the same way, in reverse).
  const pinned = $derived(prefs.app?.viewer.pin_controls ?? false);
  const docked = $derived(pinned && !showHud && cur?.kind === "video" && !immersive.on);
  let transportEl = $state<HTMLElement | null>(null);
  let pinDx = $state(0);
  function measurePin() {
    if (!dockEl || !transportEl) return;
    // Offsets ignore transforms, so this is the shown layout even while docked.
    pinDx = dockEl.offsetWidth / 2 - (transportEl.offsetLeft + transportEl.offsetWidth / 2);
  }
  $effect(() => {
    if (!dockEl || !transportEl) return;
    const ro = new ResizeObserver(measurePin);
    ro.observe(dockEl);
    return () => ro.disconnect();
  });
  $effect(() => {
    if (docked) untrack(measurePin);
  });
  function togglePin() {
    if (!prefs.app) return;
    prefs.app.viewer.pin_controls = !prefs.app.viewer.pin_controls;
    savePrefs();
  }

  // ---- HUD auto-hide
  let hudTimer: ReturnType<typeof setTimeout> | undefined;
  function poke() {
    hud = true;
    clearTimeout(hudTimer);
    // Not while the loop panel is open: hiding the controls there made a press on the seek
    // bar count as a press outside the panel, which closed it.
    hudTimer = setTimeout(() => (hud = showLoops), 2000);
  }

  // The loop panel kept the controls up; once it closes they hide again as usual.
  $effect(() => {
    if (!showLoops) untrack(poke);
  });

  onMount(() => {
    window.addEventListener("keydown", onKey);
    document.addEventListener("visibilitychange", onVisibility);
    poke();
  });
  onDestroy(() => {
    clearTimeout(ssTimer);
    unlistenConvert.then((f) => f());
    window.removeEventListener("keydown", onKey);
    document.removeEventListener("visibilitychange", onVisibility);
    for (const timer of retryTimers.values()) clearTimeout(timer);
    clearTimeout(hudTimer);
    clearTimeout(tapTimer);
    clearTimeout(slideTimer);
    clearTimeout(resizeTimer);
    // Stop reading ahead for a viewer that is gone.
    api.prefetchPlan([], { width: 0, height: 0 }).catch(() => {});
  });
</script>

<svelte:window onresize={onResize} />

<div
  class="viewer"
  class:hide-cursor={!showHud}
  class:pulling={shrink < 1}
  style:background={shrink < 1 ? `rgb(0 0 0 / ${1 - (1 - shrink) / (1 - minShrink())})` : undefined}
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
    <div class="strip" class:sliding style:transform="translate({swipe}px, {pull}px) scale({shrink})" style:--slide-ms="{slideMs()}ms">
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
              onloadedmetadata={(e) => {
                measureFrames(e.currentTarget);
                onLoaded();
              }}
              onvolumechange={onVolumeChange}
              onplay={() => (paused = false)}
              onpause={() => (paused = true)}
              oncanplay={(e) => (paused = e.currentTarget.paused)}
              ontimeupdate={onTime}
              onloadeddata={() => loadDone(`m:${s.id}`)}
              onerror={(e) => {
                // A format the WebView cannot decode will not decode on a retry either.
                const undecodable =
                  (e.currentTarget as HTMLVideoElement).error?.code === MediaError.MEDIA_ERR_DECODE || (b.compatible === false && !b.has_variant);
                if ((undecodable || loadFailed(`m:${s.id}`)) && s.id === id) error = true;
              }}
            ></video>
          {:else if b?.kind === "nonmedia"}
            <div class="nomedia"><Icon name="file" size={48} stroke={1.2} /><span>{b.name}</span><small>{t("panel.noPreview")}</small></div>
          {:else if b}
            <!-- The thumbnail stands in until the picture loads, and under a neighbour while it
                 slides in, so tiles not yet drawn show it instead of black. (At rest it is
                 gone, so transparent pictures show as they are.) -->
            {#if b.kind === "image" && (!loaded.has(s.id) || s.pos !== 0 || sliding || rested !== s.id)}
              <img
                class="ph"
                src={again(`t:${s.id}`, thumbUrl(s.id))}
                alt=""
                draggable="false"
                style:transform={s.pos === 0 ? zoom.transform : null}
                onload={() => loadDone(`t:${s.id}`)}
                onerror={() => loadFailed(`t:${s.id}`)}
              />
            {/if}
            <!-- Neighbouring videos show their thumbnail until they become current. -->
            <img
              src={b.kind === "video" ? again(`t:${s.id}`, thumbUrl(s.id)) : imageSrc(b)}
              alt={b.name}
              draggable="false"
              style:transform={s.pos === 0 ? zoom.transform : null}
              onload={(e) => {
                loadDone(`${b.kind === "video" ? "t" : "m"}:${s.id}`);
                if (b.kind !== "image") return;
                const img = e.currentTarget as HTMLImageElement;
                if (s.id === id && !loaded.has(s.id)) noteSlow(s.id, img);
                loaded.add(s.id);
                const size = { w: img.naturalWidth, h: img.naturalHeight };
                naturals.set(s.id, size);
                if (s.id === id) zoom.natural = size;
              }}
              onerror={() => {
                if (b.kind === "video") loadFailed(`t:${s.id}`);
                else if (loadFailed(`m:${s.id}`) && s.id === id) error = true;
              }}
            />
          {/if}
        </div>
      {/each}
    </div>
  </div>

  {#if error && !(cur?.kind === "video")}
    <div class="error">{cur?.name ?? known.get(id)?.name ?? ""}: {t("viewer.cannotDisplay")}</div>
  {/if}

  {#if immersive.on}
    <!-- Immersive: no banners. -->
  {:else if brief && converting[brief.id]}
    <div class="banner" role="status" transition:pop={{ base: "translateX(-50%)" }}>
      <span class="spinner"></span>
      <span>{t("viewer.converting")}</span>
      <span class="bar"><i style:width="{converting[brief.id].progress * 100}%"></i></span>
      <span class="tabular">{Math.round(converting[brief.id].progress * 100)}%</span>
      <button class="btn sm" onclick={() => api.cancelConvert(brief!.id)}>{t("common.cancel")}</button>
    </div>
  {:else if showBanner && brief}
    <div class="banner" role="alert" transition:pop={{ base: "translateX(-50%)" }} onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
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
  {#if notice && !immersive.on}
    <div class="banner notice" class:err={notice.err} role="status" transition:pop={{ base: "translateX(-50%)" }}>
      <span>{notice.text}</span>
    </div>
  {/if}
  {#if convertError && !immersive.on}
    <div class="banner err" role="alert" transition:pop={{ base: "translateX(-50%)" }}>
      <span>{t("viewer.convertFailed")}: {convertError}</span>
      <button class="icon-btn sm" aria-label={t("common.close")} onclick={() => (convertError = null)}><Icon name="x" size={13} /></button>
    </div>
  {/if}

  {#if showLoops && cur?.kind === "video" && !immersive.on}
    <div
      class="loops"
      role="presentation"
      transition:pop
      use:dismiss={{ onclose: () => (showLoops = false), keep: () => [dockEl], ignore: onVideoControls }}
      onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
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
      transition:fade
      role="presentation"
      onpointerdown={(e) => e.stopPropagation()}
      onpointerup={(e) => e.stopPropagation()}
      onclick={() => (showInfo = false)}
    ></div>
    <div class="side" role="presentation" transition:edge onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
      <InfoPanel ids={[id]} {onchanged} />
    </div>
  {/if}

  <div class="hud" class:visible={showHud} role="presentation" onpointerdown={(e) => e.stopPropagation()} onpointerup={(e) => e.stopPropagation()}>
    <span class="tools">
      <button class="hb fav" class:on={isFav} aria-pressed={isFav} onclick={() => setFav(!isFav)} title={t("viewer.favorite")}
        ><Icon name="heart" size={15} /></button
      >
      <button class="hb" class:on={showInfo} onclick={() => (showInfo = !showInfo)} title={t("viewer.info")}><Icon name="info" size={15} /></button>
      <button class="hb" disabled={!cur || cur.kind === "nonmedia"} onclick={openEditor} title={t("viewer.edit")} aria-label={t("viewer.edit")}
        ><Icon name="pencil" size={15} /></button
      >
      <button class="hb del" onclick={() => id !== undefined && (confirmDelete = id)} title={t("viewer.delete")} aria-label={t("viewer.delete")}
        ><Icon name="trash" size={15} /></button
      >
    </span>
    <span class="mid">
      {#if prefs.app?.viewer.show_name}<span class="name">{(known.get(id) ?? brief)?.name ?? ""}</span>{/if}
      <span class="pos">{index + 1} / {ids.length}</span>
      {#if slideshow}
        <span class="ss">{ssPaused ? t("viewer.slideshowPaused") : t("viewer.slideshow")}</span>
      {/if}
      {#if loadingTask}
        {@const f = fraction(loadingTask)}
        <span class="load"><span class="spinner"></span><span class="tabular">{fmtBytes(loadingTask.done)} / {fmtBytes(loadingTask.total)}</span>
          <span class="bar"><i style:width="{(f ?? 0) * 100}%"></i></span></span>
      {/if}
    </span>
    <span class="tools">
      {#if onsplit}
        <button class="hb" onclick={onsplit} title={t("viewer.split")}><Icon name="columns" size={15} /></button>
      {/if}
      <button class="hb" onclick={() => (immersive.on = true)} title={t("viewer.immersive")}><Icon name="eyeOff" size={15} /></button>
      <button class="hb" onclick={() => toggleFullscreen()} title={t("viewer.fullscreen")}><Icon name="maximize" size={15} /></button>
    </span>
    <span class="tools">
      <button class="hb" onclick={close} title={t("common.close")}><Icon name="x" size={15} /></button>
    </span>
  </div>
  <!-- Bottom: slideshow, video transport, then the rest. -->
  <div
    class="dock"
    bind:this={dockEl}
    class:visible={showHud}
    class:docked
    class:above-controls={cur?.kind === "video"}
    role="presentation"
    onpointerdown={(e) => e.stopPropagation()}
    onpointerup={(e) => e.stopPropagation()}
  >
    <span class="tools solo">
      <button class="hb" class:on={slideshow} onclick={toggleSlideshow} title={t("viewer.slideshow")}><Icon name={slideshow ? "stop" : "film"} size={15} /></button>
    </span>
    {#if cur?.kind === "video"}
      {@const step = settings.playback.seek_step_secs}
      <span class="pin-y" bind:this={transportEl} style:--pin-dx="{pinDx}px"><span class="tools transport">
        <button class="hb" onclick={() => seek(-step)} title={t("viewer.seekBack", { n: step })} aria-label={t("viewer.seekBack", { n: step })}
          ><Icon name="rewind" size={15} /></button
        >
        <button class="hb" onclick={() => stepFrame(-1)} title={t("viewer.frameBack")} aria-label={t("viewer.frameBack")}><Icon name="frameBack" size={15} /></button>
        <button class="hb play" onclick={togglePlay} title={paused ? t("viewer.play") : t("viewer.pause")} aria-label={paused ? t("viewer.play") : t("viewer.pause")}
          ><Icon name={paused ? "play" : "pause"} size={17} /></button
        >
        <button class="hb" disabled={savingFrame} onclick={saveFrame} title={t("viewer.saveFrame")} aria-label={t("viewer.saveFrame")}
          >{#if savingFrame}<span class="spinner"></span>{:else}<Icon name="camera" size={15} />{/if}</button
        >
        <button class="hb" onclick={() => stepFrame(1)} title={t("viewer.frameForward")} aria-label={t("viewer.frameForward")}
          ><Icon name="frameForward" size={15} /></button
        >
        <button class="hb" onclick={() => seek(step)} title={t("viewer.seekForward", { n: step })} aria-label={t("viewer.seekForward", { n: step })}
          ><Icon name="fastForward" size={15} /></button
        >
      </span></span>
    {/if}
    {#if cur?.kind === "video" || onpop}
      <span class="tools">
        {#if cur?.kind === "video"}
          <button class="hb" class:on={showLoops} onclick={() => (showLoops = !showLoops)} title={t("viewer.loops")}><Icon name="loop" size={15} /></button>
          <button class="hb" class:on={pinned} aria-pressed={pinned} onclick={togglePin} title={t("viewer.pinControls")} aria-label={t("viewer.pinControls")}
            ><Icon name="pin" size={15} /></button
          >
        {/if}
        {#if onpop}
          <button class="hb" onclick={onpop} title={t("viewer.popOut")}><Icon name="external" size={15} /></button>
        {/if}
      </span>
    {/if}
    <span class="tools vol">
      <VolumeControl dark />
    </span>
  </div>
  {#if confirmDelete !== null}
    <div
      out:fade
      class="scrim"
      role="presentation"
      onpointerdown={(e) => e.stopPropagation()}
      onpointerup={(e) => e.stopPropagation()}
      onclick={(e) => e.target === e.currentTarget && !deleting && (confirmDelete = null)}
    >
      <div out:pop class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="vdel-title">
        <header><h2 id="vdel-title">{t("del.title")}</h2></header>
        <div class="body">
          <p class="text"><strong>{known.get(confirmDelete)?.name ?? cur?.name ?? ""}</strong></p>
          <p class="text">{t("viewer.deleteBody")}</p>
        </div>
        <footer>
          <span class="grow"></span>
          <button class="btn ghost" disabled={deleting} onclick={() => (confirmDelete = null)}>{t("common.cancel")}</button>
          <button class="btn danger" disabled={deleting} onclick={deleteConfirmed}>
            {#if deleting}<span class="spinner"></span>{:else}<Icon name="trash" size={14} />{/if}{t("del.confirm")}
          </button>
        </footer>
      </div>
    </div>
  {/if}
  {#if editing && cur}
    <Editor brief={cur} src={cur.kind === "video" ? src(cur.id) : imageSrc(cur)} poster={thumbUrl(cur.id)} onclose={() => (editing = false)} onsaved={edited} />
  {/if}
  {#if prefs.app?.viewer.show_nav_buttons ?? true}
    <button class="nav prev" class:visible={showHud} onclick={() => go(-1)} aria-label={t("viewer.prev")}><Icon name="chevronLeft" size={26} /></button>
    <button class="nav next" class:visible={showHud} onclick={() => go(1)} aria-label={t("viewer.next")}><Icon name="chevronRight" size={26} /></button>
  {/if}
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
    /* Height of a video's own control bar, which the dock sits above. */
    --controls-h: 48px;
    --above-dock: calc(var(--dock-bottom) + 56px);
  }
  .viewer:has(.dock.above-controls) {
    --dock-bottom: calc(16px + var(--controls-h) + env(safe-area-inset-bottom, 0px));
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
  /* Pulled down to close: only the shrinking item, over the library showing through. */
  .viewer.pulling > :not(.stage) {
    opacity: 0;
    pointer-events: none;
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
  /* No big play/pause button in the middle of a video: a tap shows the controls only. */
  video::-webkit-media-controls-overlay-play-button,
  video::-webkit-media-controls-start-playback-button {
    display: none !important;
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
  .banner.notice {
    padding-right: 18px;
  }
  .banner.notice.err {
    bottom: var(--above-dock);
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
    /* A landscape phone has less room than the list's own height. */
    max-height: calc(100% - var(--above-dock) - 16px);
    overflow-y: auto;
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
    pointer-events: none;
  }
  /* The dock's groups fade on their own, so a pinned transport can stay. */
  .dock > * {
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--t-med) var(--ease);
  }
  .dock.visible > *,
  .dock.docked > .pin-y {
    opacity: 1;
    pointer-events: auto;
  }
  /* Docking is two moves: down first, then to the centre; undocking reverses them. */
  .pin-y {
    display: flex;
    transition:
      opacity var(--t-med) var(--ease),
      transform var(--t-med) var(--ease) var(--t-med);
  }
  .pin-y > .transport {
    transition: transform var(--t-med) var(--ease);
  }
  .dock.docked > .pin-y {
    transform: translateY(var(--controls-h));
    transition-delay: 0s;
  }
  .dock.docked > .pin-y > .transport {
    transform: translateX(var(--pin-dx));
    transition-delay: var(--t-med);
  }
  .hud.visible,
  .nav.visible {
    opacity: 1;
    pointer-events: auto;
  }
  .mid {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .name {
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
      color var(--t-fast) var(--ease),
      transform var(--t-fast) var(--ease);
  }
  .hb.play {
    margin: 0 2px;
    background: rgb(255 255 255 / 0.12);
    color: #fff;
  }
  .hb:hover {
    background: var(--hover);
    color: #fff;
  }
  .hb:active {
    transform: scale(0.9);
  }
  .hb.on {
    color: var(--accent);
  }
  .hb.fav.on {
    color: #ff4d6d;
  }
  .hb.del:hover {
    color: var(--danger);
  }
  .hb:disabled {
    opacity: 0.6;
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
      gap: 6px;
      padding: 0 6px;
    }
    .mid {
      justify-content: center;
      gap: 8px;
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
