<script lang="ts">
  import { expand, fade, pop } from "../ui/motion";
  import { onMount } from "svelte";
  import { api, type MirrorRow, type MirrorTriggers, type SourceRow } from "../api";
  import { onBack } from "../back.svelte";
  import { fmtDate } from "../format";
  import { t } from "../i18n.svelte";
  import { tasks } from "../tasks.svelte";
  import Icon from "../ui/Icon.svelte";
  import Select from "../ui/Select.svelte";
  import { errText } from "./shared";

  /**
   * Mirrors of one source (`mirror.rs`): kept copied into a folder of another source,
   * synced by itself as its triggers say (bundle opened, both connected again, source
   * changed in a scan, every so often) or by hand. Only what a mirror copied is ever
   * replaced or deleted in the target.
   */
  let {
    source,
    sources,
    onclose,
    onchange,
  }: {
    source: SourceRow;
    sources: SourceRow[];
    onclose: () => void;
    /** Mirrors were added, changed or removed (the library re-reads which sources are replicas). */
    onchange?: () => void;
  } = $props();

  let mirrors = $state<MirrorRow[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // New mirror
  let target = $state("");
  let folder = $state("");
  let replica = $state(true);
  let triggers = $state<MirrorTriggers>({ on_open: true, on_connect: true, on_scan: false, interval_mins: null });
  /** Mirror whose trigger settings are open. */
  let editing = $state<number | null>(null);
  /** The library's interval, which mirrors follow unless they set their own. */
  let defaultMins = $state(30);
  /** Interval choices offered, in minutes ("" follows the library, 0 never). */
  const INTERVALS = [0, 15, 30, 60, 180, 360, 720, 1440];

  /** Writable places: shards and local folders. */
  const targets = $derived(sources.filter((s) => s.id !== source.id && (s.driver === "shard" || s.driver === "local" || s.driver === "bundle")));
  const mine = $derived(mirrors.filter((m) => m.source_id === source.id));
  const nameOf = (id: number) => sources.find((s) => s.id === id)?.name ?? `#${id}`;
  const syncing = $derived(tasks.list.some((x) => x.kind === "mirror" && x.state === "running"));

  async function refresh() {
    mirrors = await api.mirrorList().catch(() => []);
  }
  onMount(() => {
    // svelte-ignore state_referenced_locally
    folder = source.name;
    refresh();
    api.getSettings().then((s) => (defaultMins = Number(s.mirror?.resync_mins ?? 30)), () => {});
  });
  // Results arrive when a sync ends.
  let was = false;
  $effect(() => {
    const now = syncing;
    if (was && !now) refresh();
    was = now;
  });
  $effect(() => onBack(onclose));

  async function run(f: () => Promise<unknown>) {
    busy = true;
    error = null;
    try {
      await f();
      await refresh();
      onchange?.();
    } catch (e) {
      error = errText(e);
    } finally {
      busy = false;
    }
  }

  const add = () =>
    run(async () => {
      await api.mirrorSave(null, source.id, Number(target), folder.trim(), true, replica, { ...triggers });
      target = "";
    });
  const update = (m: MirrorRow, change: Partial<MirrorRow>) => {
    const n = { ...m, ...change };
    return run(() => api.mirrorSave(n.id, n.source_id, n.target_id, n.target_folder, n.enabled, n.replica));
  };
  const setTriggers = (m: MirrorRow, change: Partial<MirrorTriggers>) => {
    const tr = { ...m.triggers, ...change };
    // Shown at once; the list refreshes when saved.
    mirrors = mirrors.map((x) => (x.id === m.id ? { ...x, triggers: tr } : x));
    return run(() => api.mirrorSave(m.id, m.source_id, m.target_id, m.target_folder, m.enabled, m.replica, tr));
  };

  function fmtMins(n: number): string {
    if (n % 1440 === 0) return t("mirror.everyDays", { n: n / 1440 });
    if (n % 60 === 0) return t("mirror.everyHours", { n: n / 60 });
    return t("mirror.everyMins", { n });
  }

  /** One line saying when a mirror syncs by itself. */
  function triggerSummary(tr: MirrorTriggers): string {
    const mins = tr.interval_mins ?? defaultMins;
    const parts = [
      tr.on_open && t("mirror.onOpenShort"),
      tr.on_connect && t("mirror.onConnectShort"),
      tr.on_scan && t("mirror.onScanShort"),
      mins > 0 && fmtMins(mins),
    ].filter(Boolean);
    return parts.length ? parts.join(" · ") : t("mirror.manualOnly");
  }

  const intervalOptions = $derived([
    { value: "", label: t("mirror.intervalDefault", { every: defaultMins > 0 ? fmtMins(defaultMins) : t("mirror.intervalOff") }) },
    ...INTERVALS.map((n) => ({ value: String(n), label: n === 0 ? t("mirror.intervalOff") : fmtMins(n) })),
  ]);
</script>

<div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onclose()}>
  <div out:pop class="dialog mirror" role="dialog" aria-modal="true" aria-labelledby="mirror-title">
    <header>
      <h2 id="mirror-title">{t("mirror.title", { name: source.name })}</h2>
      <button class="icon-btn" onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>
    </header>
    <div class="body form">
      <p class="hint">{t("mirror.intro")}</p>

      {#each mine as m (m.id)}
        <div class="card">
          <div class="line">
            <Icon name="mirror" size={14} />
            <span class="grow"
              >{nameOf(m.source_id)}<span class="arrow"><Icon name="arrowRight" size={13} /></span><b>{nameOf(m.target_id)}</b
              >{m.target_folder ? ` / ${m.target_folder}` : ""}</span
            >
            <button type="button" class="switch" class:on={m.enabled} aria-label={t("mirror.enabled")} title={t("mirror.enabled")}
              onclick={() => update(m, { enabled: !m.enabled })}></button>
          </div>
          <label class="check">
            <button type="button" class="switch" class:on={m.replica} aria-label={t("mirror.replica")}
              onclick={() => update(m, { replica: !m.replica })}></button>
            <span>{t("mirror.replica")}</span>
          </label>
          <div class="when">
            <button class="when-head" aria-expanded={editing === m.id} onclick={() => (editing = editing === m.id ? null : m.id)}>
              <Icon name="zap" size={13} />
              <span class="grow"><span class="faint">{t("mirror.when")}</span> {triggerSummary(m.triggers)}</span>
              <span class="chev" class:open={editing === m.id}><Icon name="chevronDown" size={13} /></span>
            </button>
            {#if editing === m.id}
              <div transition:expand>{@render triggerEditor(m.triggers, (c) => setTriggers(m, c))}</div>
            {/if}
          </div>
          <p class="hint">
            {m.last_sync_ms ? t("mirror.last", { at: fmtDate(m.last_sync_ms) }) : t("mirror.never")}
            {#if m.last_result}· {m.last_result}{/if}
          </p>
          <div class="line">
            <button class="btn sm" disabled={busy || syncing} onclick={() => run(() => api.mirrorSync(m.id))}>
              <Icon name="refresh" size={13} />{syncing ? t("mirror.syncing") : t("mirror.syncNow")}
            </button>
            <span class="grow"></span>
            <button class="btn sm ghost danger" disabled={busy} onclick={() => run(() => api.mirrorDelete(m.id))}>
              <Icon name="trash" size={13} />{t("mirror.remove")}
            </button>
          </div>
        </div>
      {/each}

      <div class="card">
        <span class="eyebrow">{t("mirror.add")}</span>
        <Select
          value={target}
          label={t("mirror.target")}
          options={[{ value: "", label: t("mirror.target") }, ...targets.map((s) => ({ value: String(s.id), label: s.name }))]}
          onchange={(v) => (target = v)}
        />
        <label>
          <span>{t("mirror.folder")}</span>
          <input class="input" bind:value={folder} placeholder={t("mirror.folderRoot")} />
        </label>
        <label class="check">
          <button type="button" class="switch" class:on={replica} aria-label={t("mirror.replica")} onclick={() => (replica = !replica)}></button>
          <span>{t("mirror.replica")}</span>
        </label>
        <p class="hint">{t("mirror.replicaHint")}</p>
        <span class="eyebrow">{t("mirror.when")}</span>
        {@render triggerEditor(triggers, (c) => (triggers = { ...triggers, ...c }))}
        <div class="line">
          <span class="grow"></span>
          <button class="btn sm primary" disabled={busy || !target} onclick={add}><Icon name="plus" size={13} />{t("mirror.addBtn")}</button>
        </div>
      </div>

      {#if error}<p class="msg bad">{error}</p>{/if}
    </div>
  </div>
</div>

{#snippet triggerEditor(tr: MirrorTriggers, set: (c: Partial<MirrorTriggers>) => void)}
  <div class="triggers">
    {#each [["on_open", "mirror.onOpen"], ["on_connect", "mirror.onConnect"], ["on_scan", "mirror.onScan"]] as const as [key, label] (key)}
      <label class="check">
        <button type="button" class="switch" class:on={tr[key]} aria-label={t(label)} onclick={() => set({ [key]: !tr[key] })}></button>
        <span>{t(label)}</span>
      </label>
    {/each}
    <div class="interval">
      <span>{t("mirror.interval")}</span>
      <Select
        value={tr.interval_mins == null ? "" : String(tr.interval_mins)}
        label={t("mirror.interval")}
        options={intervalOptions}
        onchange={(v) => set({ interval_mins: v === "" ? null : Number(v) })}
      />
    </div>
    <p class="hint">{t("mirror.whenHint")}</p>
  </div>
{/snippet}

<style>
  .when {
    display: grid;
    gap: 8px;
  }
  .when-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 6px 8px;
    margin: 0 -8px;
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--fg-2);
    font-size: 12px;
    text-align: left;
    cursor: pointer;
  }
  .when-head:hover {
    background: var(--hover);
  }
  .faint {
    color: var(--faint);
  }
  .chev {
    display: inline-flex;
    transition: transform var(--t-fast) var(--ease);
  }
  .chev.open {
    transform: rotate(180deg);
  }
  .triggers {
    display: grid;
    gap: 10px;
    padding: 10px 12px;
    border-radius: var(--radius-md);
    background: var(--surface);
    border: 1px solid var(--border);
  }
  .interval {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .interval > span {
    flex: 1;
    min-width: 120px;
  }
  .interval :global(.select) {
    flex: 1 1 220px;
    min-width: 0;
  }
  .mirror {
    width: min(520px, 100%);
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 12px;
  }
  .card {
    display: grid;
    gap: 8px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .arrow {
    display: inline-block;
    vertical-align: -2px;
    margin: 0 6px;
    color: var(--muted);
  }
  .grow {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  label {
    display: grid;
    gap: 6px;
  }
  label > span:first-child {
    font-size: 12px;
    color: var(--muted);
  }
  label.check {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  label.check > span {
    font-size: inherit;
    color: var(--fg);
  }
  .hint {
    font-size: 12px;
    color: var(--faint);
    margin: 0;
    line-height: 1.5;
  }
  .msg {
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius-md);
    font-size: 12px;
  }
  .bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .danger {
    color: var(--danger);
  }
</style>
