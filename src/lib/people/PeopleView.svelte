<script lang="ts">
  import { onDestroy } from "svelte";
  import { api, asCmdError, faceUrl, type FaceKind, type FaceRow, type MlStatus, type PersonRow } from "../api";
  import { onBack } from "../back.svelte";
  import { t } from "../i18n.svelte";
  import { quoteParam } from "../query";
  import { onSync } from "../sync";
  import { tasks } from "../tasks.svelte";
  import ContextMenu, { type MenuItem } from "../ui/ContextMenu.svelte";
  import Icon from "../ui/Icon.svelte";
  import ModelStore from "./ModelStore.svelte";

  /**
   * People and characters found by image analysis (`ml.rs`): groups to name, merge and
   * correct. Names become values of the built-in people / characters fields, so "show
   * photos" is just a search.
   */
  let {
    onshow,
    onopen,
    onreveal,
    onclose,
    hidden = false,
  }: {
    /** Kept open but not drawn (while the viewer shows one of its images). */
    hidden?: boolean;
    onshow: (query: string) => void;
    /** Opens the viewer at `id` within `list`. */
    onopen: (id: number, list: number[]) => void;
    /** Shows `id` in its folder in the library. */
    onreveal: (id: number) => void;
    onclose: () => void;
  } = $props();

  /** A face's menu (right click): open its image, find it in the library, or move it. */
  let menu = $state<{ x: number; y: number; face: FaceRow } | null>(null);
  // The menu closes before an item acts, so each item keeps its own face.
  const menuItems = $derived.by<MenuItem[]>(() => {
    const face = menu?.face;
    if (!face) return [];
    return [
      { label: t("people.openImage"), icon: "image", action: () => onopen(face.media_id, [...new Set(faces.map((x) => x.media_id))]) },
      { label: t("people.revealImage"), icon: "folder", action: () => onreveal(face.media_id) },
      {
        label: t("people.moveTo"),
        icon: "users",
        action: () => {
          moving = face;
          moveFilter = "";
        },
      },
      { label: t("people.notThis"), icon: "x", danger: true, action: () => notThis(face) },
    ];
  });

  /** "Move to another person": the face being moved, and the picker's filter / new name. */
  let moving = $state<FaceRow | null>(null);
  let moveFilter = $state("");
  const moveTargets = $derived.by(() => {
    const q = moveFilter.trim().toLowerCase();
    return people.filter((p) => p.id !== open?.id && (!q || (p.name || t("people.unnamed")).toLowerCase().includes(q)));
  });
  $effect(() => {
    if (moving) return onBack(() => (moving = null));
  });

  async function moveTo(person: number | null) {
    const face = moving;
    if (!face) return;
    moving = null;
    await run(async () => {
      if (person == null) await api.mlNewPerson(kind, moveFilter.trim(), [face.id]);
      else await api.mlAssignFaces([face.id], person);
    });
    faces = faces.filter((f) => f.id !== face.id);
    refreshPeople();
  }

  const PAGE = 200;

  let kind = $state<FaceKind>("real");
  let status = $state<MlStatus | null>(null);
  let people = $state<PersonRow[]>([]);
  let picked = $state<Set<number>>(new Set());
  let open = $state<PersonRow | null>(null);
  let faces = $state<FaceRow[]>([]);
  let more = $state(false);
  let name = $state("");
  let error = $state("");

  $effect(() => {
    if (!store && !hidden) return onBack(() => (open ? (open = null) : onclose()));
  });

  async function refreshStatus() {
    status = await api.mlStatus().catch(() => null);
  }
  async function refreshPeople() {
    people = await api.mlPeople(kind).catch(() => []);
    picked = new Set([...picked].filter((id) => people.some((p) => p.id === id)));
    if (open) {
      const again = people.find((p) => p.id === open!.id);
      if (again) open = again;
      else open = null;
    }
  }
  $effect(() => {
    kind;
    open = null;
    picked = new Set();
    refreshPeople();
  });
  refreshStatus();

  // Results arrive while a job runs and when it ends.
  const analyzing = $derived(tasks.list.some((x) => x.kind === "analyze" && x.state === "running"));
  let wasAnalyzing = false;
  $effect(() => {
    const now = analyzing;
    if (wasAnalyzing && !now) {
      refreshStatus();
      refreshPeople();
    }
    wasAnalyzing = now;
  });
  const unlisten = [onSync("people", () => refreshPeople())];
  onDestroy(() => unlisten.forEach((p) => p.then((f) => f())));

  async function loadFaces(reset: boolean) {
    if (!open) return;
    const id = open.id;
    const page = await api.mlPersonFaces(id, reset ? 0 : faces.length, PAGE).catch(() => []);
    if (open?.id !== id) return;
    faces = reset ? page : [...faces, ...page];
    more = page.length === PAGE;
  }
  $effect(() => {
    const p = open;
    name = p?.name ?? "";
    error = "";
    if (p) loadFaces(true);
    else faces = [];
  });

  async function run(f: () => Promise<unknown>) {
    error = "";
    try {
      await f();
    } catch (e) {
      error = asCmdError(e).message;
    }
  }

  const analyze = () => run(() => api.mlAnalyze()).then(refreshStatus);
  const retry = () => run(() => api.mlRetryFailed()).then(refreshStatus);
  let store = $state(false);
  const regroup = () => run(() => api.mlRegroup()).then(refreshPeople);

  async function saveName() {
    if (!open) return;
    const p = open;
    await run(async () => {
      const id = await api.mlRenamePerson(p.id, name);
      await refreshPeople();
      open = people.find((x) => x.id === id) ?? null;
    });
  }

  async function notThis(face: FaceRow) {
    await run(() => api.mlAssignFaces([face.id], null));
    faces = faces.filter((f) => f.id !== face.id);
    refreshPeople();
  }

  async function merge() {
    const ids = [...picked];
    if (ids.length < 2) return;
    // A named person keeps the name; otherwise the largest group takes the rest.
    const chosen = people.filter((p) => ids.includes(p.id));
    const into = (chosen.find((p) => p.name) ?? chosen[0]).id;
    await run(() => api.mlMergePeople(into, ids.filter((x) => x !== into)));
    picked = new Set();
    refreshPeople();
  }

  async function hide(p: PersonRow) {
    await run(() => api.mlHidePerson(p.id, true));
    open = null;
    refreshPeople();
  }

  function toggle(id: number) {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    picked = next;
  }

  function show(p: PersonRow) {
    if (!p.name) return;
    onshow(`${t(kind === "real" ? "people.field" : "people.charField")}(${quoteParam(p.name)})`);
  }

  const runtimeOk = $derived(!!status?.supported && !!status.runtime);
  const hasPacks = $derived(
    !!status?.packs.some((p) => (kind === "real" ? p.kind === "face_detector" : p.kind === "anime_detector")),
  );
</script>

<div class="scrim" role="presentation" style:display={hidden ? "none" : null} onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="dialog people" role="dialog" aria-modal="true" aria-labelledby="people-title">
    <header>
      {#if open}
        <button class="icon-btn" aria-label={t("common.back")} onclick={() => (open = null)}><Icon name="chevronLeft" size={16} /></button>
      {/if}
      <h2 id="people-title">{open ? open.name || t("people.unnamed") : t("people.title")}</h2>
      {#if !open}
        <div class="seg sm">
          <button class:on={kind === "real"} onclick={() => (kind = "real")}>{t("people.real")}</button>
          <button class:on={kind === "anime"} onclick={() => (kind = "anime")}>{t("people.anime")}</button>
        </div>
      {/if}
      <button class="icon-btn" aria-label={t("common.close")} onclick={onclose}><Icon name="x" size={16} /></button>
    </header>

    {#if !open}
      <div class="status">
        {#if !status}
          <span class="spinner"></span>
        {:else if !status.supported}
          <span class="note">{t("people.unsupported")}</span>
        {:else if !status.runtime}
          <span class="note warn" title={status.runtime_error ?? ""}><Icon name="alert" size={13} />{t("people.noRuntime")}</span>
        {:else if !hasPacks}
          <span class="note warn" title={status.model_dirs.join("\n")}><Icon name="alert" size={13} />{t("people.noPacks")}</span>
        {:else if status.counts}
          <span class="note tabular">
            {t("people.counts", {
              done: status.counts.done.toLocaleString(),
              images: status.counts.images.toLocaleString(),
              faces: status.counts.faces.toLocaleString(),
            })}
          </span>
        {/if}
        {#if status?.counts?.failed}
          <button class="btn sm ghost" disabled={analyzing} onclick={retry} title={t("people.retryHint")}>
            <Icon name="alert" size={13} />{t("people.failed", { n: status.counts.failed.toLocaleString() })}
          </button>
        {/if}
        <span class="grow"></span>
        {#if status?.supported}
          <button class="btn sm ghost" onclick={() => (store = true)}><Icon name="download" size={13} />{t("people.getModels")}</button>
        {/if}
        {#if picked.size >= 2}
          <button class="btn sm" onclick={merge}><Icon name="layers" size={13} />{t("people.merge", { n: picked.size })}</button>
        {/if}
        <button class="btn sm ghost" onclick={regroup}><Icon name="refresh" size={13} />{t("people.regroup")}</button>
        <button class="btn sm primary" disabled={!runtimeOk || analyzing} onclick={analyze}>
          {#if analyzing}<span class="spinner"></span>{t("people.analyzing")}{:else}<Icon name="scan" size={13} />{t("people.analyze")}{/if}
        </button>
      </div>
      {#if status?.pack_errors.length}
        <p class="err small">{status.pack_errors.join(" · ")}</p>
      {/if}
      {#if error}<p class="err small">{error}</p>{/if}
      <div class="body grid">
        {#each people as p (p.id)}
          <div class="card" class:picked={picked.has(p.id)}>
            <button class="face" onclick={() => (open = p)}>
              {#if p.cover != null}
                <img src={faceUrl(p.cover)} alt="" loading="lazy" draggable="false" />
              {:else}
                <Icon name="users" size={28} stroke={1.3} />
              {/if}
            </button>
            <button class="pick" aria-label={t("people.pick")} aria-pressed={picked.has(p.id)} onclick={() => toggle(p.id)}>
              <Icon name={picked.has(p.id) ? "checkSquare" : "plus"} size={13} />
            </button>
            <span class="pname" class:unnamed={!p.name}>{p.name || t("people.unnamed")}</span>
            <span class="pcount tabular">{t("people.items", { n: p.items.toLocaleString() })}</span>
          </div>
        {:else}
          <p class="empty">{status?.counts?.faces ? t("people.noGroups") : t("people.none")}</p>
        {/each}
      </div>
    {:else}
      <div class="status">
        <input
          class="input grow"
          placeholder={t("people.namePlaceholder")}
          bind:value={name}
          onkeydown={(e) => e.key === "Enter" && saveName()}
        />
        <button class="btn sm primary" disabled={name.trim() === open.name} onclick={saveName}><Icon name="check" size={13} />{t("common.save")}</button>
        <button class="btn sm" disabled={!open.name} onclick={() => open && show(open)}><Icon name="image" size={13} />{t("people.show")}</button>
        <button class="btn sm ghost" onclick={() => open && hide(open)}><Icon name="eyeOff" size={13} />{t("people.hide")}</button>
      </div>
      <p class="hint small">{t("people.nameHint")}</p>
      {#if error}<p class="err small">{error}</p>{/if}
      <div class="body grid faces">
        {#each faces as f (f.id)}
          <div
            class="card"
            role="presentation"
            oncontextmenu={(e) => {
              e.preventDefault();
              menu = { x: e.clientX, y: e.clientY, face: f };
            }}
          >
            <button class="face" title={t("people.faceHint")} onclick={(e) => (menu = { x: e.clientX, y: e.clientY, face: f })}
              ><img src={faceUrl(f.id)} alt="" loading="lazy" draggable="false" /></button
            >
            <button class="pick danger" title={t("people.notThis")} aria-label={t("people.notThis")} onclick={() => notThis(f)}>
              <Icon name="x" size={13} />
            </button>
          </div>
        {/each}
        {#if more}
          <button class="btn sm more" onclick={() => loadFaces(false)}>{t("people.more")}</button>
        {/if}
      </div>
    {/if}
  </div>
</div>

{#if store}
  <ModelStore onclose={() => (store = false)} onchanged={refreshStatus} />
{/if}

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menuItems} onclose={() => (menu = null)} />
{/if}

{#if moving}
  <div class="scrim" role="presentation" onclick={(e) => e.target === e.currentTarget && (moving = null)}>
    <div class="dialog move" role="dialog" aria-modal="true" aria-labelledby="move-title">
      <header>
        <span class="face mini"><img src={faceUrl(moving.id)} alt="" draggable="false" /></span>
        <h2 id="move-title">{t("people.moveTo")}</h2>
        <button class="icon-btn" aria-label={t("common.close")} onclick={() => (moving = null)}><Icon name="x" size={16} /></button>
      </header>
      <div class="move-body">
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="input"
          placeholder={t("people.moveFilter")}
          bind:value={moveFilter}
          autofocus
          onkeydown={(e) => {
            if (e.key !== "Enter" || e.isComposing) return;
            const exact = moveTargets.find((p) => p.name && p.name.toLowerCase() === moveFilter.trim().toLowerCase());
            moveTo(exact ? exact.id : moveTargets.length === 1 ? moveTargets[0].id : null);
          }}
        />
        <button class="target new" onclick={() => moveTo(null)}>
          <span class="face mini"><Icon name="plus" size={16} /></span>
          <span class="pname">{moveFilter.trim() ? t("people.moveNewNamed", { name: moveFilter.trim() }) : t("people.moveNew")}</span>
        </button>
        <div class="targets">
          {#each moveTargets as p (p.id)}
            <button class="target" onclick={() => moveTo(p.id)}>
              <span class="face mini">
                {#if p.cover != null}<img src={faceUrl(p.cover)} alt="" loading="lazy" draggable="false" />{:else}<Icon name="users" size={16} />{/if}
              </span>
              <span class="pname" class:unnamed={!p.name}>{p.name || t("people.unnamed")}</span>
              <span class="pcount tabular">{t("people.items", { n: p.items.toLocaleString() })}</span>
            </button>
          {/each}
        </div>
      </div>
    </div>
  </div>
{/if}

<style>
  .people {
    width: min(980px, 100%);
    height: min(760px, 100%);
  }
  header {
    gap: 10px;
  }
  .status {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
    padding: 10px 16px;
    border-bottom: 1px solid var(--border);
    flex-shrink: 0;
  }
  .note {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--muted);
    font-size: 12px;
  }
  .note.warn {
    color: var(--warn);
  }
  .err,
  .hint {
    margin: 6px 16px 0;
  }
  .err {
    color: var(--danger);
  }
  .hint {
    color: var(--faint);
  }
  .small {
    font-size: 12px;
  }
  .grid {
    flex: 1;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(120px, 1fr));
    align-content: start;
    gap: 14px;
  }
  .grid.faces {
    grid-template-columns: repeat(auto-fill, minmax(88px, 1fr));
    gap: 8px;
  }
  .card {
    position: relative;
    display: grid;
    gap: 4px;
    min-width: 0;
  }
  .face {
    display: grid;
    place-items: center;
    aspect-ratio: 1;
    width: 100%;
    padding: 0;
    border: 0;
    border-radius: var(--radius-md);
    overflow: hidden;
    background: var(--tile);
    color: var(--faint);
  }
  .face img {
    width: 100%;
    height: 100%;
    object-fit: cover;
  }
  .card.picked .face {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .pick {
    position: absolute;
    top: 6px;
    right: 6px;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border: 0;
    border-radius: 50%;
    background: rgb(0 0 0 / 0.55);
    color: #fff;
    opacity: 0;
    transition: opacity var(--t-fast) var(--ease);
  }
  .card:hover .pick,
  .card.picked .pick {
    opacity: 1;
  }
  .pick.danger:hover {
    background: var(--danger);
  }
  @media (hover: none) {
    .pick {
      opacity: 1;
    }
  }
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 500;
  }
  .pname.unnamed {
    color: var(--muted);
    font-weight: 400;
  }
  .pcount {
    font-size: 11px;
    color: var(--faint);
  }
  .empty {
    grid-column: 1 / -1;
    margin: 24px 0;
    text-align: center;
    color: var(--faint);
  }
  .more {
    grid-column: 1 / -1;
    justify-self: center;
  }
  .move {
    width: min(420px, 100%);
    max-height: min(620px, 100%);
  }
  .move-body {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 16px 16px;
    min-height: 0;
  }
  .targets {
    display: flex;
    flex-direction: column;
    gap: 2px;
    overflow-y: auto;
    min-height: 0;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--fg);
    text-align: left;
  }
  .target:hover,
  .target:focus-visible {
    background: var(--hover);
  }
  .target .pname {
    flex: 1;
    min-width: 0;
  }
  .target.new {
    color: var(--accent);
  }
  .face.mini {
    width: 36px;
    flex-shrink: 0;
  }
</style>
