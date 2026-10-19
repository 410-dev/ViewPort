<script lang="ts">
  import { ask } from "@tauri-apps/plugin-dialog";
  import { untrack } from "svelte";
  import { onBack } from "../back.svelte";
  import { pickFile, pickFiles, pickFolder } from "../pick.svelte";
  import { api, type BundleSummary, type ShardStatus, type SourceRow, type TransferMode } from "../api";
  import { fmtBytes, fmtCount } from "../format";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";
  import Select from "../ui/Select.svelte";
  import { errText } from "./shared";

  /** Manages one shard source: unlock, add and extract files, maintenance, encryption, location. */
  let {
    source,
    status,
    bundle,
    sources,
    onclose,
    onchanged,
  }: {
    source: SourceRow;
    status: ShardStatus | null;
    bundle: BundleSummary;
    sources: SourceRow[];
    onclose: () => void;
    /** Something about the shard changed; the caller refreshes sources and status. */
    onchanged: () => void;
  } = $props();

  let busy = $state(false);
  let message = $state<{ ok: boolean; text: string } | null>(null);
  let name = $state(untrack(() => source.name));

  let password = $state("");
  let password2 = $state("");
  let keyfile = $state("");
  let remember = $state(true);
  let mode = $state<TransferMode>("copy");
  let dest = $state("");
  let fromSource = $state("");
  let placeMode = $state<TransferMode>("move");
  let editing = $state<"password" | "encrypt" | null>(null);

  const locked = $derived(!!status && status.found && status.encrypted && !status.unlocked);
  const canWrite = $derived(!!status?.writable);
  const others = $derived(sources.filter((s) => s.id !== source.id));
  const mismatch = $derived(password2 !== "" && password !== password2);

  async function run(f: () => Promise<string | void>) {
    busy = true;
    message = null;
    try {
      const text = await f();
      message = { ok: true, text: text || t("shard.done") };
      onchanged();
    } catch (e) {
      message = { ok: false, text: errText(e) };
    } finally {
      busy = false;
    }
  }

  function resetSecrets() {
    password = "";
    password2 = "";
    keyfile = "";
    editing = null;
  }

  async function browseKeyfile() {
    const p = await pickFile(t("shard.keyfile"));
    if (p) keyfile = p;
  }

  const unlock = () =>
    run(async () => {
      await api.unlockShard(source.id, password, keyfile || null, bundle.locked && remember);
      resetSecrets();
    });
  const lock = () => run(() => api.lockShard(source.id));

  async function addFiles(folder: boolean) {
    const paths = folder ? [await pickFolder(t("shard.addFolder"))].filter((p): p is string => !!p) : await pickFiles(t("shard.addFiles"));
    if (paths.length === 0) return;
    await run(async () => {
      await api.shardImport(source.id, paths, dest, mode);
      return t("shard.started");
    });
  }
  const addFromSource = () =>
    run(async () => {
      await api.shardFromSource(Number(fromSource), source.id, dest, mode);
      return t("shard.started");
    });
  async function extract() {
    const target = await pickFolder(t("shard.exportPick"));
    if (!target) return;
    await run(async () => {
      await api.shardExport(source.id, dest ? [dest] : [], target);
      return t("shard.started");
    });
  }
  const verify = () =>
    run(async () => {
      const r = await api.shardVerify(source.id);
      if (r.damaged.length) throw { code: "damaged", message: t("shard.verifyBad", { n: r.damaged.length, list: r.damaged.slice(0, 10).join(", ") }) };
      return t("shard.verifyOk", { n: fmtCount(r.items) });
    });
  const compact = () =>
    run(async () => {
      const r = await api.shardCompact(source.id);
      return t("shard.compactDone", { n: r.chunks_removed, bytes: fmtBytes(r.bytes_freed) });
    });
  const changePassword = () =>
    run(async () => {
      await api.shardChangePassword(source.id, password, keyfile || null);
      resetSecrets();
    });
  const encryptOn = () =>
    run(async () => {
      await api.shardSetEncryption(source.id, password, keyfile || null, bundle.locked && remember);
      resetSecrets();
    });
  async function encryptOff() {
    if (!(await ask(t("shard.encryptOffConfirm"), { kind: "warning" }))) return;
    await run(() => api.shardSetEncryption(source.id, null, null, false));
  }
  async function relocate(toBundle: boolean) {
    const target = toBundle ? null : await pickFolder(t("shard.toFolder"));
    if (!toBundle && !target) return;
    await run(async () => {
      await api.shardRelocate(source.id, target, placeMode);
    });
  }
  async function repoint() {
    const p = await pickFolder(t("shard.shardFolder"), { pickSuffix: ".vpshard" });
    if (!p) return;
    await run(async () => {
      await api.updateSource(source.id, name, p, null, source.enabled);
    });
  }
  const rename = () =>
    run(async () => {
      await api.updateSource(source.id, name, source.root, null, source.enabled);
    });

  $effect(() => onBack(onclose));
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true">
    <header>
      <Icon name="archive" size={16} />
      <h2>{t("shard.manage")}</h2>
      <button class="icon-btn" onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>
    </header>

    <div class="body">
      <section>
        <span class="row">
          <input class="input" bind:value={name} aria-label={t("shard.name")} />
          <button class="btn" disabled={busy || !name.trim() || name === source.name} onclick={rename}>{t("common.save")}</button>
        </span>
        <div class="badges">
          {#if status?.encrypted}
            <span class="badge" class:warn={locked}><Icon name={locked ? "lock" : "unlock"} size={12} />{locked ? t("shard.status.locked") : t("shard.status.encrypted")}</span>
          {:else if status?.found}
            <span class="badge">{t("shard.status.plain")}</span>
          {/if}
          {#if status?.remembered}<span class="badge"><Icon name="key" size={12} />{t("shard.status.remembered")}</span>{/if}
          {#if status?.in_bundle}<span class="badge"><Icon name="layers" size={12} />{t("shard.inBundle")}</span>{/if}
          {#if status && status.unlocked && !status.writable}<span class="badge warn">{t("shard.status.readonly")}</span>{/if}
        </div>
        <p class="path" title={status?.path}>{status?.path}</p>
        {#if status?.stats}
          {@const s = status.stats}
          <p class="stat">{t("shard.stats", { files: fmtCount(s.files), bytes: fmtBytes(s.bytes) })}</p>
          <p class="stat">{t("shard.disk", { chunks: s.chunks, disk: fmtBytes(s.disk_bytes), garbage: fmtBytes(s.garbage_bytes) })}</p>
        {/if}
        {#if status?.busy}<p class="msg warn">{t("shard.status.busy")}</p>{/if}
        {#if status && !status.found}
          <p class="msg warn">{t("shard.status.missing")}</p>
          <div><button class="btn" disabled={busy} onclick={repoint}><Icon name="folder" size={14} />{t("common.browse")}</button></div>
        {/if}
      </section>

      {#if locked}
        <section>
          <h3>{t("shard.unlock")}</h3>
          <input class="input" type="password" autocomplete="current-password" placeholder={t("shard.password")} bind:value={password} />
          {#if status?.requires_keyfile}
            <span class="row">
              <input class="input" readonly value={keyfile} placeholder={t("shard.keyfile")} />
              <button class="btn" onclick={browseKeyfile}>{t("common.browse")}</button>
            </span>
          {/if}
          {#if bundle.locked}
            <label class="check">
              <button type="button" class="switch" class:on={remember} aria-label={t("shard.remember")} onclick={() => (remember = !remember)}></button>
              <span>{t("shard.remember")}</span>
            </label>
          {/if}
          <div><button class="btn primary" disabled={busy || !password} onclick={unlock}><Icon name="unlock" size={14} />{t("shard.unlock")}</button></div>
        </section>
      {:else if status?.unlocked}
        <section>
          <h3>{t("shard.contents")}</h3>
          <input class="input" bind:value={dest} placeholder={t("shard.dest")} />
          {#if canWrite}
            <div class="seg sm">
              <button class:on={mode === "copy"} onclick={() => (mode = "copy")}>{t("shard.mode.copy")}</button>
              <button class:on={mode === "move"} onclick={() => (mode = "move")}>{t("shard.mode.move")}</button>
            </div>
            {#if mode === "move"}<p class="hint">{t("shard.moveHint")}</p>{/if}
            <div class="actions">
              <button class="btn" disabled={busy} onclick={() => addFiles(false)}><Icon name="file" size={14} />{t("shard.addFiles")}</button>
              <button class="btn" disabled={busy} onclick={() => addFiles(true)}><Icon name="folderPlus" size={14} />{t("shard.addFolder")}</button>
            </div>
            {#if others.length}
              <span class="row">
                <Select
                  class="grow"
                  value={fromSource}
                  label={t("shard.fromSource")}
                  options={[{ value: "", label: t("shard.fromSource") }, ...others.map((s) => ({ value: String(s.id), label: s.name }))]}
                  onchange={(v) => (fromSource = v)}
                />
                <button class="btn" disabled={busy || !fromSource} onclick={addFromSource}><Icon name="upload" size={14} />{t("shard.addSource")}</button>
              </span>
            {/if}
          {/if}
          <div class="actions">
            <button class="btn" disabled={busy} onclick={extract}><Icon name="download" size={14} />{t("shard.export")}</button>
          </div>
        </section>

        <section>
          <h3>{t("shard.maintenance")}</h3>
          <div class="actions">
            <button class="btn" disabled={busy} onclick={verify}><Icon name="shield" size={14} />{t("shard.verify")}</button>
            {#if canWrite}<button class="btn" disabled={busy} onclick={compact}><Icon name="zap" size={14} />{t("shard.compact")}</button>{/if}
          </div>
        </section>

        {#if canWrite}
          <section>
            <h3>{t("shard.encryption")}</h3>
            <div class="actions">
              {#if status.encrypted}
                <button class="btn" class:on={editing === "password"} disabled={busy} onclick={() => (editing = editing === "password" ? null : "password")}
                  ><Icon name="key" size={14} />{t("shard.changePassword")}</button
                >
                <button class="btn danger" disabled={busy} onclick={encryptOff}>{t("shard.encryptOff")}</button>
                <button class="btn ghost" disabled={busy} onclick={lock} title={t("shard.lockHint")}><Icon name="lock" size={14} />{t("shard.lock")}</button>
              {:else}
                <button class="btn" class:on={editing === "encrypt"} disabled={busy} onclick={() => (editing = editing === "encrypt" ? null : "encrypt")}
                  ><Icon name="lock" size={14} />{t("shard.encryptOn")}</button
                >
              {/if}
            </div>
            {#if editing}
              <input class="input" type="password" autocomplete="new-password" placeholder={t("shard.password")} bind:value={password} />
              <input class="input" type="password" autocomplete="new-password" placeholder={t("shard.passwordConfirm")} bind:value={password2} />
              {#if mismatch}<p class="msg bad">{t("shard.passwordMismatch")}</p>{/if}
              <span class="row">
                <input class="input" readonly value={keyfile} placeholder={t("shard.keyfile")} />
                <button class="btn" onclick={browseKeyfile}>{t("common.browse")}</button>
              </span>
              {#if editing === "encrypt"}
                {#if bundle.locked}
                  <label class="check">
                    <button type="button" class="switch" class:on={remember} aria-label={t("shard.remember")} onclick={() => (remember = !remember)}></button>
                    <span>{t("shard.remember")}</span>
                  </label>
                {:else}
                  <p class="msg warn"><Icon name="alert" size={13} />{t("shard.plainWarn")}</p>
                {/if}
                <p class="hint">{t("shard.repackHint")}</p>
              {/if}
              <div>
                <button
                  class="btn primary"
                  disabled={busy || !password || password !== password2}
                  onclick={editing === "password" ? changePassword : encryptOn}>{t("shard.apply")}</button
                >
              </div>
            {/if}
          </section>
        {/if}
      {/if}

      {#if status?.found}
        <section>
          <h3>{t("shard.relocate")}</h3>
          <div class="seg sm">
            <button class:on={placeMode === "copy"} onclick={() => (placeMode = "copy")}>{t("shard.mode.copy")}</button>
            <button class:on={placeMode === "move"} onclick={() => (placeMode = "move")}>{t("shard.mode.move")}</button>
          </div>
          <div class="actions">
            {#if !status.in_bundle}
              <button class="btn" disabled={busy} onclick={() => relocate(true)}><Icon name="layers" size={14} />{t("shard.toBundle")}</button>
            {/if}
            <button class="btn" disabled={busy} onclick={() => relocate(false)}><Icon name="hardDrive" size={14} />{t("shard.toFolder")}</button>
          </div>
        </section>
      {/if}

      {#if message}<p class="msg" class:ok={message.ok} class:bad={!message.ok}>{message.text}</p>{/if}
    </div>

    <footer>
      <span class="grow"></span>
      <button class="btn ghost" onclick={onclose}>{t("common.close")}</button>
    </footer>
  </div>
</div>

<style>
  .body {
    display: grid;
    gap: 18px;
  }
  section {
    display: grid;
    gap: 8px;
  }
  h3 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .row .input,
  .row :global(.grow) {
    flex: 1;
    min-width: 0;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .btn.on {
    background: var(--accent-soft);
  }
  .seg {
    justify-self: start;
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    height: 22px;
    padding: 0 9px;
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    color: var(--fg-2);
    font-size: 11px;
  }
  .badge.warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 14%, transparent);
  }
  .path,
  .stat {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .stat {
    color: var(--fg-2);
  }
  label.check {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .hint {
    font-size: 12px;
    color: var(--faint);
    margin: 0;
    line-height: 1.5;
  }
  .msg {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius-md);
    font-size: 12px;
    line-height: 1.5;
    overflow-wrap: anywhere;
  }
  .msg :global(svg) {
    margin-top: 2px;
  }
  .ok {
    color: var(--ok);
    background: color-mix(in srgb, var(--ok) 12%, transparent);
  }
  .bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
  }
</style>
