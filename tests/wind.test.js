import test from "node:test";
import assert from "node:assert/strict";
import { decodeWind, sampleWind, Particles, PX_PER_MPS, speedColor, TRAIL, trailVertices, MAX_SEGMENT_PX } from "../src/rust-renderer/wind.js";

/** An encoded field as Rust's wx::wind::encode writes it. */
function encoded({ ni, nj, lat0, lon0, dlat, dlon, run = 1_794_830_400, fhr = 7, hpa = 250 }, uv) {
  const buf = new ArrayBuffer(48 + uv.length * 4);
  const d = new DataView(buf);
  d.setUint32(0, ni, true);
  d.setUint32(4, nj, true);
  [lat0, lon0, dlat, dlon].forEach((x, i) => d.setFloat32(8 + 4 * i, x, true));
  d.setBigInt64(24, BigInt(run), true);
  d.setUint32(32, fhr, true);
  d.setUint32(36, hpa, true);
  new Float32Array(buf, 48).set(uv);
  return buf;
}

// a 3 x 2 field: row 0 (north, 60N), row 1 (south, 50N); columns at 100W, 90W, 80W
const field = () => decodeWind(encoded({ ni: 3, nj: 2, lat0: 60, lon0: -100, dlat: 10, dlon: 10 }, [
  0, 0, 10, 0, 20, 0, // north row: u = 0, 10, 20
  0, 10, 10, 10, 20, 10, // south row: v = 10 everywhere
]));

test("decodes the header and field Rust sends", () => {
  const f = field();
  assert.deepEqual([f.ni, f.nj, f.lat0, f.lon0, f.dlat, f.dlon, f.run, f.fhr, f.hpa], [3, 2, 60, -100, 10, 10, 1_794_830_400, 7, 250]);
  assert.equal(f.valid, 1_794_830_400 + 7 * 3600);
  assert.equal(f.uv.length, 12);
  assert.throws(() => decodeWind(new ArrayBuffer(10)), /short/);
  assert.throws(() => decodeWind(encoded({ ni: 3, nj: 2, lat0: 60, lon0: -100, dlat: 10, dlon: 10 }, [0, 0])), /12 values/);
});

test("bilinear sampling between grid points, nothing outside", () => {
  const f = field();
  assert.deepEqual(sampleWind(f, -100, 60), [0, 0]);
  assert.deepEqual(sampleWind(f, -90, 50), [10, 10]);
  const [u, v] = sampleWind(f, -95, 55); // the middle of the west cell
  assert.ok(Math.abs(u - 5) < 1e-6 && Math.abs(v - 5) < 1e-6, `${u} ${v}`);
  assert.equal(sampleWind(f, -101, 55), null);
  assert.equal(sampleWind(f, -85, 61), null);
  assert.deepEqual(sampleWind(f, -80, 50), [20, 10], "the last column and row are inside");
});

test("particles move with the wind at a screen speed that does not depend on zoom", () => {
  const f = field();
  const rng = mulberry(1);
  for (const scale of [512 * 2 ** 4, 512 * 2 ** 9]) {
    const p = new Particles(1, rng);
    p.lon[0] = -90;
    p.lat[0] = 50; // u = 10, v = 10
    p.age[0] = 0;
    p.step(f, [-100, 50, -80, 60], scale);
    const pxPerDeg = scale / 360;
    const dxPx = (p.lon[0] - p.prevLon[0]) * pxPerDeg;
    const dyPx = ((p.lat[0] - p.prevLat[0]) * pxPerDeg) / Math.cos((50 * Math.PI) / 180);
    assert.ok(Math.abs(dxPx - 10 * PX_PER_MPS) < 1e-6, `east ${dxPx} px at scale ${scale}`);
    assert.ok(Math.abs(dyPx - 10 * PX_PER_MPS) < 1e-3, `north ${dyPx} px at scale ${scale}`);
    assert.ok(p.lat[0] > 50, "v > 0 moves north");
  }
});

test("particles that age out or leave the field or the view start again inside the view", () => {
  const f = field();
  const p = new Particles(3, mulberry(2));
  const view = [-98, 51, -82, 59];
  p.lon.set([-120, -90, -90]); // outside the field
  p.lat.set([55, 55, 55]);
  p.age.set([0, 0, p.maxAge[2] + 1]); // the third has lived its life
  p.step(f, view, 512 * 16);
  for (let i = 0; i < 3; i += 1) {
    if (i === 1) continue;
    assert.ok(p.lon[i] >= view[0] && p.lon[i] <= view[2] && p.lat[i] >= view[1] && p.lat[i] <= view[3], `particle ${i} respawned in view: ${p.lon[i]},${p.lat[i]}`);
    assert.equal(p.prevLon[i], p.lon[i], "a respawned particle draws no streak from where it was");
    assert.equal(p.age[i], 0);
  }
  assert.ok(p.lon[1] !== -90 || p.lat[1] !== 55, "the one inside moved");
});

test("speed colours: calm to jet", () => {
  assert.notDeepEqual(speedColor(5), speedColor(70));
  assert.equal(speedColor(5).length, 4);
  assert.ok(speedColor(70)[3] >= speedColor(5)[3], "stronger wind is at least as opaque");
});

function mulberry(seed) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

test("each particle keeps its recent path in world coordinates, and a respawn starts a new one", () => {
  const f = field();
  const p = new Particles(2, mulberry(3));
  const view = [-98, 51, -82, 59];
  p.step(f, view, 512 * 16); // all respawn
  assert.deepEqual([p.histLen[0], p.histLen[1]], [1, 1]);
  for (let k = 0; k < 30; k++) p.step(f, view, 512 * 16);
  assert.ok(p.histLen[0] <= TRAIL && p.histLen[0] > 1, `history ${p.histLen[0]} of at most ${TRAIL}`);
  const [hx, hy] = p.point(0, 0);
  assert.ok(Math.abs(hx - (p.lon[0] + 180) / 360) < 1e-12, "the newest point is where the particle is");
  assert.ok(hy > 0 && hy < 1);
  p.age[1] = Infinity;
  p.step(f, view, 512 * 16);
  assert.equal(p.histLen[1], 1, "a respawned particle has no path behind it");
});

test("the trail vertices: one segment per step of history, fading from head to tail, through any camera", async () => {
  const { Camera } = await import("../src/rust-renderer/geo.js");
  const f = field();
  const p = new Particles(3, mulberry(4));
  const view = [-98, 51, -82, 59];
  for (let k = 0; k < 6; k++) p.step(f, view, 512 * 16);
  const cam = new Camera(0.24, 0.32, 4, 1200, 800, 0.3);
  const out = new Float32Array(p.n * TRAIL * 2 * 6);
  const n = trailVertices(p, cam, out);
  const want = [0, 1, 2].reduce((a, i) => a + Math.max(0, p.histLen[i] - 1), 0);
  assert.equal(n, want * 2, "two vertices per segment");
  const first = [0, 1, 2].find((i) => p.histLen[i] > 1);
  const [wx, wy] = p.point(first, 0);
  const [sx, sy] = cam.worldToScreen(wx, wy);
  assert.ok(Math.abs(out[0] - sx) < 1e-3 && Math.abs(out[1] - sy) < 1e-3, `${out[0]},${out[1]} vs ${sx},${sy}`);
  const alphas = [];
  for (let v = 0; v < (p.histLen[first] - 1) * 2; v += 2) alphas.push(out[v * 6 + 5]);
  assert.ok(alphas.length > 1 && alphas.every((a, i) => i === 0 || a < alphas[i - 1]), `alphas ${alphas}`);
});


/** How evenly particles cover `view`: coefficient of variation of their count over a 4 x 4 grid. */
function evenness(p, [w, s, e, n]) {
  const cells = new Array(16).fill(0);
  let k = 0;
  for (let i = 0; i < p.n; i++) {
    const x = (p.lon[i] - w) / (e - w), y = (p.lat[i] - s) / (n - s);
    if (x < 0 || y < 0 || x >= 1 || y >= 1) continue;
    cells[Math.floor(y * 4) * 4 + Math.floor(x * 4)] += 1;
    k += 1;
  }
  const mean = k / 16;
  return Math.sqrt(cells.reduce((a, c) => a + (c - mean) ** 2, 0) / 16) / mean;
}

// still air everywhere: particles only move by respawning, so evenness is the respawn rule alone
const still = () => decodeWind(encoded({ ni: 2, nj: 2, lat0: 80, lon0: -170, dlat: 160, dlon: 160 }, new Array(8).fill(0)));

test("a fast zoom out spreads the particles over the whole new view at once", () => {
  const p = new Particles(4000, mulberry(5));
  const small = [-100, 35, -90, 41], big = [-120, 23, -70, 53]; // 25 times the area
  p.step(still(), small, 512 * 64);
  for (let k = 0; k < 5; k++) p.step(still(), small, 512 * 64, small);
  p.step(still(), big, 512 * 8, small);
  const e = evenness(p, big);
  assert.ok(e < 0.25, `evenness ${e.toFixed(2)} after zooming out: bunched where the old view was`);
});

test("a fast pan fills the newly shown side as densely as the rest", () => {
  const p = new Particles(4000, mulberry(6));
  const a = [-100, 35, -90, 41], b = [-97, 35, -87, 41]; // a third of the screen east
  p.step(still(), a, 512 * 64);
  for (let k = 0; k < 5; k++) p.step(still(), a, 512 * 64, a);
  p.step(still(), b, 512 * 64, a);
  const e = evenness(p, b);
  assert.ok(e < 0.25, `evenness ${e.toFixed(2)} after a pan`);
});

test("a trail stops at a segment longer than any one-frame move: the camera stretched it, not the wind", async () => {
  const { Camera } = await import("../src/rust-renderer/geo.js");
  const p = new Particles(1, mulberry(7));
  p.lon[0] = -95;
  p.lat[0] = 40;
  p.histLen[0] = 0;
  p.record(0);
  p.lon[0] = -94.99;
  p.record(0); // a short step
  p.lon[0] = -94.98;
  p.record(0);
  const cam = new Camera((-94.98 + 180) / 360, 0.38, 12, 1200, 800, 0); // zoomed far in: steps are now long
  const out = new Float32Array(TRAIL * 2 * 6);
  const n = trailVertices(p, cam, out);
  for (let v = 0; v < n; v += 2) {
    const len = Math.hypot(out[(v + 1) * 6] - out[v * 6], out[(v + 1) * 6 + 1] - out[v * 6 + 1]);
    assert.ok(len <= 12, `a ${len.toFixed(0)} px segment was drawn`);
  }
  assert.ok(MAX_SEGMENT_PX >= 4 && MAX_SEGMENT_PX <= 12, "the cap sits above the fastest real move (100 m/s is 4 px) and well under a stretched trail");
});

test("a global field wraps: western longitudes and the 0/360 seam sample like anywhere else", () => {
  // 4 columns at 0, 90, 180, 270 east; u = 10, 20, 30, 40 on both rows
  const f = decodeWind(encoded({ ni: 4, nj: 2, lat0: 45, lon0: 0, dlat: 90, dlon: 90 }, [10, 0, 20, 0, 30, 0, 40, 0, 10, 0, 20, 0, 30, 0, 40, 0]));
  assert.deepEqual(sampleWind(f, -90, 0), [40, 0], "90 W is 270 E");
  assert.deepEqual(sampleWind(f, -45, 0), [25, 0], "315 E: halfway between 270 E and 360 = 0 E");
  assert.deepEqual(sampleWind(f, 315, 0), [25, 0]);
  assert.deepEqual(sampleWind(f, 45, 0), [15, 0]);
  assert.equal(sampleWind(f, 45, 60), null, "beyond the last row is still outside");
});

test("particles keep the same density where there is wind when only part of the view has any", () => {
  const p = new Particles(4000, mulberry(8));
  // wind only west of 95 W; the view runs 100 W to 90 W: half of it has wind
  const half = decodeWind(encoded({ ni: 2, nj: 2, lat0: 41, lon0: -100, dlat: 6, dlon: 5 }, new Array(8).fill(0)));
  const view = [-100, 35, -90, 41];
  for (let k = 0; k < 120; k++) p.step(half, view, 512 * 64, view);
  let active = 0, outside = 0;
  for (let i = 0; i < p.n; i++) {
    if (!p.active(i)) continue;
    active += 1;
    if (p.lon[i] > -95) outside += 1;
  }
  assert.equal(outside, 0, "no particle lives where there is no wind");
  assert.ok(Math.abs(active - 2000) < 200, `${active} active for half the view, not all 4000 crowded into it`);
});
