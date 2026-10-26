<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import Select from "../ui/Select.svelte";
  import { ask } from "@tauri-apps/plugin-dialog";
  import { onBack } from "../back.svelte";
  import { pickFile } from "../pick.svelte";
  import { onDestroy, onMount, untrack } from "svelte";
  import {
    api,
    asCmdError,
    onTask,
    type AppSettings,
    type BundleSummary,
    type CacheStats,
    type FfmpegStatus,
    type HeifStatus,
    type Settings,
    type SourceRow,
  } from "../api";
  import { fmtBytes } from "../format";
  import { LOCALES, QUERY_LOCALES, t, type Key } from "../i18n.svelte";
  import { applyPrefs, prefs } from "../prefs.svelte";
  import Icon from "../ui/Icon.svelte";
  import { COLUMNS } from "../views/columns";
  import { DESKTOP_ONLY, SECTIONS as ALL_SECTIONS, type Field } from "./schema";
  import { isMobile } from "../platform";
  import { broadcast } from "../sync";

  let {
    settings,
    bundle,
    onclose,
    onbundle,
  }: { settings: Settings; bundle: BundleSummary; onclose: () => void; onbundle: (b: BundleSummary) => void } = $props();

  // Edits go to copies; appearance previews live and is reverted on cancel.
  let draft = $state<Settings>(untrack(() => structuredClone($state.snapshot(settings)) as Settings));
  let appDraft = $state<AppSettings>(untrack(() => structuredClone($state.snapshot(prefs.app!)) as AppSettings));
  const initial = untrack(() => JSON.stringify([$state.snapshot(settings), $state.snapshot(prefs.app)]));
  const dirty = $derived(JSON.stringify([draft, appDraft]) !== initial);

  $effect(() => {
    applyPrefs($state.snapshot(appDraft) as AppSettings);
  });

  function get(obj: any, path: string) {
    return path.split(".").reduce((o, k) => o?.[k], obj);
  }
  function set(obj: any, path: string, value: unknown) {
    const keys = path.split(".");
    const last = keys.pop()!;
    keys.reduce((o, k) => (o[k] ??= {}), obj)[last] = value;
  }
  const target = (f: Field) => ("target" in f && f.target === "app" ? appDraft : draft);

  function options(f: Extract<Field, { kind: "select" }>) {
    if (f.path === "ui.locale")
      return [{ value: "auto", label: t("set.locale.auto") }, ...LOCALES.map((l) => ({ value: l.code, label: l.name }))];
    if (f.path === "ui.query_locale")
      return [
        { value: "auto", label: t("set.queryLocale.auto") },
        ...LOCALES.filter((l) => QUERY_LOCALES.includes(l.code)).map((l) => ({ value: l.code, label: l.name })),
      ];
    return f.options.map((o) => ({ value: o.value, label: typeof o.label === "string" ? t(o.label as Key) : o.label.raw }));
  }

  // Mobile leaves out what only applies to desktop.
  const SECTIONS = isMobile
    ? ALL_SECTIONS.map((s) => ({
        ...s,
        groups: s.groups
          .map((g) => ({
            ...g,
            fields: g.fields.filter((f) => !("path" in f && DESKTOP_ONLY.has(f.path)) && !(f.kind === "custom" && f.id === "heif")),
          }))
          .filter((g) => g.fields.length),
      })).filter((s) => s.groups.length)
    : ALL_SECTIONS;

  const ACCENTS = ["#6b7cff", "#4f8cff", "#2bb4a0", "#3ecf8e", "#e0a43a", "#f06a6d", "#d36bd8", "#9aa0ad"];

  // ---- navigation and scroll spy
  let scroller: HTMLDivElement;
  let active = $state(SECTIONS[0].id);
  let filter = $state("");
  const visibleSections = $derived(
    filter.trim()
      ? SECTIONS.map((s) => ({
          ...s,
          groups: s.groups
            .map((g) => ({
              ...g,
              fields: g.fields.filter((f) => {
                const label = "label" in f && f.label ? t(f.label) : "";
                return (t(s.title) + " " + label).toLowerCase().includes(filter.trim().toLowerCase());
              }),
            }))
            .filter((g) => g.fields.length),
        })).filter((s) => s.groups.length)
      : SECTIONS,
  );

  function go(id: string) {
    const el = scroller.querySelector<HTMLElement>(`[data-section="${id}"]`);
    if (!el) return;
    active = id;
    scroller.scrollTo({ top: el.offsetTop - 8, behavior: appDraft.ui.reduce_motion ? "auto" : "smooth" });
  }

  function onScroll() {
    const top = scroller.scrollTop + 40;
    let current = visibleSections[0]?.id;
    for (const el of scroller.querySelectorAll<HTMLElement>("[data-section]")) {
      if (el.offsetTop <= top) current = el.dataset.section!;
    }
    // At the very bottom, the last section is the active one.
    if (scroller.scrollTop + scroller.clientHeight >= scroller.scrollHeight - 2) current = visibleSections.at(-1)?.id ?? current;
    if (current) active = current;
  }

  // ---- status blocks
  let stats = $state<CacheStats | null>(null);
  /** For per-source settings (image analysis). */
  let sources = $state<SourceRow[]>([]);
  api.listSources().then((l) => (sources = l)).catch(() => {});
  let ffmpeg = $state<FfmpegStatus | null>(null);
  let heif = $state<HeifStatus | null>(null);
  let incompatible = $state<number[]>([]);
  let convertStarted = $state(false);
  const timer = setInterval(() => api.cacheStats().then((s) => (stats = s)).catch(() => {}), 1000);
  const refreshHeif = () => api.heifStatus().then((h) => (heif = h)).catch(() => {});
  onMount(() => {
    api.cacheStats().then((s) => (stats = s)).catch(() => {});
    api.ffmpegStatus().then((f) => (ffmpeg = f)).catch(() => {});
    api.incompatibleVideos().then((ids) => (incompatible = ids)).catch(() => {});
    refreshHeif();
    // The download ends as a task (done or failed).
    const off = onTask((task) => task.kind === "download_heif" && task.state !== "running" && refreshHeif());
    return () => off.then((f) => f());
  });
  onDestroy(() => clearInterval(timer));
  async function installHeif() {
    if (heif) heif.installing = true;
    await api.heifInstall().catch(() => {});
    refreshHeif();
  }
  let heifRetried = $state<number | null>(null);
  async function retryHeif() {
    heifRetried = await api.heifRetryFailed().catch(() => null);
  }

  // ---- log
  let logCopied = $state(false);
  let logError = $state("");
  async function copyLog() {
    logError = "";
    try {
      const log = await api.logRead();
      await navigator.clipboard.writeText(`${log.path}\n\n${log.text}`);
      logCopied = true;
    } catch (e) {
      logError = asCmdError(e).message;
    }
  }

  // ---- lock
  let lockPw = $state("");
  let lockPw2 = $state("");
  let lockKeyfile = $state("");
  let lockBusy = $state(false);
  let lockError = $state("");
  async function pickLockKeyfile() {
    const p = await pickFile(t("lock.keyfile"));
    if (p) lockKeyfile = p;
  }
  async function applyLock() {
    lockError = "";
    if (lockPw !== lockPw2) {
      lockError = t("lock.mismatch");
      return;
    }
    lockBusy = true;
    try {
      onbundle(await api.setLock(lockPw, lockKeyfile || null));
      lockPw = lockPw2 = lockKeyfile = "";
    } catch (e) {
      lockError = asCmdError(e).message;
    } finally {
      lockBusy = false;
    }
  }
  async function liftLock() {
    if (!(await ask(t("lock.removeConfirm"), { kind: "warning" }))) return;
    lockBusy = true;
    lockError = "";
    try {
      onbundle(await api.removeLock());
    } catch (e) {
      lockError = asCmdError(e).message;
    } finally {
      lockBusy = false;
    }
  }

  // ---- cache memory: applied in place, no restart
  let cacheBusy = $state(false);
  let cacheNote = $state("");
  /** Saves the settings as edited so far, then resizes the cache to them. */
  async function resizeCache() {
    cacheBusy = true;
    cacheNote = "";
    try {
      await api.saveSettings($state.snapshot(draft) as Settings);
      Object.assign(settings, $state.snapshot(draft));
      await api.saveAppSettings($state.snapshot(appDraft) as AppSettings);
      prefs.app = structuredClone($state.snapshot(appDraft)) as AppSettings;
      broadcast("settings");
      stats = await api.cacheResize();
      cacheNote = t("cache.resized", { size: fmtBytes(stats.capacity_bytes) });
    } catch (e) {
      cacheNote = asCmdError(e).message;
    } finally {
      cacheBusy = false;
    }
  }
  async function clearCache() {
    cacheBusy = true;
    cacheNote = "";
    try {
      stats = await api.cacheClear();
      cacheNote = t("cache.cleared");
    } catch (e) {
      cacheNote = asCmdError(e).message;
    } finally {
      cacheBusy = false;
    }
  }
  const SHARE_HELP = {
    performance: "set.scheduler.performance.help",
    predictive: "set.scheduler.predictive.help",
    preprocess: "set.scheduler.preprocess.help",
  } as const;

  // ---- save / cancel
  let saving = $state(false);
  let error = $state("");
  async function save() {
    saving = true;
    error = "";
    try {
      await api.saveSettings($state.snapshot(draft) as Settings);
      await api.saveAppSettings($state.snapshot(appDraft) as AppSettings);
      Object.assign(settings, $state.snapshot(draft));
      broadcast("settings");
      prefs.app = structuredClone($state.snapshot(appDraft)) as AppSettings;
      applyPrefs(prefs.app);
      onclose();
    } catch (e) {
      error = asCmdError(e).message;
    } finally {
      saving = false;
    }
  }
  function cancel() {
    applyPrefs(prefs.app!);
    onclose();
  }
  $effect(() => onBack(cancel));
  function reset() {
    draft = structuredClone($state.snapshot(settings)) as Settings;
    appDraft = structuredClone($state.snapshot(prefs.app!)) as AppSettings;
  }
</script>

<svelte:window onkeydown={(e) => e.key === "Escape" && cancel()} />

<div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && cancel()}>
  <div out:pop class="dialog settings" role="dialog" aria-modal="true" aria-label={t("set.title")}>
    <aside class="nav">
      <div class="nav-head">
        <h2>{t("set.title")}</h2>
        <div class="input find">
          <Icon name="search" size={14} />
          <input placeholder={t("set.search")} bind:value={filter} />
        </div>
      </div>
      <div class="nav-list">
        {#each visibleSections as s (s.id)}
          <button class="nav-item" class:on={active === s.id} onclick={() => go(s.id)}>
            <Icon name={s.icon} size={15} /><span>{t(s.title)}</span>
          </button>
        {/each}
      </div>
    </aside>

    <div class="main">
      <div class="scroll" bind:this={scroller} onscroll={onScroll}>
        {#each visibleSections as s (s.id)}
          <section data-section={s.id}>
            <h3 class="stitle">{t(s.title)}</h3>
            {#each s.groups as g, gi (gi)}
              <div class="group">
                {#if g.title}<div class="gtitle">{t(g.title)}</div>{/if}
                {#if g.note}<p class="gnote">{t(g.note)}</p>{/if}
                <div class="card">
                  {#each g.fields as f, fi (fi)}
                    {#if f.kind === "custom"}
                      <div class="custom">
                        {#if f.id === "cache"}
                          {#if stats}
                            {@const cap = Math.max(1, stats.capacity_bytes)}
                            <div class="meter">
                              <div class="mbar">
                                <i class="alloc" style:width="{(stats.allocated_bytes / cap) * 100}%"></i>
                                <i class="used" style:width="{(stats.used_bytes / cap) * 100}%"></i>
                              </div>
                              <div class="kv">
                                <span><em>{t("cache.capacity")}</em>{fmtBytes(stats.capacity_bytes)}</span>
                                <span><em>{t("cache.reserved")}</em>{fmtBytes(stats.allocated_bytes)}</span>
                                <span><em>{t("cache.used")}</em>{fmtBytes(stats.used_bytes)} · {stats.items}</span>
                                <span
                                  ><em>{t("cache.hitRate")}</em>{stats.hits + stats.misses === 0
                                    ? "–"
                                    : `${Math.round((stats.hits / (stats.hits + stats.misses)) * 100)}%`}</span
                                >
                              </div>
                            </div>
                          {/if}
                          <div class="status">
                            <span class="grow">{cacheNote || t("cache.applyNote")}</span>
                            <button class="btn sm" disabled={cacheBusy} onclick={clearCache}>{t("cache.clear")}</button>
                            <button class="btn sm primary" disabled={cacheBusy} onclick={resizeCache}>{t("cache.resize")}</button>
                          </div>
                        {:else if f.id === "scheduler"}
                          {@const mode = appDraft.hardware.scheduler}
                          {@const path = `hardware.viewer_share_${mode}`}
                          {@const pct = Math.round((get(appDraft, path) ?? 0.5) * 100)}
                          <div class="share">
                            <p class="share-help">{t(SHARE_HELP[mode])}</p>
                            <div class="share-row">
                              <span class="share-lbl">{t("set.viewerShare")}</span>
                              <input
                                class="share-range"
                                type="range"
                                min="5"
                                max="95"
                                step="5"
                                value={pct}
                                aria-label={t("set.viewerShare")}
                                oninput={(e) => set(appDraft, path, +e.currentTarget.value / 100)}
                              />
                              <span class="num">
                                <input
                                  class="input"
                                  type="number"
                                  min="5"
                                  max="95"
                                  step="1"
                                  value={pct}
                                  onchange={(e) => set(appDraft, path, Math.min(95, Math.max(5, Math.round(+e.currentTarget.value))) / 100)}
                                />
                                <span class="unit">%</span>
                              </span>
                            </div>
                            <div class="share-bar">
                              <i style:width="{pct}%"><span>{t("set.share.viewer")} {pct}</span></i>
                              <b><span>{t("set.share.rest")} {100 - pct}</span></b>
                            </div>
                          </div>
                        {:else if f.id === "ffmpeg"}
                          {#if ffmpeg?.available}
                            <div class="status ok"><Icon name="check" size={14} /><span>FFmpeg · {ffmpeg.version ?? ffmpeg.path}</span></div>
                            {#if incompatible.length > 0 && draft.playback.suggest_conversion}
                              <div class="status">
                                <span class="grow">{t("ffmpeg.incompatible", { n: incompatible.length })}</span>
                                <button
                                  class="btn sm primary"
                                  disabled={convertStarted}
                                  onclick={() => {
                                    convertStarted = true;
                                    api.convertMedia(incompatible);
                                  }}>{convertStarted ? t("ffmpeg.converting") : t("ffmpeg.convertAll")}</button
                                >
                              </div>
                            {/if}
                          {:else if ffmpeg?.os_media}
                            <div class="status ok"><Icon name="check" size={14} /><span>{t("ffmpeg.osMedia")}</span></div>
                          {:else if ffmpeg}
                            <div class="status warn"><Icon name="alert" size={14} /><span>{t("ffmpeg.missing")}</span></div>
                          {/if}
                        {:else if f.id === "heif"}
                          {#if heif?.load_errors.length}
                            <div class="status warn"><Icon name="alert" size={14} /><span>{heif.load_errors.join(" · ")}</span></div>
                          {/if}
                          {#if heif?.available}
                            <div class="status ok"><Icon name="check" size={14} /><span>libheif {heif.version ?? ""} · {heif.path}</span></div>
                            <div class="status">
                              {#if heif.installable}
                                <button class="btn sm" disabled={heif.installing} onclick={installHeif}
                                  >{heif.installing ? t("heif.installing") : t("heif.reinstall")}</button
                                >
                              {/if}
                              <button class="btn sm" onclick={retryHeif}>{t("heif.retry")}</button>
                              {#if heifRetried != null}<span class="grow">{t("heif.retried", { n: heifRetried })}</span>{/if}
                            </div>
                          {:else if heif?.installable}
                            <div class="status warn">
                              <Icon name="alert" size={14} />
                              <span class="grow">{t("heif.missing")}</span>
                              <button class="btn sm primary" disabled={heif.installing} onclick={installHeif}
                                >{heif.installing ? t("heif.installing") : t("heif.install", { size: fmtBytes(heif.size) })}</button
                              >
                            </div>
                          {:else if heif}
                            <div class="status warn"><Icon name="alert" size={14} /><span>{t("heif.system")}</span></div>
                          {/if}
                        {:else if f.id === "log"}
                          <div class="status">
                            <button class="btn sm" onclick={copyLog}>{logCopied ? t("log.copied") : t("log.copy")}</button>
                            <button class="btn sm" onclick={() => api.logOpen(false).catch((e) => (logError = asCmdError(e).message))}>{t("log.open")}</button>
                            <button class="btn sm ghost" onclick={() => api.logOpen(true).catch((e) => (logError = asCmdError(e).message))}>{t("log.folder")}</button>
                            <button class="btn sm ghost" onclick={() => api.logClear().then(() => (logCopied = false))}>{t("log.clear")}</button>
                          </div>
                          {#if logError}<div class="status warn"><Icon name="alert" size={14} /><span>{logError}</span></div>{/if}
                        {:else if f.id === "analysis_sources"}
                          {#each sources as s (s.id)}
                            <div class="source-mode">
                              <span>{s.name}</span>
                              <Select
                                value={draft.analysis?.source_modes?.[String(s.id)] ?? "auto"}
                                label={s.name}
                                options={[
                                  { value: "auto", label: t("set.analysis.mode.auto") },
                                  { value: "real", label: t("set.analysis.mode.real") },
                                  { value: "anime", label: t("set.analysis.mode.anime") },
                                  { value: "off", label: t("set.analysis.mode.off") },
                                ]}
                                onchange={(v) => {
                                  const modes = { ...(draft.analysis.source_modes ?? {}) };
                                  if (v === "auto") delete modes[String(s.id)];
                                  else modes[String(s.id)] = v;
                                  draft.analysis.source_modes = modes;
                                }}
                              />
                            </div>
                          {:else}
                            <p class="text">{t("side.noSources")}</p>
                          {/each}
                        {:else if f.id === "lock"}
                          {#if bundle.locked}
                            <p class="text">{t("lock.lockedNote")}</p>
                            <div><button class="btn danger" disabled={lockBusy} onclick={liftLock}>{t("lock.remove")}</button></div>
                          {:else}
                            <p class="text">{t("lock.note")}</p>
                            <div class="lockform">
                              <input class="input" type="password" autocomplete="new-password" placeholder={t("lock.password")} bind:value={lockPw} />
                              <input class="input" type="password" autocomplete="new-password" placeholder={t("lock.confirm")} bind:value={lockPw2} />
                              <div class="pair">
                                <input class="input" readonly placeholder={t("lock.keyfile")} value={lockKeyfile} />
                                <button class="btn" onclick={pickLockKeyfile}>{t("common.browse")}</button>
                              </div>
                              <div>
                                <button class="btn primary" disabled={lockBusy || !lockPw || bundle.mode !== "autosave"} onclick={applyLock}
                                  ><Icon name="lock" size={14} />{t("lock.apply")}</button
                                >
                              </div>
                            </div>
                          {/if}
                          {#if lockError}<p class="err">{lockError}</p>{/if}
                        {/if}
                      </div>
                    {:else}
                      {@const obj = target(f)}
                      <div class="row" class:stack={f.kind === "columns" || f.kind === "list"}>
                        <div class="lbl">
                          <span>{t(f.label)}</span>
                          {#if f.help}<small>{t(f.help)}</small>{/if}
                        </div>
                        <div class="ctl">
                          {#if f.kind === "toggle"}
                            <button
                              class="switch"
                              class:on={!!get(obj, f.path)}
                              role="switch"
                              aria-checked={!!get(obj, f.path)}
                              aria-label={t(f.label)}
                              onclick={() => set(obj, f.path, !get(obj, f.path))}
                            ></button>
                          {:else if f.kind === "number"}
                            <span class="num">
                              <input
                                class="input"
                                type="number"
                                min={f.min}
                                max={f.max}
                                step={f.step}
                                value={+(get(obj, f.path) / (f.scale ?? 1)).toFixed(4)}
                                onchange={(e) => set(obj, f.path, Math.round(+e.currentTarget.value * (f.scale ?? 1) * 1e6) / 1e6)}
                              />
                              {#if f.unit}<span class="unit">{f.unit}</span>{/if}
                            </span>
                          {:else if f.kind === "select"}
                            {@const opts = options(f)}
                            {#if opts.length <= 3}
                              <div class="seg sm">
                                {#each opts as o (o.value)}
                                  <button class:on={get(obj, f.path) === o.value} onclick={() => set(obj, f.path, o.value)}>{o.label}</button>
                                {/each}
                              </div>
                            {:else}
                              <Select class="wide-select" value={get(obj, f.path)} options={opts} label={t(f.label)} onchange={(v) => set(obj, f.path, v)} />
                            {/if}
                          {:else if f.kind === "color"}
                            <div class="swatches">
                              {#each ACCENTS as c (c)}
                                <button
                                  class="sw"
                                  class:on={get(obj, f.path) === c}
                                  style:background={c}
                                  aria-label={c}
                                  onclick={() => set(obj, f.path, c)}
                                ></button>
                              {/each}
                              <input class="input hex" value={get(obj, f.path)} onchange={(e) => set(obj, f.path, e.currentTarget.value.trim())} />
                            </div>
                          {:else if f.kind === "list"}
                            <input
                              class="input wide"
                              value={(get(obj, f.path) ?? []).join(", ")}
                              onchange={(e) => set(obj, f.path, e.currentTarget.value.split(",").map((x) => x.trim()).filter(Boolean))}
                            />
                          {:else if f.kind === "columns"}
                            {@const cols = get(obj, f.path) as string[]}
                            <div class="chips">
                              {#each COLUMNS as c (c.key)}
                                <button
                                  class="chipbtn"
                                  class:on={cols.includes(c.key)}
                                  disabled={c.key === "name"}
                                  onclick={() =>
                                    set(obj, f.path, cols.includes(c.key) ? cols.filter((k) => k !== c.key) : COLUMNS.map((x) => x.key).filter((k) => k === c.key || cols.includes(k)))}
                                >
                                  {#if cols.includes(c.key)}<Icon name="check" size={12} />{/if}{t(c.label)}
                                </button>
                              {/each}
                            </div>
                          {:else}
                            <input
                              class="input wide"
                              placeholder={f.placeholder}
                              value={get(obj, f.path) ?? ""}
                              onchange={(e) => set(obj, f.path, e.currentTarget.value)}
                            />
                          {/if}
                        </div>
                      </div>
                    {/if}
                  {/each}
                </div>
              </div>
            {/each}
          </section>
        {/each}
        <div class="tail"></div>
      </div>

      <footer>
        {#if error}<span class="err">{error}</span>{:else if dirty}<span class="dirty">{t("set.unsaved")}</span>{/if}
        <span class="grow"></span>
        <button class="btn ghost" disabled={!dirty} onclick={reset}>{t("set.revert")}</button>
        <button class="btn" onclick={cancel}>{t("common.cancel")}</button>
        <button class="btn primary" disabled={saving || !dirty} onclick={save}>{t("common.save")}</button>
      </footer>
    </div>
    <button class="icon-btn close" onclick={cancel} aria-label={t("common.close")}><Icon name="x" size={16} /></button>
  </div>
</div>

<style>
  .settings {
    position: relative;
    width: min(1060px, 100%);
    height: min(780px, 100%);
    flex-direction: row;
  }
  .nav {
    width: 232px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface-2);
    border-right: 1px solid var(--border);
    min-height: 0;
  }
  .nav-head {
    padding: 20px 14px 10px;
    display: grid;
    gap: 12px;
  }
  .nav-head h2 {
    margin: 0 6px;
    font-size: 16px;
    font-weight: 600;
  }
  .find {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 12px;
    color: var(--faint);
    background: var(--surface);
  }
  .find input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: none;
    background: transparent;
    color: var(--fg);
    font: inherit;
  }
  .nav-list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 10px 14px;
    display: grid;
    align-content: start;
    gap: 1px;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 32px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-2);
    text-align: left;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .nav-item :global(svg) {
    color: var(--muted);
  }
  .nav-item:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .nav-item.on {
    background: var(--accent-soft);
    color: var(--fg);
    font-weight: 500;
  }
  .nav-item.on :global(svg) {
    color: var(--accent);
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 8px 36px 0;
    scroll-padding-top: 8px;
  }
  section {
    padding-top: 22px;
  }
  .stitle {
    margin: 0 0 14px;
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .group {
    margin-bottom: 22px;
  }
  .gtitle {
    margin: 0 4px 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
  }
  .gnote {
    margin: -4px 4px 8px;
    font-size: 12px;
    color: var(--faint);
  }
  .card {
    border-radius: var(--radius-lg);
    background: var(--surface-2);
    overflow: hidden;
  }
  .card .input,
  .card .seg {
    background: var(--surface);
  }
  .card .input:hover {
    background: var(--hover);
  }
  .card .seg button.on {
    background: var(--surface-3);
  }
  .row,
  .custom {
    display: flex;
    align-items: center;
    gap: 24px;
    min-height: 52px;
    padding: 10px 18px;
  }
  .row + .row,
  .row + .custom,
  .custom + .row,
  .custom + .custom {
    border-top: 1px solid var(--border);
  }
  .row.stack {
    flex-wrap: wrap;
    gap: 10px 24px;
  }
  .row.stack .ctl {
    flex-basis: 100%;
    justify-content: stretch;
  }
  .lbl {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 2px;
  }
  .lbl small {
    color: var(--faint);
    font-size: 12px;
    line-height: 1.45;
  }
  .ctl {
    display: flex;
    justify-content: flex-end;
    flex-shrink: 0;
    max-width: 60%;
  }
  .ctl :global(.wide-select) {
    min-width: 220px;
  }
  .num {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .num .input {
    width: 112px;
    text-align: right;
  }
  .unit {
    width: 26px;
    color: var(--faint);
    font-size: 12px;
  }
  .input.wide {
    width: 100%;
    min-width: 280px;
  }
  .row.stack .input.wide {
    min-width: 0;
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .sw {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    border: 0;
    padding: 0;
    box-shadow: inset 0 0 0 1px rgb(255 255 255 / 0.15);
    transition: transform var(--t-fast) var(--ease);
  }
  .sw:hover {
    transform: scale(1.12);
  }
  .sw.on {
    box-shadow:
      0 0 0 2px var(--surface-2),
      0 0 0 4px var(--fg-2);
  }
  .hex {
    width: 96px;
    margin-left: 6px;
    font-variant-numeric: tabular-nums;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chipbtn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 28px;
    padding: 0 12px;
    border-radius: var(--radius-pill);
    border: 1px solid var(--border-strong);
    background: transparent;
    color: var(--muted);
    font-size: 12px;
    transition: all var(--t-fast) var(--ease);
  }
  .chipbtn:hover:not(:disabled) {
    color: var(--fg);
    background: var(--hover);
  }
  .chipbtn.on {
    border-color: var(--accent);
    background: var(--accent-soft);
    color: var(--fg);
  }
  .chipbtn:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .custom {
    flex-direction: column;
    align-items: stretch;
    gap: 12px;
    padding: 16px 18px;
  }
  .meter {
    display: grid;
    gap: 12px;
  }
  .share {
    display: grid;
    gap: 10px;
  }
  .share-help {
    margin: 0;
    color: var(--muted);
    font-size: 12px;
  }
  .share-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .share-lbl {
    flex: none;
  }
  .share-range {
    flex: 1;
    min-width: 0;
    accent-color: var(--accent);
  }
  .share-bar {
    display: flex;
    height: 20px;
    border-radius: var(--radius-pill);
    overflow: hidden;
    font-size: 11px;
    font-style: normal;
    font-weight: 500;
  }
  .share-bar i,
  .share-bar b {
    display: flex;
    align-items: center;
    padding: 0 8px;
    overflow: hidden;
    white-space: nowrap;
    font-style: normal;
    font-weight: 500;
  }
  .share-bar i {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .share-bar b {
    flex: 1;
    justify-content: flex-end;
    background: var(--surface-3);
    color: var(--muted);
  }
  .mbar {
    position: relative;
    height: 8px;
    border-radius: var(--radius-pill);
    background: var(--surface-3);
    overflow: hidden;
  }
  .mbar i {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: inherit;
    transition: width var(--t-med) linear;
  }
  .mbar .alloc {
    background: var(--accent-soft);
  }
  .mbar .used {
    background: var(--accent);
  }
  .kv {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 8px;
    font-variant-numeric: tabular-nums;
  }
  .kv span {
    display: grid;
    gap: 2px;
  }
  .kv em {
    font-style: normal;
    font-size: 11px;
    color: var(--faint);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--fg-2);
  }
  /* FFmpeg path or version can be one long unbreakable string. */
  .status > span {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .status.ok :global(svg) {
    color: var(--ok);
  }
  .status.warn {
    color: var(--warn);
  }
  .text {
    margin: 0;
    font-size: 12px;
    line-height: 1.6;
    color: var(--muted);
  }
  .lockform {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
    max-width: 420px;
  }
  .source-mode {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 4px 0;
  }
  .source-mode span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pair {
    display: flex;
    gap: 6px;
  }
  .pair .input {
    flex: 1;
  }
  .err {
    margin: 0;
    color: var(--danger);
    font-size: 12px;
  }
  .tail {
    height: 40vh;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 20px;
    border-top: 1px solid var(--border);
  }
  .dirty {
    font-size: 12px;
    color: var(--warn);
  }
  .close {
    position: absolute;
    top: 12px;
    right: 12px;
  }
  @media (max-width: 760px) {
    .settings {
      flex-direction: column;
    }
    .settings {
      height: 100%;
    }
    .nav {
      width: auto;
      border-right: 0;
      border-bottom: 1px solid var(--border);
    }
    /* Sections become one scrolling row of chips. */
    .nav-list {
      display: flex;
      overflow-x: auto;
      overflow-y: hidden;
      gap: 4px;
      padding: 0 12px 10px;
      scrollbar-width: none;
    }
    .nav-item {
      flex-shrink: 0;
      white-space: nowrap;
    }
    .main {
      flex: 1;
      min-height: 0;
    }
    footer {
      flex-wrap: wrap;
    }
    .scroll {
      padding: 8px 16px 0;
    }
    .row {
      flex-wrap: wrap;
    }
    .ctl {
      max-width: 100%;
      min-width: 0;
    }
    .input.wide {
      min-width: 0;
    }
    .kv {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
