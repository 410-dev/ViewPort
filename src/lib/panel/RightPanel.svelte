<script lang="ts">
  import { t, type Key } from "../i18n.svelte";
  import { savePrefs, ui } from "../prefs.svelte";
  import InfoPanel from "./InfoPanel.svelte";
  import Preview from "./Preview.svelte";

  let {
    focus,
    selected,
    onopen,
    onchanged,
  }: { focus: number | null; selected: number[]; onopen: (id: number) => void; onchanged: () => void } = $props();

  const MODES: { key: "preview" | "both" | "info"; label: Key }[] = [
    { key: "preview", label: "panel.mode.preview" },
    { key: "both", label: "panel.mode.both" },
    { key: "info", label: "panel.mode.info" },
  ];

  const current = $derived(focus ?? selected[0] ?? null);

  let body: HTMLDivElement;
  function dragSplit(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const rect = body.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      ui().preview_split = Math.min(0.85, Math.max(0.15, (ev.clientY - rect.top) / rect.height));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      savePrefs();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
</script>

<aside class="panel">
  <header>
    <div class="seg sm" role="tablist">
      {#each MODES as m (m.key)}
        <button
          role="tab"
          aria-selected={ui().panel_mode === m.key}
          class:on={ui().panel_mode === m.key}
          onclick={() => {
            ui().panel_mode = m.key;
            savePrefs();
          }}>{t(m.label)}</button
        >
      {/each}
    </div>
  </header>
  <div class="body" bind:this={body}>
    {#if ui().panel_mode === "preview"}
      <Preview id={current} {selected} {onopen} />
    {:else if ui().panel_mode === "info"}
      <InfoPanel ids={selected} {onchanged} />
    {:else}
      <div class="top" style:height="{ui().preview_split * 100}%">
        <Preview id={current} {selected} {onopen} />
      </div>
      <div class="split" role="separator" aria-orientation="horizontal" onpointerdown={dragSplit}></div>
      <div class="bottom">
        <InfoPanel ids={selected} {onchanged} compact />
      </div>
    {/if}
  </div>
</aside>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-width: 0;
    background: var(--surface);
    border-left: 1px solid var(--border);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 48px;
    padding: 0 12px;
    flex-shrink: 0;
  }
  header .seg {
    width: 100%;
  }
  header .seg button {
    flex: 1;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .top {
    flex-shrink: 0;
    min-height: 0;
  }
  .bottom {
    flex: 1;
    min-height: 0;
  }
  .split {
    height: 9px;
    margin: -4px 0;
    position: relative;
    z-index: 1;
    cursor: row-resize;
    flex-shrink: 0;
  }
  .split::after {
    content: "";
    position: absolute;
    left: 14px;
    right: 14px;
    top: 4px;
    height: 1px;
    background: var(--border);
    transition: background var(--t-fast) var(--ease);
  }
  .split:hover::after {
    background: var(--accent-line);
  }
</style>
