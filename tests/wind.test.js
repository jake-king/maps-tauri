import test from "node:test";
import assert from "node:assert/strict";
import { decodeWind, sampleWind, Particles, PX_PER_MPS, speedColor, TRAIL, trailVertices } from "../src/rust-renderer/wind.js";

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

