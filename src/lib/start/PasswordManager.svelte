<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import Icon from "../ui/Icon.svelte";
  import { vaultApi, type RecentBundle, type VaultEntry } from "../api";
  import { onBack } from "../back.svelte";
  import { pickFile, pickFolder } from "../pick.svelte";
  import { t } from "../i18n.svelte";
  import { refreshVault, vault, vaultError } from "./vault.svelte";

  let { recent, onclose }: { recent: RecentBundle[]; onclose: () => void } = $props();

  /** Adding (choose a bundle, then its password) or changing one entry's password. */
  type Form =
    | { kind: "pick" }
    | { kind: "add"; path: string; keyfileNeeded: boolean | null }
    | { kind: "change"; entry: VaultEntry };

  let form = $state<Form | null>(null);
  let confirmDelete = $state<string | null>(null);
  let password = $state("");
  let keyfile = $state("");
  let error = $state("");
  let busy = $state(false);

  $effect(() => {
    refreshVault();
  });
  $effect(() => onBack(() => (form ? (form = null) : onclose())));

  const saved = $derived(new Set(vault.entries.map((e) => e.id)));
  const candidates = $derived(recent.filter((r) => r.exists && r.locked && !(r.id && saved.has(r.id))));

  function name(path: string) {
    return path.split(/[\\/]/).pop()!.replace(/\.vpbun$/i, "");
  }

  function startForm(f: Form) {
    password = "";
    keyfile = f.kind === "change" ? (f.entry.keyfile ?? "") : "";
    error = "";
    confirmDelete = null;
    form = f;
  }

  async function browse() {
    const path = await pickFolder(t("vault.browse"), { pickSuffix: ".vpbun" });
    if (path) startForm({ kind: "add", path, keyfileNeeded: null });
  }

  async function pickKeyfile() {
    const p = await pickFile(t("start.keyfile"));
    if (p) keyfile = p;
  }

  async function save() {
    if (!form || form.kind === "pick" || !password) return;
    busy = true;
    error = "";
    try {
      if (form.kind === "add") await vaultApi.save(form.path, password, keyfile || null);
      else await vaultApi.change(form.entry.id, password, null, keyfile || null);
      password = "";
      form = null;
      await refreshVault();
    } catch (e) {
      error = vaultError(e);
    } finally {
      busy = false;
    }
  }

  async function remove(id: string) {
    await vaultApi.remove(id).catch(() => {});
    confirmDelete = null;
    await refreshVault();
  }

  const title = $derived(
    !form ? t("vault.title") : form.kind === "pick" ? t("vault.pick") : form.kind === "add" ? name(form.path) : form.entry.name,
  );
</script>

<div out:fade class="scrim" role="presentation">
  <div out:pop class="dialog manager" role="dialog" aria-modal="true" aria-labelledby="vault-title">
    <header>
      {#if form}
        <button class="icon-btn" aria-label={t("common.back")} onclick={() => (form = null)}><Icon name="chevronLeft" size={16} /></button>
      {/if}
      <h2 id="vault-title">{title}</h2>
      <button class="icon-btn" aria-label={t("common.close")} onclick={onclose}><Icon name="x" size={16} /></button>
    </header>

    {#if !form}
      <div class="body list">
        {#if vault.entries.length === 0}
          <p class="muted">{t("vault.empty")}</p>
        {:else}
          <ul>
            {#each vault.entries as e (e.id)}
              <li>
                <span class="ico"><Icon name="lock" size={15} /></span>
                <span class="txt">
                  <span class="name">{e.name}</span>
                  <span class="path">{e.path}</span>
                  <span class="path">{t("vault.savedAt", { date: new Date(e.saved_ms).toLocaleDateString() })}</span>
                </span>
                {#if confirmDelete === e.id}
                  <span class="confirm">
                    <button class="btn sm ghost" onclick={() => (confirmDelete = null)}>{t("common.cancel")}</button>
                    <button class="btn sm danger" onclick={() => remove(e.id)}>{t("vault.delete")}</button>
                  </span>
                {:else}
                  <button class="icon-btn" title={t("vault.change")} aria-label={t("vault.change")} onclick={() => startForm({ kind: "change", entry: e })}>
                    <Icon name="pencil" size={14} />
                  </button>
                  <button class="icon-btn" title={t("vault.delete")} aria-label={t("vault.delete")} onclick={() => (confirmDelete = e.id)}>
                    <Icon name="trash" size={14} />
                  </button>
                {/if}
              </li>
              {#if confirmDelete === e.id}
                <li class="warn">{t("vault.deleteConfirm", { name: e.name })}</li>
              {/if}
            {/each}
          </ul>
        {/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn primary" onclick={() => startForm({ kind: "pick" })}><Icon name="plus" size={14} />{t("vault.add")}</button>
      </footer>
    {:else if form.kind === "pick"}
      <div class="body list">
        {#if candidates.length === 0}
          <p class="muted">{t("vault.pick.none")}</p>
        {:else}
          <ul>
            {#each candidates as r (r.path)}
              <li>
                <button class="pick" onclick={() => startForm({ kind: "add", path: r.path, keyfileNeeded: r.requires_keyfile })}>
                  <span class="ico"><Icon name="lock" size={15} /></span>
                  <span class="txt">
                    <span class="name">{name(r.path)}</span>
                    <span class="path">{r.path}</span>
                  </span>
                </button>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn" onclick={browse}><Icon name="folder" size={14} />{t("vault.browse")}</button>
      </footer>
    {:else}
      <div class="body form">
        <p class="path">{form.kind === "add" ? form.path : form.entry.path}</p>
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input"
          type="password"
          autocomplete="off"
          placeholder={form.kind === "change" ? t("vault.newPassword") : t("start.password")}
          bind:value={password}
          autofocus
          onkeydown={(e) => e.key === "Enter" && save()}
        />
        {#if (form.kind === "add" && form.keyfileNeeded !== false) || (form.kind === "change" && form.entry.keyfile)}
          <div class="row">
            <input class="input" readonly placeholder={t("start.keyfile")} value={keyfile} />
            <button class="btn" type="button" onclick={pickKeyfile}>{t("common.browse")}</button>
          </div>
        {/if}
        {#if busy}<p class="muted">{t("vault.check")}</p>{/if}
        {#if error}<p class="error">{error}</p>{/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" onclick={() => (form = null)}>{t("common.cancel")}</button>
        <button class="btn primary" disabled={busy || !password} onclick={save}><Icon name="key" size={14} />{t("common.save")}</button>
      </footer>
    {/if}
  </div>
</div>

<style>
  .manager {
    width: min(520px, 100%);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 2px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  li.warn {
    padding: 0 8px 8px 52px;
    font-size: 12px;
    color: var(--danger);
  }
  .list > ul > li:not(.warn) {
    padding: 6px 4px 6px 8px;
    border-radius: var(--radius-md);
  }
  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 0;
    border: 0;
    background: transparent;
    color: inherit;
    text-align: left;
  }
  li:has(.pick):hover {
    background: var(--hover);
  }
  .ico {
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
  .name,
  .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    font-size: 12px;
    color: var(--faint);
  }
  .confirm {
    display: flex;
    gap: 4px;
    flex-shrink: 0;
  }
  .muted {
    margin: 0;
    color: var(--faint);
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
  .error {
    color: var(--danger);
    margin: 0;
    font-size: 12px;
  }
</style>
