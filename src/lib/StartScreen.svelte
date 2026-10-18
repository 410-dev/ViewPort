<script lang="ts">
  import { open, save } from "@tauri-apps/plugin-dialog";
  import { api, asCmdError, type BundleSummary, type CmdError } from "./api";
  import { LOCALES, t } from "./i18n.svelte";
  import { prefs, savePrefs } from "./prefs.svelte";
  import Icon from "./ui/Icon.svelte";

  const THEMES = ["dark", "light", "system"] as const;

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
  <div class="card">
    <header>
      <span class="logo"><Icon name="layers" size={22} /></span>
      <div>
        <h1>ViewPort</h1>
        <p>{t("app.tagline")}</p>
      </div>
    </header>

    <div class="actions">
      <button class="btn primary" disabled={busy} onclick={create}><Icon name="plus" size={14} />{t("start.create")}</button>
      <button class="btn" disabled={busy} onclick={pickAndOpen}><Icon name="folder" size={14} />{t("start.open")}</button>
      <button class="btn" disabled={busy} onclick={importArchive}><Icon name="archive" size={14} />{t("start.import")}</button>
    </div>

    {#if error}
      <p class="error">{error.message}</p>
    {/if}

    <section>
      <div class="eyebrow">{t("start.recent")}</div>
      {#if recent.length === 0}
        <p class="muted">{t("start.none")}</p>
      {:else}
        <ul>
          {#each recent as path (path)}
            <li>
              <button class="recent" disabled={busy} onclick={() => openPath(path)}>
                <span class="ico"><Icon name="layers" size={15} /></span>
                <span class="txt">
                  <span class="name">{path.split(/[\\/]/).pop()}</span>
                  <span class="path">{path}</span>
                </span>
                <Icon name="chevronRight" size={14} />
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>

  {#if prefs.app}
    <div class="prefs">
      <Icon name="globe" size={14} />
      <select
        class="input"
        value={prefs.app.ui.locale}
        onchange={(e) => {
          prefs.app!.ui.locale = e.currentTarget.value;
          savePrefs(0);
        }}
      >
        <option value="auto">{t("set.locale.auto")}</option>
        {#each LOCALES as l (l.code)}<option value={l.code}>{l.name}</option>{/each}
      </select>
      <div class="seg sm">
        {#each THEMES as th (th)}
          <button
            class:on={prefs.app.ui.theme === th}
            onclick={() => {
              prefs.app!.ui.theme = th;
              savePrefs(0);
            }}>{t(`set.theme.${th}`)}</button
          >
        {/each}
      </div>
    </div>
  {/if}
</div>

{#if pendingLock}
  <div class="scrim" role="presentation">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="lock-title">
      <header><h2 id="lock-title">{t("start.locked.title")}</h2></header>
      <div class="body form">
        <p class="path">{pendingLock.path}</p>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input"
          type="password"
          autocomplete="current-password"
          placeholder={t("start.password")}
          bind:value={password}
          autofocus
          onkeydown={(e) => e.key === "Enter" && password && unlock()}
        />
        {#if pendingLock.requiresKeyfile}
          <div class="row">
            <input class="input" readonly placeholder={t("start.keyfile")} value={keyfile} />
            <button class="btn" type="button" onclick={pickKeyfile}>{t("common.browse")}</button>
          </div>
        {/if}
        {#if unlockError}<p class="error">{unlockError}</p>{/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" type="button" onclick={() => (pendingLock = null)}>{t("common.cancel")}</button>
        <button class="btn primary" disabled={busy || !password} onclick={unlock}><Icon name="key" size={14} />{t("start.unlock")}</button>
      </footer>
    </div>
  </div>
{/if}

{#if pendingNewer}
  <div class="scrim" role="presentation">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="newer-title">
      <header><h2 id="newer-title">{t("start.tooNew.title")}</h2></header>
      <div class="body"><p class="text">{t("start.tooNew.body", pendingNewer)}</p></div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" onclick={() => (pendingNewer = null)}>{t("common.cancel")}</button>
        <button
          class="btn primary"
          onclick={() => {
            const p = pendingNewer!.path;
            pendingNewer = null;
            openPath(p, true);
          }}>{t("start.tooNew.confirm")}</button
        >
      </footer>
    </div>
  </div>
{/if}

<style>
  .start {
    height: 100vh;
    display: grid;
    place-items: center;
    align-content: center;
    gap: 18px;
    padding: 24px;
    overflow-y: auto;
  }
  .card {
    width: min(560px, 100%);
    display: grid;
    gap: 22px;
    padding: 28px;
    border-radius: 18px;
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
  header {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .logo {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: 13px;
    background: var(--accent);
    color: var(--accent-fg);
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 650;
    letter-spacing: -0.02em;
  }
  header p {
    margin: 2px 0 0;
    color: var(--muted);
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  section {
    display: grid;
    gap: 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    max-height: 320px;
    overflow-y: auto;
  }
  .recent {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 8px 14px 8px 8px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--muted);
    text-align: left;
    transition: background var(--t-fast) var(--ease);
  }
  .recent:hover:not(:disabled) {
    background: var(--hover);
  }
  .recent .ico {
    display: grid;
    place-items: center;
    width: 32px;
    height: 32px;
    border-radius: 9px;
    background: var(--surface-2);
    color: var(--accent);
    flex-shrink: 0;
  }
  .txt {
    flex: 1;
    min-width: 0;
    display: grid;
  }
  .name {
    color: var(--fg);
    font-weight: 500;
  }
  .path,
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    font-size: 12px;
    color: var(--faint);
  }
  .muted {
    margin: 0;
    color: var(--faint);
  }
  .error {
    color: var(--danger);
    margin: 0;
    font-size: 12px;
  }
  .prefs {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--faint);
  }
  .prefs .input {
    height: var(--h-control-sm);
    font-size: 12px;
  }
  .form {
    display: grid;
    gap: 10px;
  }
  .form .path {
    white-space: normal;
    word-break: break-all;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row .input {
    flex: 1;
  }
  .text {
    margin: 0;
    line-height: 1.6;
    color: var(--fg-2);
  }
</style>
