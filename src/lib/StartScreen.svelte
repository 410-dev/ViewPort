<script lang="ts">
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { api, asCmdError, type BundleSummary, type CmdError } from "./api";
  import { t } from "./i18n.svelte";

  let { onopened }: { onopened: (b: BundleSummary) => void } = $props();

  let recent = $state<string[]>([]);
  let error = $state<CmdError | null>(null);
  let pendingNewer = $state<{ path: string; found: number; supported: number } | null>(null);
  let pendingLock = $state<{ path: string; requiresKeyfile: boolean; allowNewer: boolean } | null>(null);
  let password = $state("");
  let keyfile = $state("");
  let unlockError = $state("");
  let busy = $state(false);

  $effect(() => {
    api.recentBundles().then((r) => (recent = r));
  });

  async function run(f: () => Promise<BundleSummary>) {
    busy = true;
    error = null;
    try {
      onopened(await f());
    } catch (e) {
      error = asCmdError(e);
    } finally {
      busy = false;
    }
  }

  async function create() {
    const path = await save({
      title: t("start.create"),
      defaultPath: `${t("start.newName")}.vpbun`,
      filters: [{ name: "ViewPort bundle", extensions: ["vpbun"] }],
    });
    if (!path) return;
    const name = path.split(/[\\/]/).pop()!.replace(/\.vpbun$/i, "");
    await run(() => api.createBundle(path, name));
  }

  async function pickAndOpen() {
    const path = await open({ title: t("start.open"), directory: true });
    if (typeof path === "string") await openPath(path);
  }

  async function openPath(path: string, allowNewer = false) {
    try {
      busy = true;
      error = null;
      onopened(await api.openBundle(path, allowNewer));
    } catch (e) {
      const err = asCmdError(e);
      // An archive is extracted first; continue with the extracted folder.
      const dir = err.detail?.path ?? path;
      if (err.code === "bundle_too_new") pendingNewer = { path: dir, found: err.detail.found, supported: err.detail.supported };
      else if (err.code === "locked") {
        password = "";
        keyfile = "";
        unlockError = "";
        pendingLock = { path: dir, requiresKeyfile: !!err.detail?.requires_keyfile, allowNewer };
      } else error = err;
    } finally {
      busy = false;
    }
  }

  async function importArchive() {
    const path = await open({
      title: t("start.import"),
      filters: [{ name: "ViewPort bundle archive", extensions: ["zip", "vpbun"] }],
    });
    if (typeof path === "string") await openPath(path);
  }

  async function pickKeyfile() {
    const p = await open({ title: t("start.keyfile") });
    if (typeof p === "string") keyfile = p;
  }

  async function unlock() {
    if (!pendingLock) return;
    busy = true;
    unlockError = "";
    try {
      const b = await api.unlockBundle(pendingLock.path, password, keyfile || null, pendingLock.allowNewer);
      pendingLock = null;
      password = "";
      onopened(b);
    } catch (e) {
      unlockError = asCmdError(e).message;
    } finally {
      busy = false;
    }
  }
</script>

<div class="start">
  <header>
    <h1>ViewPort</h1>
    <p>{t("app.tagline")}</p>
  </header>

  <div class="actions">
    <button class="primary" disabled={busy} onclick={create}>{t("start.create")}</button>
    <button disabled={busy} onclick={pickAndOpen}>{t("start.open")}</button>
    <button disabled={busy} onclick={importArchive}>{t("start.import")}</button>
  </div>

  {#if error}
    <p class="error">{error.message}</p>
  {/if}

  <section>
    <h2>{t("start.recent")}</h2>
    {#if recent.length === 0}
      <p class="muted">{t("start.none")}</p>
    {:else}
      <ul>
        {#each recent as path (path)}
          <li>
            <button class="recent" disabled={busy} onclick={() => openPath(path)}>
              <span class="name">{path.split(/[\\/]/).pop()}</span>
              <span class="path">{path}</span>
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

{#if pendingLock}
  <div class="backdrop" role="presentation">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="lock-title">
      <h2 id="lock-title">{t("start.locked.title")}</h2>
      <p class="path">{pendingLock.path}</p>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        type="password"
        autocomplete="current-password"
        placeholder={t("start.password")}
        bind:value={password}
        autofocus
        onkeydown={(e) => e.key === "Enter" && password && unlock()}
      />
      {#if pendingLock.requiresKeyfile}
        <div class="row">
          <input readonly placeholder={t("start.keyfile")} value={keyfile} />
          <button type="button" onclick={pickKeyfile}>{t("start.browse")}</button>
        </div>
      {/if}
      {#if unlockError}<p class="error">{unlockError}</p>{/if}
      <div class="actions end">
        <button type="button" onclick={() => (pendingLock = null)}>{t("common.cancel")}</button>
        <button class="primary" disabled={busy || !password} onclick={unlock}>{t("start.unlock")}</button>
      </div>
    </div>
  </div>
{/if}

{#if pendingNewer}
  <div class="backdrop" role="presentation">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="newer-title">
      <h2 id="newer-title">{t("start.tooNew.title")}</h2>
      <p>{t("start.tooNew.body", pendingNewer)}</p>
      <div class="actions end">
        <button onclick={() => (pendingNewer = null)}>{t("common.cancel")}</button>
        <button
          class="primary"
          onclick={() => {
            const p = pendingNewer!.path;
            pendingNewer = null;
            openPath(p, true);
          }}>{t("start.tooNew.confirm")}</button
        >
      </div>
    </div>
  </div>
{/if}

<style>
  .start {
    max-width: 560px;
    margin: 0 auto;
    padding: 64px 16px;
    display: grid;
    gap: 24px;
  }
  h1 {
    margin: 0;
    font-size: 32px;
    letter-spacing: -0.02em;
  }
  header p {
    margin: 4px 0 0;
    color: var(--muted);
  }
  h2 {
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    margin: 0 0 8px;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  .actions.end {
    justify-content: flex-end;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 4px;
  }
  .recent {
    width: 100%;
    text-align: left;
    display: grid;
    gap: 2px;
    background: var(--panel);
  }
  .recent .path {
    font-size: 12px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .muted {
    color: var(--muted);
  }
  .error {
    color: var(--danger);
    margin: 0;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    background: #0008;
    display: grid;
    place-items: center;
    padding: 16px;
  }
  .dialog {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 20px;
    max-width: 440px;
    display: grid;
    gap: 12px;
  }
  .dialog h2 {
    text-transform: none;
    letter-spacing: 0;
    font-size: 17px;
    color: var(--fg);
    margin: 0;
  }
  .dialog p {
    margin: 0;
    line-height: 1.5;
  }
  .dialog input {
    font: inherit;
    color: inherit;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 7px 10px;
    min-width: 0;
  }
  .dialog .row {
    display: flex;
    gap: 6px;
  }
  .dialog .row input {
    flex: 1;
  }
  .path {
    font-size: 12px;
    color: var(--muted);
    word-break: break-all;
  }
</style>
