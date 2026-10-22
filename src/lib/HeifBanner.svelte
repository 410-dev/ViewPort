<script lang="ts">
  import { onMount } from "svelte";
  import { api, onHeifInstalled, onHeifNeeded, onTask, type HeifStatus } from "./api";
  import { fmtBytes } from "./format";
  import { t } from "./i18n.svelte";
  import { isMobile } from "./platform";
  import { prefs, savePrefs } from "./prefs.svelte";
  import Icon from "./ui/Icon.svelte";

  /** Bottom banner offering to download the HEIF decoder once an image needed it (`heif.rs`). */
  let { oninstalled }: { oninstalled: () => void } = $props();

  let status = $state<HeifStatus | null>(null);
  let closed = $state(false);
  let error = $state("");

  const visible = $derived(
    !closed &&
      !!status &&
      status.needed &&
      status.installable &&
      !status.available &&
      (prefs.app?.decoders?.heif_offer ?? true),
  );

  const refresh = () => api.heifStatus().then((s) => (status = s)).catch(() => {});

  onMount(() => {
    if (isMobile) return;
    refresh();
    const offs = [
      onHeifNeeded(refresh),
      onHeifInstalled(() => {
        refresh();
        oninstalled();
      }),
      onTask((task) => {
        if (task.kind !== "download_heif" || task.state === "running") return;
        if (task.state === "failed") error = task.error ?? "";
        refresh();
      }),
    ];
    return () => offs.forEach((p) => p.then((f) => f()));
  });

  async function install() {
    error = "";
    if (status) status.installing = true;
    await api.heifInstall().catch((e) => (error = String(e?.message ?? e)));
    refresh();
  }

  function never() {
    if (prefs.app) {
      prefs.app.decoders = { ...prefs.app.decoders, heif_offer: false };
      savePrefs(0);
    }
  }
</script>

{#if visible && status}
  <div class="banner" role="status">
    <Icon name="image" size={15} />
    <span class="text">
      {t("heif.banner")}
      {#if error}<span class="err">{t("heif.failed")} {error}</span>{/if}
    </span>
    <button class="btn sm primary" disabled={status.installing} onclick={install}>
      {status.installing ? t("heif.installing") : t("heif.install", { size: fmtBytes(status.size) })}
    </button>
    <button class="btn sm ghost" onclick={never}>{t("heif.never")}</button>
    <button class="icon-btn sm" onclick={() => (closed = true)} aria-label={t("common.close")}><Icon name="x" size={14} /></button>
  </div>
{/if}

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 40px;
    padding: 6px 8px 6px 16px;
    border-top: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--fg-2);
    font-size: 13px;
    flex-wrap: wrap;
  }
  .text {
    flex: 1;
    min-width: 200px;
  }
  .err {
    display: block;
    color: var(--danger);
    font-size: 12px;
  }
</style>
