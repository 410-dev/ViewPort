<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { api, asCmdError, type PackKind, type StoreItem } from "../api";
  import { onBack } from "../back.svelte";
  import { fmtBytes } from "../format";
  import { i18n, t, type Key } from "../i18n.svelte";
  import { fraction, tasks } from "../tasks.svelte";
  import Icon from "../ui/Icon.svelte";

  /**
   * Downloadable model packs (`ml_store.rs`). Nothing is fetched until the user has opened
   * a model's license step and accepted it; the accepted license name goes with the request.
   */
  let { onclose, onchanged }: { onclose: () => void; onchanged: () => void } = $props();

  let items = $state<StoreItem[]>([]);
  let error = $state("");
  /** The model whose license step is open, and whether its box is ticked. */
  let consent = $state<{ id: string; agreed: boolean } | null>(null);

  $effect(() => onBack(() => (consent ? (consent = null) : onclose())));

  async function refresh() {
    items = await api.mlCatalog().catch((e) => {
      error = asCmdError(e).message;
      return [];
    });
  }
  refresh();

  // A download is a task named after the model; refresh when one ends.
  const running = $derived(tasks.list.filter((x) => x.kind === "download_model" && x.state === "running"));
  let lastRunning = 0;
  $effect(() => {
    const n = running.length;
    if (n < lastRunning) {
      refresh();
      onchanged();
    }
    lastRunning = n;
  });
  const task = (item: StoreItem) => tasks.list.find((x) => x.kind === "download_model" && x.label === item.name && x.state === "running");
  const failed = (item: StoreItem) => tasks.list.find((x) => x.kind === "download_model" && x.label === item.name && x.state === "failed");

  const KIND: Record<PackKind, Key> = {
    face_detector: "store.kind.faceDetector",
    face_embedder: "store.kind.faceEmbedder",
    anime_detector: "store.kind.animeDetector",
    anime_embedder: "store.kind.animeEmbedder",
    tagger: "store.kind.tagger",
  };

  async function download(item: StoreItem) {
    error = "";
    try {
      await api.mlDownload(item.id, item.license.name);
      consent = null;
      refresh();
    } catch (e) {
      error = asCmdError(e).message;
    }
  }

  const open = (url: string) => openUrl(url).catch((e) => (error = asCmdError(e).message));
</script>

<div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog store" role="dialog" aria-modal="true" aria-labelledby="store-title">
    <header>
      <h2 id="store-title">{t("store.title")}</h2>
      <button class="icon-btn" aria-label={t("common.close")} onclick={onclose}><Icon name="x" size={16} /></button>
    </header>
    <div class="body">
      <p class="note">{t("store.note")}</p>
      {#if error}<p class="err">{error}</p>{/if}
      <ul>
        {#each items as item (item.id)}
          {@const tk = task(item)}
          {@const bad = failed(item)}
          <li>
            <div class="head">
              <div class="title">
                <strong>{item.name}</strong>
                <span class="kind">{t(KIND[item.kind])}</span>
              </div>
              {#if tk}
                {@const f = fraction(tk)}
                <span class="prog"><span class="spinner"></span>{f == null ? "…" : `${Math.round(f * 100)}%`}</span>
                <button class="btn sm ghost" onclick={() => api.cancelTask(tk.id)}>{t("common.cancel")}</button>
              {:else if item.installed}
                <span class="ok"><Icon name="check" size={13} />{t("store.installed")}</span>
              {:else}
                <button class="btn sm primary" onclick={() => (consent = { id: item.id, agreed: false })}>
                  <Icon name="download" size={13} />{t("store.get")}
                </button>
              {/if}
            </div>
            <p class="summary">{item.summary[i18n.locale] ?? item.summary.en ?? ""}</p>
            <p class="meta">
              {item.size > 0 ? fmtBytes(item.size) : ""} · {t("store.license")}
              <button class="link" onclick={() => open(item.license.url)}>{item.license.name}</button>
              · <button class="link" onclick={() => open(item.homepage)}>{t("store.homepage")}</button>
            </p>
            {#if bad}<p class="err small">{t("store.failed")}: {bad.error ?? ""}</p>{/if}
            {#if consent?.id === item.id}
              <div class="consent">
                <p>{t("store.consent.body", { name: item.name, license: item.license.name })}</p>
                <button class="btn sm" onclick={() => open(item.license.url)}><Icon name="external" size={13} />{t("store.consent.read")}</button>
                <label class="check">
                  <input type="checkbox" bind:checked={consent.agreed} />
                  <span>{t("store.consent.agree", { license: item.license.name })}</span>
                </label>
                <div class="actions">
                  <button class="btn sm ghost" onclick={() => (consent = null)}>{t("common.cancel")}</button>
                  <button class="btn sm primary" disabled={!consent.agreed} onclick={() => download(item)}>
                    <Icon name="download" size={13} />{t("store.consent.download")}
                  </button>
                </div>
                <p class="small dim">{t("store.consent.where", { path: item.path })}</p>
              </div>
            {/if}
          </li>
        {/each}
      </ul>
    </div>
  </div>
</div>

<style>
  .store {
    width: min(640px, 100%);
    max-height: min(760px, 100%);
  }
  .note {
    margin: 0 0 12px;
    color: var(--muted);
    font-size: 12px;
    line-height: 1.5;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 10px;
  }
  li {
    padding: 12px 14px;
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .title {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 8px;
  }
  .kind {
    font-size: 11px;
    color: var(--faint);
  }
  .summary {
    margin: 6px 0 4px;
    font-size: 13px;
    color: var(--fg-2);
  }
  .meta {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    text-decoration: underline;
    font: inherit;
  }
  .ok,
  .prog {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
  }
  .ok {
    color: var(--ok, var(--accent));
  }
  .consent {
    display: grid;
    gap: 8px;
    margin-top: 10px;
    padding: 12px;
    border-radius: var(--radius-md);
    border: 1px solid var(--border);
    background: var(--surface);
  }
  .consent p {
    margin: 0;
    line-height: 1.5;
    font-size: 13px;
  }
  .consent > .btn {
    justify-self: start;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }
  .err {
    margin: 0 0 8px;
    color: var(--danger);
    font-size: 12px;
  }
  .small {
    font-size: 12px !important;
  }
  .dim {
    color: var(--faint);
    overflow-wrap: anywhere;
  }
</style>
