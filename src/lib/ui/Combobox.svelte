<script lang="ts">
  import Dropdown from "./Dropdown.svelte";

  /**
   * Text input with suggestions in the app's own dropdown (replaces `<datalist>`), used for
   * list (enum) values such as tags. `suggest` is called as the text changes.
   */
  let {
    value = $bindable(""),
    suggest,
    onpick,
    onenter,
    oninput,
    placeholder = "",
    class: cls = "",
  }: {
    value?: string;
    suggest: (prefix: string) => Promise<string[]>;
    /** A suggestion was chosen (value is already set to it). */
    onpick?: (value: string) => void;
    /** Enter without a highlighted suggestion. */
    onenter?: (value: string) => void;
    oninput?: (value: string) => void;
    placeholder?: string;
    class?: string;
  } = $props();

  let el = $state<HTMLInputElement | null>(null);
  const listId = `cb-${Math.random().toString(36).slice(2, 9)}`;
  let items = $state<string[]>([]);
  let open = $state(false);
  let active = $state(-1);
  let seq = 0;

  async function refresh() {
    const id = ++seq;
    const list = await suggest(value).catch(() => [] as string[]);
    if (id !== seq) return;
    // Hide an exact match only list: nothing left to choose.
    items = list.filter((s) => s !== value || list.length > 1);
    active = -1;
    open = items.length > 0 && document.activeElement === el;
  }

  function pick(i: number) {
    const v = items[i];
    if (v === undefined) return;
    value = v;
    open = false;
    oninput?.(v);
    onpick?.(v);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      if (!open) refresh();
      else active = Math.min(items.length - 1, active + 1);
    } else if (e.key === "ArrowUp" && open) active = Math.max(-1, active - 1);
    else if (e.key === "Enter") {
      if (open && active >= 0) pick(active);
      else {
        open = false;
        onenter?.(value);
      }
    } else if (e.key === "Escape" && open) {
      open = false;
      e.stopPropagation();
    } else return;
    e.preventDefault();
  }
</script>

<input
  bind:this={el}
  bind:value
  class="input {cls}"
  {placeholder}
  autocomplete="off"
  spellcheck="false"
  role="combobox"
  aria-expanded={open}
  aria-controls={listId}
  aria-autocomplete="list"
  onfocus={refresh}
  oninput={() => {
    oninput?.(value);
    refresh();
  }}
  onkeydown={onKey}
  onblur={() => setTimeout(() => (open = false), 120)}
/>
{#if open && el}
  <Dropdown
    id={listId}
    anchor={el}
    items={items.map((label) => ({ label }))}
    {active}
    onhover={(i) => (active = i)}
    onpick={pick}
    onclose={() => (open = false)}
  />
{/if}
