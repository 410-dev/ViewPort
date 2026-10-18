<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  type Span = { start: number; end: number };
  type QueryCheck =
    | { status: "ok"; normalized: string }
    | { status: "error"; error: { kind: { code: string }; span: Span }; span: Span };

  let src = $state(
    "생성 날짜(2026-10-18:이후) 그리고 (변경 날짜(2026-09-18:이후) 혹은 크기(100MB:이상)) 그리고 유형(다음을 포함:이미지:비디오) 그리고 오름차순 정렬(이름)",
  );
  let locale = $state("ko");
  let result = $state<QueryCheck | null>(null);

  $effect(() => {
    const text = src;
    const loc = locale;
    invoke<QueryCheck>("check_query", { src: text, locale: loc }).then((r) => (result = r));
  });
</script>

<main>
  <h1>ViewPort</h1>
  <p class="hint">Query language preview. The full editor UI comes later.</p>
  <label>
    Locale
    <select bind:value={locale}>
      <option value="ko">한국어</option>
      <option value="en">English</option>
    </select>
  </label>
  <textarea bind:value={src} rows="5" spellcheck="false"></textarea>
  {#if result?.status === "ok"}
    <pre class="ok">{result.normalized}</pre>
  {:else if result?.status === "error"}
    <pre class="err">{src.slice(0, result.span.start)}<mark>{src.slice(result.span.start, result.span.end) || " "}</mark>{src.slice(result.span.end)}</pre>
    <p class="err">{result.error.kind.code}</p>
  {/if}
</main>

<style>
  :root {
    --bg: #f6f6f6;
    --fg: #0f0f0f;
    --panel: #ffffff;
    --ok: #1a7f37;
    --err: #cf222e;
    color-scheme: light dark;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #1e1e1e;
      --fg: #eaeaea;
      --panel: #2a2a2a;
      --ok: #4ac26b;
      --err: #ff7b72;
    }
  }
  :global(body) {
    margin: 0;
    background: var(--bg);
    color: var(--fg);
    font-family: system-ui, sans-serif;
  }
  main {
    max-width: 960px;
    margin: 0 auto;
    padding: 24px 16px;
    display: grid;
    gap: 12px;
  }
  textarea,
  pre {
    font: 14px/1.5 ui-monospace, monospace;
    background: var(--panel);
    color: var(--fg);
    border: 1px solid #8884;
    border-radius: 6px;
    padding: 10px;
    white-space: pre-wrap;
    word-break: break-word;
    margin: 0;
  }
  .hint {
    opacity: 0.7;
    margin: 0;
  }
  .ok {
    border-color: var(--ok);
  }
  .err {
    color: var(--err);
  }
  mark {
    background: var(--err);
    color: #fff;
  }
</style>
