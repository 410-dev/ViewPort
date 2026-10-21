<script lang="ts">
  import { api, asCmdError, displayUrl, mediaUrl, thumbUrl } from "../api";
  import { pickFolder } from "../pick.svelte";
  import { fmtBytes, fmtDuration } from "../format";
  import { t } from "../i18n.svelte";
  import { prefs, ui } from "../prefs.svelte";
  import { fraction, tasks } from "../tasks.svelte";
  import Icon from "../ui/Icon.svelte";
  import { briefs, reportBroken, want } from "../views/briefs.svelte";

  /**
   * Preview of the focused item. The thumbnail shows at once; the original loads after
   * `ui.preview_delay_ms`, so scrubbing through a slow drive with the arrow keys only
   * reads the file you stop on. Read progress comes from the backend task registry.
   */
  let {
    id,
    selected,
    onopen,
    onopenwindow,
  }: { id: number | null; selected: number[]; onopen: (id: number) => void; onopenwindow?: (id: number) => void } = $props();

  const b = $derived(id == null ? null : briefs.get(id) ?? null);
  $effect(() => {
    if (id != null) want([id]);
  });
  $effect(() => want(selected.slice(0, 9)));

  let armed = $state<number | null>(null);
  let loaded = $state(false);
  let failed = $state(false);
  let forced = $state<number | null>(null);

  $effect(() => {
    const current = id;
    loaded = false;
    failed = false;
    armed = null;
    if (current == null) return;
    const timer = setTimeout(() => (armed = current), ui().preview_delay_ms);
    return () => clearTimeout(timer);
  });

  const tooBig = $derived(!!b && ui().preview_max_bytes > 0 && b.size > ui().preview_max_bytes && forced !== b.id);
  const show = $derived(!!b && armed === b.id && !tooBig);
  const read = $derived(tasks.list.find((x) => x.kind === "read" && x.media_id === id && x.state === "running") ?? null);

  /** Copies the items into a folder (the only thing non-media files can do). */
  async function exportItems(ids: number[]) {
    const dest = await pickFolder(t("panel.exportTo"));
    if (dest) api.exportMedia(ids, dest).catch((e) => alert(asCmdError(e).message));
  }
</script>

<div class="preview">
  {#if selected.length > 1}
    <div class="multi">
      <div class="stack">
        {#each selected.slice(0, 9) as sid (sid)}
          <div class="mini"><img src={thumbUrl(sid)} alt="" loading="lazy" /></div>
        {/each}
      </div>
      <p>{t("panel.selected", { n: selected.length.toLocaleString() })}</p>
      <button class="btn sm" onclick={() => exportItems(selected)}><Icon name="download" size={14} />{t("panel.export")}</button>
    </div>
  {:else if b && b.kind === "nonmedia"}
    <div class="stage nopreview">
      <Icon name="file" size={40} stroke={1.3} />
      <span>{t("panel.noPreview")}</span>
      <button class="btn sm" onclick={() => exportItems([b.id])}><Icon name="download" size={14} />{t("panel.export")}</button>
    </div>
    <div class="meta">
      <span class="top"><span class="name" title={b.name}>{b.name}</span></span>
      <span class="sub tabular">{b.ext.toUpperCase()} · {fmtBytes(b.size)}</span>
    </div>
  {:else if b}
    <button class="stage" ondblclick={() => onopen(b.id)} title={t("panel.openHint")}>
      {#if !loaded}
        <img class="thumb" src={thumbUrl(b.id)} alt="" />
      {/if}
      {#if show}
        {#key b.id}
          {#if b.kind === "video"}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              src={mediaUrl(b.id)}
              muted
              loop
              playsinline
              autoplay={ui().preview_autoplay}
              controls
              preload="metadata"
              class:hidden={!loaded}
              onloadeddata={() => (loaded = true)}
              onerror={() => {
                failed = true;
                reportBroken(b.id);
              }}
            ></video>
          {:else}
            <img
              class="full"
              class:hidden={!loaded}
              src={displayUrl(b, prefs.app?.viewer.display_max_px ?? 4096)}
              alt={b.name}
              decoding="async"
              onload={() => (loaded = true)}
              onerror={() => {
                failed = true;
                reportBroken(b.id);
              }}
            />
          {/if}
        {/key}
      {/if}
      {#if tooBig}
        <span class="overlay">
          <span class="btn sm" role="presentation" onclick={(e) => { e.stopPropagation(); forced = b.id; }}>
            <Icon name="eye" size={14} />{t("panel.loadOriginal", { size: fmtBytes(b.size) })}
          </span>
        </span>
      {:else if failed}
        <span class="overlay note"><Icon name="alert" size={14} />{t("panel.cannotPreview")}</span>
      {:else if show && !loaded}
        <span class="loading">
          <span class="spinner"></span>
          {#if read}
            {@const f = fraction(read)}
            <span class="tabular">{fmtBytes(read.done)} / {fmtBytes(read.total)}</span>
            <span class="bar"><i style:width="{(f ?? 0) * 100}%"></i></span>
          {:else}
            <span>{t("panel.loading")}</span>
          {/if}
        </span>
      {/if}
    </button>
    <div class="meta">
      <span class="top">
        <span class="name" title={b.name}>{b.name}</span>
        <button class="icon-btn sm" title={t("panel.export")} onclick={() => exportItems([b.id])}><Icon name="download" size={13} /></button>
        <button class="icon-btn sm" title={t("panel.openViewer")} onclick={() => onopen(b.id)}><Icon name="maximize" size={13} /></button>
        {#if onopenwindow}
          <button class="icon-btn sm" title={t("panel.openWindow")} onclick={() => onopenwindow(b.id)}><Icon name="external" size={13} /></button>
        {/if}
      </span>
      <span class="sub tabular">
        {b.ext.toUpperCase()}
        {#if b.width}· {b.width} × {b.height}{/if}
        · {fmtBytes(b.size)}
        {#if b.kind === "video" && b.duration_ms}· {fmtDuration(b.duration_ms)}{/if}
      </span>
    </div>
  {:else}
    <div class="empty"><Icon name="image" size={28} stroke={1.3} /><span>{t("panel.nothing")}</span></div>
  {/if}
</div>

<style>
  .preview {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    padding: 12px;
    gap: 10px;
  }
  .stage {
    position: relative;
    flex: 1;
    min-height: 0;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--radius-md);
    background: var(--tile);
    overflow: hidden;
    cursor: zoom-in;
  }
  .stage > img,
  .stage > video {
    grid-area: 1 / 1;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .thumb {
    filter: blur(0.5px);
  }
  .hidden {
    visibility: hidden;
  }
  .loading,
  .overlay {
    position: absolute;
    left: 50%;
    bottom: 12px;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 8px;
    height: 28px;
    padding: 0 12px;
    border-radius: var(--radius-pill);
    background: color-mix(in srgb, var(--surface) 88%, transparent);
    backdrop-filter: blur(6px);
    color: var(--fg-2);
    font-size: 12px;
    white-space: nowrap;
    box-shadow: 0 2px 10px rgb(0 0 0 / 0.25);
  }
  .overlay {
    background: transparent;
    box-shadow: none;
    backdrop-filter: none;
    padding: 0;
  }
  .overlay.note {
    background: color-mix(in srgb, var(--surface) 88%, transparent);
    padding: 0 12px;
    color: var(--warn);
  }
  .loading .bar {
    width: 80px;
  }
  .meta {
    display: grid;
    gap: 2px;
    padding: 0 2px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 2px;
    min-width: 0;
  }
  .name {
    flex: 1;
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .sub {
    color: var(--muted);
    font-size: 12px;
  }
  .stage.nopreview {
    align-content: center;
    gap: 12px;
    color: var(--muted);
    font-size: 12px;
    text-align: center;
    padding: 16px;
    cursor: default;
  }
  .multi {
    flex: 1;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 12px;
    color: var(--muted);
  }
  .stack {
    display: grid;
    grid-template-columns: repeat(3, 64px);
    gap: 6px;
  }
  .mini {
    width: 64px;
    height: 64px;
    border-radius: var(--radius-sm);
    background: var(--tile);
    overflow: hidden;
  }
  .mini img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .multi p {
    margin: 0;
  }
  .empty {
    flex: 1;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 8px;
    color: var(--faint);
  }
</style>
