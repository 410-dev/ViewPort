<script lang="ts">
  import { flip } from "svelte/animate";
  import { cubicOut } from "svelte/easing";
  import type { TransitionConfig } from "svelte/transition";
  import { t } from "../i18n.svelte";
  import { dismiss, pause, resume, toasts } from "../toast.svelte";
  import Icon from "./Icon.svelte";
  import { ms, reduced } from "./motion";

  /**
   * The stack of "done" pills (`toast.svelte.ts`). The newest is at the bottom; the others
   * move up. A pill rises in, and leaves by fading (time up) or along the way it was
   * swiped (sideways or down, by finger or mouse).
   */

  const dur = (k = 1) => {
    if (reduced()) return 0;
    const v = ms(getComputedStyle(document.documentElement).getPropertyValue("--t-pop")) ?? 220;
    return v * k;
  };

  /** Where each pill is being dragged (px), by id. */
  let drag = $state<Record<number, { x: number; y: number }>>({});
  /** Pills let go past the threshold: the way they leave. */
  const flung = new Map<number, { x: number; y: number; from: { x: number; y: number } }>();
  let press: { id: number; pointerId: number; x0: number; y0: number; t0: number } | null = null;

  const THRESHOLD = 64;

  function down(e: PointerEvent, id: number) {
    if (e.pointerType === "mouse" && e.button !== 0) return;
    press = { id, pointerId: e.pointerId, x0: e.clientX, y0: e.clientY, t0: performance.now() };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    pause(id);
  }

  function move(e: PointerEvent) {
    if (!press || e.pointerId !== press.pointerId) return;
    // Sideways freely; downward only (up has nowhere to go).
    drag[press.id] = { x: e.clientX - press.x0, y: Math.max(0, e.clientY - press.y0) };
  }

  function up(e: PointerEvent) {
    if (!press || e.pointerId !== press.pointerId) return;
    const { id, t0 } = press;
    press = null;
    const d = drag[id] ?? { x: 0, y: 0 };
    const secs = Math.max(0.05, (performance.now() - t0) / 1000);
    const fast = Math.hypot(d.x, d.y) / secs > 600 && Math.hypot(d.x, d.y) > 16;
    if (Math.abs(d.x) > THRESHOLD || d.y > THRESHOLD * 0.6 || fast) {
      flung.set(id, { ...(Math.abs(d.x) >= d.y ? { x: Math.sign(d.x) || 1, y: 0 } : { x: 0, y: 1 }), from: d });
      dismiss(id);
    } else {
      resume(id);
    }
    delete drag[id];
  }

  function cancel(e: PointerEvent) {
    if (!press || e.pointerId !== press.pointerId) return;
    resume(press.id);
    delete drag[press.id];
    press = null;
  }

  /** In: rises a little, grows and fades in. */
  function rise(_n: Element): TransitionConfig {
    return {
      duration: dur(1.2),
      easing: cubicOut,
      css: (t, u) => `opacity: ${t}; transform: translateY(${u * 24}px) scale(${0.9 + 0.1 * t})`,
    };
  }

  /** Out: along the swipe (from where it was let go), or a fade and shrink when time is up. */
  function leave(_node: Element, { id }: { id: number }): TransitionConfig {
    const way = flung.get(id);
    flung.delete(id);
    if (!way) {
      return { duration: dur(), easing: cubicOut, css: (t) => `opacity: ${t}; transform: scale(${0.92 + 0.08 * t})` };
    }
    const { x: x0, y: y0 } = way.from;
    const far = way.x ? window.innerWidth * 0.6 : 120;
    return {
      duration: dur(),
      easing: cubicOut,
      css: (t, u) => `opacity: ${t}; transform: translate(${x0 + way.x * far * u}px, ${y0 + way.y * far * u}px)`,
    };
  }
</script>

<div class="toasts" aria-live="polite">
  {#each toasts.list as tst (tst.id)}
    {@const d = drag[tst.id]}
    <div
      class="toast"
      class:dragging={!!d}
      role="status"
      style:transform={d ? `translate(${d.x}px, ${d.y}px)` : undefined}
      style:opacity={d ? Math.max(0.25, 1 - Math.max(Math.abs(d.x), d.y * 1.5) / 220) : undefined}
      animate:flip={{ duration: dur(1.2), easing: cubicOut }}
      in:rise
      out:leave={{ id: tst.id }}
      onpointerdown={(e) => down(e, tst.id)}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={cancel}
      onmouseenter={() => pause(tst.id)}
      onmouseleave={() => !press && resume(tst.id)}
    >
      <span class="ico"><Icon name={tst.icon} size={13} stroke={3} /></span>
      <span class="text">{tst.text}</span>
      <button class="close" aria-label={t("toast.dismiss")} onpointerdown={(e) => e.stopPropagation()} onclick={() => dismiss(tst.id)}>
        <Icon name="x" size={12} />
      </button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    left: 0;
    right: 0;
    bottom: calc(env(safe-area-inset-bottom, 0px) + 44px);
    z-index: 2000;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    padding: 0 16px;
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: 10px;
    max-width: min(560px, 100%);
    min-height: 40px;
    padding: 6px 6px 6px 8px;
    border-radius: var(--radius-pill);
    background: var(--surface-3, var(--surface-2));
    color: var(--fg);
    border: 1px solid var(--border);
    box-shadow: var(--shadow);
    font-size: 13px;
    line-height: 1.35;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    cursor: grab;
  }
  .toast:not(.dragging) {
    transition:
      transform var(--t-fast) var(--ease),
      opacity var(--t-fast) var(--ease);
  }
  .toast.dragging {
    cursor: grabbing;
  }
  .ico {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--accent);
    color: var(--accent-fg);
  }
  .text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .close {
    flex-shrink: 0;
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    padding: 0;
    border: 0;
    border-radius: 50%;
    background: transparent;
    color: var(--muted);
    cursor: pointer;
  }
  .close:hover {
    background: var(--hover);
    color: var(--fg);
  }
</style>
