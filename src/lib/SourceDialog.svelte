<script lang="ts">
  import { open } from "@tauri-apps/plugin-dialog";
  import { untrack } from "svelte";
  import { api, asCmdError, type DriverKind, type SourceRow } from "./api";
  import { i18n } from "./i18n.svelte";

  /** Adds a network source, or edits any source (`source` given). */
  let { source = null, onclose, ondone }: { source?: SourceRow | null; onclose: () => void; ondone: (s: SourceRow) => void } =
    $props();

  type L = { ko: string; en: string };
  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);
  const fieldLabels: Record<string, L> = {
    host: { ko: "서버 주소", en: "Server" },
    port: { ko: "포트", en: "Port" },
    share: { ko: "공유 이름", en: "Share" },
    username: { ko: "사용자", en: "User" },
    password: { ko: "비밀번호", en: "Password" },
    domain: { ko: "도메인", en: "Domain" },
    timeout_secs: { ko: "시간 제한 (초)", en: "Timeout (s)" },
  };
  const driverLabels: Record<string, L> = { smb: { ko: "SMB (Windows 공유, NAS)", en: "SMB (Windows share, NAS)" } };

  // The dialog edits a snapshot of the source it was opened with.
  const initial = untrack(() => source);
  let kinds = $state<DriverKind[]>([]);
  let driver = $state(initial?.driver ?? "smb");
  let name = $state(initial?.name ?? "");
  let root = $state(initial?.root ?? "");
  let enabled = $state(initial?.enabled ?? true);
  let config = $state<Record<string, any>>({});
  let busy = $state(false);
  let message = $state<{ ok: boolean; text: string } | null>(null);

  const isLocal = $derived(driver === "local" || driver === "bundle");
  const fields = $derived(kinds.find((k) => k.kind === driver)?.fields ?? []);

  api.driverKinds().then((k) => {
    kinds = k;
    if (!initial) {
      for (const f of k.find((x) => x.kind === driver)?.fields ?? []) if (f.default != null) config[f.key] = f.default;
    }
  });
  if (initial && initial.driver !== "local" && initial.driver !== "bundle") {
    api.sourceConfig(initial.id).then((c) => (config = c));
  }

  function values() {
    const out: Record<string, any> = {};
    for (const f of fields) {
      const v = config[f.key];
      if (v === undefined || v === "") continue;
      out[f.key] = f.kind.type === "integer" ? Number(v) : v;
    }
    return out;
  }

  async function test() {
    busy = true;
    message = null;
    try {
      const n = await api.testSource(driver, root, values());
      message = { ok: true, text: lbl({ ko: `연결됨 · 항목 ${n}개`, en: `Connected · ${n} entries` }) };
    } catch (e) {
      message = { ok: false, text: asCmdError(e).message };
    } finally {
      busy = false;
    }
  }

  async function save() {
    busy = true;
    message = null;
    try {
      const s = source
        ? await api.updateSource(source.id, name, root, isLocal ? null : values(), enabled)
        : await api.addSource(driver, name, root, values());
      ondone(s);
    } catch (e) {
      message = { ok: false, text: asCmdError(e).message };
    } finally {
      busy = false;
    }
  }

  async function pickFolder() {
    const p = await open({ directory: true });
    if (typeof p === "string") root = p;
  }
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="panel" role="dialog" aria-modal="true">
    <h2>{source ? lbl({ ko: "소스 편집", en: "Edit source" }) : lbl({ ko: "네트워크 소스 추가", en: "Add network source" })}</h2>

    {#if !source}
      <label>
        <span>{lbl({ ko: "종류", en: "Type" })}</span>
        <select bind:value={driver}>
          {#each kinds as k (k.kind)}<option value={k.kind}>{driverLabels[k.kind] ? lbl(driverLabels[k.kind]) : k.kind}</option>{/each}
        </select>
      </label>
    {/if}

    <label>
      <span>{lbl({ ko: "표시 이름", en: "Name" })}</span>
      <input bind:value={name} placeholder={lbl({ ko: "비우면 자동", en: "Automatic if empty" })} />
    </label>

    {#if !isLocal}
      {#each fields as f (f.key)}
        <label>
          <span>{fieldLabels[f.key] ? lbl(fieldLabels[f.key]) : f.key}{f.required ? " *" : ""}</span>
          {#if f.kind.type === "secret"}
            <input type="password" autocomplete="off" bind:value={config[f.key]} />
          {:else if f.kind.type === "integer"}
            <input type="number" min={f.kind.min} max={f.kind.max} bind:value={config[f.key]} />
          {:else if f.kind.type === "bool"}
            <input type="checkbox" bind:checked={config[f.key]} />
          {:else}
            <input bind:value={config[f.key]} />
          {/if}
        </label>
      {/each}
      <label>
        <span>{lbl({ ko: "공유 안 폴더 (선택)", en: "Folder in share (optional)" })}</span>
        <input bind:value={root} placeholder="photos/2026" />
      </label>
    {:else}
      <label>
        <span>{lbl({ ko: "폴더", en: "Folder" })}</span>
        <span class="row">
          <input bind:value={root} readonly={driver === "bundle"} />
          {#if driver === "local"}<button onclick={pickFolder}>{lbl({ ko: "찾아보기", en: "Browse" })}</button>{/if}
        </span>
      </label>
      <p class="hint">
        {lbl({
          ko: "다른 컴퓨터에서 번들을 열어 경로가 바뀌었다면 새 위치를 지정하세요. 태그와 통계는 그대로 유지됩니다.",
          en: "If the bundle was opened on another computer and the path changed, point it at the new location. Tags and stats are kept.",
        })}
      </p>
    {/if}

    {#if source}
      <label class="check">
        <input type="checkbox" bind:checked={enabled} />
        <span>{lbl({ ko: "사용", en: "Enabled" })}</span>
      </label>
    {/if}

    {#if !isLocal}
      <p class="hint">
        {lbl({
          ko: "비밀번호는 번들이 아닌 이 기기의 자격 증명 저장소에 보관됩니다.",
          en: "Passwords are kept in this device's credential store, not in the bundle.",
        })}
      </p>
    {/if}

    {#if message}<p class:ok={message.ok} class:bad={!message.ok}>{message.text}</p>{/if}

    <footer>
      {#if !isLocal}<button disabled={busy} onclick={test}>{lbl({ ko: "연결 테스트", en: "Test connection" })}</button>{/if}
      <span class="grow"></span>
      <button onclick={onclose}>{lbl({ ko: "취소", en: "Cancel" })}</button>
      <button class="primary" disabled={busy} onclick={save}>{source ? lbl({ ko: "저장", en: "Save" }) : lbl({ ko: "추가", en: "Add" })}</button>
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: #0009;
    display: grid;
    place-items: center;
    z-index: 20;
    padding: 16px;
  }
  .panel {
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    width: min(460px, 100%);
    max-height: 100%;
    overflow-y: auto;
    padding: 16px;
    display: grid;
    gap: 10px;
    box-sizing: border-box;
  }
  h2 {
    margin: 0;
    font-size: 16px;
  }
  label {
    display: grid;
    gap: 3px;
  }
  label > span:first-child {
    font-size: 12px;
    color: var(--muted);
  }
  label.check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  input:not([type="checkbox"]),
  select {
    font: inherit;
    color: inherit;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 6px 8px;
    min-width: 0;
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row input {
    flex: 1;
  }
  .hint {
    font-size: 12px;
    color: var(--muted);
    margin: 0;
  }
  .ok {
    color: var(--accent);
    margin: 0;
  }
  .bad {
    color: var(--danger);
    margin: 0;
    overflow-wrap: anywhere;
  }
  footer {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
</style>
