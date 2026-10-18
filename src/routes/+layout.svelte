<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { matches } from "$lib/keys";
  import { prefs } from "$lib/prefs.svelte";

  let { children } = $props();

  // The panic key works in every window and takes precedence over everything else
  // (captured before any component sees the key).
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.repeat || !matches(e, prefs.app?.privacy.panic_keys)) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      document.querySelectorAll("video").forEach((v) => (v.muted = true));
      api.panic().catch(() => {});
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  });
</script>

{@render children()}
