import type { SchedulerMode } from "./api";

/**
 * Which items the viewer reads ahead, most wanted first: the current one, its direct
 * neighbours (the swipe strip draws them), then both sides interleaved so that at every
 * point about `forward` of what was taken lies ahead. The backend loads them in this
 * order until its share of the cache is used (`prefetch_plan`).
 *
 * - performance: both sides evenly, as far as `max` items each.
 * - predictive: as far, weighted by `forward` (where the viewer has been heading).
 * - preprocess: just `ahead` and `behind` items.
 */
export function lookahead(
  ids: number[],
  index: number,
  mode: SchedulerMode,
  o: { ahead: number; behind: number; max: number; forward: number },
): number[] {
  const n = ids.length;
  if (n === 0) return [];
  const span = Math.max(0, Math.min(o.max, n - 1));
  const [aheadN, behindN] = mode === "preprocess" ? [Math.min(o.ahead, n - 1), Math.min(o.behind, n - 1)] : [span, span];
  const forward =
    mode === "performance" ? 0.5 : mode === "predictive" ? o.forward : aheadN + behindN > 0 ? aheadN / (aheadN + behindN) : 0.5;
  const at = (k: number) => ids[(((index + k) % n) + n) % n];
  const out = new Set<number>([at(0)]);
  if (n > 1) {
    out.add(at(1));
    out.add(at(-1));
  }
  let a = 1;
  let b = 1;
  while (a < aheadN || b < behindN) {
    const takeAhead = b >= behindN || (a < aheadN && a / (a + b) < forward);
    if (takeAhead) out.add(at(++a));
    else out.add(at(-++b));
  }
  return [...out];
}

/**
 * Where the viewer is heading, -1 (backwards) .. 1 (forwards): each move pulls the
 * estimate toward its direction, earlier moves weighing `decay`.
 */
export function heading(prev: number, move: number, decay: number): number {
  const d = Math.min(0.99, Math.max(0, decay));
  return d * prev + (1 - d) * Math.sign(move);
}
