<script lang="ts">
  import type { BundleSummary, SavedQuery, ScanEvent, SourceRow } from "../api";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";

  let {
    bundle,
    sources,
    selected,
    scans,
    saved,
    activeQuery,
    secondary = false,
    onselect,
    onaddfolder,
    onaddnetwork,
    onrescan,
    onedit,
    onremove,
    onrunsaved,
    oneditsaved,
    ondeletesaved,
    onsettings,
    onclose,
    onsave,
  }: {
    bundle: BundleSummary;
    sources: SourceRow[];
    selected: number | null;
    scans: Record<number, ScanEvent>;
    saved: SavedQuery[];
    activeQuery: string;
    secondary?: boolean;
    onselect: (id: number | null) => void;
    onaddfolder: () => void;
    onaddnetwork: () => void;
    onrescan: (s: SourceRow) => void;
    onedit: (s: SourceRow) => void;
    onremove: (s: SourceRow) => void;
    onrunsaved: (q: SavedQuery) => void;
    oneditsaved: (q: SavedQuery) => void;
    ondeletesaved: (q: SavedQuery) => void;
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
        </div>
      {/if}
      {#each sources as s (s.id)}
        {@const scanning = scans[s.id] && !scans[s.id].done}
        <div class="row" class:off={!s.enabled}>
          <button class="item" class:on={selected === s.id} onclick={() => onselect(s.id)} title={s.root}>
            <Icon name={s.driver === "local" || s.driver === "bundle" ? "folder" : "server"} size={15} />
            <span class="label">{s.name}</span>
            {#if scanning}<span class="spinner"></span>{/if}
          </button>
          <span class="actions">
            <button class="icon-btn sm" title={t("side.rescan")} onclick={() => onrescan(s)}><Icon name="refresh" size={13} /></button>
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
        <p class="empty">{t("side.noSaved")}</p>
      {/each}
    </div>
  </div>

  <div class="foot">
    <button class="item" onclick={onsettings}><Icon name="settings" size={15} /><span class="label">{t("side.settings")}</span></button>
    <button class="item" onclick={onclose}
      ><Icon name={secondary ? "x" : "logout"} size={15} /><span class="label">{secondary ? t("side.closeWindow") : t("side.close")}</span></button
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
