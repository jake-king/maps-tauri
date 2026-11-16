import test from "node:test";
import assert from "node:assert/strict";
import { FrameLoop } from "../src/rust-renderer/loop.js";

/** A loop over a fake clock where each upload costs `cost` ms; records what happened. */
function rig({ cost = 1, budgetMs = 4 } = {}) {
  let clock = 0;
  const log = { renders: 0, uploads: [] };
  const loop = new FrameLoop({
    render: () => log.renders++,
    upload: (item) => {
      clock += cost;
      log.uploads.push(item);
    },
    budgetMs,
    now: () => clock,
  });
  const tick = (dt = 16) => {
    clock += dt;
    return loop.tick(clock);
  };
  return { loop, log, tick };
}

test("draws once at start, then never while nothing changes", () => {
  const { log, tick } = rig();
  for (let i = 0; i < 100; i++) tick();
  assert.equal(log.renders, 1);
});

test("any number of invalidations before a tick is one redraw", () => {
  const { loop, log, tick } = rig();
  tick();
  for (let i = 0; i < 5; i++) loop.invalidate();
  tick();
  tick();
  assert.equal(log.renders, 2);
});

test("arrivals are uploaded in the frame, most urgent first, then redrawn", () => {
  const { loop, log, tick } = rig({ cost: 1 });
  tick();
  loop.arrive("ring", 2000);
  loop.arrive("visible", 3);
  loop.arrive("fallback", 1000);
  assert.deepEqual(log.uploads, [], "nothing touches the GPU outside a tick");
  const r = tick();
  assert.deepEqual(log.uploads, ["visible", "fallback", "ring"]);
  assert.deepEqual(r, { uploaded: 3, drew: true });
});

test("uploads stop at the frame's budget, at least one per frame", () => {
  const { loop, log, tick } = rig({ cost: 3, budgetMs: 4 });
  tick();
  for (let i = 0; i < 5; i++) loop.arrive(i, i);
  assert.equal(tick().uploaded, 2); // 3 ms, then 6 ms: past 4 after the second
  assert.equal(tick().uploaded, 2);
  assert.equal(tick().uploaded, 1);
  assert.deepEqual(log.uploads, [0, 1, 2, 3, 4]);
  const slow = rig({ cost: 50, budgetMs: 4 });
  slow.tick();
  slow.loop.arrive("big", 0);
  slow.loop.arrive("next", 1);
  assert.equal(slow.tick().uploaded, 1, "one over-budget upload still goes: no starvation");
});

test("an arrival during a tick waits for the next", () => {
  const seen = [];
  let clock = 0;
  const loop = new FrameLoop({
    render: () => {},
    upload: (x) => {
      seen.push(x);
      if (x === "a") loop.arrive("b", 0);
    },
    now: () => clock,
  });
  loop.arrive("a", 0);
  loop.tick((clock += 16));
  assert.deepEqual(seen, ["a"]);
  loop.tick((clock += 16));
  assert.deepEqual(seen, ["a", "b"]);
});

test("an updater that reports a change causes a redraw; one that does not, none", () => {
  const { loop, log, tick } = rig();
  tick();
  let changed = false;
  loop.addUpdater(() => changed);
  tick();
  assert.equal(log.renders, 1);
  changed = true;
  tick();
  assert.equal(log.renders, 2);
});

test("a failing upload is reported and the loop carries on", () => {
  let clock = 0;
  const errors = [];
  let renders = 0;
  const loop = new FrameLoop({
    render: () => renders++,
    upload: (x) => {
      if (x === "bad") throw new Error("corrupt");
    },
    onError: (e) => errors.push(e.message),
    now: () => clock,
  });
  loop.arrive("bad", 0);
  loop.arrive("good", 1);
  loop.tick((clock += 16));
  assert.deepEqual(errors, ["corrupt"]);
  assert.equal(loop.pending, 0);
  assert.equal(renders, 1);
});

test("quiet: no arrivals waiting and nothing to redraw", () => {
  const { loop, tick } = rig();
  assert.equal(loop.quiet, false, "the first frame is owed");
  tick();
  assert.equal(loop.quiet, true);
  loop.arrive("x", 0);
  assert.equal(loop.quiet, false);
  tick();
  assert.equal(loop.quiet, true);
});

test("the frame clock: ticks, draws, and frames late by more than half again the median", () => {
  const { loop, tick } = rig();
  for (let i = 0; i < 30; i++) tick(16);
  tick(40); // a hitch
  const s = loop.stats();
  assert.equal(s.ticks, 31);
  assert.equal(s.draws, 1);
  assert.equal(s.late, 1);
  assert.equal(Math.round(s.interval), 16);
});
