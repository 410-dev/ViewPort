<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { api, initMediaUrls, onAppSettings, type BundleSummary, type WindowPayload } from "$lib/api";
  import { startAudio } from "$lib/audio.svelte";
  import Library from "$lib/Library.svelte";
  import { windowLabel } from "$lib/platform";
  import { loadPrefs, prefs } from "$lib/prefs.svelte";
  import StartScreen from "$lib/StartScreen.svelte";
  import { startTaskTracking } from "$lib/tasks.svelte";
  import ViewerWindow from "$lib/ViewerWindow.svelte";

  /** Every window loads this page; its label says what it is (`main`, `viewer-N`, `library-N`). */
  const label = windowLabel();
  const main = label === "main";

  let bundle = $state<BundleSummary | null>(null);
  let payload = $state<WindowPayload | null>(null);
  let ready = $state(false);

  $effect(() => {
    (async () => {
      await loadPrefs().catch(() => {});
      await initMediaUrls();
      startTaskTracking();
      startAudio();
      // Settings saved in another window apply here too.
      onAppSettings(() => loadPrefs().catch(() => {}));
      bundle = await api.currentBundle();
      if (!main) {
        payload = await api.takeWindowPayload().catch(() => null);
        // Extra windows only exist on an open workspace; a reloaded viewer has nothing to show.
        if (!bundle || (label.startsWith("viewer") && payload?.kind !== "viewer")) return getCurrentWindow().close();
      } else if (!bundle && !sessionStorage.getItem("vp-started")) {
        // Only on a real app start, not when the user closed the bundle and the page reloaded.
        sessionStorage.setItem("vp-started", "1");
        const recent = await api.recentBundles();
        if (prefs.app?.reopen_last_bundle && recent[0]?.exists) {
          bundle = await api.openBundle(recent[0].path).catch(() => null);
        }
      }
      ready = true;
    })();
  });
</script>

{#if ready && prefs.app}
  {#if payload?.kind === "viewer"}
    <ViewerWindow ids={payload.ids} index={payload.index} />
  {:else if bundle}
    {#key bundle.path}
      <Library
        {bundle}
        secondary={!main}
        initial={payload?.kind === "library" ? payload : null}
        onclose={() => (bundle = null)}
        onbundle={(b) => (bundle = b)}
      />
    {/key}
  {:else if main}
    <StartScreen onopened={(b) => (bundle = b)} />
  {/if}
{/if}
