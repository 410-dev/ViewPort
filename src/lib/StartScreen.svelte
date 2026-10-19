<script lang="ts">
  import Select from "./ui/Select.svelte";
  import { api, asCmdError, platformApi, type BundleSummary, type CmdError, type RecentBundle } from "./api";
  import { onBack } from "./back.svelte";
  import { pickFile, pickFolder, pickSave } from "./pick.svelte";
  import { isAndroid } from "./platform";
  import { LOCALES, t } from "./i18n.svelte";
  import { prefs, savePrefs } from "./prefs.svelte";
  import Icon from "./ui/Icon.svelte";

  const THEMES = ["dark", "light", "system"] as const;

  let { onopened }: { onopened: (b: BundleSummary) => void } = $props();

  let recent = $state<RecentBundle[]>([]);
  const hideMissing = $derived(!!prefs.app?.hide_missing_recent);
  const shown = $derived(hideMissing ? recent.filter((r) => r.exists) : recent);
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

  // Android: media and bundles are read by path, which needs "all files access".
  let storageOk = $state(true);
  $effect(() => {
    if (isAndroid) platformApi.info().then((i) => (storageOk = i.storage_granted)).catch(() => {});
  });
  async function grantStorage() {
    storageOk = await platformApi.requestStorageAccess().catch(() => false);
  }

  $effect(() => {
    if (pendingLock) return onBack(() => (pendingLock = null));
  });
  $effect(() => {
    if (pendingNewer) return onBack(() => (pendingNewer = null));
  });

  function toggleHideMissing() {
    if (!prefs.app) return;
    prefs.app.hide_missing_recent = !prefs.app.hide_missing_recent;
    savePrefs(0);
  }

  async function forget(path: string) {
    await api.forgetRecentBundle(path).catch(() => {});
    recent = recent.filter((r) => r.path !== path);
  }

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
    const path = await pickSave(t("start.create"), `${t("start.newName")}.vpbun`, { name: "ViewPort bundle", extensions: ["vpbun"] });
    if (!path) return;
    const name = path.split(/[\\/]/).pop()!.replace(/\.vpbun$/i, "");
    await run(() => api.createBundle(path, name));
  }

  async function pickAndOpen() {
    const path = await pickFolder(t("start.open"), { pickSuffix: ".vpbun" });
    if (path) await openPath(path);
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
    const path = await pickFile(t("start.import"), { name: "ViewPort bundle archive", extensions: ["zip", "vpbun"] });
    if (path) await openPath(path);
  }

  async function pickKeyfile() {
    const p = await pickFile(t("start.keyfile"));
    if (p) keyfile = p;
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

    {#if !storageOk}
      <div class="perm">
        <Icon name="shield" size={18} />
        <div class="grow">
          <strong>{t("start.storage.title")}</strong>
          <p>{t("start.storage.body")}</p>
        </div>
        <button class="btn primary sm" onclick={grantStorage}>{t("start.storage.grant")}</button>
      </div>
    {/if}

    {#if error}
      <p class="error">{error.message}</p>
    {/if}

    <section>
      <div class="recent-head">
        <div class="eyebrow grow">{t("start.recent")}</div>
        {#if prefs.app}
          <label class="hide-missing">
            <span>{t("start.hideMissing")}</span>
            <button type="button" class="switch" class:on={hideMissing} aria-label={t("start.hideMissing")} onclick={toggleHideMissing}></button>
          </label>
        {/if}
      </div>
      {#if shown.length === 0}
        <p class="muted">{t("start.none")}</p>
      {:else}
        <ul>
          {#each shown as r (r.path)}
            <li class:missing={!r.exists}>
              <button class="recent" disabled={busy} onclick={() => openPath(r.path)}>
                <span class="ico"><Icon name="layers" size={15} /></span>
                <span class="txt">
                  <span class="name">{r.path.split(/[\\/]/).pop()}</span>
                  <span class="path">{r.exists ? r.path : `${t("start.missing")} · ${r.path}`}</span>
                </span>
              </button>
              <button class="icon-btn forget" aria-label={t("start.forget")} title={t("start.forget")} onclick={() => forget(r.path)}>
                <Icon name="x" size={14} />
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
      <Select
        class="sm"
        value={prefs.app.ui.locale}
        options={[{ value: "auto", label: t("set.locale.auto") }, ...LOCALES.map((l) => ({ value: l.code, label: l.name }))]}
        onchange={(v) => {
          prefs.app!.ui.locale = v;
          savePrefs(0);
        }}
      />
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
  /* minmax(0, 1fr) tracks throughout: auto tracks would grow to the min-content width of
     a long recent path (or an input plus its button) and push the card off-screen. */
  .start {
    height: 100vh;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    place-items: center;
    align-content: center;
    gap: 18px;
    padding: 24px;
    overflow-y: auto;
  }
  .card {
    width: min(560px, 100%);
    display: grid;
    grid-template-columns: minmax(0, 1fr);
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
    grid-template-columns: minmax(0, 1fr);
    gap: 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 2px;
    max-height: 320px;
    overflow-y: auto;
  }
  .recent-head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .hide-missing {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--faint);
    cursor: pointer;
  }
  li {
    display: flex;
    align-items: center;
    gap: 2px;
  }
  li.missing .recent {
    opacity: 0.55;
  }
  .forget {
    flex-shrink: 0;
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease);
  }
  li:hover .forget,
  .forget:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .forget {
      opacity: 1;
    }
  }
  .recent {
    flex: 1;
    min-width: 0;
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
  .perm {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 14px;
    border-radius: var(--radius-md);
    background: var(--accent-soft);
    color: var(--accent);
  }
  .perm strong {
    color: var(--fg);
  }
  .perm p {
    margin: 2px 0 0;
    font-size: 12px;
    color: var(--fg-2);
  }
  .error {
    color: var(--danger);
    margin: 0;
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .prefs {
    display: flex;
    flex-wrap: wrap;
    justify-content: center;
    max-width: 100%;
    align-items: center;
    gap: 8px;
    color: var(--faint);
  }
  .prefs :global(.select) {
    height: var(--h-control-sm);
    font-size: 12px;
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
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
