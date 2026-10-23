<script lang="ts">
  import { pop } from "./motion";
  import { onBack } from "../back.svelte";
  import { onDestroy, onMount } from "svelte";
  import { portal } from "./portal";
  import Icon from "./Icon.svelte";

  /**
   * Floating option list anchored to an element, drawn by the app (not the WebView's
   * native popup) so it matches the UI in every theme and platform. Used by `Select` and
   * `Combobox`; keyboard handling stays with the owner, which passes `active`.
   */
  let {
    anchor,
    items,
    active = -1,
    selected = -1,
    onpick,
    onclose,
    onhover,
    id,
  }: {
    id?: string;
    anchor: HTMLElement;
    items: { label: string; detail?: string }[];
    active?: number;
    selected?: number;
    onpick: (index: number) => void;
    onclose: () => void;
    onhover?: (index: number) => void;
  } = $props();

  $effect(() => onBack(onclose));

  const GAP = 4;
  const MAX_H = 280;

  let list: HTMLUListElement;
  let pos = $state({ left: 0, top: 0, width: 0, maxHeight: MAX_H, above: false });

  function place() {
    const r = anchor.getBoundingClientRect();
    const below = window.innerHeight - r.bottom - GAP - 8;
    const above = r.top - GAP - 8;
    const up = below < Math.min(MAX_H, 160) && above > below;
    const maxHeight = Math.min(MAX_H, Math.max(80, up ? above : below));
    const width = Math.max(r.width, 160);
    const left = Math.min(r.left, window.innerWidth - width - 8);
    pos = { left: Math.max(8, left), top: up ? r.top - GAP : r.bottom + GAP, width, maxHeight, above: up };
  }

  function outside(e: PointerEvent) {
    const t = e.target as Node;
    if (!list?.contains(t) && !anchor.contains(t)) onclose();
  }

  onMount(() => {
    place();
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    window.addEventListener("pointerdown", outside, true);
  });
  onDestroy(() => {
    window.removeEventListener("resize", place);
    window.removeEventListener("scroll", place, true);
    window.removeEventListener("pointerdown", outside, true);
  });

  // Keep the keyboard-active option in view.
  $effect(() => {
    const i = active;
    list?.children[i]?.scrollIntoView({ block: "nearest" });
  });
</script>

<ul
  bind:this={list}
  use:portal
  {id}
  class="dropdown"
  class:above={pos.above}
  out:pop={{ base: pos.above ? "translateY(-100%)" : "", y: pos.above ? -4 : 4, duration: 110 }}
  role="listbox"
  style:left="{pos.left}px"
  style:top="{pos.top}px"
  style:width="{pos.width}px"
  style:max-height="{pos.maxHeight}px"
>
  {#each items as it, i (i)}
    <li>
      <button
        type="button"
        role="option"
        aria-selected={i === selected}
        class:active={i === active}
        class:selected={i === selected}
        onmousedown={(e) => e.preventDefault()}
        onmouseenter={() => onhover?.(i)}
        onclick={() => onpick(i)}
      >
        <span class="label">{it.label}</span>
        {#if it.detail}<small>{it.detail}</small>{/if}
        {#if i === selected}<Icon name="check" size={13} />{/if}
      </button>
    </li>
  {/each}
</ul>

<style>
  .dropdown {
    position: fixed;
    z-index: 1000;
    max-width: calc(100vw - 16px);
    margin: 0;
    padding: 4px;
    list-style: none;
    overflow-y: auto;
    border-radius: var(--radius-md);
    /* One step above whatever it opens over (dialogs use --surface). */
    background: var(--surface-2);
    border: 1px solid var(--border-strong);
    box-shadow: var(--shadow);
    font-family: var(--font-ui);
    font-size: var(--font-size);
    color: var(--fg);
    animation: vp-pop var(--t-fast) var(--ease);
  }
  .dropdown.above {
    transform: translateY(-100%);
    animation: none;
  }
  button {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 30px;
    padding: 0 12px;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--fg);
    text-align: left;
    font: inherit;
  }
  button.active {
    background: var(--surface-3);
  }
  button.selected {
    color: var(--accent);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  small {
    color: var(--faint);
  }
</style>
