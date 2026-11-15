import { test } from "node:test";
import assert from "node:assert/strict";
import { overlaps, uncoveredCount } from "../src/devtools/coverage.js";

const t = (z, x, y, wrap = 0) => ({ z, x, y, wrap });

test("a tile overlaps itself, its ancestors and its descendants", () => {
  assert.equal(overlaps(t(14, 2830, 6550), t(14, 2830, 6550)), true);
  assert.equal(overlaps(t(14, 2830, 6550), t(13, 1415, 3275)), true); // parent
  assert.equal(overlaps(t(14, 2830, 6550), t(10, 176, 409)), true);   // 2830>>4, 6550>>4
  assert.equal(overlaps(t(14, 2830, 6550), t(15, 5661, 13101)), true); // a child
});

test("neighbours, other branches and other world copies do not overlap", () => {
  assert.equal(overlaps(t(14, 2830, 6550), t(14, 2831, 6550)), false);
  assert.equal(overlaps(t(14, 2830, 6550), t(13, 1416, 3275)), false);
  assert.equal(overlaps(t(14, 2830, 6550), t(15, 5662, 13100)), false); // child of the neighbour
  assert.equal(overlaps(t(14, 2830, 6550), t(14, 2830, 6550, 1)), false);
  assert.equal(overlaps(t(14, 2830, 6550), t(10, 176, 410)), false);
});

test("uncoveredCount counts only needed tiles nothing overlaps", () => {
  const needed = [t(14, 0, 0), t(14, 1, 0), t(14, 2, 0), t(14, 3, 0)];
  assert.equal(uncoveredCount(needed, []), 4);
  assert.equal(uncoveredCount(needed, [t(13, 0, 0)]), 2);          // a parent covers two
  assert.equal(uncoveredCount(needed, [t(13, 0, 0), t(15, 6, 1)]), 1);
  assert.equal(uncoveredCount(needed, [t(12, 0, 0)]), 0);          // a grandparent covers all
  assert.equal(uncoveredCount([], [t(12, 0, 0)]), 0);
});
