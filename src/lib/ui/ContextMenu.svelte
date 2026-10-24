<script lang="ts">
  import { pop } from "./motion";
  import { onDestroy, onMount } from "svelte";
  import { onBack } from "../back.svelte";
  import Icon, { type IconName } from "./Icon.svelte";
  import { portal } from "./portal";

  export interface MenuItem {
    label: string;
    icon: IconName;
    /** Drawn in the danger color (deleting). */
    danger?: boolean;
    action: () => void;
  }

  /** Menu at a point (right click, or a long press on touch screens), kept inside the window. */
  let { x, y, items, onclose }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  $effect(() => onBack(onclose));

  const MARGIN = 8;
  let menu: HTMLUListElement;
  let pos = $state({ left: 0, top: 0 });

  function outside(e: PointerEvent) {
    if (!menu?.contains(e.target as Node)) onclose();
  }

  onMount(() => {
    const r = menu.getBoundingClientRect();
    pos = {
      left: Math.max(MARGIN, Math.min(x, window.innerWidth - r.width - MARGIN)),
      top: Math.max(MARGIN, Math.min(y, window.innerHeight - r.height - MARGIN)),
    };
    window.addEventListener("pointerdown", outside, true);
    window.addEventListener("resize", onclose);
  });
  onDestroy(() => {
    window.removeEventListener("pointerdown", outside, true);
    window.removeEventListener("resize", onclose);
  });
</script>

<ul bind:this={menu} use:portal out:pop={{ y: 4, duration: 110 }} class="ctx" role="menu" style:left="{pos.left}px" style:top="{pos.top}px" oncontextmenu={(e) => e.preventDefault()}>
  {#each items as item (item.label)}
    <li role="none">
      <button
        role="menuitem"
        class:danger={item.danger}
        onclick={() => {
          onclose();
          item.action();
        }}><Icon name={item.icon} size={15} /><span>{item.label}</span></button
      >
    </li>
  {/each}
</ul>

<style>
  .ctx {
    position: fixed;
    z-index: 60;
    min-width: 180px;
    max-width: calc(100vw - 16px);
    margin: 0;
    padding: 4px;
    list-style: none;
    border-radius: var(--radius-md);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    animation: vp-pop var(--t-med) var(--ease-pop);
    transform-origin: top left;
  }
  button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 38px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--fg);
    text-align: left;
    white-space: nowrap;
  }
  button:hover,
  button:focus-visible {
    background: var(--hover);
  }
  button.danger {
    color: var(--danger);
  }
  /* Long labels (file names) are cut, not pushed past the menu edge. */
  button span {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
</style>
