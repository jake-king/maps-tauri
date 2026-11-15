import { test } from "node:test";
import assert from "node:assert/strict";
import { placeLabels, rotatedBox } from "../src/rust-renderer/labels.js";

const box = (x0, y0, x1, y1) => ({ x0, y0, x1, y1 });

test("greedy placement keeps the higher-ranked of overlapping labels", () => {
  const kept = placeLabels([
    { key: "minor", rank: [5, 1, 0], box: box(0, 0, 50, 10) },
    { key: "city", rank: [0, 1, 0], box: box(40, 0, 90, 20) },
    { key: "apart", rank: [5, 1, 0], box: box(200, 200, 250, 210) },
  ]);
  assert.deepEqual(kept.map((c) => c.key), ["city", "apart"]);
});

test("rank compares element by element; touching edges do not overlap", () => {
  const kept = placeLabels([
    { key: "b", rank: [1, 1, 5], box: box(0, 0, 10, 10) },
    { key: "a", rank: [1, 0, 9], box: box(5, 5, 15, 15) }, // placed last frame: wins over b
    { key: "c", rank: [1, 1, 0], box: box(15, 0, 25, 10) }, // touches a's right edge exactly
  ]);
  assert.deepEqual(kept.map((c) => c.key), ["a", "c"]);
});

test("overlaps are found across grid cells", () => {
  const kept = placeLabels([
    { key: "wide", rank: [0], box: box(0, 0, 300, 10) },
    { key: "far-right", rank: [1], box: box(290, 5, 320, 15) },
  ], 64);
  assert.deepEqual(kept.map((c) => c.key), ["wide"]);
});

test("overlaps are found in lower grid rows, not only the first", () => {
  const kept = placeLabels([
    { key: "bottom", rank: [0], box: box(0, 250, 10, 300) }, // grid rows 3-4 only
    // starts in row 1, where nothing is; overlaps "bottom" only at y 250-260, in rows 3-4
    { key: "spanning", rank: [1], box: box(5, 100, 40, 260) },
  ], 64);
  assert.deepEqual(kept.map((c) => c.key), ["bottom"]);
});

test("a rotated box is the bounding box of the turned rectangle", () => {
  const b = rotatedBox(0, 0, 100, 10, Math.PI / 2, 0);
  assert.ok(Math.abs(b.x1 - 5) < 1e-9 && Math.abs(b.y1 - 50) < 1e-9);
  const d = rotatedBox(0, 0, 100, 0, Math.PI / 4, 0);
  assert.ok(Math.abs(d.x1 - 50 * Math.SQRT1_2) < 1e-9);
});
