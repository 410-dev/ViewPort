<script lang="ts">
  import { onMount } from "svelte";
  import { asCmdError, platformApi, type DirListing, type StorageRoot } from "../api";
  import { onBack } from "../back.svelte";
  import { fmtBytes } from "../format";
  import { t } from "../i18n.svelte";
  import { picker, type PickRequest } from "../pick.svelte";
  import Icon from "./Icon.svelte";

  /** In-app file / folder browser for platforms without usable native dialogs (see `pick.svelte.ts`). */
  let { req }: { req: PickRequest } = $props();

  const LAST = "vp-picker-last";

  let roots = $state<StorageRoot[]>([]);
  let listing = $state<DirListing | null>(null);
  let error = $state("");
  let loading = $state(false);
  // `req` reads through to `picker.req`, so take what is needed before clearing it.
  // svelte-ignore state_referenced_locally
  const { resolve, mode, extensions, pickSuffix, title, ext } = req;
  // svelte-ignore state_referenced_locally
  let name = $state(req.name);

  const join = (dir: string, child: string) => (dir.endsWith("/") ? dir + child : `${dir}/${child}`);
  const rootOf = (path: string) =>
    roots.filter((r) => path === r.path || path.startsWith(r.path + "/")).sort((a, b) => b.path.length - a.path.length)[0];
  const rootLabel = (r: StorageRoot) =>
    r.kind === "internal" ? t("pick.internal") : r.kind === "home" ? t("pick.home") : t("pick.external", { name: r.name });

  /** The current path shown from its storage root: "Internal storage / DCIM / Camera". */
  const crumbs = $derived.by(() => {
    if (!listing) return [];
    const root = rootOf(listing.path);
    if (!root) return [{ label: listing.path, path: listing.path }];
    const out = [{ label: rootLabel(root), path: root.path }];
    let cur = root.path;
    for (const part of listing.path.slice(root.path.length).split("/").filter(Boolean)) {
      cur = join(cur, part);
      out.push({ label: part, path: cur });
    }
    return out;
  });
  const atRoot = $derived(!!listing && roots.some((r) => r.path === listing!.path));

  async function go(path: string): Promise<boolean> {
    loading = true;
    error = "";
    try {
      listing = await platformApi.browseDir(path, mode === "file", extensions);
      try {
        localStorage.setItem(LAST, path);
      } catch {}
      return true;
    } catch (e) {
      const err = asCmdError(e);
      error = err.code === "permission" ? t("pick.denied") : err.message;
      return false;
    } finally {
      loading = false;
    }
  }

  onMount(async () => {
    roots = await platformApi.storageRoots().catch(() => []);
    let last: string | null = null;
    try {
      last = localStorage.getItem(LAST);
    } catch {}
    if (last && (await go(last))) return;
    await go(roots[0]?.path ?? "/");
  });


  function done(path: string | null) {
    if (picker.req?.resolve === resolve) picker.req = null;
    resolve(path);
  }

  // Back goes up a folder, then cancels.
  $effect(() =>
    onBack(() => {
      if (listing?.parent && !atRoot) go(listing.parent);
      else done(null);
    }),
  );

  function open(entry: DirListing["entries"][number]) {
    if (!listing) return;
    const path = join(listing.path, entry.name);
    if (entry.dir) {
      if (pickSuffix && entry.name.toLowerCase().endsWith(pickSuffix)) done(path);
      else go(path);
    } else if (mode === "file") done(path);
  }

  const saveName = $derived.by(() => {
    const n = name.trim().replace(/[\\/]/g, "");
    if (!n) return "";
    return ext && !n.toLowerCase().endsWith(`.${ext}`) ? `${n}.${ext}` : n;
  });
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && done(null)}>
  <div class="dialog picker" role="dialog" aria-modal="true" aria-label={title}>
    <header>
      <h2>{title}</h2>
      <button class="icon-btn" aria-label={t("common.close")} onclick={() => done(null)}><Icon name="x" size={16} /></button>
    </header>

    {#if roots.length > 1}
      <div class="roots">
        {#each roots as r (r.path)}
          <button class="chip" class:on={listing && rootOf(listing.path)?.path === r.path} onclick={() => go(r.path)}>
            <Icon name="hardDrive" size={13} />{rootLabel(r)}
          </button>
        {/each}
      </div>
    {/if}

    <div class="pathbar">
      <button class="icon-btn" disabled={!listing?.parent || atRoot} aria-label={t("tb.up")} onclick={() => listing?.parent && go(listing.parent)}>
        <Icon name="arrowUp" size={16} />
      </button>
      <div class="crumbs">
        {#each crumbs as c, i (c.path)}
          {#if i > 0}<Icon name="chevronRight" size={12} />{/if}
          <button class="crumb" class:cur={i === crumbs.length - 1} onclick={() => go(c.path)}>{c.label}</button>
        {/each}
      </div>
      {#if loading}<span class="spinner"></span>{/if}
    </div>

    <div class="body list">
      {#if error}
        <p class="msg err">{error}</p>
      {/if}
      {#if listing}
        {#each listing.entries as e (e.name)}
          {@const bundle = e.dir && !!pickSuffix && e.name.toLowerCase().endsWith(pickSuffix)}
          <button class="entry" class:file={!e.dir} onclick={() => open(e)}>
            <span class="ico" class:bundle><Icon name={bundle ? "layers" : e.dir ? "folder" : "file"} size={18} /></span>
            <span class="ename">{e.name}</span>
            {#if !e.dir}<span class="size tabular">{fmtBytes(e.size)}</span>{:else if !bundle}<Icon name="chevronRight" size={14} />{/if}
          </button>
        {:else}
          {#if !error}<p class="msg">{t("pick.empty")}</p>{/if}
        {/each}
      {/if}
    </div>

    {#if mode !== "file"}
      <footer>
        {#if mode === "save"}
          <input class="input grow" placeholder={t("pick.name")} bind:value={name} onkeydown={(e) => e.key === "Enter" && saveName && listing && done(join(listing.path, saveName))} />
          <button class="btn primary" disabled={!saveName || !listing} onclick={() => listing && done(join(listing.path, saveName))}>
            {t("pick.create")}
          </button>
        {:else}
          <span class="grow hint">{pickSuffix ? t("pick.bundleHint") : ""}</span>
          <button class="btn primary" disabled={!listing} onclick={() => listing && done(listing.path)}><Icon name="check" size={14} />{t("pick.thisFolder")}</button>
        {/if}
      </footer>
    {/if}
  </div>
</div>

<style>
  .picker {
    width: min(560px, 100%);
    height: min(680px, 100%);
  }
  .roots {
    display: flex;
    gap: 6px;
    padding: 10px 16px 0;
    overflow-x: auto;
    flex-shrink: 0;
  }
  .roots .chip {
    border: 0;
    white-space: nowrap;
    height: 30px;
  }
  .roots .chip.on {
    background: var(--accent-soft);
    color: var(--accent);
  }
  .pathbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
    color: var(--faint);
  }
  .crumbs {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 2px;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .crumb {
    border: 0;
    background: transparent;
    color: var(--muted);
    padding: 4px 6px;
    border-radius: var(--radius-sm);
    white-space: nowrap;
  }
  .crumb.cur {
    color: var(--fg);
    font-weight: 600;
  }
  .list {
    flex: 1;
    padding: 6px 8px !important;
  }
  .entry {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 12px;
    min-height: 44px;
    padding: 4px 10px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--fg);
    text-align: left;
  }
  .entry:active,
  .entry:hover {
    background: var(--hover);
  }
  .ico {
    display: grid;
    place-items: center;
    color: var(--accent);
  }
  .entry.file .ico {
    color: var(--muted);
  }
  .ename {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .size {
    font-size: 12px;
    color: var(--faint);
  }
  .msg {
    margin: 16px 10px;
    color: var(--faint);
  }
  .msg.err {
    color: var(--danger);
  }
  .hint {
    font-size: 12px;
    color: var(--faint);
  }
  footer .input {
    min-width: 0;
  }
</style>
