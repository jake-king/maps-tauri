import { test } from "node:test";
import assert from "node:assert/strict";
import { lngLatToWorld, worldToLngLat, Camera, visibleTiles, ringTiles, tileRect, ancestor, interpolate, parseMeshHeader, tileZoom } from "../src/rust-renderer/geo.js";

const close = (a, b, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} vs ${b}`);

test("mercator: known points and a round trip", () => {
  const [x, y] = lngLatToWorld(0, 0);
  close(x, 0.5);
  close(y, 0.5);
  const [, yn] = lngLatToWorld(0, 85.0511287798);
  close(yn, 0, 1e-9);
  const [lng, lat] = worldToLngLat(...lngLatToWorld(-118.45, 34.02));
  close(lng, -118.45);
  close(lat, 34.02);
});

test("the z14 tile under Santa Monica matches the archive's own tile (2801, 6543)", () => {
  const [x, y] = lngLatToWorld(-118.45, 34.025);
  assert.deepEqual([Math.floor(x * 2 ** 14), Math.floor(y * 2 ** 14)], [2801, 6543]);
});

test("camera: screen and world invert; scale doubles per zoom", () => {
  const c = new Camera(0.3, 0.4, 12.5, 1200, 800);
  const [px, py] = c.worldToScreen(0.3001, 0.3999);
  const [wx, wy] = c.screenToWorld(px, py);
  close(wx, 0.3001);
  close(wy, 0.3999);
  assert.deepEqual(c.worldToScreen(0.3, 0.4), [600, 400]);
  close(new Camera(0, 0, 13.5, 1, 1).scale / c.scale, 2);
});

test("visible tiles: the right zoom, the whole view, nearest first", () => {
  const cam = new Camera(...lngLatToWorld(-118.45, 34.025), 14.3, 1200, 800);
  const tiles = visibleTiles(cam, 0, 14);
  assert.ok(tiles.every(([z]) => z === 14));
  assert.deepEqual(tiles[0], [14, 2801, 6543]);
  // the tiles' rectangles cover the whole viewport
  for (const [px, py] of [[0, 0], [1199, 0], [0, 799], [1199, 799], [600, 400]]) {
    assert.ok(tiles.some(([z, x, y]) => {
      const r = tileRect(cam, z, x, y);
      return px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h;
    }), `pixel ${px},${py} uncovered`);
  }
  // overzoom: past the archive's max zoom the tiles stay at 14 and get bigger
  const deep = visibleTiles(new Camera(cam.cx, cam.cy, 17.2, 1200, 800), 0, 14);
  assert.ok(deep.every(([z]) => z === 14) && deep.length <= 4);
  assert.equal(tileZoom(3.9, 0, 14), 3);
});

test("the prefetch ring: the tiles around the view, none of the visible ones", () => {
  const cam = new Camera(...lngLatToWorld(-118.45, 34.025), 14.3, 1200, 800);
  const vis = visibleTiles(cam, 0, 14).map((t) => t.join());
  const ring = ringTiles(cam, 0, 14, 1);
  const xs = visibleTiles(cam, 0, 14).map((t) => t[1]), ys = visibleTiles(cam, 0, 14).map((t) => t[2]);
  const w = Math.max(...xs) - Math.min(...xs) + 1, h = Math.max(...ys) - Math.min(...ys) + 1;
  assert.equal(ring.length, (w + 2) * (h + 2) - w * h);
  assert.ok(ring.every((t) => !vis.includes(t.join())));
  assert.ok(ring.every(([z, x, y]) => z === 14 && x >= Math.min(...xs) - 1 && x <= Math.max(...xs) + 1 && y >= Math.min(...ys) - 1 && y <= Math.max(...ys) + 1));
  assert.deepEqual(ringTiles(cam, 0, 14, 0), []);
  // at the edge of the world the ring is clipped, not negative
  assert.ok(ringTiles(new Camera(0.0001, 0.0001, 3, 100, 100), 0, 14, 1).every(([, x, y]) => x >= 0 && y >= 0));
});

test("a z14 tile is 512 px at zoom 14 and 1024 at zoom 15", () => {
  const r = tileRect(new Camera(0.5, 0.5, 14, 100, 100), 14, 0, 0);
  close(r.w, 512);
  close(tileRect(new Camera(0.5, 0.5, 15, 100, 100), 14, 0, 0).w, 1024);
});

test("ancestors", () => {
  assert.deepEqual(ancestor(14, 2801, 6543, 12), [12, 700, 1635]);
  assert.deepEqual(ancestor(14, 2801, 6543, 14), [14, 2801, 6543]);
});

test("exponential interpolation matches MapLibre's formula", () => {
  const s = [[11, 0], [12.5, 0.5], [15, 2], [18, 11]];
  assert.equal(interpolate(s, 1.6, 5), 0);
  assert.equal(interpolate(s, 1.6, 20), 11);
  assert.equal(interpolate(s, 1.6, 15), 2);
  close(interpolate(s, 1.6, 16.5), 2 + ((1.6 ** 1.5 - 1) / (1.6 ** 3 - 1)) * 9);
  close(interpolate([[0, 0], [10, 10]], 1, 2.5), 2.5);
});

function meshBuffer(fillV, fillI, lineV, lineI, flags = 0) {
  const pad4 = (n) => (n + 3) & ~3;
  const iw = (wide) => (wide ? 4 : 2);
  const size = 28 + pad4(fillV * 6) + pad4(fillI * iw(flags & 1)) + lineV * 8 + pad4(lineI * iw(flags & 2));
  const b = new ArrayBuffer(size);
  const v = new DataView(b);
  "MESH".split("").forEach((c, i) => v.setUint8(i, c.charCodeAt(0)));
  [2, flags, fillV, fillI, lineV, lineI].forEach((n, i) => v.setUint32(4 + 4 * i, n, true));
  return b;
}

test("mesh header: offsets follow the wire format, padding included", () => {
  const h = parseMeshHeader(meshBuffer(3, 3, 4, 6));
  assert.deepEqual([h.fillVerts, h.fillIdx, h.lineVerts, h.lineIdx], [28, 48, 56, 88]);
  const w = parseMeshHeader(meshBuffer(1, 3, 2, 3, 3));
  assert.deepEqual([w.fillWide, w.lineWide, w.fillIdx, w.lineVerts, w.lineIdx], [true, true, 36, 48, 64]);
});

test("mesh header refuses bad input", () => {
  assert.throws(() => parseMeshHeader(new ArrayBuffer(10)), /shorter/);
  const b = meshBuffer(1, 0, 0, 0);
  new DataView(b).setUint8(0, 88);
  assert.throws(() => parseMeshHeader(b), /magic/);
  const c = meshBuffer(1, 0, 0, 0);
  new DataView(c).setUint32(4, 1, true);
  assert.throws(() => parseMeshHeader(c), /version/);
  assert.throws(() => parseMeshHeader(meshBuffer(1, 0, 0, 0).slice(0, 32)), /header says/);
});
