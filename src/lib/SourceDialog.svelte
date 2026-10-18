<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { untrack } from "svelte";
  import { api, asCmdError, type DriverKind, type SourceRow } from "./api";
  import { t, tx } from "./i18n.svelte";
  import Icon from "./ui/Icon.svelte";

  /** Adds a network source, or edits any source (`source` given). */
  let { source = null, onclose, ondone }: { source?: SourceRow | null; onclose: () => void; ondone: (s: SourceRow) => void } =
    $props();

  // Driver and field labels come from the dictionary (`src.driver.<kind>`, `src.field.<key>`);
  // unknown drivers fall back to their own names.
  const fieldLabel = (key: string) => tx(`src.field.${key}`, key);
  const driverLabel = (kind: string) => tx(`src.driver.${kind}`, kind);

  // The dialog edits a snapshot of the source it was opened with.
  const initial = untrack(() => source);
  let kinds = $state<DriverKind[]>([]);
  let driver = $state(initial?.driver ?? "smb");
  let name = $state(initial?.name ?? "");
  let root = $state(initial?.root ?? "");
  let enabled = $state(initial?.enabled ?? true);
  let config = $state<Record<string, any>>({});
  let busy = $state(false);
  let message = $state<{ ok: boolean; text: string } | null>(null);

  const isLocal = $derived(driver === "local" || driver === "bundle");
  const fields = $derived(kinds.find((k) => k.kind === driver)?.fields ?? []);

  api.driverKinds().then((k) => {
    kinds = k;
    if (!initial) {
      for (const f of k.find((x) => x.kind === driver)?.fields ?? []) if (f.default != null) config[f.key] = f.default;
    }
  });
  if (initial && initial.driver !== "local" && initial.driver !== "bundle") {
    api.sourceConfig(initial.id).then((c) => (config = c));
  }

  function values() {
    const out: Record<string, any> = {};
    for (const f of fields) {
      const v = config[f.key];
      if (v === undefined || v === "") continue;
      out[f.key] = f.kind.type === "integer" ? Number(v) : v;
    }
    return out;
  }

  async function test() {
    busy = true;
    message = null;
    try {
      const n = await api.testSource(driver, root, values());
      message = { ok: true, text: t("src.connected", { n }) };
    } catch (e) {
      message = { ok: false, text: asCmdError(e).message };
    } finally {
      busy = false;
    }
  }

  async function save() {
    busy = true;
    message = null;
    try {
      const s = source
        ? await api.updateSource(source.id, name, root, isLocal ? null : values(), enabled)
        : await api.addSource(driver, name, root, values());
      ondone(s);
    } catch (e) {
      message = { ok: false, text: asCmdError(e).message };
    } finally {
      busy = false;
    }
  }

  async function pickFolder() {
    const p = await open({ directory: true });
    if (typeof p === "string") root = p;
  }
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true">
    <header>
      <h2>{source ? t("src.edit") : t("src.add")}</h2>
      <button class="icon-btn" onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>
    </header>

    <div class="body form">
      {#if !source}
        <label>
          <span>{t("src.type")}</span>
          <select class="input" bind:value={driver}>
            {#each kinds as k (k.kind)}<option value={k.kind}>{driverLabel(k.kind)}</option>{/each}
          </select>
        </label>
      {/if}

      <label>
        <span>{t("src.name")}</span>
        <input class="input" bind:value={name} placeholder={t("src.nameAuto")} />
      </label>

      {#if !isLocal}
        {#each fields as f (f.key)}
          {#if f.kind.type === "bool"}
            <label class="check">
              <button type="button" class="switch" class:on={!!config[f.key]} aria-label={fieldLabel(f.key)} onclick={() => (config[f.key] = !config[f.key])}></button>
              <span>{fieldLabel(f.key)}</span>
            </label>
          {:else}
            <label>
              <span>{fieldLabel(f.key)}{f.required ? " *" : ""}</span>
              {#if f.kind.type === "secret"}
                <input class="input" type="password" autocomplete="off" bind:value={config[f.key]} />
              {:else if f.kind.type === "integer"}
                <input class="input" type="number" min={f.kind.min} max={f.kind.max} bind:value={config[f.key]} />
              {:else}
                <input class="input" bind:value={config[f.key]} />
              {/if}
            </label>
          {/if}
        {/each}
        <label>
          <span>{t("src.subfolder")}</span>
          <input class="input" bind:value={root} placeholder="photos/2026" />
        </label>
        <p class="hint"><Icon name="key" size={12} />{t("src.secretHint")}</p>
      {:else}
        <label>
          <span>{t("src.folder")}</span>
          <span class="row">
            <input class="input" bind:value={root} readonly={driver === "bundle"} />
            {#if driver === "local"}<button class="btn" onclick={pickFolder}>{t("common.browse")}</button>{/if}
          </span>
        </label>
        <p class="hint">{t("src.movedHint")}</p>
      {/if}

      {#if source}
        <label class="check">
          <button type="button" class="switch" class:on={enabled} aria-label={t("src.enabled")} onclick={() => (enabled = !enabled)}></button>
          <span>{t("src.enabled")}</span>
        </label>
      {/if}

      {#if message}<p class="msg" class:ok={message.ok} class:bad={!message.ok}>{message.text}</p>{/if}
    </div>

    <footer>
      {#if !isLocal}<button class="btn ghost" disabled={busy} onclick={test}><Icon name="zap" size={14} />{t("src.test")}</button>{/if}
      <span class="grow"></span>
      <button class="btn ghost" onclick={onclose}>{t("common.cancel")}</button>
      <button class="btn primary" disabled={busy} onclick={save}>{source ? t("common.save") : t("common.add")}</button>
    </footer>
  </div>
</div>

<style>
  .form {
    display: grid;
    gap: 12px;
  }
  label {
    display: grid;
    gap: 5px;
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
  .row {
    display: flex;
    gap: 6px;
  }
  .row .input {
    flex: 1;
  }
  .hint {
    display: flex;
    gap: 6px;
    align-items: center;
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
    overflow-wrap: anywhere;
  }
  .ok {
    color: var(--ok);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
  }
  .bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
</style>
