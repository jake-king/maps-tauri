import { test } from "node:test";
import assert from "node:assert/strict";
import { placeLabels, rotatedBox, layoutOnPath } from "../src/rust-renderer/labels.js";

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

test("a label with options takes the first that fits", () => {
  const kept = placeLabels([
    { key: "blocker", rank: [0], box: box(100, 0, 200, 10) },
    { key: "poi", rank: [1], options: [{ box: box(150, 0, 190, 10), side: "right" }, { box: box(40, 0, 90, 10), side: "left" }] },
    { key: "stuck", rank: [2], options: [{ box: box(110, 0, 120, 10) }, { box: box(45, 2, 60, 8) }] },
  ]);
  assert.deepEqual(kept.map((c) => c.key), ["blocker", "poi"]);
  assert.equal(kept[1].chosen.side, "left");
});

test("a multi-box label is kept only if every one of its boxes fits", () => {
  const kept = placeLabels([
    { key: "blocker", rank: [0], box: box(100, 0, 110, 10) },
    { key: "curve", rank: [1], boxes: [box(0, 0, 10, 10), box(50, 0, 60, 10), box(105, 0, 115, 10)] },
    { key: "free", rank: [2], boxes: [box(0, 50, 10, 60), box(20, 50, 30, 60)] },
  ]);
  assert.deepEqual(kept.map((c) => c.key), ["blocker", "free"]);
});

test("layoutOnPath: centred, one glyph per character, along the path", () => {
  const lay = layoutOnPath([[0, 0], [100, 0]], [10, 10, 10]);
  assert.deepEqual(lay.glyphs.map((g) => g.x), [40, 50, 60]);
  assert.ok(lay.glyphs.every((g) => g.y === 0 && g.angle === 0) && lay.straight);
  // round a 30-degree bend: glyphs past the corner take the new direction; not straight
  const bend = layoutOnPath([[0, 0], [50, 0], [50 + 50 * Math.cos(Math.PI / 6), 50 * Math.sin(Math.PI / 6)]], [10, 10, 10, 10, 10]);
  assert.ok(bend && !bend.straight);
  assert.equal(bend.glyphs[0].angle, 0);
  assert.ok(Math.abs(bend.glyphs[4].angle - Math.PI / 6) < 1e-9);
});

test("layoutOnPath refuses: too short, a hairpin, upside down", () => {
  assert.equal(layoutOnPath([[0, 0], [20, 0]], [10, 10]), null); // 20 < 20 + pad
  assert.equal(layoutOnPath([[0, 0], [50, 0], [50, 50]], [10, 10, 10, 10, 10, 10, 10]), null); // a 90-degree turn
  assert.equal(layoutOnPath([[100, 0], [0, 0]], [10, 10]), null); // running right to left
  assert.ok(layoutOnPath([[0, 0], [50, 0], [50, 50]], [10, 10, 10, 10, 10, 10, 10], { maxAngle: Math.PI }) !== null);
});

test("layoutOnPath tries further anchors when the middle is a hairpin", () => {
  // straight for 100, a hairpin at 100-110, straight again: the middle (55%) lands on the bend
  const pts = [[0, 0], [100, 0], [105, 5], [100, 10], [0, 10]];
  assert.equal(layoutOnPath(pts, [8, 8, 8], { anchors: [0.5] }), null);
  const lay = layoutOnPath(pts, [8, 8, 8], { anchors: [0.5, 0.2] });
  assert.ok(lay && lay.glyphs.every((g) => g.angle === 0) && lay.glyphs[0].x < 60);
});

test("a rotated box is the bounding box of the turned rectangle", () => {
  const b = rotatedBox(0, 0, 100, 10, Math.PI / 2, 0);
  assert.ok(Math.abs(b.x1 - 5) < 1e-9 && Math.abs(b.y1 - 50) < 1e-9);
  const d = rotatedBox(0, 0, 100, 0, Math.PI / 4, 0);
  assert.ok(Math.abs(d.x1 - 50 * Math.SQRT1_2) < 1e-9);
});
