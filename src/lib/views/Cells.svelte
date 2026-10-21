<script lang="ts">
  import type { Snippet } from "svelte";
  import { thumbUrl, type MediaBrief } from "../api";
  import Icon, { type IconName } from "../ui/Icon.svelte";
  import type { Column } from "./columns";
  import { reportBroken } from "./briefs.svelte";

  /** One row of cells for the list and tree views. */
  let {
    columns,
    brief = null,
    id = null,
    name = "",
    icon = null,
    indent = 0,
    extra = "",
    lead,
  }: {
    columns: Column[];
    brief?: MediaBrief | null;
    /** Media id; shows a small thumbnail. */
    id?: number | null;
    /** Name for non-media rows (folders, sources). */
    name?: string;
    icon?: IconName | null;
    indent?: number;
    /** Text for the second column of non-media rows (e.g. item count). */
    extra?: string;
    lead?: Snippet;
  } = $props();

  let broken = $state(false);
  const noThumb = $derived(broken || brief?.thumb === "unsupported" || brief?.thumb === "failed");
</script>

{#each columns as c, i (c.key)}
  {#if c.key === "name"}
    <span class="cell name" style:padding-left="{10 + indent * 18}px">
      {@render lead?.()}
      {#if icon}
        <span class="ico" class:folder={icon === "folder" || icon === "server"}><Icon name={icon} size={15} /></span>
      {:else if id != null}
        <span class="thumb">
          {#if !noThumb}
            <img
              src={thumbUrl(id)}
              alt=""
              loading="lazy"
              decoding="async"
              draggable="false"
              onerror={() => {
                broken = true;
                reportBroken(id);
              }}
            />
          {:else}
            <Icon name={brief?.kind === "video" ? "film" : brief?.kind === "nonmedia" ? "file" : "image"} size={13} />
          {/if}
        </span>
      {/if}
      <span class="text">{brief ? brief.name : name}</span>
    </span>
  {:else}
    <span class="cell" class:right={c.align === "right"} style:width="{c.width}px"
      >{brief ? c.value(brief) : i === 1 ? extra : ""}</span
    >
  {/if}
{/each}

<style>
  .cell {
    flex-shrink: 0;
    padding: 0 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .cell.right {
    text-align: right;
  }
  .name {
    flex: 1;
    min-width: 120px;
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--fg);
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .thumb {
    width: 22px;
    height: 22px;
    border-radius: 5px;
    background: var(--tile);
    overflow: hidden;
    flex-shrink: 0;
    display: grid;
    place-items: center;
    color: var(--faint);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .ico {
    width: 22px;
    display: grid;
    place-items: center;
    color: var(--muted);
    flex-shrink: 0;
  }
  .ico.folder {
    color: var(--accent);
  }
</style>
