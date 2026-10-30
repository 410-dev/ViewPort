<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { api } from "$lib/api";
  import { matches } from "$lib/keys";
  import { applyPrefs, pageZoom, prefs, savePrefs } from "$lib/prefs.svelte";
  import { broadcast, onSync } from "$lib/sync";
  import { isMobile } from "$lib/platform";
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

  // The page itself never zooms by pinching or Ctrl+wheel (the viewer zooms its picture on
  // its own). On desktop the zoom keys scale the window instead, in every window.
  onMount(() => {
    const noWheelZoom = (e: WheelEvent) => e.ctrlKey && e.preventDefault();
    const noGesture = (e: Event) => e.preventDefault();
    const onKey = (e: KeyboardEvent) => {
      const u = prefs.app?.ui;
      if (isMobile || !u) return;
      let z: number;
      if (matches(e, u.page_zoom_in_keys)) z = u.page_zoom + (u.page_zoom_step || 10);
      else if (matches(e, u.page_zoom_out_keys)) z = u.page_zoom - (u.page_zoom_step || 10);
      else if (matches(e, u.page_zoom_reset_keys)) z = 100;
      else return;
      e.preventDefault();
      e.stopImmediatePropagation();
      u.page_zoom = pageZoom(u, z);
      savePrefs(0, () => broadcast("zoom"));
    };
    window.addEventListener("wheel", noWheelZoom, { passive: false, capture: true });
    document.addEventListener("gesturestart", noGesture);
    window.addEventListener("keydown", onKey, { capture: true });
    const off = onSync("zoom", async () => {
      const app = await api.getAppSettings();
      if (!prefs.app) return;
      prefs.app.ui.page_zoom = app.ui.page_zoom;
      applyPrefs(prefs.app);
    });
    return () => {
      window.removeEventListener("wheel", noWheelZoom, { capture: true });
      document.removeEventListener("gesturestart", noGesture);
      window.removeEventListener("keydown", onKey, { capture: true });
      off.then((f) => f());
    };
  });
</script>

{@render children()}

{#if picker.req}
  {#key picker.req}
    <PathPicker req={picker.req} />
  {/key}
{/if}
