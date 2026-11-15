// Pure pieces of the F4 overlay: rolling windows, summaries and the drawing geometry. Nothing here
// touches the DOM, the map or the clock, so every function is tested exactly in tests/stats.test.js.

/** A fixed-size window of the last `n` samples, oldest first. */
export class Ring {
  constructor(n) {
    this.n = n;
    this.buf = [];
  }
  push(v) {
    this.buf.push(v);
    if (this.buf.length > this.n) this.buf.shift();
  }
  values() {
    return this.buf.slice();
  }
  /** `{ avg, worst, count }` over the window; zeros when empty. */
  summary() {
    const count = this.buf.length;
    if (count === 0) return { avg: 0, worst: 0, count: 0 };
    let sum = 0;
    let worst = -Infinity;
    for (const v of this.buf) {
      sum += v;
      if (v > worst) worst = v;
    }
    return { avg: sum / count, worst, count };
  }
}

/**
 * A row's bar: `value` against one frame's `budget`, as a width in pixels and whether it is over.
 * A full bar is one budget; past that the bar stays full and turns red, as in guile-2d's F4.
 */
export function bar(value, budget, width) {
  if (!(budget > 0) || !(value > 0)) return { w: 0, over: false };
  return { w: Math.min(width, Math.round((value / budget) * width)), over: value > budget };
}

/**
 * The graph's columns: one per sample, height scaled so the top of the graph is `scale` ms
 * (two budgets by default, so the budget line sits halfway), clamped to the graph.
 */
export function columns(values, budget, height, scale = 2 * budget) {
  return values.map((v) => ({
    h: Math.min(height, Math.max(0, Math.round((v / scale) * height))),
    over: v > budget,
  }));
}

/** The y of the budget line inside a graph of `height`, for the same `scale`. */
export function budgetY(budget, height, scale = 2 * budget) {
  return height - Math.round((budget / scale) * height);
}

/** The display's frame budget in ms from recent frame intervals: the median, snapped to a common rate. */
export function frameBudget(intervals) {
  if (intervals.length === 0) return 1000 / 60;
  const s = [...intervals].sort((a, b) => a - b);
  const median = s[Math.floor(s.length / 2)];
  const rates = [30, 60, 75, 90, 120, 144, 165, 240];
  let best = 60;
  for (const r of rates) if (Math.abs(1000 / r - median) < Math.abs(1000 / best - median)) best = r;
  return 1000 / best;
}

/** Keep a dragged overlay of `size` fully inside the viewport. */
export function clampPosition(pos, size, viewport) {
  return {
    x: Math.min(Math.max(0, pos.x), Math.max(0, viewport.w - size.w)),
    y: Math.min(Math.max(0, pos.y), Math.max(0, viewport.h - size.h)),
  };
}

/** `1536` → `1.5 KB`; binary units, one decimal past bytes. */
export function formatBytes(n) {
  if (!(n >= 0)) return "?";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i += 1;
  }
  return i === 0 ? `${n} B` : `${n.toFixed(1)} ${units[i]}`;
}
