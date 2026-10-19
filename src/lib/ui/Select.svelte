<script lang="ts">
  import Dropdown from "./Dropdown.svelte";
  import Icon from "./Icon.svelte";

  /** Pill-shaped select with the app's own dropdown (replaces `<select>`). */
  let {
    value,
    options,
    onchange,
    class: cls = "",
    disabled = false,
    label = "",
  }: {
    value: string;
    options: { value: string; label: string }[];
    onchange: (value: string) => void;
    class?: string;
    disabled?: boolean;
    /** Accessible name. */
    label?: string;
  } = $props();

  let btn = $state<HTMLButtonElement | null>(null);
  let open = $state(false);
  let active = $state(-1);
  const index = $derived(options.findIndex((o) => o.value === value));

  function show() {
    if (disabled) return;
    active = Math.max(0, index);
    open = true;
  }

  function pick(i: number) {
    open = false;
    const o = options[i];
    if (o && o.value !== value) onchange(o.value);
    btn?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(e.key)) {
        e.preventDefault();
        show();
      }
      return;
    }
    if (e.key === "ArrowDown") active = Math.min(options.length - 1, active + 1);
    else if (e.key === "ArrowUp") active = Math.max(0, active - 1);
    else if (e.key === "Home") active = 0;
    else if (e.key === "End") active = options.length - 1;
    else if (e.key === "Enter" || e.key === " ") pick(active);
    else if (e.key === "Escape" || e.key === "Tab") {
      open = false;
      if (e.key === "Tab") return;
      e.stopPropagation();
    } else return;
    e.preventDefault();
  }
</script>

<button
  type="button"
  class="input select {cls}"
  class:open
  bind:this={btn}
  {disabled}
  aria-haspopup="listbox"
  aria-expanded={open}
  aria-label={label || undefined}
  onclick={() => (open ? (open = false) : show())}
  onkeydown={onKey}
>
  <span class="value">{options[index]?.label ?? ""}</span>
  <span class="chev" class:up={open}><Icon name="chevronDown" size={14} /></span>
</button>
{#if open && btn}
  <Dropdown
    anchor={btn}
    items={options}
    {active}
    selected={index}
    onhover={(i) => (active = i)}
    onpick={pick}
    onclose={() => (open = false)}
  />
{/if}

<style>
  .select {
    display: inline-flex;
    max-width: 100%;
    align-items: center;
    gap: 8px;
    padding-right: 10px;
    text-align: left;
    cursor: pointer;
  }
  .select.open {
    border-color: var(--accent-line);
  }
  .value {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    display: grid;
    color: var(--muted);
    transition: transform var(--t-fast) var(--ease);
  }
  .chev.up {
    transform: rotate(180deg);
  }
</style>
