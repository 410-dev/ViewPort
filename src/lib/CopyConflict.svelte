<script lang="ts">
  import { onMount } from "svelte";
  import { api, onCopyConflict, onCopyConflictDone, type CopyConflict, type OnConflict } from "./api";
  import { onBack } from "./back.svelte";
  import { fmtBytes, fmtDate } from "./format";
  import { t, type Key } from "./i18n.svelte";

  /**
   * A copy into another source found a file already at the target path (`shards.rs`
   * `copy_files`) and waits for the user. "For every remaining file" answers the rest of
   * that copy at once, so a copy of tens of thousands of files asks once.
   */
  let queue = $state<CopyConflict[]>([]);
  let all = $state(false);
  const current = $derived(queue[0] ?? null);

  onMount(() => {
    const offs = [
      onCopyConflict((c) => (queue = [...queue, c])),
      onCopyConflictDone((id) => (queue = queue.filter((c) => c.id !== id))),
    ];
    return () => offs.forEach((p) => p.then((f) => f()));
  });

  $effect(() => {
    if (current) return onBack(() => answer(null));
  });

  function answer(choice: OnConflict | null) {
    const c = current;
    if (!c) return;
    queue = queue.slice(1);
    api.copyConflictAnswer(c.id, choice, all).catch(() => {});
    all = false;
  }

  const CHOICES: { choice: OnConflict; label: Key; help: Key }[] = [
    { choice: "skip_same", label: "conflict.skipSame", help: "conflict.skipSame.help" },
    { choice: "skip_name", label: "conflict.skipName", help: "conflict.skipName.help" },
    { choice: "rename", label: "conflict.rename", help: "conflict.rename.help" },
    { choice: "overwrite", label: "conflict.overwrite", help: "conflict.overwrite.help" },
  ];
</script>

{#if current}
  <div class="scrim" role="presentation">
    <div class="dialog conflict" role="dialog" aria-modal="true" aria-labelledby="conflict-title">
      <header><h2 id="conflict-title">{t("conflict.title")}</h2></header>
      <div class="body">
        <p class="path" title={current.target}>{current.target}</p>
        <div class="facts">
          <span></span><span class="head">{t("conflict.size")}</span><span class="head">{t("conflict.modified")}</span>
          <span class="head">{t("conflict.existing")}</span>
          <span class="tabular">{fmtBytes(current.existing_size)}</span>
          <span class="tabular">{fmtDate(current.existing_modified_ms)}</span>
          <span class="head">{t("conflict.incoming")}</span>
          <span class="tabular">{fmtBytes(current.incoming_size)}</span>
          <span class="tabular">{fmtDate(current.incoming_modified_ms)}</span>
        </div>
        {#if current.same != null}
          <p class="same" class:diff={!current.same}>{current.same ? t("conflict.same") : t("conflict.different")}</p>
        {/if}
        <div class="choices">
          {#each CHOICES as c (c.choice)}
            <button class="choice" onclick={() => answer(c.choice)}>
              <span class="label">{t(c.label)}</span>
              <span class="help">{t(c.help)}</span>
            </button>
          {/each}
        </div>
        <label class="all">
          <input type="checkbox" bind:checked={all} />
          <span>{t("conflict.all")}</span>
          {#if queue.length > 1}<span class="more tabular">+{queue.length - 1}</span>{/if}
        </label>
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn ghost" onclick={() => answer(null)}>{t("conflict.cancelCopy")}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .conflict {
    width: min(520px, 100%);
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 4px 20px 8px;
  }
  .path {
    margin: 0;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .facts {
    display: grid;
    grid-template-columns: auto 1fr 1.6fr;
    gap: 4px 14px;
    font-size: 13px;
  }
  .head {
    color: var(--muted);
  }
  .same {
    margin: 0;
    font-size: 12px;
    color: var(--ok);
  }
  .same.diff {
    color: var(--warn);
  }
  .choices {
    display: grid;
    gap: 6px;
  }
  .choice {
    display: grid;
    gap: 2px;
    padding: 9px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
    color: var(--fg);
    text-align: left;
  }
  .choice:hover,
  .choice:focus-visible {
    background: var(--hover);
    border-color: var(--accent);
  }
  .label {
    font-weight: 500;
  }
  .help {
    font-size: 12px;
    color: var(--muted);
  }
  .all {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .more {
    margin-left: auto;
    color: var(--faint);
    font-size: 12px;
  }
</style>
