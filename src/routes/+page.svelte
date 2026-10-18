<script lang="ts">
  import { api, type BundleSummary } from "$lib/api";
  import Library from "$lib/Library.svelte";
  import StartScreen from "$lib/StartScreen.svelte";

  let bundle = $state<BundleSummary | null>(null);
  let ready = $state(false);

  // A reload of the webview keeps the bundle open in the backend.
  $effect(() => {
    (async () => {
      bundle = await api.currentBundle();
      if (!bundle && !sessionStorage.getItem("vp-started")) {
        // Only on a real app start, not when the user closed the bundle and the page reloaded.
        sessionStorage.setItem("vp-started", "1");
        const [app, recent] = await Promise.all([api.getAppSettings(), api.recentBundles()]);
        if (app.reopen_last_bundle && recent[0]) {
          bundle = await api.openBundle(recent[0]).catch(() => null);
        }
      }
      ready = true;
    })();
  });
</script>

{#if ready}
  {#if bundle}
    {#key bundle.path}
      <Library {bundle} onclose={() => (bundle = null)} />
    {/key}
  {:else}
    <StartScreen onopened={(b) => (bundle = b)} />
  {/if}
{/if}

<style>
  :global(:root) {
    --bg: #f5f5f4;
    --panel: #ffffff;
    --tile: #e7e5e4;
    --fg: #1c1917;
    --muted: #78716c;
    --border: #e7e5e4;
    --accent: #2563eb;
    --accent-soft: #dbeafe;
    --danger: #dc2626;
    --warn: #b45309;
    color-scheme: light;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #121212;
      --panel: #1a1a1a;
      --tile: #242424;
      --fg: #e7e5e4;
      --muted: #a1a1aa;
      --border: #2a2a2a;
      --accent: #60a5fa;
      --accent-soft: #1e3a5f;
      --danger: #f87171;
      --warn: #fbbf24;
      color-scheme: dark;
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font: 14px/1.4 system-ui, -apple-system, "Segoe UI", "Malgun Gothic", sans-serif;
    overflow: hidden;
  }
  :global(button) {
    font: inherit;
    color: inherit;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 12px;
    cursor: pointer;
  }
  :global(button:hover:not(:disabled)) {
    border-color: var(--muted);
  }
  :global(button.primary) {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  :global(button:disabled) {
    opacity: 0.5;
    cursor: default;
  }
  :global(input:not([type="range"])) {
    font: inherit;
    color: inherit;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 7px 10px;
    outline: none;
  }
  :global(input:focus) {
    border-color: var(--accent);
  }
</style>
