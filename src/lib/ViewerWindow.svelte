<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy } from "svelte";
  import { api, type Settings } from "./api";
  import { onSync, broadcast } from "./sync";
  import { invalidate } from "./views/briefs.svelte";
  import ViewerShell from "./ViewerShell.svelte";

  /** A standalone viewer window on the open workspace. */
  let { ids, index }: { ids: number[]; index: number } = $props();

  let settings = $state<Settings | null>(null);
  api.getSettings().then((s) => (settings = s)).catch(() => getCurrentWindow().close());

  const unlisten = [
    onSync("settings", () => api.getSettings().then((s) => (settings = s))),
    onSync("media", (changed) => invalidate(changed)),
  ];
  onDestroy(() => unlisten.forEach((p) => p.then((f) => f())));
</script>

{#if settings}
  <ViewerShell
    panes={[{ ids, index }]}
    {settings}
    onclose={() => getCurrentWindow().close()}
    onchanged={() => broadcast("media")}
  />
{/if}
