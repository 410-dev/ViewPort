<script lang="ts">
  import { untrack } from "svelte";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { api } from "../api";
  import { t } from "../i18n.svelte";
  import Icon from "../ui/Icon.svelte";
  import { briefs, want } from "./briefs.svelte";
  import Cells from "./Cells.svelte";
  import HeaderRow from "./HeaderRow.svelte";
  import { arrowStep } from "./selection.svelte";
  import type { ViewProps } from "./types";

  /**
   * Folder tree with files inline, like a file manager's expandable details view.
   * Children are loaded when a folder is expanded; the current query filters the files.
   */
  let { selection, overscan, columns, rowPx, sort, tree, onsort, onopen, onopenwindow }: ViewProps = $props();

  type Children = { folders: { name: string; count: number }[]; files: number[] };
  type Row =
    | { kind: "node"; key: string; src: number; path: string; name: string; count: number | null; depth: number; source: boolean }
    | { kind: "file"; id: number; depth: number; index: number }
    | { kind: "loading"; key: string; depth: number };

  const keyOf = (src: number, path: string) => `${src}:${path}`;
  const children = new SvelteMap<string, Children>();
  const expanded = new SvelteSet<string>();
  const loading = new SvelteSet<string>();
  let generation = 0;

  async function load(src: number, path: string) {
    const key = keyOf(src, path);
    if (loading.has(key)) return;
    loading.add(key);
    const gen = generation;
    try {
      const [folders, found] = await Promise.all([
        api.childFolders(src, path),
        api.search(tree.query, src, path, false),
      ]);
      if (gen === generation) children.set(key, { folders, files: found.ids });
    } catch {
      if (gen === generation) children.set(key, { folders: [], files: [] });
    } finally {
      loading.delete(key);
    }
  }

  // A new scope or query reloads what is open.
  $effect(() => {
    const scope = { src: tree.sourceId, folder: tree.folder, query: tree.query, sources: tree.sources.length };
    untrack(() => {
      generation++;
      children.clear();
      loading.clear();
      if (scope.src !== null) load(scope.src, scope.folder);
      for (const key of expanded) {
        const i = key.indexOf(":");
        load(+key.slice(0, i), key.slice(i + 1));
      }
    });
  });

  function toggle(src: number, path: string) {
    const key = keyOf(src, path);
    if (expanded.has(key)) expanded.delete(key);
    else {
      expanded.add(key);
      if (!children.has(key)) load(src, path);
    }
  }

  const flat = $derived.by(() => {
    const rows: Row[] = [];
    const files: number[] = [];
    const walk = (src: number, path: string, depth: number) => {
      const key = keyOf(src, path);
      const c = children.get(key);
      if (!c) {
        rows.push({ kind: "loading", key, depth });
        return;
      }
      for (const f of c.folders) {
        const p = path ? `${path}/${f.name}` : f.name;
        const k = keyOf(src, p);
        rows.push({ kind: "node", key: k, src, path: p, name: f.name, count: f.count, depth, source: false });
        if (expanded.has(k)) walk(src, p, depth + 1);
      }
      for (const id of c.files) {
        rows.push({ kind: "file", id, depth, index: files.length });
        files.push(id);
      }
    };
    if (tree.sourceId === null) {
      for (const s of tree.sources) {
        const k = keyOf(s.id, "");
        rows.push({ kind: "node", key: k, src: s.id, path: "", name: s.name, count: null, depth: 0, source: true });
        if (expanded.has(k)) walk(s.id, "", 1);
      }
    } else {
      walk(tree.sourceId, tree.folder, 0);
    }
    return { rows, files };
  });

  const HEAD = 33;
  let el: HTMLDivElement;
  let height = $state(0);
  let scrollTop = $state(0);
  const over = $derived(overscan * 4);
  const start = $derived(Math.max(0, Math.floor(scrollTop / rowPx) - over));
  const end = $derived(Math.min(flat.rows.length, Math.ceil((scrollTop + height) / rowPx) + over));
  const visible = $derived(flat.rows.slice(start, end).map((r, i) => ({ r, slot: start + i })));

  $effect(() => want(visible.flatMap(({ r }) => (r.kind === "file" ? [r.id] : []))));

  function onKey(e: KeyboardEvent) {
    const ids = flat.files;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      return selection.selectAll(ids);
    }
    if (e.key === "Escape") return selection.clear();
    if (e.key === "Enter" && !e.ctrlKey && !e.metaKey && selection.focus != null) {
      e.preventDefault();
      return onopen(selection.focus, ids);
    }
    const step = arrowStep(e, 1, Math.max(1, Math.floor((height - HEAD) / rowPx)));
    if (!step) return;
    e.preventDefault();
    const i = selection.move(Number.isFinite(step) ? step : step > 0 ? ids.length : -ids.length, ids, e.shiftKey);
    if (i < 0) return;
    const slot = flat.rows.findIndex((r) => r.kind === "file" && r.index === i);
    const top = slot * rowPx;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + rowPx > el.scrollTop + height - HEAD) el.scrollTop = top + rowPx - (height - HEAD);
  }
</script>

<div
  class="tree"
  bind:this={el}
  bind:clientHeight={height}
  onscroll={() => (scrollTop = el.scrollTop)}
  onkeydown={onKey}
  role="treegrid"
  aria-multiselectable="true"
  tabindex="0"
>
  <HeaderRow {columns} {sort} {onsort} />
  <div class="spacer" style:height="{flat.rows.length * rowPx + 8}px">
    {#each visible as { r, slot } (r.kind === "file" ? r.id : r.key)}
      {#if r.kind === "node"}
        {@const open = expanded.has(r.key)}
        {@const ref = { sourceId: r.src, path: r.path, count: r.count ?? 0 }}
        <button
          class="row"
          class:selected={!r.source && selection.isFolder(r.src, r.path)}
          style:height="{rowPx}px"
          style:transform="translateY({slot * rowPx}px)"
          onpointerdown={(e) => selection.pointer(e)}
          onclick={() => {
            if (!r.source && !selection.touch) selection.selectFolder(ref);
            toggle(r.src, r.path);
          }}
          oncontextmenu={(e) => !r.source && selection.holdFolder(e, ref)}
          aria-expanded={open}
          tabindex="-1"
        >
          <Cells
            {columns}
            name={r.name}
            icon={r.source ? "server" : "folder"}
            indent={r.depth}
            extra={r.count == null ? "" : t("lib.items", { n: r.count.toLocaleString() })}
          >
            {#snippet lead()}
              <span class="chev" class:open>
                {#if loading.has(r.key)}<span class="spinner"></span>{:else}<Icon name="chevronRight" size={13} />{/if}
              </span>
            {/snippet}
          </Cells>
        </button>
      {:else if r.kind === "file"}
        <button
          class="row"
          class:selected={selection.set.has(r.id)}
          class:focus={selection.focus === r.id}
          style:height="{rowPx}px"
          style:transform="translateY({slot * rowPx}px)"
          onpointerdown={(e) => selection.pointer(e)}
          onclick={(e) => selection.activate(e, r.index, flat.files, onopen)}
          oncontextmenu={(e) => selection.hold(e, r.index, flat.files)}
          ondblclick={() => onopen(r.id, flat.files)}
          onauxclick={(e) => e.button === 1 && onopenwindow?.(r.id, flat.files)}
          onmousedown={(e) => e.button === 1 && e.preventDefault()}
          tabindex="-1"
        >
          <Cells {columns} id={r.id} brief={briefs.get(r.id) ?? null} indent={r.depth}>
            {#snippet lead()}<span class="chev"></span>{/snippet}
          </Cells>
        </button>
      {:else}
        <div class="row loading" style:height="{rowPx}px" style:transform="translateY({slot * rowPx}px)">
          <span class="pad" style:padding-left="{40 + r.depth * 18}px"><span class="spinner"></span></span>
        </div>
      {/if}
    {/each}
  </div>
  {#if flat.rows.length === 0}
    <p class="empty">{t("lib.emptyFolder")}</p>
  {/if}
</div>

<style>
  .tree {
    height: 100%;
    overflow: auto;
    outline: none;
    contain: strict;
  }
  .spacer {
    position: relative;
    margin: 4px 8px;
  }
  .row {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    text-align: left;
    will-change: transform;
    transition: background var(--t-fast) var(--ease);
  }
  .row:hover:not(.loading) {
    background: var(--surface-2);
  }
  .row.selected {
    background: var(--accent-soft);
  }
  .row.focus {
    box-shadow: inset 0 0 0 1px var(--accent-line);
  }
  .chev {
    width: 14px;
    margin-left: -6px;
    display: grid;
    place-items: center;
    color: var(--faint);
    transition: transform var(--t-fast) var(--ease);
    flex-shrink: 0;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .empty {
    color: var(--muted);
    text-align: center;
    margin-top: 60px;
  }
</style>
