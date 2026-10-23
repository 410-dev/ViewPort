<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import { onMount } from "svelte";
  import { api, type MirrorRow, type SourceRow } from "../api";
  import { onBack } from "../back.svelte";
  import { fmtDate } from "../format";
  import { t } from "../i18n.svelte";
  import { tasks } from "../tasks.svelte";
  import Icon from "../ui/Icon.svelte";
  import Select from "../ui/Select.svelte";
  import { errText } from "./shared";

  /**
   * Mirrors of one source (`mirror.rs`): kept copied into a folder of another source,
   * synced whenever both are connected. Only what a mirror copied is ever replaced or
   * deleted in the target.
   */
  let { source, sources, onclose }: { source: SourceRow; sources: SourceRow[]; onclose: () => void } = $props();

  let mirrors = $state<MirrorRow[]>([]);
  let error = $state<string | null>(null);
  let busy = $state(false);

  // New mirror
  let target = $state("");
  let folder = $state("");
  let deletes = $state(true);

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
    } catch (e) {
      error = errText(e);
    } finally {
      busy = false;
    }
  }

  const add = () =>
    run(async () => {
      await api.mirrorSave(null, source.id, Number(target), folder.trim(), true, deletes);
      target = "";
    });
  const update = (m: MirrorRow, change: Partial<MirrorRow>) => {
    const n = { ...m, ...change };
    return run(() => api.mirrorSave(n.id, n.source_id, n.target_id, n.target_folder, n.enabled, n.propagate_deletes));
  };
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
            <Icon name="loop" size={14} />
            <span class="grow"><b>{nameOf(m.target_id)}</b>{m.target_folder ? ` / ${m.target_folder}` : ""}</span>
            <button type="button" class="switch" class:on={m.enabled} aria-label={t("mirror.enabled")} title={t("mirror.enabled")}
              onclick={() => update(m, { enabled: !m.enabled })}></button>
          </div>
          <label class="check">
            <button type="button" class="switch" class:on={m.propagate_deletes} aria-label={t("mirror.deletes")}
              onclick={() => update(m, { propagate_deletes: !m.propagate_deletes })}></button>
            <span>{t("mirror.deletes")}</span>
          </label>
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
          <button type="button" class="switch" class:on={deletes} aria-label={t("mirror.deletes")} onclick={() => (deletes = !deletes)}></button>
          <span>{t("mirror.deletes")}</span>
        </label>
        <p class="hint">{t("mirror.deletesHint")}</p>
        <div class="line">
          <span class="grow"></span>
          <button class="btn sm primary" disabled={busy || !target} onclick={add}><Icon name="plus" size={13} />{t("mirror.addBtn")}</button>
        </div>
      </div>

      {#if error}<p class="msg bad">{error}</p>{/if}
    </div>
  </div>
</div>

<style>
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
