<script lang="ts">
  import { onBack } from "../back.svelte";
  import { pickFile, pickFolder } from "../pick.svelte";
  import { api, type BundleSummary, type ShardPeek, type ShardPlace, type SourceRow, type TransferMode } from "../api";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";
  import Select from "../ui/Select.svelte";
  import { errText } from "./shared";

  /** Adds a shard source: a new shard (in the bundle or elsewhere), or an existing one. */
  let {
    bundle,
    sources,
    onclose,
    ondone,
  }: { bundle: BundleSummary; sources: SourceRow[]; onclose: () => void; ondone: (s: SourceRow) => void } = $props();

  let tab = $state<"create" | "attach">("create");
  let busy = $state(false);
  let error = $state<string | null>(null);

  // Create
  let name = $state("");
  let elsewhere = $state(false);
  let parent = $state("");
  let encrypt = $state(false);
  let password = $state("");
  let password2 = $state("");
  let keyfile = $state("");
  let remember = $state(true);
  let initial = $state<"none" | "folder" | "source">("none");
  let initialFolder = $state("");
  let initialSource = $state<string>("");
  let mode = $state<TransferMode>("copy");

  // Attach
  let path = $state("");
  let peek = $state<ShardPeek | null>(null);
  let place = $state<ShardPlace>("link");

  const others = $derived(sources.filter((s) => s.driver !== "shard"));
  const encrypted = $derived(tab === "create" ? encrypt : !!peek?.encrypted);
  const mismatch = $derived(tab === "create" && encrypt && password2 !== "" && password !== password2);
  const canSubmit = $derived(
    tab === "create"
      ? (!elsewhere || !!parent) &&
          (!encrypt || (!!password && password === password2)) &&
          (initial !== "folder" || !!initialFolder) &&
          (initial !== "source" || !!initialSource)
      : !!peek,
  );

  async function browseParent() {
    const p = await pickFolder(t("shard.parentFolder"));
    if (p) parent = p;
  }
  async function browseKeyfile() {
    const p = await pickFile(t("shard.keyfile"));
    if (p) keyfile = p;
  }
  async function browseInitial() {
    const p = await pickFolder(t("shard.pickFolder"));
    if (p) initialFolder = p;
  }
  async function browseShard() {
    const p = await pickFolder(t("shard.shardFolder"), { pickSuffix: ".vpshard" });
    if (!p) return;
    path = p;
    peek = null;
    error = null;
    try {
      peek = await api.inspectShard(p);
    } catch (e) {
      error = errText(e);
    }
  }

  async function submit() {
    busy = true;
    error = null;
    try {
      const kf = keyfile || null;
      const rememberIt = bundle.locked && remember;
      if (tab === "create") {
        const s = await api.createShard(name.trim(), elsewhere ? parent : null, encrypt ? password : null, encrypt ? kf : null, rememberIt);
        if (initial === "folder") await api.shardImport(s.id, [initialFolder], "", mode);
        if (initial === "source") await api.shardFromSource(Number(initialSource), s.id, "", mode);
        ondone(s);
      } else {
        ondone(await api.attachShard(path, place, password || null, kf, rememberIt));
      }
    } catch (e) {
      error = errText(e);
    } finally {
      busy = false;
    }
  }

  $effect(() => onBack(onclose));
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onclose()}>
  <div class="dialog" role="dialog" aria-modal="true">
    <header>
      <h2>{t("shard.titleAdd")}</h2>
      <button class="icon-btn" onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>
    </header>

    <div class="body form">
      <p class="hint">{t("shard.intro")}</p>
      <div class="seg">
        <button class:on={tab === "create"} onclick={() => (tab = "create")}><Icon name="plus" size={14} />{t("shard.create")}</button>
        <button class:on={tab === "attach"} onclick={() => (tab = "attach")}><Icon name="archive" size={14} />{t("shard.attach")}</button>
      </div>

      {#if tab === "create"}
        <label>
          <span>{t("shard.name")}</span>
          <input class="input" bind:value={name} placeholder="shard" />
        </label>
        <div class="field">
          <span>{t("shard.location")}</span>
          <div class="seg sm">
            <button class:on={!elsewhere} onclick={() => (elsewhere = false)}>{t("shard.inBundle")}</button>
            <button class:on={elsewhere} onclick={() => (elsewhere = true)}>{t("shard.elsewhere")}</button>
          </div>
          {#if elsewhere}
            <span class="row">
              <input class="input" readonly value={parent} placeholder={t("shard.parentFolder")} />
              <button class="btn" onclick={browseParent}>{t("common.browse")}</button>
            </span>
          {/if}
        </div>
        <label class="check">
          <button type="button" class="switch" class:on={encrypt} aria-label={t("shard.encrypt")} onclick={() => (encrypt = !encrypt)}></button>
          <span>{t("shard.encrypt")}</span>
        </label>
        {#if encrypt}
          <input class="input" type="password" autocomplete="new-password" placeholder={t("shard.password")} bind:value={password} />
          <input class="input" type="password" autocomplete="new-password" placeholder={t("shard.passwordConfirm")} bind:value={password2} />
          {#if mismatch}<p class="msg bad">{t("shard.passwordMismatch")}</p>{/if}
          <span class="row">
            <input class="input" readonly value={keyfile} placeholder={t("shard.keyfile")} />
            <button class="btn" onclick={browseKeyfile}>{t("common.browse")}</button>
          </span>
        {/if}

        <div class="field">
          <span>{t("shard.initial")}</span>
          <div class="seg sm">
            <button class:on={initial === "none"} onclick={() => (initial = "none")}>{t("shard.initial.none")}</button>
            <button class:on={initial === "folder"} onclick={() => (initial = "folder")}>{t("shard.initial.folder")}</button>
            <button class:on={initial === "source"} onclick={() => (initial = "source")} disabled={others.length === 0}
              >{t("shard.initial.source")}</button
            >
          </div>
          {#if initial === "folder"}
            <span class="row">
              <input class="input" readonly value={initialFolder} placeholder={t("shard.pickFolder")} />
              <button class="btn" onclick={browseInitial}>{t("common.browse")}</button>
            </span>
          {:else if initial === "source"}
            <Select
              value={initialSource}
              label={t("shard.fromSource")}
              options={[{ value: "", label: t("shard.fromSource") }, ...others.map((s) => ({ value: String(s.id), label: s.name }))]}
              onchange={(v) => (initialSource = v)}
            />
          {/if}
          {#if initial !== "none"}
            <div class="seg sm">
              <button class:on={mode === "copy"} onclick={() => (mode = "copy")}>{t("shard.mode.copy")}</button>
              <button class:on={mode === "move"} onclick={() => (mode = "move")}>{t("shard.mode.move")}</button>
            </div>
            {#if mode === "move"}<p class="hint">{t("shard.moveHint")}</p>{/if}
          {/if}
        </div>
      {:else}
        <div class="field">
          <span>{t("shard.shardFolder")}</span>
          <span class="row">
            <input class="input" readonly value={path} />
            <button class="btn" onclick={browseShard}>{t("common.browse")}</button>
          </span>
          {#if peek}<p class="hint"><Icon name="archive" size={12} />{peek.name}</p>{/if}
        </div>
        <div class="field">
          <div class="seg sm">
            <button class:on={place === "link"} onclick={() => (place = "link")}>{t("shard.place.link")}</button>
            <button class:on={place === "copy"} onclick={() => (place = "copy")}>{t("shard.place.copy")}</button>
            <button class:on={place === "move"} onclick={() => (place = "move")}>{t("shard.place.move")}</button>
          </div>
          <p class="hint">{t("shard.placeHint")}</p>
        </div>
        {#if peek?.encrypted}
          <p class="hint"><Icon name="lock" size={12} />{t("shard.encryptedFound")}</p>
          <input class="input" type="password" autocomplete="current-password" placeholder={t("shard.password")} bind:value={password} />
          {#if peek.requires_keyfile}
            <span class="row">
              <input class="input" readonly value={keyfile} placeholder={t("shard.keyfile")} />
              <button class="btn" onclick={browseKeyfile}>{t("common.browse")}</button>
            </span>
          {/if}
        {/if}
      {/if}

      {#if encrypted}
        {#if bundle.locked}
          <label class="check">
            <button type="button" class="switch" class:on={remember} aria-label={t("shard.remember")} onclick={() => (remember = !remember)}></button>
            <span>{t("shard.remember")}</span>
          </label>
          <p class="hint">{t("shard.rememberHint")}</p>
        {:else}
          <p class="msg warn"><Icon name="alert" size={13} />{t("shard.plainWarn")}</p>
          <p class="hint">{t("shard.rememberLockedOnly")}</p>
        {/if}
      {/if}

      {#if error}<p class="msg bad">{error}</p>{/if}
    </div>

    <footer>
      <span class="grow"></span>
      <button class="btn ghost" onclick={onclose} disabled={busy}>{t("common.cancel")}</button>
      <button class="btn primary" disabled={busy || !canSubmit} onclick={submit}>
        {tab === "create" ? t("shard.createBtn") : t("shard.attachBtn")}
      </button>
    </footer>
  </div>
</div>

<style>
  .form {
    display: grid;
    gap: 12px;
  }
  label,
  .field {
    display: grid;
    gap: 6px;
  }
  label > span:first-child,
  .field > span:first-child {
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
  .seg {
    justify-self: start;
    max-width: 100%;
    overflow-x: auto;
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
  .bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .warn {
    color: var(--warn);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
  }
</style>
