import { test } from "node:test";
import assert from "node:assert/strict";
import { GpuLedger, bytesPerPixel } from "../src/devtools/gpumem.js";
import { arrayBytes, tileBytesByField } from "../src/devtools/tilemem.js";

const ARRAY = 0x8892, TEX = 0x0de1, RGBA = 0x1908, ALPHA = 0x1906;

test("buffers: bytes follow the bound buffer, resize replaces, delete subtracts", () => {
  const g = new GpuLedger();
  const a = {}, b = {};
  g.bindBuffer(ARRAY, a);
  g.bufferData(ARRAY, new ArrayBuffer(100));
  g.bindBuffer(ARRAY, b);
  g.bufferData(ARRAY, 60);
  assert.deepEqual(g.snapshot().buffers, { bytes: 160, count: 2 });
  g.bindBuffer(ARRAY, a);
  g.bufferData(ARRAY, new Uint16Array(10)); // 20 bytes replaces 100
  assert.deepEqual(g.snapshot().buffers, { bytes: 80, count: 2 });
  g.deleteBuffer(b);
  g.deleteBuffer(b); // twice is harmless
  assert.deepEqual(g.snapshot().buffers, { bytes: 20, count: 1 });
  g.bindBuffer(ARRAY, null);
  g.bufferData(ARRAY, 999); // nothing bound: not counted
  assert.equal(g.snapshot().buffers.bytes, 20);
});

test("textures: per unit binding, per level, by format", () => {
  const g = new GpuLedger();
  const t0 = {}, t1 = {};
  g.activeTexture(0);
  g.bindTexture(TEX, t0);
  g.activeTexture(1);
  g.bindTexture(TEX, t1);
  g.texLevel(TEX, 0, ALPHA, 10, 10); // goes to t1, the texture bound on unit 1
  g.activeTexture(0);
  g.texLevel(TEX, 0, RGBA, 4, 4); // t0: 64
  g.texLevel(TEX, 1, RGBA, 2, 2); // t0 level 1: 16
  g.texLevel(TEX, 0, RGBA, 4, 4); // redefining level 0 does not double it
  assert.deepEqual(g.snapshot().textures, { bytes: 100 + 64 + 16, count: 2 });
  g.deleteTexture(t0);
  assert.deepEqual(g.snapshot().textures, { bytes: 100, count: 1 });
});

test("texStorage sums its mip chain; formats have their sizes", () => {
  const g = new GpuLedger();
  g.bindTexture(TEX, {});
  g.texStorage(TEX, 3, 0x8058, 8, 4); // 8x4 + 4x2 + 2x1, RGBA8
  assert.equal(g.snapshot().textures.bytes, (32 + 8 + 2) * 4);
  assert.equal(bytesPerPixel(0x881a), 8);
  assert.equal(bytesPerPixel(12345), 4);
});

test("programs: linked ones counted once, deletes remove", () => {
  const g = new GpuLedger();
  const p = {}, q = {};
  g.linkProgram(p);
  g.linkProgram(p);
  g.linkProgram(q);
  g.deleteProgram(p);
  assert.equal(g.snapshot().programs, 1);
});

test("renderbuffers count samples", () => {
  const g = new GpuLedger();
  g.bindRenderbuffer({});
  g.renderbufferStorage(0x88f0, 10, 10, 4);
  assert.deepEqual(g.snapshot().renderbuffers, { bytes: 1600, count: 1 });
});

test("arrayBytes counts each buffer once, through views, maps and nesting", () => {
  const shared = new ArrayBuffer(50);
  const obj = { a: new Uint8Array(shared), b: new Float32Array(shared, 0, 2), c: { d: [new ArrayBuffer(10)] },
                m: new Map([["k", new ArrayBuffer(7)]]), context: new ArrayBuffer(1000) /* skipped key */ };
  assert.equal(arrayBytes(obj, new Set()), 67);
  const cyc = { x: new ArrayBuffer(3) };
  cyc.self = cyc;
  assert.equal(arrayBytes(cyc, new Set()), 3);
});

test("tileBytesByField groups by field and dedupes across tiles", () => {
  const raw = new ArrayBuffer(100);
  const t1 = { latestRawTileData: raw, buckets: { roads: { arr: new ArrayBuffer(30) } }, uid: 1 };
  const t2 = { latestRawTileData: raw, buckets: { water: new ArrayBuffer(20) } };
  assert.deepEqual(tileBytesByField([t1, t2]), { latestRawTileData: 100, buckets: 50 });
});
