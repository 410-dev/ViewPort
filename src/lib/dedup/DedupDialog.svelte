<script lang="ts">
  import { fade, pop } from "../ui/motion";
  import { onMount } from "svelte";
  import {
    api,
    thumbUrl,
    type DupApplyGroup,
    type DupApplyReport,
    type DupEntry,
    type DupGroup,
    type DupItem,
    type DupReportRow,
    type DupScope,
    type SourceRow,
  } from "../api";
  import { onBack } from "../back.svelte";
  import { fmtBytes, fmtCount, fmtDate } from "../format";
  import { t } from "../i18n.svelte";
  import { broadcast } from "../sync";
  import { fraction, tasks } from "../tasks.svelte";
  import Icon from "../ui/Icon.svelte";
  import Select from "../ui/Select.svelte";
  import { errText } from "../shard/shared";

  /**
   * Duplicate finder (`dedup.rs`): finds files with the same contents within each source or
   * across sources, and lets the user pick per file what to do. Every file can be marked for
   * deletion; a shard file can instead share another copy's stored data ("save space",
   * copy-on-write), which keeps the file. Nothing happens until "Apply".
   */
  let {
    sources,
    initial,
    onchange,
    onclose,
  }: { sources: SourceRow[]; initial: number | null; /** Files were deleted. */ onchange?: () => void; onclose: () => void } = $props();

  type Mark = "delete" | "link";
  type Basis = "added" | "modified" | "created";

  let picked = $state<number[]>([]);
  let across = $state(true);
  let reports = $state<DupReportRow[]>([]);
  let groups = $state<DupGroup[] | null>(null);
  let scope = $state<DupScope | null>(null);
  /** Summary of the scan shown (absent for a saved report). */
  let stats = $state<{ checked: number; read_bytes: number; elapsed_ms: number; failed: string[] } | null>(null);
  let openReport = $state<DupReportRow | null>(null);
  let marks = $state<Record<number, Mark>>({});
  let basis = $state<Basis>("added");
  let shown = $state(150);
  let busy = $state<"scan" | "apply" | null>(null);
  let confirming = $state(false);
  let error = $state<string | null>(null);
  let done = $state<DupApplyReport | null>(null);
  let savedNote = $state<string | null>(null);
  let renaming = $state<{ id: number; label: string } | null>(null);

  const usable = $derived(sources.filter((s) => s.enabled));
  const nameOf = (id: number) => sources.find((s) => s.id === id)?.name ?? `#${id}`;
  const isShard = (id: number) => sources.find((s) => s.id === id)?.driver === "shard";
  const scanTask = $derived(tasks.list.find((x) => x.kind === "dedup_scan" && x.state === "running"));
  const applyTask = $derived(tasks.list.find((x) => x.kind === "dedup_apply" && x.state === "running"));
  const live = (it: DupItem) => !it.action || it.action === "linked";

  onMount(() => {
    // svelte-ignore state_referenced_locally
    picked = initial != null && usable.some((s) => s.id === initial) ? [initial] : usable.map((s) => s.id);
    loadReports();
  });
  $effect(() => onBack(() => (busy ? undefined : groups ? back() : onclose())));

  async function loadReports() {
    reports = await api.dupReports().catch(() => []);
  }

  async function scan() {
    if (!picked.length) return;
    busy = "scan";
    error = null;
    done = null;
    try {
      const sc: DupScope = { sources: [...picked], across };
      const r = await api.dupScan(sc.sources, sc.across);
      groups = r.groups;
      scope = sc;
      stats = { checked: r.checked, read_bytes: r.read_bytes, elapsed_ms: r.elapsed_ms, failed: r.failed };
      openReport = null;
      marks = {};
      shown = 150;
    } catch (e) {
      error = errText(e);
    } finally {
      busy = null;
    }
  }

  async function open(r: DupReportRow) {
    error = null;
    done = null;
    try {
      const got = await api.dupReport(r.id);
      if (!got) return void (await loadReports());
      groups = got.groups;
      scope = got.report.scope;
      stats = null;
      openReport = got.report;
      marks = {};
      shown = 150;
    } catch (e) {
      error = errText(e);
    }
  }

  function back() {
    groups = null;
    openReport = null;
    confirming = false;
    done = null;
    loadReports();
  }

  // ------------------------------------------------------------ marks

  function setMark(it: DupItem, m: Mark, on: boolean) {
    const next = { ...marks };
    if (on) next[it.media_id] = m;
    else if (next[it.media_id] === m) delete next[it.media_id];
    marks = next;
  }

  /** Shard items with another item of the same shard in the group: they can share its data. */
  function canLink(g: DupGroup, it: DupItem): boolean {
    return live(it) && isShard(it.source_id) && g.items.filter((x) => live(x) && x.source_id === it.source_id).length > 1;
  }

  /** Whether the item already shares its data with every other item of its shard here. */
  function shared(g: DupGroup, it: DupItem): boolean {
    const same = g.items.filter((x) => live(x) && x.source_id === it.source_id && x.media_id !== it.media_id);
    return !!it.body && same.length > 0 && same.every((x) => x.body === it.body);
  }

  function dateOf(it: DupItem): number {
    const order: Record<Basis, (number | null)[]> = {
      added: [it.added_ms, it.created_ms, it.modified_ms],
      modified: [it.modified_ms, it.created_ms, it.added_ms],
      created: [it.created_ms, it.modified_ms, it.added_ms],
    };
    return order[basis].find((v) => v != null && v > 0) ?? Number.MAX_SAFE_INTEGER;
  }

  /** Marks every item but the oldest (or newest) of each group for deletion. */
  function keepOnly(which: "oldest" | "newest") {
    const next: Record<number, Mark> = {};
    for (const g of groups ?? []) {
      const items = g.items.filter(live);
      if (items.length < 2) continue;
      const sorted = [...items].sort((a, b) => (which === "oldest" ? dateOf(a) - dateOf(b) : dateOf(b) - dateOf(a)) || a.media_id - b.media_id);
      for (const it of sorted.slice(1)) next[it.media_id] = "delete";
    }
    marks = next;
  }

  /** Shard items marked for deletion share another copy's data instead, keeping the file. */
  function shardsSaveSpace() {
    const next = { ...marks };
    for (const g of groups ?? []) {
      for (const it of g.items) {
        if (canLink(g, it) && (next[it.media_id] === "delete" || !next[it.media_id]) && !shared(g, it)) next[it.media_id] = "link";
      }
    }
    marks = next;
  }

  /** Distinct stored copies in a shard after the marks are applied (links merge into one). */
  function copiesAfter(items: DupItem[]): number {
    const kept = items.filter((x) => marks[x.media_id] !== "delete");
    const plain = kept.filter((x) => marks[x.media_id] !== "link");
    const linked = kept.length > plain.length;
    const bodies = new Set(plain.map((x) => x.body ?? `m${x.media_id}`));
    return linked && bodies.size === 0 ? 1 : bodies.size;
  }

  const summary = $derived.by(() => {
    let del = 0,
      link = 0,
      freed = 0,
      emptied = 0;
    for (const g of groups ?? []) {
      const items = g.items.filter(live);
      if (!items.length) continue;
      if (items.every((x) => marks[x.media_id] === "delete")) emptied++;
      const bySource = new Map<number, DupItem[]>();
      for (const it of items) {
        const m = marks[it.media_id];
        if (m === "delete") del++;
        if (m === "link") link++;
        bySource.set(it.source_id, [...(bySource.get(it.source_id) ?? []), it]);
      }
      for (const [sid, list] of bySource) {
        if (isShard(sid)) {
          const before = new Set(list.map((x) => x.body ?? `m${x.media_id}`)).size;
          freed += (before - copiesAfter(list)) * g.size;
        } else {
          freed += list.filter((x) => marks[x.media_id] === "delete").length * g.size;
        }
      }
    }
    return { del, link, freed, emptied };
  });

  const totals = $derived.by(() => {
    let files = 0,
      wasted = 0;
    for (const g of groups ?? []) {
      const n = g.items.filter(live).length;
      files += n;
      wasted += Math.max(0, n - 1) * g.size;
    }
    return { files, wasted };
  });

  // ------------------------------------------------------------ actions

  async function apply() {
    confirming = false;
    const list: DupApplyGroup[] = [];
    for (const g of groups ?? []) {
      const items = g.items.filter(live).map((x) => x.media_id);
      const del = items.filter((id) => marks[id] === "delete");
      const link = items.filter((id) => marks[id] === "link");
      if (del.length || link.length) list.push({ items, delete: del, link });
    }
    if (!list.length) return;
    busy = "apply";
    error = null;
    try {
      done = await api.dupApply(list);
      const deleted = list.flatMap((g) => g.delete);
      if (deleted.length) {
        broadcast("media", deleted);
        onchange?.();
      }
      // Look again: what was deleted is gone, what now shares its data shows so.
      if (openReport) {
        const r = openReport;
        await open(r);
      } else if (scope) {
        const sc = scope;
        const r = await api.dupScan(sc.sources, sc.across);
        groups = r.groups;
        stats = { checked: r.checked, read_bytes: r.read_bytes, elapsed_ms: r.elapsed_ms, failed: r.failed };
      }
      marks = {};
    } catch (e) {
      error = errText(e);
    } finally {
      busy = null;
    }
  }

  async function saveReport() {
    if (!groups || !scope) return;
    const entries: DupEntry[] = [];
    groups.forEach((g, grp) => {
      for (const it of g.items) {
        entries.push({
          grp,
          media_id: it.action === "gone" ? null : it.media_id,
          source_id: it.source_id,
          rel_path: it.rel_path,
          size: g.size,
          sha256: g.sha256,
          action: it.action === "gone" ? "" : it.action,
        });
      }
    });
    try {
      await api.dupSaveReport("", scope, entries);
      savedNote = t("dup.saved");
      setTimeout(() => (savedNote = null), 2500);
    } catch (e) {
      error = errText(e);
    }
  }

  async function rename() {
    if (!renaming) return;
    const r = renaming;
    renaming = null;
    await api.dupRenameReport(r.id, r.label).catch((e) => (error = errText(e)));
    loadReports();
  }

  async function remove(r: DupReportRow) {
    if (!confirm(t("dup.deleteReportConfirm"))) return;
    await api.dupDeleteReport(r.id).catch((e) => (error = errText(e)));
    loadReports();
  }

  function cancelScan() {
    if (scanTask) api.cancelTask(scanTask.id);
  }

  const reportName = (r: DupReportRow) => r.label || fmtDate(r.created_ms);
  const scopeText = (sc: DupScope) =>
    `${sc.sources.map(nameOf).join(", ")} · ${sc.across ? t("dup.across") : t("dup.within")}`;
  const fileName = (p: string) => p.split("/").pop() ?? p;
  const folderOf = (p: string) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");
</script>

<div out:fade class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && !busy && onclose()}>
  <div out:pop class="dialog dedup" role="dialog" aria-modal="true" aria-labelledby="dedup-title">
    <header>
      {#if groups}
        <button class="icon-btn" disabled={!!busy} onclick={back} aria-label={t("common.back")}><Icon name="chevronLeft" size={16} /></button>
      {/if}
      <h2 id="dedup-title">{openReport ? reportName(openReport) : t("dup.title")}</h2>
      <button class="icon-btn" disabled={!!busy} onclick={onclose} aria-label={t("common.close")}><Icon name="x" size={15} /></button>
    </header>

    {#if !groups}
      <div class="body form">
        <p class="hint">{t("dup.intro")}</p>
        <div class="card">
          <span class="eyebrow">{t("dup.sources")}</span>
          <div class="sources">
            {#each usable as s (s.id)}
              <label class="pick">
                <input
                  type="checkbox"
                  checked={picked.includes(s.id)}
                  onchange={(e) => (picked = e.currentTarget.checked ? [...picked, s.id] : picked.filter((x) => x !== s.id))}
                />
                <Icon name={s.driver === "shard" ? "archive" : s.driver === "local" || s.driver === "bundle" ? "folder" : "server"} size={14} />
                <span>{s.name}</span>
              </label>
            {/each}
          </div>
          <div class="line">
            <button class="btn sm ghost" onclick={() => (picked = usable.map((s) => s.id))}>{t("dup.all")}</button>
            <button class="btn sm ghost" onclick={() => (picked = [])}>{t("dup.none")}</button>
          </div>
          <div class="seg">
            <button class:on={!across} onclick={() => (across = false)}>{t("dup.within")}</button>
            <button class:on={across} onclick={() => (across = true)}>{t("dup.across")}</button>
          </div>
          <p class="hint">{across ? t("dup.acrossHint") : t("dup.withinHint")}</p>
        </div>

        {#if busy === "scan"}
          <div class="card">
            <div class="line">
              <span class="grow">{t("dup.scanning")}{scanTask?.detail ? ` · ${scanTask.detail}` : ""}</span>
              <button class="btn sm ghost" onclick={cancelScan}>{t("common.cancel")}</button>
            </div>
            {@render bar(scanTask ? fraction(scanTask) : null)}
            {#if scanTask?.total}<p class="hint">{fmtBytes(scanTask.done)} / {fmtBytes(scanTask.total)}</p>{/if}
          </div>
        {/if}

        {#if reports.length}
          <div class="card">
            <span class="eyebrow">{t("dup.reports")}</span>
            {#each reports as r (r.id)}
              <div class="line report">
                {#if renaming?.id === r.id}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input
                    class="input grow"
                    autofocus
                    bind:value={renaming.label}
                    placeholder={fmtDate(r.created_ms)}
                    onkeydown={(e) => {
                      if (e.key === "Enter") rename();
                      if (e.key === "Escape") renaming = null;
                    }}
                    onblur={rename}
                  />
                {:else}
                  <button class="link grow" onclick={() => open(r)}>
                    <b>{reportName(r)}</b>
                    <small>{t("dup.reportLine", { groups: fmtCount(r.groups), files: fmtCount(r.files), wasted: fmtBytes(r.wasted) })} · {scopeText(r.scope)}</small>
                  </button>
                  <button class="icon-btn sm" title={t("common.edit")} onclick={() => (renaming = { id: r.id, label: r.label })}><Icon name="pencil" size={13} /></button>
                  <button class="icon-btn sm" title={t("common.remove")} onclick={() => remove(r)}><Icon name="trash" size={13} /></button>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
        {#if error}<p class="msg bad">{error}</p>{/if}
      </div>
      <footer>
        <span class="grow"></span>
        <button class="btn primary" disabled={!picked.length || !!busy} onclick={scan}><Icon name="search" size={14} />{t("dup.start")}</button>
      </footer>
    {:else}
      <div class="body results">
        <div class="sum">
          <span>{t("dup.found", { groups: fmtCount(groups.length), files: fmtCount(totals.files), wasted: fmtBytes(totals.wasted) })}</span>
          {#if stats}
            <small>{t("dup.stats", { checked: fmtCount(stats.checked), read: fmtBytes(stats.read_bytes), secs: (stats.elapsed_ms / 1000).toFixed(1) })}</small>
          {/if}
          {#if scope}<small>{scopeText(scope)}</small>{/if}
        </div>
        {#if stats?.failed.length}
          <details class="failed">
            <summary>{t("dup.failed", { n: stats.failed.length })}</summary>
            {#each stats.failed.slice(0, 50) as f}<div>{f}</div>{/each}
          </details>
        {/if}

        {#if groups.length}
          <div class="bulk">
            <Select
              value={basis}
              label={t("dup.basis")}
              options={[
                { value: "added", label: t("dup.basisAdded") },
                { value: "modified", label: t("dup.basisModified") },
                { value: "created", label: t("dup.basisCreated") },
              ]}
              onchange={(v) => (basis = v as Basis)}
            />
            <button class="btn sm" onclick={() => keepOnly("oldest")}>{t("dup.keepOldest")}</button>
            <button class="btn sm" onclick={() => keepOnly("newest")}>{t("dup.keepNewest")}</button>
            {#if groups.some((g) => g.items.some((it) => canLink(g, it)))}
              <button class="btn sm" onclick={shardsSaveSpace}><Icon name="layers" size={13} />{t("dup.shardsLink")}</button>
            {/if}
            <button class="btn sm ghost" onclick={() => (marks = {})}>{t("dup.clear")}</button>
          </div>
        {:else}
          <p class="empty">{t("dup.none_found")}</p>
        {/if}

        {#each groups.slice(0, shown) as g (g.sha256 + g.items[0]?.media_id)}
          {@const items = g.items}
          {@const allGone = items.filter(live).length > 0 && items.filter(live).every((x) => marks[x.media_id] === "delete")}
          <section class="group" class:warn={allGone}>
            <div class="ghead">
              <b>{fmtBytes(g.size)}</b>
              <span>× {items.length}</span>
              <span class="faint">{g.sha256.slice(0, 12)}</span>
              {#if allGone}<span class="badge bad"><Icon name="alert" size={12} />{t("dup.allDeleted")}</span>{/if}
            </div>
            {#each items as it (it.media_id + it.rel_path)}
              {@const m = marks[it.media_id]}
              <div class="row" class:del={m === "delete"} class:off={!live(it)}>
                <img class="thumb" src={live(it) ? thumbUrl(it.media_id) : ""} alt="" loading="lazy" />
                <div class="what">
                  <div class="name" title={it.rel_path}>{fileName(it.rel_path)}</div>
                  <div class="where">
                    <Icon name={isShard(it.source_id) ? "archive" : "folder"} size={11} />{nameOf(it.source_id)}{folderOf(it.rel_path) ? ` / ${folderOf(it.rel_path)}` : ""}
                  </div>
                  <div class="when">
                    {fmtDate(dateOf(it) === Number.MAX_SAFE_INTEGER ? null : dateOf(it))}
                    {#if shared(g, it)}<span class="badge"><Icon name="layers" size={11} />{t("dup.sharing")}</span>{/if}
                    {#if it.action === "deleted"}<span class="badge">{t("dup.wasDeleted")}</span>{/if}
                    {#if it.action === "linked"}<span class="badge">{t("dup.wasLinked")}</span>{/if}
                    {#if it.action === "gone"}<span class="badge">{t("dup.gone")}</span>{/if}
                  </div>
                </div>
                {#if live(it)}
                  <div class="marks">
                    {#if canLink(g, it)}
                      <label class="mark" title={t("dup.linkHint")}>
                        <input type="checkbox" checked={m === "link"} disabled={shared(g, it) && m !== "link"} onchange={(e) => setMark(it, "link", e.currentTarget.checked)} />
                        <span>{t("dup.link")}</span>
                      </label>
                    {/if}
                    <label class="mark danger">
                      <input type="checkbox" checked={m === "delete"} onchange={(e) => setMark(it, "delete", e.currentTarget.checked)} />
                      <span>{t("dup.delete")}</span>
                    </label>
                  </div>
                {/if}
              </div>
            {/each}
          </section>
        {/each}
        {#if groups.length > shown}
          <button class="btn sm block" onclick={() => (shown += 300)}>{t("dup.more", { n: fmtCount(groups.length - shown) })}</button>
        {/if}

        {#if done}
          <p class="msg ok">
            {t("dup.done", { deleted: fmtCount(done.deleted), linked: fmtCount(done.linked), freed: fmtBytes(done.freed) })}
          </p>
          {#if done.failed.length}<p class="msg bad">{done.failed.slice(0, 5).join("\n")}</p>{/if}
        {/if}
        {#if error}<p class="msg bad">{error}</p>{/if}
      </div>

      <footer>
        {#if busy === "apply"}
          <div class="applying">
            <span>{t("dup.applying")}{applyTask?.detail ? ` · ${applyTask.detail}` : ""}</span>
            {@render bar(applyTask ? fraction(applyTask) : null)}
          </div>
        {:else if confirming}
          <span class="grow confirm">
            {t("dup.confirm", { deleted: fmtCount(summary.del), linked: fmtCount(summary.link) })}
            {#if summary.emptied}<b class="danger"> {t("dup.confirmEmptied", { n: fmtCount(summary.emptied) })}</b>{/if}
          </span>
          <button class="btn sm ghost" onclick={() => (confirming = false)}>{t("common.cancel")}</button>
          <button class="btn sm danger" onclick={apply}>{t("dup.apply")}</button>
        {:else}
          <span class="grow pick-sum">
            {t("dup.selection", { deleted: fmtCount(summary.del), linked: fmtCount(summary.link), freed: fmtBytes(summary.freed) })}
            {#if savedNote}<span class="faint"> · {savedNote}</span>{/if}
          </span>
          <button class="btn sm" disabled={!!busy || !groups.length} onclick={saveReport}><Icon name="save" size={13} />{t("dup.save")}</button>
          <button class="btn sm primary" disabled={!!busy || summary.del + summary.link === 0} onclick={() => (confirming = true)}>
            <Icon name="check" size={13} />{t("dup.apply")}
          </button>
        {/if}
      </footer>
    {/if}
  </div>
</div>

{#snippet bar(f: number | null)}
  <div class="bar" class:indeterminate={f == null}><span style:width={f == null ? "30%" : `${(f * 100).toFixed(1)}%`}></span></div>
{/snippet}

<style>
  .dedup {
    width: min(860px, 100%);
    height: min(820px, 100%);
  }
  .dedup > .body {
    flex: 1;
  }
  .form {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    align-content: start;
    gap: 12px;
  }
  .card {
    display: grid;
    gap: 8px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    background: var(--surface-2);
  }
  .sources {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
    gap: 4px 12px;
  }
  .pick {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    cursor: pointer;
  }
  .pick span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  input[type="checkbox"] {
    accent-color: var(--accent);
    width: 15px;
    height: 15px;
    margin: 0;
    flex-shrink: 0;
  }
  .line {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .report .link {
    display: grid;
    text-align: left;
    gap: 2px;
    padding: 6px 8px;
    border-radius: var(--radius-sm);
    background: none;
    border: 0;
    color: inherit;
    cursor: pointer;
  }
  .report .link:hover {
    background: var(--hover);
  }
  .report small,
  .sum small {
    color: var(--faint);
    font-size: 12px;
  }
  .hint {
    font-size: 12px;
    color: var(--faint);
    margin: 0;
    line-height: 1.5;
  }
  .results {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  /* A scrolling flex column would squeeze its children to fit (groups clip their rows). */
  .results > * {
    flex-shrink: 0;
  }
  .sum {
    display: grid;
    gap: 2px;
  }
  .failed {
    font-size: 12px;
    color: var(--muted);
  }
  .bulk {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    position: sticky;
    top: -18px;
    z-index: 1;
    margin: 0 -20px;
    padding: 8px 20px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }
  .bulk :global(.select) {
    width: 150px;
  }
  .group {
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    overflow: hidden;
  }
  .group.warn {
    border-color: color-mix(in srgb, var(--danger) 60%, var(--border));
  }
  .ghead {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 10px;
    font-size: 12px;
    background: var(--surface-2);
  }
  .faint {
    color: var(--faint);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    border-top: 1px solid var(--border);
    min-width: 0;
  }
  .row.del {
    background: color-mix(in srgb, var(--danger) 8%, transparent);
  }
  .row.del .name {
    text-decoration: line-through;
  }
  .row.off {
    opacity: 0.55;
  }
  .thumb {
    width: 44px;
    height: 44px;
    border-radius: var(--radius-sm);
    object-fit: cover;
    background: var(--surface-2);
    flex-shrink: 0;
  }
  .what {
    flex: 1;
    min-width: 0;
    display: grid;
    gap: 1px;
  }
  .name,
  .where {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .where,
  .when {
    font-size: 11px;
    color: var(--muted);
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .marks {
    display: flex;
    gap: 12px;
    flex-shrink: 0;
  }
  .mark {
    display: flex;
    align-items: center;
    gap: 5px;
    font-size: 12px;
    cursor: pointer;
    white-space: nowrap;
  }
  .mark.danger input {
    accent-color: var(--danger);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px;
    border-radius: 999px;
    font-size: 11px;
    background: var(--surface-3, var(--surface-2));
    color: var(--muted);
  }
  .badge.bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .empty {
    color: var(--faint);
    text-align: center;
    padding: 24px 0;
  }
  .pick-sum,
  .confirm {
    font-size: 12px;
    color: var(--muted);
  }
  .danger {
    color: var(--danger);
  }
  .applying {
    flex: 1;
    display: grid;
    gap: 6px;
    font-size: 12px;
  }
  .bar {
    height: 6px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    border-radius: inherit;
    transition: width 0.2s;
  }
  .bar.indeterminate span {
    animation: dup-slide 1.2s ease-in-out infinite;
  }
  @keyframes dup-slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  .msg {
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius-md);
    font-size: 12px;
    white-space: pre-line;
  }
  .bad {
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
  }
  .ok {
    color: var(--fg);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  @media (max-width: 560px) {
    .dedup {
      height: 100%;
    }
    .bulk {
      flex-wrap: nowrap;
      overflow-x: auto;
      scrollbar-width: none;
    }
    .bulk > :global(*) {
      flex-shrink: 0;
    }
    .bulk :global(.select) {
      width: 130px;
    }
    .row {
      flex-wrap: wrap;
    }
    .marks {
      width: 100%;
      justify-content: flex-end;
    }
    .pick-sum,
    .confirm {
      flex-basis: 100%;
    }
    .pick-sum + .btn,
    .confirm + .btn {
      margin-left: auto;
    }
  }
</style>
