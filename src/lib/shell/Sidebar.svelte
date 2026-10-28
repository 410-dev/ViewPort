<script lang="ts">
  import { dismiss } from "../ui/dismiss";
  import { expand } from "../ui/motion";
  import { fraction, tasks } from "../tasks.svelte";
  import { onBack } from "../back.svelte";
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
    replicas = {},
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
    onmirror,
    onrunsaved,
    oneditsaved,
    ondeletesaved,
    onsavecurrent,
    onpeople,
    ondedup,
    onsettings,
    onclose,
    onsave,
    onrename,
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
    /** Sources holding a replica mirror (target id → name of the source it copies): greyed. */
    replicas?: Record<number, string>;
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
    /** Keeps the source copied into another one (mirror settings). */
    onmirror?: (s: SourceRow) => void;
    onrunsaved: (q: SavedQuery) => void;
    oneditsaved: (q: SavedQuery) => void;
    ondeletesaved: (q: SavedQuery) => void;
    onsavecurrent: () => void;
    /** Opens the people view (image analysis). */
    onpeople?: () => void;
    /** Opens the duplicate finder. */
    ondedup?: () => void;
    onsettings: () => void;
    onclose: () => void;
    onsave: () => void;
    /** Renames the library; rejects with a message to show. */
    onrename?: (name: string) => Promise<void>;
  } = $props();

  let addMenu = $state(false);
  /** Touch screens: the row whose actions are open (`s<id>` source, `q<id>` saved query). */
  let rowMenu = $state<string | null>(null);
  let rowMenuBtn: HTMLElement | null = null;
  function toggleRowMenu(e: MouseEvent, key: string) {
    rowMenuBtn = e.currentTarget as HTMLElement;
    rowMenu = rowMenu === key ? null : key;
  }
  /** Runs a row action from the open menu. */
  const pick = (f: () => void) => () => {
    rowMenu = null;
    f();
  };
  $effect(() => {
    if (rowMenu) return onBack(() => (rowMenu = null));
  });
  $effect(() => {
    if (addMenu) return onBack(() => (addMenu = false));
  });
  let addBtn = $state<HTMLButtonElement | null>(null);

  /** The library name being edited (null: not editing). */
  let renaming = $state<string | null>(null);
  let renameError = $state("");
  function startRename() {
    if (!onrename || bundle.mode === "manual_save_only") return;
    renaming = bundle.name;
    renameError = "";
  }
  async function commitRename() {
    if (renaming == null) return;
    const name = renaming.trim();
    if (!name || name === bundle.name) {
      renaming = null;
      return;
    }
    try {
      await onrename!(name);
      renaming = null;
    } catch (e) {
      renameError = String(e);
    }
  }
  const focusSelect = (el: HTMLInputElement) => {
    el.focus();
    el.select();
  };
  $effect(() => {
    if (renaming != null) return onBack(() => (renaming = null));
  });

  /**
   * Progress (0..1, null unknown) of mirror syncs into each source. Working out what to
   * copy counts as the first tenth, copying as the rest.
   */
  const PLAN_SHARE = 0.1;
  const mirrorProgress = $derived.by(() => {
    const out: Record<number, number | null> = {};
    for (const x of tasks.list) {
      if (x.state !== "running" || x.source_id == null || (x.kind !== "mirror" && x.kind !== "mirror_plan")) continue;
      const f = fraction(x);
      out[x.source_id] = f == null ? null : x.kind === "mirror_plan" ? f * PLAN_SHARE : PLAN_SHARE + f * (1 - PLAN_SHARE);
    }
    return out;
  });
</script>

<nav class="side">
  <div class="bundle" title={bundle.path}>
    <span class="logo"><Icon name="layers" size={15} /></span>
    {#if renaming != null}
      <input
        class="input sm bname-input"
        bind:value={renaming}
        use:focusSelect
        aria-label={t("side.renameLibrary")}
        title={renameError || undefined}
        class:bad={!!renameError}
        onkeydown={(e) => {
          if (e.key === "Enter") commitRename();
          else if (e.key === "Escape") {
            e.stopPropagation();
            renaming = null;
          }
        }}
        onblur={commitRename}
      />
    {:else}
      <span class="bname" role="presentation" ondblclick={startRename}>{bundle.name}</span>
      {#if bundle.locked}<span class="lock" title={t("side.locked")}><Icon name="lock" size={13} /></span>{/if}
      {#if onrename && bundle.mode !== "manual_save_only"}
        <button class="icon-btn sm rename" title={t("side.renameLibrary")} aria-label={t("side.renameLibrary")} onclick={startRename}
          ><Icon name="pencil" size={13} /></button
        >
      {/if}
    {/if}
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
      {#if ondedup}
        <button class="item" onclick={ondedup}><Icon name="layers" size={15} /><span class="label">{t("side.dedup")}</span></button>
      {/if}
    </div>

    <div class="group">
      <div class="ghead">
        <span class="eyebrow">{t("side.sources")}</span>
        <button
          class="icon-btn sm"
          bind:this={addBtn}
          title={t("side.addSource")}
          onclick={() => (addMenu = !addMenu)}
          aria-expanded={addMenu}
        >
          <Icon name="plus" size={14} />
        </button>
      </div>
      {#if addMenu}
        <div class="menu" transition:expand use:dismiss={{ onclose: () => (addMenu = false), keep: () => [addBtn] }}>
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
        {@const replicaOf = replicas[s.id]}
        {@const progress = mirrorProgress[s.id]}
        <div class="row" class:off={!s.enabled || gone}>
          <button
            class="item"
            class:on={selected === s.id}
            class:gone
            class:replica={replicaOf != null}
            onclick={() => onselect(s.id)}
            title={gone
              ? `${t("side.offline")} · ${s.root}`
              : shardLocked
                ? t("side.shardLocked")
                : replicaOf != null
                  ? `${t("side.replicaOf", { name: replicaOf })} · ${s.root}`
                  : s.root}
          >
            <span class="icons">
              <Icon name={s.driver === "shard" ? "archive" : s.driver === "local" || s.driver === "bundle" ? "folder" : "server"} size={15} />
              {#if replicaOf != null}<span class="badge"><Icon name="mirror" size={12} /></span>{/if}
            </span>
            <span class="name">
              <span class="label">{s.name}</span>
              {#if progress !== undefined}
                <span class="bar" class:busy={progress === null} role="progressbar" aria-valuemin={0} aria-valuemax={100}
                  aria-valuenow={progress === null ? undefined : Math.round(progress * 100)} transition:expand={{ duration: 160 }}
                  ><i style:width={progress === null ? undefined : `${progress * 100}%`}></i></span
                >
              {/if}
            </span>
            {#if shardLocked}<span class="lock"><Icon name="lock" size={12} /></span>{/if}
            {#if scanning}<span class="spinner"></span>{/if}
          </button>
          <span class="actions">
            <button class="icon-btn sm" title={t("side.rescan")} onclick={() => onrescan(s)}><Icon name="refresh" size={13} /></button>
            {#if oncopy}
              <button class="icon-btn sm" title={t("side.copySource")} onclick={() => oncopy(s)}><Icon name="copy" size={13} /></button>
            {/if}
            {#if onmirror}
              <button class="icon-btn sm" title={t("side.mirror")} onclick={() => onmirror(s)}><Icon name="mirror" size={13} /></button>
            {/if}
            <button class="icon-btn sm" title={t("common.edit")} onclick={() => onedit(s)}><Icon name="pencil" size={13} /></button>
            <button class="icon-btn sm" title={t("common.remove")} onclick={() => onremove(s)}><Icon name="trash" size={13} /></button>
          </span>
          <!-- Touch screens: the actions behind one button, so the name stays readable and
               remove is not a slip away from edit. -->
          <button
            class="icon-btn sm more"
            class:open={rowMenu === `s${s.id}`}
            aria-label={t("side.actions")}
            aria-expanded={rowMenu === `s${s.id}`}
            onclick={(e) => toggleRowMenu(e, `s${s.id}`)}><Icon name="dots" size={16} /></button
          >
        </div>
        {#if rowMenu === `s${s.id}`}
          <div class="menu" transition:expand use:dismiss={{ onclose: () => (rowMenu = null), keep: () => [rowMenuBtn] }}>
            <button class="item" onclick={pick(() => onrescan(s))}><Icon name="refresh" size={15} /><span class="label">{t("side.rescan")}</span></button>
            {#if oncopy}
              <button class="item" onclick={pick(() => oncopy(s))}><Icon name="copy" size={15} /><span class="label">{t("side.copySource")}</span></button>
            {/if}
            {#if onmirror}
              <button class="item" onclick={pick(() => onmirror(s))}><Icon name="mirror" size={15} /><span class="label">{t("side.mirror")}</span></button>
            {/if}
            <button class="item" onclick={pick(() => onedit(s))}><Icon name="pencil" size={15} /><span class="label">{t("common.edit")}</span></button>
            <div class="sep"></div>
            <button class="item danger" onclick={pick(() => onremove(s))}><Icon name="trash" size={15} /><span class="label">{t("common.remove")}</span></button>
          </div>
        {/if}
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
          <button
            class="icon-btn sm more"
            class:open={rowMenu === `q${q.id}`}
            aria-label={t("side.actions")}
            aria-expanded={rowMenu === `q${q.id}`}
            onclick={(e) => toggleRowMenu(e, `q${q.id}`)}><Icon name="dots" size={16} /></button
          >
        </div>
        {#if rowMenu === `q${q.id}`}
          <div class="menu" transition:expand use:dismiss={{ onclose: () => (rowMenu = null), keep: () => [rowMenuBtn] }}>
            <button class="item" onclick={pick(() => oneditsaved(q))}><Icon name="pencil" size={15} /><span class="label">{t("common.edit")}</span></button>
            <div class="sep"></div>
            <button class="item danger" onclick={pick(() => ondeletesaved(q))}><Icon name="trash" size={15} /><span class="label">{t("common.remove")}</span></button>
          </div>
        {/if}
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
  .bname-input {
    flex: 1;
    min-width: 0;
  }
  .bname-input.bad {
    border-color: var(--danger);
  }
  .rename {
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease);
  }
  .bundle:hover .rename,
  .rename:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .rename {
      opacity: 1;
    }
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
  .more {
    display: none;
    flex-shrink: 0;
  }
  .icons {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    flex-shrink: 0;
  }
  .badge {
    display: inline-grid;
  }
  .item.replica .label {
    color: var(--faint);
  }
  .name {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
  }
  .name .label {
    flex: none;
  }
  .bar {
    position: relative;
    display: block;
    height: 3px;
    border-radius: 2px;
    background: var(--hover);
    overflow: hidden;
  }
  .bar i {
    position: absolute;
    inset: 0 auto 0 0;
    border-radius: inherit;
    background: var(--accent);
    transition: width var(--t-fast) var(--ease);
  }
  .bar.busy i {
    width: 35%;
    animation: side-bar-busy 1.2s var(--ease) infinite;
  }
  @keyframes side-bar-busy {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(290%);
    }
  }
  .more.open {
    background: var(--hover);
  }
  .menu .sep {
    height: 1px;
    margin: 3px 8px;
    background: var(--border);
  }
  .item.danger,
  .item.danger :global(svg) {
    color: var(--danger);
  }
  @media (hover: none) {
    .actions {
      display: none;
    }
    .more {
      display: inline-grid;
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
