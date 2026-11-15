import { test } from "node:test";
import assert from "node:assert/strict";
import { rng, stressScript, stressRegion, KINDS } from "../src/devtools/stress.js";

const region = { minLon: -118.6, maxLon: -118.3, minLat: 33.9, maxLat: 34.1, minZoom: 11, maxZoom: 16.5 };

test("rng: the same seed gives the same sequence, another seed another, all in [0, 1)", () => {
  const a = rng(7), b = rng(7), c = rng(8);
  const xa = Array.from({ length: 1000 }, a), xb = Array.from({ length: 1000 }, b), xc = Array.from({ length: 1000 }, c);
  assert.deepEqual(xa, xb);
  assert.notDeepEqual(xa, xc);
  assert.ok(xa.every((x) => x >= 0 && x < 1));
  // roughly uniform: each tenth holds 100 +- 40 of 1000
  const bins = Array(10).fill(0);
  for (const x of xa) bins[Math.floor(x * 10)] += 1;
  assert.ok(bins.every((n) => n > 60 && n < 140), bins.join(","));
});

test("stressScript: deterministic, every kind present, every value inside the region", () => {
  const s = stressScript(1, 2000, region);
  assert.deepEqual(s, stressScript(1, 2000, region));
  assert.notDeepEqual(s, stressScript(2, 2000, region));
  assert.equal(s.length, 2000);
  for (const k of KINDS) assert.ok(s.filter((o) => o.kind === k).length > 150, `${k} under-represented`);
  for (const o of s) {
    if (o.center) {
      assert.ok(o.center[0] >= region.minLon && o.center[0] <= region.maxLon, `lon ${o.center[0]}`);
      assert.ok(o.center[1] >= region.minLat && o.center[1] <= region.maxLat, `lat ${o.center[1]}`);
    }
    if (o.zoom !== undefined) assert.ok(o.zoom >= region.minZoom && o.zoom <= region.maxZoom, `zoom ${o.zoom}`);
    if (o.bearing !== undefined) assert.ok(Math.abs(o.bearing) <= 180);
    if (o.x !== undefined) assert.ok(o.x > 0 && o.x < 1 && o.y > 0 && o.y < 1);
    assert.ok(Number.isInteger(o.after) && o.after >= 0);
  }
});

test("stressScript: most animated moves are interrupted by the next one", () => {
  const anim = stressScript(3, 2000, region).filter((o) => o.duration);
  const interrupted = anim.filter((o) => o.after < o.duration).length / anim.length;
  assert.ok(interrupted > 0.7 && interrupted < 0.95, `interrupted ${interrupted}`);
});

test("stressRegion: a cutout is inset a quarter, and its lowest zoom keeps a turned view inside the data", () => {
  const bounds = { minLon: -118.6, maxLon: -118.2, minLat: 33.9, maxLat: 34.2 };
  const r = stressRegion(bounds, 1200, 800);
  assert.deepEqual([r.minLon, r.maxLon].map((x) => +x.toFixed(6)), [-118.5, -118.3]);
  assert.deepEqual([r.minLat, r.maxLat].map((x) => +x.toFixed(6)), [33.975, 34.125]);
  // at minZoom the viewport's half-diagonal, in degrees of longitude, fits in the 0.075 deg lat margin
  const lat = ((r.minLat + r.maxLat) / 2) * (Math.PI / 180);
  const halfDiagDeg = (Math.hypot(1200, 800) / 2) * 360 / (512 * 2 ** r.minZoom);
  assert.ok(halfDiagDeg <= (0.3 / 4 / Math.cos(lat)) * 1.001, `${halfDiagDeg}`);
  assert.ok(halfDiagDeg > (0.3 / 4 / Math.cos(lat)) * 0.98, "not more conservative than needed");
  assert.ok(r.maxZoom >= 16.5);
});

test("stressRegion: a world-sized archive gets a box around home, from zoom 10", () => {
  const r = stressRegion({ minLon: -180, maxLon: 180, minLat: -85, maxLat: 85 }, 1200, 800, [2.35, 48.85]);
  assert.deepEqual([r.minLon, r.maxLon, r.minLat, r.maxLat].map((x) => +x.toFixed(6)), [1.95, 2.75, 48.55, 49.15]);
  assert.equal(r.minZoom, 10);
});
