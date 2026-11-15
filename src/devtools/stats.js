// Pure pieces of the F4 overlay: rolling windows, summaries and the drawing geometry. Nothing here
// touches the DOM, the map or the clock, so every function is tested exactly in tests/stats.test.js.

/**
 * A fixed-size window of the last `n` samples. Storage is one Float64Array allocated up front and
 * written in a circle, so push() allocates nothing: guile-2d's rule that a frame allocates nothing.
 * values() copies (oldest first) and is for drawing the overlay, not the per-frame path.
 */
export class Ring {
  constructor(n) {
    this.n = n;
    this.buf = new Float64Array(n);
    this.head = 0; // next slot to write
    this.count = 0;
  }
  push(v) {
    this.buf[this.head] = v;
    this.head = (this.head + 1) % this.n;
    if (this.count < this.n) this.count += 1;
  }
  values() {
    const out = new Array(this.count);
    const start = (this.head - this.count + this.n) % this.n;
    for (let i = 0; i < this.count; i++) out[i] = this.buf[(start + i) % this.n];
    return out;
  }
  /** `{ avg, worst, count }` over the window; zeros when empty. Allocates only the result. */
  summary() {
    const count = this.count;
    if (count === 0) return { avg: 0, worst: 0, count: 0 };
    let sum = 0;
    let worst = -Infinity;
    for (let i = 0; i < count; i++) {
      const v = this.buf[i];
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
 * (two budgets by default, so the budget line sits halfway), clamped to the graph. A column is
 * `over` past `overAt`: for frame intervals pass 1.5 budgets, because WebKit rounds
 * performance.now() to whole ms and a perfect 16.7 ms frame can measure 18.
 */
export function columns(values, budget, height, scale = 2 * budget, overAt = budget) {
  return values.map((v) => ({
    h: Math.min(height, Math.max(0, Math.round((v / scale) * height))),
    over: v > overAt,
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
