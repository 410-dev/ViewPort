<script lang="ts">
  import { untrack } from "svelte";
  import { api, asCmdError, type EditSpec, type MediaBrief, type SaveMode } from "../api";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";
  import { fade, pop } from "../ui/motion";
  import { dismiss } from "../ui/dismiss";
  import { onBack } from "../back.svelte";
  import { prefs } from "../prefs.svelte";
  import { tasks } from "../tasks.svelte";
  import { audible, audio } from "../audio.svelte";
  import { FULL, fitCrop, isFull, largestCrop, rotatedSize, towards, turnCrop, type Rect } from "./geometry";

  /**
   * Edits the item shown in a viewer pane: crop and rotation (pictures and videos), cut and
   * speed (videos). Saves through `edit_media` as a copy or over the original.
   */
  let {
    brief,
    src,
    poster,
    onclose,
    onsaved,
  }: {
    brief: MediaBrief;
    /** URL of the picture or video to show while editing. */
    src: string;
    /** A video's thumbnail, shown until its first frame is decoded. */
    poster?: string;
    onclose: () => void;
    /** Saved: `id` holds the result (a new item for a copy). */
    onsaved: (id: number, mode: SaveMode) => void;
  } = $props();

  const isVideo = untrack(() => brief.kind === "video");
  type Tab = "crop" | "trim" | "speed";
  let tab = $state<Tab>("crop");

  // ---- picture geometry. Sizes are the item's as shown (EXIF orientation applied).
  let natW = $state(0);
  let natH = $state(0);
  /** Quarter turns clockwise (0..3) plus a fine angle (-45..45). */
  let quarter = $state(0);
  let fine = $state(0);
  let expand = $state(false);
  let crop = $state<Rect>(FULL);
  /** The crop was set by hand (drag or ratio); otherwise it follows the angle. */
  let touched = $state(false);
  const RATIOS = ["free", "original", "1:1", "4:3", "3:4", "16:9", "9:16"] as const;
  type Ratio = (typeof RATIOS)[number];
  let ratio = $state<Ratio>("free");
  const deg = $derived(quarter * 90 + fine);

  function ratioValue(r: Ratio): number | null {
    if (r === "free") return null;
    if (r === "original") return quarter % 2 ? natH / natW : natW / natH;
    const [a, b] = r.split(":").map(Number);
    return a / b;
  }

  /** The crop when nobody set it: the whole box, or the largest part of the picture's
   * shape inside it (turned without filling). */
  function defaultCrop(): Rect {
    if (!natW || !natH) return FULL;
    const r = ratioValue(ratio);
    if (expand && r === null) return FULL;
    return largestCrop(natW, natH, deg, expand, r ?? (quarter % 2 ? natH / natW : natW / natH));
  }

  function refit() {
    crop = touched ? fitCrop(natW, natH, deg, expand, crop) : defaultCrop();
  }

  function setFine(v: number) {
    fine = Math.max(-45, Math.min(45, Math.round(v * 10) / 10));
    refit();
  }

  function turn(dir: 1 | -1) {
    quarter = (quarter + dir + 4) % 4;
    crop = turnCrop(crop, dir);
    const swap: Partial<Record<Ratio, Ratio>> = { "4:3": "3:4", "3:4": "4:3", "16:9": "9:16", "9:16": "16:9" };
    ratio = swap[ratio] ?? ratio;
    refit();
  }

  function setExpand(on: boolean) {
    expand = on;
    refit();
  }

  function setRatio(r: Ratio) {
    ratio = r;
    touched = r !== "free";
    crop = defaultCrop();
  }

  function reset() {
    quarter = 0;
    fine = 0;
    expand = false;
    ratio = "free";
    touched = false;
    crop = FULL;
    trimStart = 0;
    trimEnd = duration;
    speed = 1;
    refit();
  }

  // ---- stage layout
  let stageW = $state(0);
  let stageH = $state(0);
  const box = $derived.by(() => {
    if (!natW || !natH || !stageW || !stageH) return null;
    const [bw, bh] = rotatedSize(natW, natH, deg);
    const pad = stageW < 640 ? 20 : 36;
    const k = Math.max(0.01, Math.min((stageW - pad * 2) / bw, (stageH - pad * 2) / bh));
    return { w: bw * k, h: bh * k, k };
  });

  // ---- crop dragging
  type Handle = "move" | "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";
  let drag: { handle: Handle; x: number; y: number; start: Rect } | null = $state(null);
  const MIN = 0.04;

  function startDrag(e: PointerEvent, handle: Handle) {
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    drag = { handle, x: e.clientX, y: e.clientY, start: { ...crop } };
  }

  function moveDrag(e: PointerEvent) {
    if (!drag || !box) return;
    const dx = (e.clientX - drag.x) / box.w;
    const dy = (e.clientY - drag.y) / box.h;
    const s = drag.start;
    const h = drag.handle;
    let c: Rect;
    if (h === "move") {
      c = { ...s, x: Math.min(1 - s.w, Math.max(0, s.x + dx)), y: Math.min(1 - s.h, Math.max(0, s.y + dy)) };
      // Slide along an edge that stops the diagonal move.
      const both = towards(natW, natH, deg, expand, crop, c);
      const alongX = towards(natW, natH, deg, expand, both, { ...both, x: c.x });
      crop = towards(natW, natH, deg, expand, alongX, { ...alongX, y: c.y });
      return;
    }
    let { x, y, w, h: ht } = s;
    if (h.includes("w")) {
      const nx = Math.min(s.x + s.w - MIN, Math.max(0, s.x + dx));
      w = s.x + s.w - nx;
      x = nx;
    }
    if (h.includes("e")) w = Math.min(1 - s.x, Math.max(MIN, s.w + dx));
    if (h.includes("n")) {
      const ny = Math.min(s.y + s.h - MIN, Math.max(0, s.y + dy));
      ht = s.y + s.h - ny;
      y = ny;
    }
    if (h.includes("s")) ht = Math.min(1 - s.y, Math.max(MIN, s.h + dy));
    const r = ratioValue(ratio);
    if (r !== null) {
      // Keep the ratio (in pixels): the dragged side leads, the other follows.
      const [bw, bh] = rotatedSize(natW, natH, deg);
      const toH = (cw: number) => (cw * bw) / r / bh;
      const toW = (ch: number) => (ch * bh * r) / bw;
      if (h === "n" || h === "s") {
        const nw = toW(ht);
        x = s.x + (s.w - nw) / 2;
        w = nw;
      } else if (h === "e" || h === "w") {
        const nh = toH(w);
        y = s.y + (s.h - nh) / 2;
        ht = nh;
      } else {
        const nh = toH(w);
        if (h.includes("n")) y = s.y + s.h - nh;
        ht = nh;
      }
    }
    c = { x, y, w, h: ht };
    crop = towards(natW, natH, deg, expand, crop, c);
    touched = true;
  }

  function endDrag() {
    if (drag && drag.handle === "move") touched = true;
    drag = null;
  }

  // ---- video
  let video = $state<HTMLVideoElement | null>(null);
  let duration = $state(0);
  let trimStart = $state(0);
  let trimEnd = $state(0);
  let speed = $state(1);
  let now = $state(0);
  let playing = $state(false);
  const speeds = $derived(prefs.app?.viewer.edit_speeds ?? [0.25, 0.5, 1, 2, 4]);

  function onMeta(e: Event) {
    const v = e.currentTarget as HTMLVideoElement;
    natW = v.videoWidth;
    natH = v.videoHeight;
    duration = isFinite(v.duration) ? v.duration * 1000 : (brief.duration_ms ?? 0);
    trimEnd = duration;
    // Android's WebView shows nothing until a frame is decoded: ask for the first one.
    v.currentTime = 0.001;
    refit();
  }

  function onTime() {
    if (!video) return;
    now = video.currentTime * 1000;
    // Play only the kept span, over and over.
    if (!video.paused && (now >= trimEnd - 20 || now < trimStart - 250)) video.currentTime = trimStart / 1000;
  }

  $effect(() => {
    if (!video) return;
    video.volume = audio.volume;
    video.muted = !audible(true);
  });
  $effect(() => {
    if (video) video.playbackRate = Math.min(16, Math.max(0.0625, speed));
  });

  function togglePlay() {
    if (!video) return;
    if (video.paused) {
      if (now < trimStart || now >= trimEnd - 20) video.currentTime = trimStart / 1000;
      video.play().catch(() => {});
    } else video.pause();
  }

  let track = $state<HTMLElement | null>(null);
  let trimDrag: "start" | "end" | "head" | null = null;
  const MIN_SPAN = 100;

  function trimAt(e: PointerEvent): number {
    const r = track!.getBoundingClientRect();
    return Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)) * duration;
  }

  function startTrim(e: PointerEvent, which: "start" | "end" | "head") {
    e.preventDefault();
    e.stopPropagation();
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    trimDrag = which;
    video?.pause();
    moveTrim(e);
  }

  function moveTrim(e: PointerEvent) {
    if (!trimDrag || !track || !video) return;
    const at = trimAt(e);
    if (trimDrag === "start") {
      trimStart = Math.min(at, trimEnd - MIN_SPAN);
      video.currentTime = trimStart / 1000;
    } else if (trimDrag === "end") {
      trimEnd = Math.max(at, trimStart + MIN_SPAN);
      video.currentTime = trimEnd / 1000;
    } else {
      video.currentTime = at / 1000;
    }
    // A paused WebView may report the new time only later; "start/end here" needs it now.
    now = video.currentTime * 1000;
  }

  function setStartHere() {
    trimStart = Math.min(now, trimEnd - MIN_SPAN);
  }

  function setEndHere() {
    trimEnd = Math.max(now, trimStart + MIN_SPAN);
  }

  function fmt(ms: number) {
    const s = Math.max(0, ms / 1000);
    const m = Math.floor(s / 60);
    return `${m}:${(s % 60).toFixed(1).padStart(4, "0")}`;
  }

  // ---- saving
  function spec(): EditSpec {
    const trimmed = isVideo && duration > 0 && (trimStart > 1 || trimEnd < duration - 1);
    return {
      rotate_deg: deg,
      expand,
      // A free rotation always sends its crop: the inside is what the editor showed.
      crop: isFull(crop) && (fine === 0 || expand) ? null : { ...crop },
      trim: trimmed ? { start_ms: Math.round(trimStart), end_ms: Math.round(trimEnd) } : null,
      speed: isVideo ? speed : 1,
    };
  }

  const changed = $derived(
    (quarter !== 0 || fine !== 0 || !isFull(crop) || (isVideo && (speed !== 1 || trimStart > 1 || (duration > 0 && trimEnd < duration - 1)))),
  );

  let menu = $state(false);
  let menuBtn = $state<HTMLElement | null>(null);
  let confirming = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  const task = $derived(tasks.list.find((x) => x.kind === "edit" && x.media_id === brief.id && x.state === "running"));

  async function save(mode: SaveMode) {
    menu = false;
    confirming = false;
    saving = true;
    error = null;
    video?.pause();
    try {
      const id = await api.editMedia(brief.id, spec(), mode);
      onsaved(id, mode);
    } catch (e) {
      const err = asCmdError(e);
      if (err.code !== "cancelled") error = `${t("edit.failed")}: ${err.message}`;
    } finally {
      saving = false;
    }
  }

  function cancelSave() {
    if (task) api.cancelTask(task.id).catch(() => {});
  }

  $effect(() =>
    onBack(() => {
      if (saving) return;
      if (confirming) confirming = false;
      else if (menu) menu = false;
      else onclose();
    }),
  );

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape" && !saving) {
      e.stopPropagation();
      if (confirming) confirming = false;
      else if (menu) menu = false;
      else onclose();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<div
  class="editor"
  role="dialog"
  tabindex="-1"
  aria-modal="true"
  aria-label={t("edit.title")}
  transition:fade
  onpointerdown={(e) => e.stopPropagation()}
  onpointerup={(e) => e.stopPropagation()}
  onwheel={(e) => e.stopPropagation()}
>
  <header class="topbar">
    <button class="pill" onclick={onclose} disabled={saving}><Icon name="x" size={15} /><span class="label">{t("common.cancel")}</span></button>
    <span class="title">{t("edit.title")}</span>
    <button class="pill" onclick={reset} disabled={saving || !changed} title={t("edit.reset")}><Icon name="refresh" size={15} /><span class="label">{t("edit.reset")}</span></button>
    <span class="save-wrap">
      <button class="pill primary" bind:this={menuBtn} disabled={saving || !changed} onclick={() => (menu = !menu)}>
        <Icon name="save" size={15} /><span>{t("edit.save")}</span><Icon name="chevronDown" size={13} />
      </button>
      {#if menu}
        <div class="menu" transition:pop use:dismiss={{ onclose: () => (menu = false), keep: () => [menuBtn] }}>
          <button onclick={() => save("copy")}><Icon name="copy" size={15} />{t("edit.saveCopy")}</button>
          <button class="danger" onclick={() => ((menu = false), (confirming = true))}><Icon name="save" size={15} />{t("edit.overwrite")}</button>
        </div>
      {/if}
    </span>
  </header>

  <div class="stage" bind:clientWidth={stageW} bind:clientHeight={stageH}>
    <div class="box" class:expand class:ready={!!box} style:width="{box?.w ?? 0}px" style:height="{box?.h ?? 0}px">
      {#if isVideo}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          bind:this={video}
          class="media"
          {src}
          {poster}
          playsinline
          preload="auto"
          style:width="{natW * (box?.k ?? 0)}px"
          style:height="{natH * (box?.k ?? 0)}px"
          style:transform="translate(-50%, -50%) rotate({deg}deg)"
          onloadedmetadata={onMeta}
          ontimeupdate={onTime}
          onplay={() => (playing = true)}
          onpause={() => (playing = false)}
        ></video>
      {:else}
        <img
          class="media"
          {src}
          alt=""
          draggable="false"
          style:width="{natW * (box?.k ?? 0)}px"
          style:height="{natH * (box?.k ?? 0)}px"
          style:transform="translate(-50%, -50%) rotate({deg}deg)"
          onload={(e) => {
            const img = e.currentTarget as HTMLImageElement;
            natW = img.naturalWidth;
            natH = img.naturalHeight;
            refit();
          }}
        />
      {/if}
      {#if box && tab === "crop"}
        <div
          class="crop"
          class:dragging={!!drag}
          style:left="{crop.x * 100}%"
          style:top="{crop.y * 100}%"
          style:width="{crop.w * 100}%"
          style:height="{crop.h * 100}%"
          role="presentation"
          onpointerdown={(e) => startDrag(e, "move")}
          onpointermove={moveDrag}
          onpointerup={endDrag}
          onpointercancel={endDrag}
        >
          <i class="third v1"></i><i class="third v2"></i><i class="third h1"></i><i class="third h2"></i>
          {#each ["n", "s", "e", "w", "ne", "nw", "se", "sw"] as const as h (h)}
            <span
              class="handle {h}"
              role="presentation"
              onpointerdown={(e) => startDrag(e, h)}
              onpointermove={moveDrag}
              onpointerup={endDrag}
              onpointercancel={endDrag}
            ></span>
          {/each}
        </div>
      {:else if box}
        <div class="crop still" style:left="{crop.x * 100}%" style:top="{crop.y * 100}%" style:width="{crop.w * 100}%" style:height="{crop.h * 100}%"></div>
      {/if}
    </div>
    {#if isVideo}
      <button class="play" onclick={togglePlay} aria-label={playing ? t("viewer.pause") : t("viewer.play")}><Icon name={playing ? "pause" : "play"} size={20} /></button>
    {/if}
  </div>

  <footer class="tools">
    <div class="panel">
      {#if tab === "crop"}
        <div class="row angle">
          <button class="icon" onclick={() => turn(-1)} title={t("edit.rotateLeft")} aria-label={t("edit.rotateLeft")}><Icon name="rotateLeft" size={17} /></button>
          <label class="dial">
            <span class="val tabular" role="button" tabindex="-1" ondblclick={() => setFine(0)}>{fine.toFixed(1)}°</span>
            <input type="range" min="-45" max="45" step="0.1" value={fine} aria-label={t("edit.angle")} oninput={(e) => setFine(Number(e.currentTarget.value))} />
          </label>
          <button class="icon" onclick={() => turn(1)} title={t("edit.rotateRight")} aria-label={t("edit.rotateRight")}><Icon name="rotateRight" size={17} /></button>
        </div>
        <div class="row chips">
          <button class="chip toggle" class:on={expand} aria-pressed={expand} onclick={() => setExpand(!expand)}>
            <span class="switch" class:on={expand}></span>{t("edit.expand")}
          </button>
          <span class="divider"></span>
          {#each RATIOS as r (r)}
            <button class="chip" class:on={ratio === r} onclick={() => setRatio(r)}>{r === "free" ? t("edit.ratioFree") : r === "original" ? t("edit.ratioOriginal") : r}</button>
          {/each}
        </div>
      {:else if tab === "trim"}
        <div class="row times tabular">
          <span>{t("edit.trimStart")} {fmt(trimStart)}</span>
          <span class="mid">{fmt(now)}</span>
          <span>{t("edit.trimEnd")} {fmt(trimEnd)}</span>
        </div>
        <div
          class="track"
          bind:this={track}
          role="presentation"
          onpointerdown={(e) => startTrim(e, "head")}
          onpointermove={moveTrim}
          onpointerup={() => (trimDrag = null)}
          onpointercancel={() => (trimDrag = null)}
        >
          {#if duration > 0}
            <span class="kept" style:left="{(trimStart / duration) * 100}%" style:width="{((trimEnd - trimStart) / duration) * 100}%"></span>
            <span class="head" style:left="{(now / duration) * 100}%"></span>
            <span
              class="grip start"
              style:left="{(trimStart / duration) * 100}%"
              role="presentation"
              onpointerdown={(e) => startTrim(e, "start")}
              onpointermove={moveTrim}
              onpointerup={() => (trimDrag = null)}
            ></span>
            <span
              class="grip end"
              style:left="{(trimEnd / duration) * 100}%"
              role="presentation"
              onpointerdown={(e) => startTrim(e, "end")}
              onpointermove={moveTrim}
              onpointerup={() => (trimDrag = null)}
            ></span>
          {/if}
        </div>
        <div class="row chips">
          <button class="chip" onclick={setStartHere}>{t("edit.setStart")}</button>
          <button class="chip" onclick={setEndHere}>{t("edit.setEnd")}</button>
          <span class="sep"></span>
          <span class="note tabular">{t("edit.result", { t: fmt((trimEnd - trimStart) / speed) })}</span>
        </div>
      {:else}
        <div class="row chips speeds">
          {#each speeds as s (s)}
            <button class="chip" class:on={speed === s} onclick={() => (speed = s)}>{s}×</button>
          {/each}
        </div>
        <div class="row">
          <span class="note tabular">{t("edit.result", { t: fmt((trimEnd - trimStart) / speed) })}</span>
        </div>
      {/if}
    </div>
    {#if isVideo}
      <nav class="tabs" aria-label={t("edit.title")}>
        <button class:on={tab === "crop"} onclick={() => (tab = "crop")}><Icon name="crop" size={16} />{t("edit.tabCrop")}</button>
        <button class:on={tab === "trim"} onclick={() => (tab = "trim")}><Icon name="scissors" size={16} />{t("edit.tabTrim")}</button>
        <button class:on={tab === "speed"} onclick={() => (tab = "speed")}><Icon name="gauge" size={16} />{t("edit.tabSpeed")}</button>
      </nav>
    {/if}
  </footer>

  {#if saving}
    <div class="busy" transition:fade>
      <div class="busy-card">
        <span class="spinner"></span>
        <span>{t("edit.saving")}</span>
        {#if task && task.total}
          <span class="bar"><i style:width="{(task.done / task.total) * 100}%"></i></span>
          <span class="tabular">{Math.round((task.done / task.total) * 100)}%</span>
        {/if}
        {#if task?.cancellable}
          <button class="btn sm" onclick={cancelSave}>{t("common.cancel")}</button>
        {/if}
      </div>
    </div>
  {/if}
  {#if error}
    <div class="error" role="alert" transition:pop>
      <span>{error}</span>
      <button class="icon" aria-label={t("common.close")} onclick={() => (error = null)}><Icon name="x" size={13} /></button>
    </div>
  {/if}
  {#if confirming}
    <div class="scrim" out:fade role="presentation" onclick={(e) => e.target === e.currentTarget && (confirming = false)}>
      <div out:pop class="dialog confirm" role="alertdialog" aria-modal="true" aria-labelledby="edit-ow-title">
        <header><h2 id="edit-ow-title">{t("edit.overwriteTitle")}</h2></header>
        <div class="body">
          <p class="text"><strong>{brief.name}</strong></p>
          <p class="text">{t("edit.overwriteBody")}</p>
        </div>
        <footer>
          <span class="grow"></span>
          <button class="btn ghost" onclick={() => (confirming = false)}>{t("common.cancel")}</button>
          <button class="btn danger" onclick={() => save("overwrite")}><Icon name="save" size={14} />{t("edit.overwrite")}</button>
        </footer>
      </div>
    </div>
  {/if}
</div>

<style>
  .editor {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: #0b0c0f;
    color: var(--fg);
    touch-action: none;
    user-select: none;
  }
  .topbar {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: calc(10px + env(safe-area-inset-top, 0px)) 12px 10px;
  }
  .title {
    flex: 1;
    min-width: 0;
    text-align: center;
    font-weight: 600;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: 34px;
    padding: 0 14px;
    border: 0;
    border-radius: var(--radius-pill);
    background: rgb(255 255 255 / 0.08);
    color: var(--fg);
    font-weight: 500;
    white-space: nowrap;
  }
  .pill:hover:not(:disabled) {
    background: rgb(255 255 255 / 0.14);
  }
  .pill:disabled {
    opacity: 0.4;
  }
  .pill.primary {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .pill.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--accent) 88%, white);
  }
  .save-wrap {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 6px);
    z-index: 5;
    display: flex;
    flex-direction: column;
    min-width: 180px;
    padding: 4px;
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
    box-shadow: 0 10px 30px rgb(0 0 0 / 0.5);
  }
  .menu button {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 40px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius);
    background: transparent;
    color: var(--fg);
    text-align: left;
    white-space: nowrap;
  }
  .menu button:hover {
    background: var(--hover);
  }
  .menu button.danger {
    color: var(--danger);
  }

  .stage {
    position: relative;
    flex: 1 1 0;
    min-height: 0;
    display: grid;
    place-items: center;
    overflow: hidden;
  }
  .box {
    position: relative;
    visibility: hidden;
  }
  .box.ready {
    visibility: visible;
  }
  /* The fill a turned picture leaves when filling is on. */
  .box.expand {
    background: #000;
    outline: 1px dashed rgb(255 255 255 / 0.25);
  }
  .media {
    position: absolute;
    left: 50%;
    top: 50%;
    max-width: none;
    pointer-events: none;
    transition: transform var(--t-med) var(--ease);
  }
  .crop {
    position: absolute;
    box-shadow: 0 0 0 9999px rgb(0 0 0 / 0.6);
    outline: 1px solid rgb(255 255 255 / 0.9);
    cursor: move;
    touch-action: none;
  }
  .crop.still {
    outline-color: rgb(255 255 255 / 0.35);
    pointer-events: none;
  }
  .third {
    position: absolute;
    background: rgb(255 255 255 / 0.35);
    opacity: 0;
    transition: opacity var(--t-med) var(--ease);
    pointer-events: none;
  }
  .crop.dragging .third {
    opacity: 1;
  }
  .third.v1,
  .third.v2 {
    top: 0;
    bottom: 0;
    width: 1px;
  }
  .third.v1 {
    left: 33.33%;
  }
  .third.v2 {
    left: 66.66%;
  }
  .third.h1,
  .third.h2 {
    left: 0;
    right: 0;
    height: 1px;
  }
  .third.h1 {
    top: 33.33%;
  }
  .third.h2 {
    top: 66.66%;
  }
  .handle {
    position: absolute;
    width: 28px;
    height: 28px;
    margin: -14px 0 0 -14px;
    touch-action: none;
  }
  .handle::after {
    content: "";
    position: absolute;
    inset: 9px;
    border-radius: 50%;
    background: #fff;
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.6);
  }
  .handle.n,
  .handle.s {
    left: 50%;
    cursor: ns-resize;
  }
  .handle.e,
  .handle.w {
    top: 50%;
    cursor: ew-resize;
  }
  .handle.n,
  .handle.ne,
  .handle.nw {
    top: 0;
  }
  .handle.s,
  .handle.se,
  .handle.sw {
    top: 100%;
  }
  .handle.w,
  .handle.nw,
  .handle.sw {
    left: 0;
  }
  .handle.e,
  .handle.ne,
  .handle.se {
    left: 100%;
  }
  .handle.ne,
  .handle.sw {
    cursor: nesw-resize;
  }
  .handle.nw,
  .handle.se {
    cursor: nwse-resize;
  }
  .play {
    position: absolute;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border: 0;
    border-radius: 50%;
    background: rgb(23 25 31 / 0.75);
    backdrop-filter: blur(8px);
    color: #fff;
  }

  .tools {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 10px 12px calc(10px + env(safe-area-inset-bottom, 0px));
    background: #111318;
    border-top: 1px solid var(--border);
  }
  /* Every tab takes the same height, so the picture does not jump when switching. */
  .panel {
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 10px;
    min-height: 108px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .angle {
    gap: 12px;
  }
  .dial {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }
  .dial input {
    width: 100%;
    accent-color: var(--accent);
  }
  .val {
    font-size: 12px;
    color: var(--fg-2);
  }
  .icon {
    display: grid;
    place-items: center;
    flex-shrink: 0;
    width: 40px;
    height: 40px;
    border: 0;
    border-radius: 50%;
    background: rgb(255 255 255 / 0.08);
    color: var(--fg);
  }
  .icon:hover {
    background: rgb(255 255 255 / 0.14);
  }
  .chips {
    overflow-x: auto;
    scrollbar-width: none;
    touch-action: pan-x;
  }
  .chips::-webkit-scrollbar {
    display: none;
  }
  .speeds {
    flex-wrap: wrap;
    overflow: visible;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
    height: 32px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-pill);
    background: rgb(255 255 255 / 0.07);
    color: var(--fg-2);
    font-size: 13px;
    white-space: nowrap;
  }
  .chip.on:not(.toggle) {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .chip.toggle .switch {
    pointer-events: none;
  }
  .sep {
    flex: 1;
    min-width: 8px;
  }
  .divider {
    flex-shrink: 0;
    width: 1px;
    height: 20px;
    background: var(--border);
  }
  .note {
    flex-shrink: 0;
    font-size: 12px;
    color: var(--muted);
  }
  .times {
    justify-content: space-between;
    font-size: 12px;
    color: var(--fg-2);
  }
  .times .mid {
    color: var(--fg);
    font-weight: 600;
  }
  /* Far enough from the screen edges that dragging a grip is not the system back gesture. */
  .track {
    position: relative;
    height: 40px;
    margin: 0 28px;
    border-radius: 8px;
    background: rgb(255 255 255 / 0.06);
    touch-action: none;
  }
  .kept {
    position: absolute;
    top: 0;
    bottom: 0;
    border-radius: 8px;
    background: color-mix(in srgb, var(--accent) 30%, transparent);
    box-shadow: inset 0 0 0 2px var(--accent);
  }
  .head {
    position: absolute;
    top: -4px;
    bottom: -4px;
    width: 2px;
    margin-left: -1px;
    background: #fff;
    pointer-events: none;
  }
  .grip {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 24px;
    margin-left: -12px;
    cursor: ew-resize;
    touch-action: none;
  }
  .grip::after {
    content: "";
    position: absolute;
    left: 8px;
    top: 6px;
    bottom: 6px;
    width: 8px;
    border-radius: 4px;
    background: var(--accent);
    box-shadow: 0 1px 3px rgb(0 0 0 / 0.5);
  }
  .tabs {
    display: flex;
    gap: 4px;
    padding: 3px;
    border-radius: var(--radius-pill);
    background: rgb(255 255 255 / 0.05);
  }
  .tabs button {
    flex: 1;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: 36px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--muted);
    font-weight: 500;
    white-space: nowrap;
  }
  .tabs button.on {
    background: rgb(255 255 255 / 0.12);
    color: var(--fg);
  }

  .busy {
    position: absolute;
    inset: 0;
    z-index: 6;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 0.55);
  }
  .busy-card {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }
  .busy-card .bar {
    width: 120px;
  }
  .busy-card .bar i {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .error {
    position: absolute;
    left: 50%;
    top: 64px;
    z-index: 7;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: calc(100% - 24px);
    padding: 8px 8px 8px 14px;
    border-radius: var(--radius-lg);
    background: color-mix(in srgb, var(--danger) 22%, var(--surface-2));
    color: var(--fg);
  }
  .error .icon {
    width: 28px;
    height: 28px;
  }
  @media (max-width: 600px) {
    .pill .label {
      display: none;
    }
    .pill {
      padding: 0 11px;
    }
  }
</style>
