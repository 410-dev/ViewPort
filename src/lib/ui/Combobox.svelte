<script lang="ts">
  import { t } from "../i18n.svelte";
  import { ui } from "../prefs.svelte";
  import Dropdown from "./Dropdown.svelte";
  import Icon from "./Icon.svelte";

  /**
   * Text input with suggestions in the app's own dropdown (replaces `<datalist>`), used for
   * list (enum) values such as tags. `suggest(prefix, limit)` is called as the text
   * changes; the button beside the input lists every known value. Typing completes the
   * first matching value inline (the completed part stays selected, so typing on
   * replaces it); text that matches nothing is offered as a new value when `onenter`
   * accepts new values.
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
    suggest: (prefix: string, limit: number) => Promise<string[]>;
    /** A suggestion was chosen (value is already set to it). */
    onpick?: (value: string) => void;
    /** Enter without a highlighted suggestion, or the "new value" entry. */
    onenter?: (value: string) => void;
    oninput?: (value: string) => void;
    placeholder?: string;
    class?: string;
  } = $props();

  let el = $state<HTMLInputElement | null>(null);
  let box = $state<HTMLElement | null>(null);
  const listId = `cb-${Math.random().toString(36).slice(2, 9)}`;
  let items = $state<string[]>([]);
  let open = $state(false);
  /** The list shows every value (opened with the button). */
  let all = $state(false);
  let active = $state(-1);
  let seq = 0;

  const same = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "accent" }) === 0;
  /** Typed text no known value equals: offered as a new value. */
  const fresh = $derived(!!onenter && value.trim() !== "" && !all && !items.some((s) => same(s, value.trim())));
  const rows = $derived([
    ...items.map((label) => ({ label })),
    ...(fresh ? [{ label: value.trim(), detail: t("cb.new") }] : []),
  ]);

  /**
   * `complete`: the user just typed (not deleted). The first match completes inline, or,
   * while an IME composes (Hangul), is only highlighted: changing the text mid-composition
   * breaks the IME. Enter takes the highlighted value either way.
   */
  async function refresh(complete: "inline" | "highlight" | false = false) {
    const id = ++seq;
    const typed = value;
    const list = await suggest(typed, ui().suggest_limit).catch(() => [] as string[]);
    if (id !== seq || value !== typed) return;
    all = false;
    // Hide an exact match only list: nothing left to choose.
    items = list.filter((s) => s !== typed || list.length > 1);
    active = -1;
    const focused = document.activeElement === el;
    open = (items.length > 0 || fresh) && focused;
    if (!complete || !focused || !typed || !el) return;
    const lower = typed.toLocaleLowerCase();
    const match = items.find((s) => s.length > typed.length && s.toLocaleLowerCase().startsWith(lower));
    if (!match) return;
    active = items.indexOf(match);
    if (complete === "highlight") return;
    value = match;
    oninput?.(match);
    // Select the completed part once the input shows the new value.
    queueMicrotask(() => el?.setSelectionRange(typed.length, match.length, "backward"));
  }

  /** The button: every known value, or close the list. */
  async function showAll() {
    if (open && all) {
      open = false;
      return;
    }
    const id = ++seq;
    const list = await suggest("", ui().value_list_limit).catch(() => [] as string[]);
    if (id !== seq) return;
    items = list;
    all = true;
    active = items.findIndex((s) => same(s, value));
    open = items.length > 0;
    // Keep the keyboard closed on touch screens: picking is enough there.
    if (matchMedia("(hover: hover)").matches) el?.focus();
  }

  function pick(i: number) {
    if (i === items.length && fresh) {
      const v = value.trim();
      open = false;
      onenter?.(v);
      return;
    }
    const v = items[i];
    if (v === undefined) return;
    value = v;
    open = false;
    oninput?.(v);
    onpick?.(v);
  }

  /** Enter pressed while an IME was composing: handled once the text is committed. */
  let enterAfterComposition = false;

  function enter() {
    if (open && active >= 0) return pick(active);
    open = false;
    onenter?.(value);
  }

  function compositionEnd() {
    if (!enterAfterComposition) return;
    enterAfterComposition = false;
    // The committed text reaches `value` with the following input event.
    setTimeout(() => {
      const v = value.trim().toLocaleLowerCase();
      const i = v ? items.findIndex((s) => s.toLocaleLowerCase().startsWith(v)) : -1;
      if (open && i >= 0) pick(i);
      else enter();
    });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && e.isComposing) {
      enterAfterComposition = true;
      return;
    }
    if (e.key === "ArrowDown") {
      if (!open) refresh();
      else active = Math.min(rows.length - 1, active + 1);
    } else if (e.key === "ArrowUp" && open) active = Math.max(-1, active - 1);
    else if (e.key === "Enter") enter();
    else if (e.key === "Escape" && open) {
      open = false;
      e.stopPropagation();
    } else return;
    e.preventDefault();
  }
</script>

<span class="cb {cls}" bind:this={box}>
  <input
    bind:this={el}
    bind:value
    class="input"
    {placeholder}
    autocomplete="off"
    spellcheck="false"
    role="combobox"
    aria-expanded={open}
    aria-controls={listId}
    aria-autocomplete="both"
    onfocus={() => refresh()}
    oninput={(e) => {
      oninput?.(value);
      const kind = (e as unknown as InputEvent).inputType ?? "";
      refresh(kind === "insertCompositionText" ? "highlight" : kind.startsWith("insert") ? "inline" : false);
    }}
    onkeydown={onKey}
    oncompositionend={compositionEnd}
    onblur={() => setTimeout(() => (open = false), 120)}
  />
  <!-- pointerdown is cancelled so the input keeps focus (and its list) on desktop. -->
  <button
    type="button"
    class="drop"
    class:on={open && all}
    tabindex="-1"
    aria-label={t("cb.showAll")}
    title={t("cb.showAll")}
    onpointerdown={(e) => e.preventDefault()}
    onclick={showAll}><Icon name="chevronDown" size={14} /></button
  >
</span>
{#if open && box}
  <Dropdown
    id={listId}
    anchor={box}
    items={rows}
    {active}
    onhover={(i) => (active = i)}
    onpick={pick}
    onclose={() => (open = false)}
  />
{/if}

<style>
  .cb {
    position: relative;
    display: flex;
    min-width: 0;
    width: 100%;
  }
  .cb .input {
    flex: 1;
    width: 100%;
    min-width: 0;
    padding-right: calc(var(--h-control) - 4px);
  }
  .drop {
    position: absolute;
    top: 2px;
    right: 2px;
    bottom: 2px;
    display: grid;
    place-items: center;
    width: calc(var(--h-control) - 4px);
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--muted);
  }
  .drop:hover,
  .drop.on {
    background: var(--hover);
    color: var(--fg);
  }
  .drop.on :global(svg) {
    transform: rotate(180deg);
  }
</style>
