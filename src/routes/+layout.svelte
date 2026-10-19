<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { matches } from "$lib/keys";
  import { prefs } from "$lib/prefs.svelte";
  import { exitImmersive } from "$lib/immersive.svelte";
  import { startBackButton } from "$lib/back.svelte";
  import { picker } from "$lib/pick.svelte";
  import PathPicker from "$lib/ui/PathPicker.svelte";

  let { children } = $props();

  // The panic key is a fast double press (default: Escape twice), in every window and in any
  // mode. It is captured before any component sees the key. A single press keeps its normal
  // meaning; in immersive mode it leaves immersive mode first.
  onMount(() => {
    startBackButton();
    let last = 0;
    const onKey = (e: KeyboardEvent) => {
      const p = prefs.app?.privacy;
      if (e.repeat || !p || !matches(e, p.panic_keys)) return;
      const now = performance.now();
      if (now - last <= (p.panic_double_ms || 400)) {
        last = 0;
        e.preventDefault();
        e.stopImmediatePropagation();
        document.querySelectorAll("video").forEach((v) => (v.muted = true));
        api.panic().catch(() => {});
        return;
      }
      last = now;
      if (exitImmersive()) {
        e.preventDefault();
        e.stopImmediatePropagation();
      }
    };
    window.addEventListener("keydown", onKey, { capture: true });
    return () => window.removeEventListener("keydown", onKey, { capture: true });
  });
</script>

{@render children()}

{#if picker.req}
  {#key picker.req}
    <PathPicker req={picker.req} />
  {/key}
{/if}
