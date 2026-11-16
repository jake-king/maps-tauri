// The native renderer's frame loop, after the game-engine model: one place where state changes. It
// ticks every display frame; whatever arrived since the last tick (tile meshes, radar frames) waits in
// an inbox and is uploaded inside the frame, most urgent first, within a time budget; then the map is
// redrawn only if something changed. Async callbacks never touch the GPU or draw.
//
// Ticking costs nothing measurable at rest (4.0% of a core with or without it, measured
// 2026-11-16); redrawing every frame cost 65.7%, so the redraw waits for a change. Pure: no DOM, no
// WebGL, a clock passed in; tested in tests/loop.test.js.

export class FrameLoop {
  /**
   * @param render   draws the frame
   * @param upload   takes one arrival onto the GPU
   * @param budgetMs upload time per frame; past it the rest wait for the next frame (at least one
   *                 goes every frame, so a large arrival cannot starve the queue)
   * @param onError  an upload that threw (the item is dropped, the loop carries on)
   * @param now      the clock, in ms
   */
  constructor({ render, upload, budgetMs = 4, onError = (e) => console.error(e), now = () => performance.now() }) {
    Object.assign(this, { render, upload, budgetMs, onError, now });
    this.inbox = [];
    this.updaters = [];
    this.dirty = true; // the first frame is owed
    this.ticks = 0;
    this.draws = 0;
    this.late = 0;
    this.last = null;
    this.intervals = []; // the last 120 tick intervals, for the median
  }

  /** Something arrived; it is uploaded in the next frame with room, lower `priority` first. */
  arrive(item, priority) {
    this.inbox.push({ item, priority });
  }

  /** The picture is out of date: redraw on the next tick (once, however many calls). */
  invalidate() {
    this.dirty = true;
  }

  /** `fn(now)` runs every tick and returns true when the picture changed (an animation's frame). */
  addUpdater(fn) {
    this.updaters.push(fn);
  }

  get pending() {
    return this.inbox.length;
  }

  /** Nothing waiting and nothing to redraw. */
  get quiet() {
    return this.inbox.length === 0 && !this.dirty;
  }

  /** One display frame: take in what arrived, update, draw if anything changed. */
  tick(t) {
    this.ticks += 1;
    if (this.last !== null) {
      const dt = t - this.last;
      if (this.intervals.length >= 10 && dt > 1.5 * median(this.intervals)) this.late += 1;
      this.intervals.push(dt);
      if (this.intervals.length > 120) this.intervals.shift();
    }
    this.last = t;

    let uploaded = 0;
    if (this.inbox.length) {
      // what arrives while this frame uploads waits for the next one
      const batch = this.inbox.sort((a, b) => a.priority - b.priority);
      this.inbox = [];
      const start = this.now();
      let i = 0;
      for (; i < batch.length; i++) {
        if (i > 0 && this.now() - start >= this.budgetMs) break;
        try {
          this.upload(batch[i].item);
        } catch (e) {
          this.onError(e);
        }
        uploaded += 1;
      }
      this.inbox = batch.slice(i).concat(this.inbox);
      this.dirty = true;
    }
    for (const u of this.updaters) if (u(t)) this.dirty = true;

    if (!this.dirty) return { uploaded, drew: false };
    this.dirty = false;
    this.draws += 1;
    this.render(t);
    return { uploaded, drew: true };
  }

  /** Ticks, draws, frames later than 1.5x the median interval, and that median (ms). */
  stats() {
    return { ticks: this.ticks, draws: this.draws, late: this.late, interval: median(this.intervals) };
  }
}

function median(xs) {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[s.length >> 1];
}
