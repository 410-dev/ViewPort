<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { api, type AppSettings, type CacheStats, type Settings } from "./api";
  import { i18n } from "./i18n.svelte";

  let { settings, onclose }: { settings: Settings; onclose: () => void } = $props();

  type L = { ko: string; en: string };
  type Field =
    | { kind: "number"; path: string; label: L; min?: number; max?: number; step?: number; scale?: number; unit?: string }
    | { kind: "bool"; path: string; label: L }
    | { kind: "select"; path: string; label: L; options: { value: string; label: L }[] }
    | { kind: "list"; path: string; label: L }
    | { kind: "text"; path: string; label: L };
  type Section = { title: L; note?: L; fields: Field[]; app?: boolean };

  const GB = 1024 ** 3;
  const MB = 1024 ** 2;

  const sections: Section[] = [
    {
      title: { ko: "일반", en: "General" },
      fields: [
        {
          kind: "select",
          path: "general.locale",
          label: { ko: "언어 / 쿼리 키워드", en: "Language / query keywords" },
          options: [
            { value: "ko", label: { ko: "한국어", en: "Korean" } },
            { value: "en", label: { ko: "영어", en: "English" } },
          ],
        },
      ],
    },
    {
      title: { ko: "메모리 캐시", en: "Memory cache" },
      note: { ko: "용량·선점 변경은 번들을 다시 열면 적용됩니다.", en: "Size and preallocation apply when the bundle is reopened." },
      fields: [
        { kind: "number", path: "cache.memory_bytes", label: { ko: "캐시 용량", en: "Cache size" }, min: 0.25, step: 0.25, scale: GB, unit: "GB" },
        { kind: "bool", path: "cache.preallocate", label: { ko: "시작 시 메모리 선점", en: "Reserve memory on open" } },
        { kind: "number", path: "cache.max_item_bytes", label: { ko: "캐시할 최대 파일 크기", en: "Largest file to cache" }, min: 1, step: 1, scale: MB, unit: "MB" },
        { kind: "number", path: "cache.autoload_top_n", label: { ko: "자주 보는 미디어 미리 읽기 (개)", en: "Preload most viewed (items)" }, min: 0, step: 1 },
        { kind: "number", path: "cache.autoload_ratio", label: { ko: "미리 읽기 상한 (%)", en: "Preload limit (%)" }, min: 0, max: 100, step: 5, scale: 0.01 },
        { kind: "number", path: "cache.prefetch_ahead", label: { ko: "앞으로 미리 읽기 (개)", en: "Prefetch ahead" }, min: 0, max: 20, step: 1 },
        { kind: "number", path: "cache.prefetch_behind", label: { ko: "뒤로 미리 읽기 (개)", en: "Prefetch behind" }, min: 0, max: 20, step: 1 },
      ],
    },
    {
      title: { ko: "재생", en: "Playback" },
      fields: [
        { kind: "number", path: "playback.seek_step_secs", label: { ko: "탐색 간격 (초)", en: "Seek step (s)" }, min: 1, step: 1 },
        { kind: "number", path: "playback.volume", label: { ko: "볼륨 (%)", en: "Volume (%)" }, min: 0, max: 100, step: 5, scale: 0.01 },
        { kind: "bool", path: "playback.muted", label: { ko: "음소거로 시작", en: "Start muted" } },
        {
          kind: "select",
          path: "playback.fallback_mode",
          label: { ko: "재생 불가 포맷 처리", en: "Unsupported formats" },
          options: [
            { value: "auto", label: { ko: "필요할 때만 변환", en: "Convert when needed" } },
            { value: "always", label: { ko: "항상 변환 경로 사용", en: "Always use conversion" } },
            { value: "never", label: { ko: "변환 안 함", en: "Never convert" } },
          ],
        },
        { kind: "bool", path: "playback.suggest_conversion", label: { ko: "고호환 포맷 변환 권장 알림", en: "Suggest compatible conversion" } },
        { kind: "text", path: "playback.ffmpeg_path", label: { ko: "FFmpeg 경로 (비우면 자동 탐색)", en: "FFmpeg path (empty = auto)" } },
      ],
    },
    {
      title: { ko: "슬라이드쇼", en: "Slideshow" },
      fields: [
        { kind: "number", path: "slideshow.image_interval_secs", label: { ko: "이미지 표시 시간 (초)", en: "Image duration (s)" }, min: 0.5, step: 0.5 },
        { kind: "bool", path: "slideshow.wrap_around", label: { ko: "끝나면 처음부터", en: "Loop at the end" } },
      ],
    },
    {
      title: { ko: "썸네일·그리드", en: "Thumbnails & grid" },
      fields: [
        { kind: "number", path: "thumbnails.max_edge_px", label: { ko: "썸네일 크기 (px)", en: "Thumbnail size (px)" }, min: 64, max: 1024, step: 32 },
        { kind: "number", path: "thumbnails.quality", label: { ko: "썸네일 품질", en: "Thumbnail quality" }, min: 1, max: 100, step: 1 },
        { kind: "bool", path: "grid.show_names", label: { ko: "파일 이름 표시", en: "Show file names" } },
        { kind: "text", path: "grid.default_sort", label: { ko: "기본 정렬 (쿼리 문법)", en: "Default sort (query syntax)" } },
      ],
    },
    {
      title: { ko: "스캔", en: "Scanning" },
      fields: [
        { kind: "list", path: "scan.image_extensions", label: { ko: "이미지 확장자", en: "Image extensions" } },
        { kind: "list", path: "scan.video_extensions", label: { ko: "영상 확장자", en: "Video extensions" } },
        { kind: "bool", path: "scan.skip_hidden", label: { ko: "숨김 파일 건너뛰기", en: "Skip hidden files" } },
        { kind: "bool", path: "scan.thumbnails_after_scan", label: { ko: "스캔 후 썸네일 생성", en: "Generate thumbnails after scan" } },
      ],
    },
    {
      title: { ko: "단축키", en: "Shortcuts" },
      note: { ko: "쉼표로 구분. 예: ArrowRight, d, Shift+ArrowRight", en: "Comma separated, e.g. ArrowRight, d, Shift+ArrowRight" },
      fields: [
        { kind: "list", path: "keys.next", label: { ko: "다음", en: "Next" } },
        { kind: "list", path: "keys.prev", label: { ko: "이전", en: "Previous" } },
        { kind: "list", path: "keys.seek_forward", label: { ko: "앞으로 탐색", en: "Seek forward" } },
        { kind: "list", path: "keys.seek_backward", label: { ko: "뒤로 탐색", en: "Seek backward" } },
        { kind: "list", path: "keys.toggle_play", label: { ko: "재생/일시정지", en: "Play/pause" } },
        { kind: "list", path: "keys.toggle_fullscreen", label: { ko: "전체 화면", en: "Fullscreen" } },
        { kind: "list", path: "keys.toggle_slideshow", label: { ko: "슬라이드쇼", en: "Slideshow" } },
        { kind: "list", path: "keys.close_viewer", label: { ko: "뷰어 닫기", en: "Close viewer" } },
      ],
    },
    {
      title: { ko: "이 기기", en: "This device" },
      note: { ko: "앱을 다시 시작하면 적용됩니다.", en: "Applies after restarting the app." },
      app: true,
      fields: [
        { kind: "bool", path: "reopen_last_bundle", label: { ko: "시작 시 마지막 번들 열기", en: "Reopen last bundle on start" } },
        { kind: "bool", path: "hardware.gpu", label: { ko: "GPU 렌더링", en: "GPU rendering" } },
        { kind: "bool", path: "hardware.video_decode", label: { ko: "영상 하드웨어 디코딩", en: "Hardware video decoding" } },
        { kind: "bool", path: "hardware.video_encode", label: { ko: "변환 시 하드웨어 인코딩", en: "Hardware encoding for conversion" } },
      ],
    },
  ];

  const lbl = (l: L) => (i18n.locale === "ko" ? l.ko : l.en);

  // Edits go to a copy; the open settings change only on save.
  let draft = $state<Settings>(untrack(() => structuredClone($state.snapshot(settings)) as Settings));
  let appDraft = $state<AppSettings | null>(null);
  api.getAppSettings().then((a) => (appDraft = a));

  function get(obj: any, path: string) {
    return path.split(".").reduce((o, k) => o?.[k], obj);
  }
  function set(obj: any, path: string, value: unknown) {
    const keys = path.split(".");
    const last = keys.pop()!;
    keys.reduce((o, k) => (o[k] ??= {}), obj)[last] = value;
  }
  const target = (s: Section) => (s.app ? appDraft : draft);

  let stats = $state<CacheStats | null>(null);
  const timer = setInterval(() => api.cacheStats().then((s) => (stats = s)).catch(() => {}), 1000);
  api.cacheStats().then((s) => (stats = s)).catch(() => {});
  onDestroy(() => clearInterval(timer));

  const fmt = (b: number) => (b >= GB ? `${(b / GB).toFixed(2)} GB` : `${(b / MB).toFixed(1)} MB`);

  let saving = $state(false);
  let error = $state("");
  async function save() {
    saving = true;
    error = "";
    try {
      await api.saveSettings($state.snapshot(draft) as Settings);
      if (appDraft) await api.saveAppSettings($state.snapshot(appDraft) as AppSettings);
      Object.assign(settings, $state.snapshot(draft));
      i18n.locale = draft.general.locale;
      onclose();
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      saving = false;
    }
  }
</script>

<div class="backdrop" role="presentation" onclick={(e) => e.target === e.currentTarget && onclose()}>
  <div class="panel" role="dialog" aria-modal="true" aria-label="Settings">
    <header>
      <h2>{lbl({ ko: "설정", en: "Settings" })}</h2>
      <button class="icon" onclick={onclose} aria-label="Close">✕</button>
    </header>

    <div class="body">
      {#if stats}
        <section class="stats">
          <h3>{lbl({ ko: "캐시 상태", en: "Cache status" })}</h3>
          <div class="bar">
            <div class="alloc" style:width="{(stats.allocated_bytes / Math.max(1, stats.capacity_bytes)) * 100}%"></div>
            <div class="used" style:width="{(stats.used_bytes / Math.max(1, stats.capacity_bytes)) * 100}%"></div>
          </div>
          <dl>
            <dt>{lbl({ ko: "용량", en: "Capacity" })}</dt>
            <dd>{fmt(stats.capacity_bytes)}</dd>
            <dt>{lbl({ ko: "선점됨", en: "Reserved" })}</dt>
            <dd>{fmt(stats.allocated_bytes)}</dd>
            <dt>{lbl({ ko: "사용 중", en: "In use" })}</dt>
            <dd>{fmt(stats.used_bytes)} · {stats.items} {lbl({ ko: "개", en: "items" })}</dd>
            <dt>{lbl({ ko: "적중률", en: "Hit rate" })}</dt>
            <dd>
              {stats.hits + stats.misses === 0 ? "–" : `${Math.round((stats.hits / (stats.hits + stats.misses)) * 100)}%`}
              ({stats.hits}/{stats.hits + stats.misses})
            </dd>
          </dl>
        </section>
      {/if}

      {#each sections as s (s.title.en)}
        {@const obj = target(s)}
        {#if obj}
          <section>
            <h3>{lbl(s.title)}</h3>
            {#if s.note}<p class="note">{lbl(s.note)}</p>{/if}
            {#each s.fields as f (f.path)}
              <label class="row" class:check={f.kind === "bool"}>
                <span>{lbl(f.label)}</span>
                {#if f.kind === "bool"}
                  <input type="checkbox" checked={!!get(obj, f.path)} onchange={(e) => set(obj, f.path, e.currentTarget.checked)} />
                {:else if f.kind === "number"}
                  <span class="num">
                    <input
                      type="number"
                      min={f.min}
                      max={f.max}
                      step={f.step}
                      value={+(get(obj, f.path) / (f.scale ?? 1)).toFixed(4)}
                      onchange={(e) => set(obj, f.path, Math.round(+e.currentTarget.value * (f.scale ?? 1) * 1e6) / 1e6)}
                    />
                    {#if f.unit}<span>{f.unit}</span>{/if}
                  </span>
                {:else if f.kind === "select"}
                  <select value={get(obj, f.path)} onchange={(e) => set(obj, f.path, e.currentTarget.value)}>
                    {#each f.options as o (o.value)}<option value={o.value}>{lbl(o.label)}</option>{/each}
                  </select>
                {:else if f.kind === "list"}
                  <input
                    value={(get(obj, f.path) ?? []).join(", ")}
                    onchange={(e) =>
                      set(
                        obj,
                        f.path,
                        e.currentTarget.value.split(",").map((x) => x.trim()).filter(Boolean),
                      )}
                  />
                {:else}
                  <input value={get(obj, f.path) ?? ""} onchange={(e) => set(obj, f.path, e.currentTarget.value)} />
                {/if}
              </label>
            {/each}
          </section>
        {/if}
      {/each}
    </div>

    <footer>
      {#if error}<span class="error">{error}</span>{/if}
      <button onclick={onclose}>{lbl({ ko: "취소", en: "Cancel" })}</button>
      <button class="primary" disabled={saving} onclick={save}>{lbl({ ko: "저장", en: "Save" })}</button>
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
    width: min(640px, 100%);
    max-height: 100%;
    display: flex;
    flex-direction: column;
  }
  header,
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 16px;
  }
  header {
    border-bottom: 1px solid var(--border);
  }
  header h2 {
    flex: 1;
    margin: 0;
    font-size: 16px;
  }
  footer {
    border-top: 1px solid var(--border);
    justify-content: flex-end;
  }
  .body {
    overflow-y: auto;
    padding: 8px 16px 16px;
  }
  section {
    padding: 8px 0;
  }
  h3 {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
    margin: 8px 0;
  }
  .note {
    margin: -4px 0 8px;
    font-size: 12px;
    color: var(--muted);
  }
  .row {
    display: grid;
    grid-template-columns: 1fr minmax(160px, 45%);
    align-items: center;
    gap: 12px;
    padding: 4px 0;
  }
  .row.check {
    grid-template-columns: 1fr auto;
  }
  .row input:not([type="checkbox"]),
  .row select {
    width: 100%;
    box-sizing: border-box;
    font: inherit;
    color: inherit;
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 5px 8px;
  }
  .num {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .icon {
    border: 0;
    background: transparent;
  }
  .bar {
    position: relative;
    height: 8px;
    background: var(--tile);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar div {
    position: absolute;
    inset: 0 auto 0 0;
  }
  .alloc {
    background: var(--accent-soft);
  }
  .used {
    background: var(--accent);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 16px;
    margin: 10px 0 0;
    font-size: 13px;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    font-variant-numeric: tabular-nums;
  }
  .error {
    color: var(--danger);
    margin-right: auto;
  }
  @media (max-width: 520px) {
    .row {
      grid-template-columns: 1fr;
      gap: 4px;
    }
  }
</style>
