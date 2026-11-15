import { test } from "node:test";
import assert from "node:assert/strict";
import { Ring, bar, columns, budgetY, frameBudget, clampPosition, formatBytes } from "../src/devtools/stats.js";

test("Ring keeps the last n, oldest first, and summarises them", () => {
  const r = new Ring(3);
  assert.deepEqual(r.summary(), { avg: 0, worst: 0, count: 0 });
  for (const v of [5, 1, 9, 3]) r.push(v);
  assert.deepEqual(r.values(), [1, 9, 3]);
  assert.deepEqual(r.summary(), { avg: 13 / 3, worst: 9, count: 3 });
});

test("Ring's worst is the true maximum, negatives included", () => {
  const r = new Ring(4);
  for (const v of [-3, -1, -2]) r.push(v);
  assert.equal(r.summary().worst, -1);
});

test("bar: proportional under budget, full and red over it, empty for nothing", () => {
  assert.deepEqual(bar(8, 16, 100), { w: 50, over: false });
  assert.deepEqual(bar(16, 16, 100), { w: 100, over: false });
  assert.deepEqual(bar(40, 16, 100), { w: 100, over: true });
  assert.deepEqual(bar(0, 16, 100), { w: 0, over: false });
  assert.deepEqual(bar(5, 0, 100), { w: 0, over: false });
  assert.deepEqual(bar(NaN, 16, 100), { w: 0, over: false });
});

test("columns: budget at half height by default, clamped, over flagged", () => {
  assert.deepEqual(columns([0, 8, 16, 32, 64], 16, 40), [
    { h: 0, over: false }, { h: 10, over: false }, { h: 20, over: false },
    { h: 40, over: true }, { h: 40, over: true },
  ]);
  assert.equal(budgetY(16, 40), 20);
  assert.deepEqual(columns([16], 16, 40, 64), [{ h: 10, over: false }]);
});

test("frameBudget snaps the median interval to a display rate", () => {
  assert.equal(frameBudget([]), 1000 / 60);
  assert.equal(frameBudget([16.6, 16.8, 16.7, 50]), 1000 / 60);
  assert.equal(frameBudget([6.9, 7.0, 6.95]), 1000 / 144);
  assert.equal(frameBudget([33, 34, 33.4]), 1000 / 30);
  // one unusually short interval must not move the budget: the median decides, not the minimum
  assert.equal(frameBudget([8.3, 16.7, 16.6, 16.8]), 1000 / 60);
});

test("clampPosition keeps the overlay on screen", () => {
  const size = { w: 300, h: 200 };
  const vp = { w: 1000, h: 800 };
  assert.deepEqual(clampPosition({ x: -50, y: 900 }, size, vp), { x: 0, y: 600 });
  assert.deepEqual(clampPosition({ x: 900, y: 10 }, size, vp), { x: 700, y: 10 });
  assert.deepEqual(clampPosition({ x: 5, y: 5 }, { w: 2000, h: 2000 }, vp), { x: 0, y: 0 });
});

test("formatBytes", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(5 * 1024 * 1024), "5.0 MB");
  assert.equal(formatBytes(-1), "?");
});
