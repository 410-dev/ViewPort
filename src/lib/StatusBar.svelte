<script lang="ts">
  import { fade, pop } from "./ui/motion";
  import { onBack } from "./back.svelte";
  import { api, type TaskInfo } from "./api";
  import { fmtBytes, fmtCount } from "./format";
  import { t, tx } from "./i18n.svelte";
  import { fraction, running, tasks } from "./tasks.svelte";
  import Icon from "./ui/Icon.svelte";
  import VolumeControl from "./ui/VolumeControl.svelte";

  /** Bottom bar: result count and timing on the left, background work on the right. */
  let { count, elapsed, selected }: { count: number; elapsed: number; selected: number } = $props();

  let open = $state(false);
  $effect(() => {
    if (open) return onBack(() => (open = false));
  });
  const active = $derived(running());
  const primary = $derived(active.find((x) => x.kind === "read") ?? active[0] ?? null);
  const failed = $derived(tasks.list.filter((x) => x.state === "failed"));

  function amount(task: TaskInfo, n: number | null) {
    if (n == null) return "";
    if (task.unit === "bytes") return fmtBytes(n);
    if (task.unit === "permille") return `${Math.round(n / 10)}%`;
    return fmtCount(n);
  }

  function progressText(task: TaskInfo) {
    if (task.unit === "permille") return amount(task, task.done);
    return task.total != null ? `${amount(task, task.done)} / ${amount(task, task.total)}` : amount(task, task.done);
  }

  function rate(task: TaskInfo) {
    if (task.state !== "running" || task.rate <= 0 || task.unit === "permille") return "";
    return task.unit === "bytes" ? `${fmtBytes(task.rate)}/s` : t("status.perSec", { n: task.rate.toFixed(task.rate < 10 ? 1 : 0) });
  }

  const title = (task: TaskInfo) => tx(`task.${task.kind}`, task.kind);
</script>

<footer class="status">
  <span class="tabular nowrap">{t("lib.items", { n: fmtCount(count) })}</span>
  {#if selected > 0}<span class="sep"></span><span class="tabular">{t("status.selected", { n: fmtCount(selected) })}</span>{/if}
  <span class="sep wide"></span>
  <span class="faint tabular wide">{elapsed.toFixed(1)} ms</span>
  <span class="grow"></span>

  <VolumeControl />
  <span class="sep"></span>
  <button class="tasks" class:idle={!primary && failed.length === 0} onclick={() => (open = !open)} aria-expanded={open}>
    {#if primary}
      {@const f = fraction(primary)}
      <span class="spinner"></span>
      <span class="what">{title(primary)}{primary.label ? ` · ${primary.label}` : ""}</span>
      <span class="bar" class:indeterminate={f == null}><i style:width="{(f ?? 0) * 100}%"></i></span>
      <span class="tabular faint">{progressText(primary)}</span>
      {#if active.length > 1}<span class="more">+{active.length - 1}</span>{/if}
    {:else if failed.length}
      <Icon name="alert" size={13} />
      <span class="warn">{t("status.failed", { n: failed.length })}</span>
    {:else}
      <Icon name="activity" size={13} />
      <span>{t("status.idle")}</span>
    {/if}
  </button>
</footer>

{#if open}
  <div class="pop-scrim" role="presentation" transition:fade onclick={() => (open = false)}></div>
  <div class="pop" role="dialog" aria-label={t("status.tasks")} transition:pop={{ y: 24 }}>
    <header>
      <span class="eyebrow">{t("status.tasks")}</span>
      <span class="grow"></span>
      <button class="icon-btn sm" onclick={() => (open = false)} aria-label={t("common.close")}><Icon name="x" size={14} /></button>
    </header>
    {#if tasks.list.length === 0}
      <p class="none">{t("status.noTasks")}</p>
    {:else}
      <ul>
        {#each [...tasks.list].reverse() as task (task.id)}
          {@const f = fraction(task)}
          <li class={task.state}>
            <div class="line">
              <span class="name">{title(task)}{task.label ? ` · ${task.label}` : ""}</span>
              {#if task.state === "running" && task.cancellable}
                <button class="icon-btn sm" title={t("common.cancel")} onclick={() => api.cancelTask(task.id)}><Icon name="x" size={12} /></button>
              {:else if task.state === "done"}
                <span class="ok"><Icon name="check" size={13} /></span>
              {:else if task.state === "cancelled"}
                <span class="faint">{t("status.cancelled")}</span>
              {/if}
            </div>
            {#if task.state === "running"}
              <span class="bar" class:indeterminate={f == null}><i style:width="{(f ?? 0) * 100}%"></i></span>
            {/if}
            <div class="line sub">
              <span class="detail" title={task.error ?? task.detail}>{task.error ?? task.detail}</span>
              <span class="tabular">{progressText(task)}{rate(task) ? ` · ${rate(task)}` : ""}</span>
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .status {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 30px;
    padding: 0 8px 0 16px;
    border-top: 1px solid var(--border);
    background: var(--surface);
    color: var(--muted);
    font-size: 12px;
    flex-shrink: 0;
  }
  .sep {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: var(--faint);
  }
  .faint {
    color: var(--faint);
  }
  .nowrap {
    white-space: nowrap;
  }
  .tasks {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 22px;
    max-width: min(560px, 60vw);
    padding: 0 10px;
    border: 0;
    border-radius: var(--radius-pill);
    background: var(--surface-2);
    color: var(--fg-2);
    font-size: 12px;
    transition: background var(--t-fast) var(--ease);
  }
  .tasks:hover {
    background: var(--hover);
  }
  .tasks.idle {
    background: transparent;
    color: var(--faint);
  }
  .tasks.idle:hover {
    background: var(--surface-2);
    color: var(--muted);
  }
  .what {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .tasks .bar {
    width: 90px;
    flex-shrink: 0;
  }
  /* Narrow screens: timing goes, the task summary shrinks instead of widening the page. */
  @media (max-width: 760px) {
    .status {
      gap: 8px;
      padding: 0 6px 0 12px;
    }
    .wide {
      display: none;
    }
    .tasks {
      min-width: 0;
      flex-shrink: 1;
      max-width: none;
    }
    .tasks .bar {
      width: 40px;
    }
  }
  .more {
    color: var(--accent);
    font-weight: 600;
  }
  .warn {
    color: var(--warn);
  }
  .pop-scrim {
    position: fixed;
    inset: 0;
    z-index: 30;
  }
  .pop {
    position: fixed;
    right: 10px;
    bottom: 38px;
    z-index: 31;
    width: min(420px, calc(100vw - 20px));
    max-height: min(480px, 70vh);
    display: flex;
    flex-direction: column;
    border-radius: var(--radius-lg);
    background: var(--surface);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
  }
  .pop header {
    display: flex;
    align-items: center;
    height: 40px;
    padding: 0 8px 0 16px;
    border-bottom: 1px solid var(--border);
  }
  .none {
    margin: 0;
    padding: 24px 16px;
    text-align: center;
    color: var(--faint);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 6px;
    overflow-y: auto;
  }
  li {
    display: grid;
    /* minmax(0, 1fr): an auto track would grow to a long task name's unwrapped width. */
    grid-template-columns: minmax(0, 1fr);
    gap: 6px;
    padding: 10px;
    border-radius: var(--radius-md);
  }
  li:hover {
    background: var(--surface-2);
  }
  li.failed .detail {
    color: var(--danger);
  }
  li.done,
  li.cancelled {
    opacity: 0.7;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 20px;
  }
  .name {
    flex: 1;
    min-width: 0;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    font-size: 11px;
    color: var(--muted);
    min-height: 0;
  }
  .detail {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ok {
    color: var(--ok);
    display: grid;
  }
</style>
