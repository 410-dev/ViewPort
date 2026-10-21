<script lang="ts">
  import type { BundleSummary, SavedQuery, ScanEvent, ShardStatus, SourceRow } from "../api";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";

  let {
    bundle,
    sources,
    selected,
    scans,
    shards = {},
    saved,
    activeQuery,
    offline = [],
    cansavecurrent = false,
    closeLabel,
    secondary = false,
    onselect,
    onaddfolder,
    onaddnetwork,
    onaddshard,
    onrescan,
    onedit,
    onremove,
    oncopy,
    onrunsaved,
    oneditsaved,
    ondeletesaved,
    onsavecurrent,
    onpeople,
    onsettings,
    onclose,
    onsave,
  }: {
    bundle: BundleSummary;
    sources: SourceRow[];
    selected: number | null;
    scans: Record<number, ScanEvent>;
    /** Status of shard sources by source id. */
    shards?: Record<number, ShardStatus>;
    saved: SavedQuery[];
    activeQuery: string;
    /** Sources whose storage is not connected: struck through. */
    offline?: number[];
    /** A search is running that is not saved yet. */
    cansavecurrent?: boolean;
    /** Overrides the label of the close item. */
    closeLabel?: string;
    secondary?: boolean;
    onselect: (id: number | null) => void;
    onaddfolder: () => void;
    onaddnetwork: () => void;
    onaddshard: () => void;
    onrescan: (s: SourceRow) => void;
    onedit: (s: SourceRow) => void;
    onremove: (s: SourceRow) => void;
    /** Copies a whole source to another one (only offered while the bundle has a shard). */
    oncopy?: (s: SourceRow) => void;
    onrunsaved: (q: SavedQuery) => void;
    oneditsaved: (q: SavedQuery) => void;
    ondeletesaved: (q: SavedQuery) => void;
    onsavecurrent: () => void;
    /** Opens the people view (image analysis). */
    onpeople?: () => void;
    onsettings: () => void;
    onclose: () => void;
    onsave: () => void;
  } = $props();

  let addMenu = $state(false);
</script>

<nav class="side">
  <div class="bundle" title={bundle.path}>
    <span class="logo"><Icon name="layers" size={15} /></span>
    <span class="bname">{bundle.name}</span>
    {#if bundle.locked}<span class="lock" title={t("side.locked")}><Icon name="lock" size={13} /></span>{/if}
  </div>
  {#if bundle.mode === "manual_save_only"}
    <div class="manual">
      <span>{t("side.manualSave")}</span>
      <button class="btn sm primary" onclick={onsave}><Icon name="save" size={13} />{t("side.save")}</button>
    </div>
  {/if}

  <div class="scroll">
    <div class="group">
      <button class="item" class:on={selected === null} onclick={() => onselect(null)}>
        <Icon name="home" size={15} /><span class="label">{t("side.all")}</span>
      </button>
      {#if onpeople}
        <button class="item" onclick={onpeople}><Icon name="users" size={15} /><span class="label">{t("side.people")}</span></button>
      {/if}
    </div>

    <div class="group">
      <div class="ghead">
        <span class="eyebrow">{t("side.sources")}</span>
        <button class="icon-btn sm" title={t("side.addSource")} onclick={() => (addMenu = !addMenu)} aria-expanded={addMenu}>
          <Icon name="plus" size={14} />
        </button>
      </div>
      {#if addMenu}
        <div class="menu">
          <button
            class="item"
            onclick={() => {
              addMenu = false;
              onaddfolder();
            }}><Icon name="folderPlus" size={15} /><span class="label">{t("side.addFolder")}</span></button
          >
          <button
            class="item"
            onclick={() => {
              addMenu = false;
              onaddnetwork();
            }}><Icon name="server" size={15} /><span class="label">{t("side.addNetwork")}</span></button
          >
          <button
            class="item"
            onclick={() => {
              addMenu = false;
              onaddshard();
            }}><Icon name="archive" size={15} /><span class="label">{t("side.addShard")}</span></button
          >
        </div>
      {/if}
      {#each sources as s (s.id)}
        {@const scanning = scans[s.id] && !scans[s.id].done}
        {@const shard = s.driver === "shard" ? shards[s.id] : undefined}
        {@const shardLocked = !!shard && shard.found && shard.encrypted && !shard.unlocked}
        {@const gone = offline.includes(s.id)}
        <div class="row" class:off={!s.enabled || gone}>
          <button
            class="item"
            class:on={selected === s.id}
            class:gone
            onclick={() => onselect(s.id)}
            title={gone ? `${t("side.offline")} · ${s.root}` : shardLocked ? t("side.shardLocked") : s.root}
          >
            <Icon name={s.driver === "shard" ? "archive" : s.driver === "local" || s.driver === "bundle" ? "folder" : "server"} size={15} />
            <span class="label">{s.name}</span>
            {#if shardLocked}<span class="lock"><Icon name="lock" size={12} /></span>{/if}
            {#if scanning}<span class="spinner"></span>{/if}
          </button>
          <span class="actions">
            <button class="icon-btn sm" title={t("side.rescan")} onclick={() => onrescan(s)}><Icon name="refresh" size={13} /></button>
            {#if oncopy}
              <button class="icon-btn sm" title={t("side.copySource")} onclick={() => oncopy(s)}><Icon name="copy" size={13} /></button>
            {/if}
            <button class="icon-btn sm" title={t("common.edit")} onclick={() => onedit(s)}><Icon name="pencil" size={13} /></button>
            <button class="icon-btn sm" title={t("common.remove")} onclick={() => onremove(s)}><Icon name="trash" size={13} /></button>
          </span>
        </div>
      {:else}
        <p class="empty">{t("side.noSources")}</p>
      {/each}
    </div>

    <div class="group">
      <div class="ghead"><span class="eyebrow">{t("side.saved")}</span></div>
      {#if cansavecurrent}
        <button class="item save-current" onclick={onsavecurrent}>
          <Icon name="plus" size={15} /><span class="label">{t("side.saveCurrent")}</span>
        </button>
      {/if}
      {#each saved as q (q.id)}
        <div class="row">
          <button class="item" class:on={activeQuery === q.text} title={q.text} onclick={() => onrunsaved(q)}>
            <Icon name="bookmark" size={15} /><span class="label">{q.label}</span>
          </button>
          <span class="actions">
            <button class="icon-btn sm" title={t("common.edit")} onclick={() => oneditsaved(q)}><Icon name="pencil" size={13} /></button>
            <button class="icon-btn sm" title={t("common.remove")} onclick={() => ondeletesaved(q)}><Icon name="trash" size={13} /></button>
          </span>
        </div>
      {:else}
        {#if !cansavecurrent}<p class="empty">{t("side.noSaved")}</p>{/if}
      {/each}
    </div>
  </div>

  <div class="foot">
    <button class="item" onclick={onsettings}><Icon name="settings" size={15} /><span class="label">{t("side.settings")}</span></button>
    <button class="item" onclick={onclose}
      ><Icon name={secondary ? "x" : "logout"} size={15} /><span class="label">{closeLabel ?? (secondary ? t("side.closeWindow") : t("side.close"))}</span></button
    >
  </div>
</nav>

<style>
  .side {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--surface);
    border-right: 1px solid var(--border);
  }
  .bundle {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 52px;
    padding: 0 16px;
    flex-shrink: 0;
  }
  .logo {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 8px;
    background: var(--accent);
    color: var(--accent-fg);
    flex-shrink: 0;
  }
  .bname {
    flex: 1;
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lock {
    color: var(--muted);
    display: grid;
  }
  .manual {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 10px 8px;
    padding: 8px 8px 8px 12px;
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--warn) 12%, transparent);
    color: var(--warn);
    font-size: 11px;
  }
  .manual span {
    flex: 1;
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 10px 12px;
  }
  .group {
    display: grid;
    gap: 1px;
    margin-bottom: 14px;
  }
  .ghead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 28px;
    padding: 0 2px 0 10px;
  }
  .menu {
    display: grid;
    gap: 1px;
    margin: 0 0 6px;
    padding: 4px;
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
  }
  .row.off {
    opacity: 0.5;
  }
  .item {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 32px;
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg-2);
    text-align: left;
    transition:
      background var(--t-fast) var(--ease),
      color var(--t-fast) var(--ease);
  }
  .item :global(svg) {
    color: var(--muted);
  }
  .item:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .item.on {
    background: var(--accent-soft);
    color: var(--fg);
    font-weight: 500;
  }
  .item.on :global(svg) {
    color: var(--accent);
  }
  .item.gone .label {
    text-decoration: line-through;
  }
  .item.save-current {
    color: var(--accent);
  }
  .item.save-current :global(svg) {
    color: var(--accent);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    position: absolute;
    right: 4px;
    display: flex;
    gap: 0;
    padding-left: 12px;
    border-radius: var(--radius-pill);
    background: linear-gradient(to right, transparent, var(--hover) 12px);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--t-fast) var(--ease);
  }
  .row:hover .actions {
    opacity: 1;
    pointer-events: auto;
  }
  @media (pointer: coarse) {
    .item {
      height: 42px;
    }
  }
  @media (hover: none) {
    .actions {
      opacity: 1 !important;
      pointer-events: auto !important;
      background: linear-gradient(to right, transparent, var(--surface) 12px);
    }
  }
  .empty {
    margin: 2px 10px 4px;
    font-size: 12px;
    color: var(--faint);
  }
  .foot {
    display: grid;
    gap: 1px;
    padding: 8px 10px 10px;
    border-top: 1px solid var(--border);
  }
</style>
