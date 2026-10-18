<script lang="ts">
  import { api, type BundleSummary } from "$lib/api";
  import Library from "$lib/Library.svelte";
  import { loadPrefs, prefs } from "$lib/prefs.svelte";
  import StartScreen from "$lib/StartScreen.svelte";
  import { startTaskTracking } from "$lib/tasks.svelte";

  let bundle = $state<BundleSummary | null>(null);
  let ready = $state(false);

  // A reload of the webview keeps the bundle open in the backend.
  $effect(() => {
    (async () => {
      await loadPrefs().catch(() => {});
      startTaskTracking();
      bundle = await api.currentBundle();
      if (!bundle && !sessionStorage.getItem("vp-started")) {
        // Only on a real app start, not when the user closed the bundle and the page reloaded.
        sessionStorage.setItem("vp-started", "1");
        const recent = await api.recentBundles();
        if (prefs.app?.reopen_last_bundle && recent[0]) {
          bundle = await api.openBundle(recent[0]).catch(() => null);
        }
      }
      ready = true;
    })();
  });
</script>

{#if ready && prefs.app}
  {#if bundle}
    {#key bundle.path}
      <Library {bundle} onclose={() => (bundle = null)} onbundle={(b) => (bundle = b)} />
    {/key}
  {:else}
    <StartScreen onopened={(b) => (bundle = b)} />
  {/if}
{/if}
