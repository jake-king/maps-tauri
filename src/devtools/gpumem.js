// The GPU ledger: bytes the app asked WebGL for, counted at the moment of allocation, the way
// guile-2d's profiler counts bytes per block. MapLibre frees each buffer's CPU copy after upload
// and an index buffer does not keep its size, so the only exact place to count is the GL call.
//
// What it sees: buffers (bufferData), textures (texImage2D / texStorage2D, per mip level) and
// renderbuffers, live (deletes subtract). What it cannot see: the driver's own overhead and the
// canvas's default framebuffer (estimated separately).

// bytes per pixel for the formats MapLibre uses; anything unknown counts as 4 (RGBA8)
const BPP = {
  0x1906: 1, // ALPHA
  0x1909: 1, // LUMINANCE
  0x190a: 2, // LUMINANCE_ALPHA
  0x1907: 3, // RGB
  0x1908: 4, // RGBA
  0x8229: 1, // R8
  0x822b: 2, // RG8
  0x8058: 4, // RGBA8
  0x881a: 8, // RGBA16F
  0x8814: 16, // RGBA32F
  0x81a5: 2, // DEPTH_COMPONENT16
  0x88f0: 4, // DEPTH24_STENCIL8
  0x84f9: 4, // DEPTH_STENCIL
  0x8d48: 1, // STENCIL_INDEX8
};
export const bytesPerPixel = (format) => BPP[format] ?? 4;

/** The ledger's bookkeeping, independent of WebGL so it can be tested with plain objects. */
export class GpuLedger {
  constructor() {
    this.sizes = { buffer: new Map(), texture: new Map(), renderbuffer: new Map() };
    this.bytes = { buffer: 0, texture: 0, renderbuffer: 0 };
    this.bound = { buffer: new Map(), renderbuffer: null };
    this.textureUnits = new Map(); // unit -> Map(target -> texture)
    this.unit = 0;
    this.programs = new Set(); // linked shader programs, live: the driver keeps each compiled
  }
  linkProgram(p) {
    this.programs.add(p);
  }
  deleteProgram(p) {
    this.programs.delete(p);
  }
  _set(kind, obj, n) {
    const m = this.sizes[kind];
    this.bytes[kind] += n - (m.get(obj) ?? 0);
    m.set(obj, n);
  }
  _delete(kind, obj) {
    const m = this.sizes[kind];
    if (!m.has(obj)) return;
    this.bytes[kind] -= m.get(obj);
    m.delete(obj);
  }
  bindBuffer(target, buf) {
    this.bound.buffer.set(target, buf);
  }
  bufferData(target, sizeOrData) {
    const buf = this.bound.buffer.get(target);
    if (!buf) return;
    const n = typeof sizeOrData === "number" ? sizeOrData : (sizeOrData?.byteLength ?? 0);
    this._set("buffer", buf, n);
  }
  deleteBuffer(buf) {
    this._delete("buffer", buf);
  }
  activeTexture(unit) {
    this.unit = unit;
  }
  bindTexture(target, tex) {
    if (!this.textureUnits.has(this.unit)) this.textureUnits.set(this.unit, new Map());
    this.textureUnits.get(this.unit).set(target, tex);
  }
  _boundTexture(target) {
    return this.textureUnits.get(this.unit)?.get(target) ?? null;
  }
  /** One mip level of the bound texture is (re)defined as w x h of `format`. */
  texLevel(target, level, format, w, h) {
    const tex = this._boundTexture(target);
    if (!tex) return;
    if (!this.levels) this.levels = new Map();
    if (!this.levels.has(tex)) this.levels.set(tex, new Map());
    const lv = this.levels.get(tex);
    lv.set(level, w * h * bytesPerPixel(format));
    let total = 0;
    for (const n of lv.values()) total += n;
    this._set("texture", tex, total);
  }
  /** Immutable storage: `levels` levels, each half the size of the last. */
  texStorage(target, levels, format, w, h) {
    let total = 0;
    for (let i = 0; i < levels; i++) total += Math.max(1, w >> i) * Math.max(1, h >> i) * bytesPerPixel(format);
    const tex = this._boundTexture(target);
    if (tex) this._set("texture", tex, total);
  }
  deleteTexture(tex) {
    this._delete("texture", tex);
    this.levels?.delete(tex);
  }
  bindRenderbuffer(rb) {
    this.bound.renderbuffer = rb;
  }
  renderbufferStorage(format, w, h, samples = 1) {
    if (this.bound.renderbuffer) this._set("renderbuffer", this.bound.renderbuffer, w * h * bytesPerPixel(format) * samples);
  }
  deleteRenderbuffer(rb) {
    this._delete("renderbuffer", rb);
  }
  snapshot() {
    return {
      buffers: { bytes: this.bytes.buffer, count: this.sizes.buffer.size },
      textures: { bytes: this.bytes.texture, count: this.sizes.texture.size },
      renderbuffers: { bytes: this.bytes.renderbuffer, count: this.sizes.renderbuffer.size },
      programs: this.programs.size,
    };
  }
}

/**
 * Wrap a WebGL prototype's allocation calls so every context made afterwards reports to `ledger`.
 * Must run before the map creates its context. The wrappers only record and forward.
 */
export function installGpuLedger(proto, ledger) {
  if (proto.__gpuLedger) return proto.__gpuLedger;
  const wrap = (name, record) => {
    const original = proto[name];
    if (typeof original !== "function") return;
    proto[name] = function (...args) {
      const out = original.apply(this, args);
      record(...args);
      return out;
    };
  };
  wrap("bindBuffer", (t, b) => ledger.bindBuffer(t, b));
  wrap("bufferData", (t, d) => ledger.bufferData(t, d));
  wrap("deleteBuffer", (b) => ledger.deleteBuffer(b));
  wrap("activeTexture", (u) => ledger.activeTexture(u));
  wrap("bindTexture", (t, x) => ledger.bindTexture(t, x));
  wrap("texImage2D", (target, level, internalformat, ...rest) => {
    // (t, level, ifmt, w, h, border, fmt, type, pixels) or (t, level, ifmt, fmt, type, source)
    if (rest.length >= 5) ledger.texLevel(target, level, internalformat, rest[0], rest[1]);
    else {
      const src = rest[2];
      const w = src?.width ?? src?.videoWidth ?? 0;
      const h = src?.height ?? src?.videoHeight ?? 0;
      ledger.texLevel(target, level, internalformat, w, h);
    }
  });
  wrap("texStorage2D", (t, levels, f, w, h) => ledger.texStorage(t, levels, f, w, h));
  wrap("deleteTexture", (x) => ledger.deleteTexture(x));
  wrap("bindRenderbuffer", (_t, rb) => ledger.bindRenderbuffer(rb));
  wrap("renderbufferStorage", (_t, f, w, h) => ledger.renderbufferStorage(f, w, h));
  wrap("renderbufferStorageMultisample", (_t, s, f, w, h) => ledger.renderbufferStorage(f, w, h, s));
  wrap("deleteRenderbuffer", (rb) => ledger.deleteRenderbuffer(rb));
  wrap("linkProgram", (p) => ledger.linkProgram(p));
  wrap("deleteProgram", (p) => ledger.deleteProgram(p));
  proto.__gpuLedger = ledger;
  return ledger;
}
