<script lang="ts">
  import { api } from "./api";
  import { onBack } from "./back.svelte";
  import { t } from "./i18n.svelte";
  import Icon from "./ui/Icon.svelte";

  /**
   * Picks a folder inside one source (a local folder, a share or a shard) to move items
   * into. Lists the folders the library knows; a new folder name is created on the move.
   */
  let {
    sourceId,
    sourceName,
    start,
    onpick,
    onclose,
  }: { sourceId: number; sourceName: string; start: string; onpick: (folder: string) => void; onclose: () => void } = $props();

  // svelte-ignore state_referenced_locally
  let folder = $state(start);
  let children = $state<{ name: string; count: number }[]>([]);
  let newName = $state("");

  $effect(() => onBack(onclose));
  $effect(() => {
    const f = folder;
    children = [];
    api.childFolders(sourceId, f).then((l) => folder === f && (children = l)).catch(() => {});
  });

  const parts = $derived(folder ? folder.split("/") : []);
  const join = (a: string, b: string) => (a ? `${a}/${b}` : b);
  const cleanNew = $derived(newName.trim().replace(/[\\/]/g, ""));
  const target = $derived(cleanNew ? join(folder, cleanNew) : folder);
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog mover" role="dialog" aria-modal="true" aria-labelledby="move-title">
    <header>
      <h2 id="move-title">{t("move.title")}</h2>
      <button class="icon-btn" aria-label={t("common.close")} onclick={onclose}><Icon name="x" size={16} /></button>
    </header>
    <div class="pathbar">
      <button class="icon-btn" disabled={!folder} aria-label={t("tb.up")} onclick={() => (folder = parts.slice(0, -1).join("/"))}>
        <Icon name="arrowUp" size={16} />
      </button>
      <div class="crumbs">
        <button class="crumb" class:cur={!folder} onclick={() => (folder = "")}>{sourceName}</button>
        {#each parts as p, i (i)}
          <Icon name="chevronRight" size={12} />
          <button class="crumb" class:cur={i === parts.length - 1} onclick={() => (folder = parts.slice(0, i + 1).join("/"))}>{p}</button>
        {/each}
      </div>
    </div>
    <div class="body list">
      {#each children as c (c.name)}
        <button class="entry" onclick={() => (folder = join(folder, c.name))}>
          <span class="ico"><Icon name="folder" size={18} /></span>
          <span class="ename">{c.name}</span>
          <span class="count tabular">{c.count.toLocaleString()}</span>
          <Icon name="chevronRight" size={14} />
        </button>
      {:else}
        <p class="msg">{t("move.noFolders")}</p>
      {/each}
    </div>
    <footer>
      <input class="input grow" placeholder={t("move.newFolder")} bind:value={newName} />
      <button class="btn primary" onclick={() => onpick(target)}><Icon name="check" size={14} />{t("move.here")}</button>
    </footer>
  </div>
</div>

<style>
  .mover {
    width: min(520px, 100%);
    height: min(600px, 100%);
  }
  .pathbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
    color: var(--faint);
    flex-shrink: 0;
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
  .entry:hover,
  .entry:active {
    background: var(--hover);
  }
  .ico {
    display: grid;
    place-items: center;
    color: var(--accent);
  }
  .ename {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: 12px;
    color: var(--faint);
  }
  .msg {
    margin: 16px 10px;
    color: var(--faint);
  }
  footer .input {
    min-width: 0;
  }
</style>
