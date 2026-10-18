<script lang="ts">
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";
  import type { Column, SortSpec } from "./columns";

  let { columns, sort, onsort }: { columns: Column[]; sort: SortSpec | null; onsort: (field: string) => void } = $props();
</script>

<div class="head" role="row">
  {#each columns as c (c.key)}
    <button
      class="h"
      class:name={c.key === "name"}
      class:right={c.align === "right"}
      class:on={sort?.field === c.sort}
      style:width={c.key === "name" ? undefined : `${c.width}px`}
      disabled={!c.sort}
      onclick={() => c.sort && onsort(c.sort)}
      role="columnheader"
    >
      <span>{t(c.label)}</span>
      {#if sort && sort.field === c.sort}<Icon name={sort.desc ? "arrowDown" : "arrowUp"} size={12} />{/if}
    </button>
  {/each}
</div>

<style>
  .head {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    height: 32px;
    align-items: stretch;
    padding: 0 8px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
  }
  .h {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 10px;
    border: 0;
    background: transparent;
    color: var(--faint);
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    border-radius: var(--radius-sm);
    transition: color var(--t-fast) var(--ease);
  }
  .h:disabled {
    cursor: default;
  }
  .h:hover:not(:disabled),
  .h.on {
    color: var(--fg);
  }
  .h.name {
    flex: 1;
    min-width: 120px;
    padding-left: 40px;
  }
  .h.right {
    justify-content: flex-end;
  }
</style>
