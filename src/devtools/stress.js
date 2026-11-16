// The stress benchmark: as much zooming, panning and turning as the map can be given, from a seeded
// script so a run can be repeated exactly. Camera moves are started before the previous one ends
// (interrupted animations are the normal case for a hand on a trackpad), mixed with wheel bursts and
// drags sent as real DOM events so the input handlers are exercised too, and teleports that land on
// a view nothing has loaded. What it measures is what a user would notice: frames that missed a
// refresh, render time to p99, background showing (with MAPS_PIXEL_CHECK=1), coarser stand-ins,
// listener errors, whether the map settles complete afterwards, and memory across the run.

import { invoke } from "@tauri-apps/api/core";

/** A small, fast, seeded generator (mulberry32): the same seed gives the same script everywhere. */
export function rng(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const KINDS = ["fling", "zoom", "rotate", "combo", "wheel", "drag", "rotdrag", "teleport"];

/**
 * The script: `ops` camera operations inside `region` ({ minLon, minLat, maxLon, maxLat, minZoom,
 * maxZoom }), each with `after`, the milliseconds to wait before the next one starts. `after` is
 * usually shorter than the operation, so most are interrupted. Screen positions are fractions of the
 * viewport (0..1), so a script does not depend on the window size.
 */
export function stressScript(seed, ops, region) {
  const r = rng(seed);
  const between = (a, b) => a + (b - a) * r();
  const pick = (xs) => xs[Math.floor(r() * xs.length)];
  const center = () => [between(region.minLon, region.maxLon), between(region.minLat, region.maxLat)];
  const zoom = () => between(region.minZoom, region.maxZoom);
  const out = [];
  for (let i = 0; i < ops; i++) {
    const kind = pick(KINDS);
    const duration = Math.round(between(150, 900));
    const after = Math.round(duration * between(0.1, 1.2)); // below 1: interrupted
    let op;
    if (kind === "fling") op = { center: center(), duration };
    else if (kind === "zoom") op = { zoom: zoom(), duration };
    else if (kind === "rotate") op = { bearing: between(-180, 180), duration };
    else if (kind === "combo") op = { center: center(), zoom: zoom(), bearing: between(-180, 180), duration };
    else if (kind === "wheel") {
      // a burst of wheel notches at one cursor position, one per frame, mostly one direction
      const dir = r() < 0.5 ? -1 : 1;
      const n = 4 + Math.floor(r() * 20);
      op = { x: between(0.1, 0.9), y: between(0.1, 0.9), deltas: Array.from({ length: n }, () => dir * between(20, 120) * (r() < 0.85 ? 1 : -1)) };
    } else if (kind === "drag" || kind === "rotdrag") {
      op = { x: between(0.2, 0.8), y: between(0.2, 0.8), dx: between(-0.6, 0.6), dy: between(-0.6, 0.6), steps: 4 + Math.floor(r() * 24) };
    } else op = { center: center(), zoom: zoom(), bearing: between(-180, 180) };
    out.push({ kind, ...op, after: kind === "wheel" || kind.endsWith("drag") || kind === "teleport" ? Math.round(between(0, 300)) : after });
  }
  return out;
}

/**
 * Where a stress run may go: the archive's bounds inset by a quarter, and a lowest zoom at which a
 * viewport of w x h CSS px, turned any way, stays inside the data (its half-diagonal fits in the
 * inset margin), so background beyond a cutout's edge is never counted as a missing tile. A
 * world-sized archive (the remote planet) gets a box around `home` instead. Tiles are 512 px.
 */
export function stressRegion(bounds, w, h, home = [-118.4912, 34.0105]) {
  const world = bounds.maxLon - bounds.minLon > 90;
  const b = world
    ? { minLon: home[0] - 0.4, maxLon: home[0] + 0.4, minLat: home[1] - 0.3, maxLat: home[1] + 0.3 }
    : (() => {
        const dx = (bounds.maxLon - bounds.minLon) / 4, dy = (bounds.maxLat - bounds.minLat) / 4;
        return { minLon: bounds.minLon + dx, maxLon: bounds.maxLon - dx, minLat: bounds.minLat + dy, maxLat: bounds.maxLat - dy };
      })();
  let minZoom = 10;
  if (!world) {
    const halfDiag = Math.hypot(w, h) / 2;
    const lat = ((b.minLat + b.maxLat) / 2) * (Math.PI / 180);
    // the narrower margin, in degrees of longitude (a degree of latitude spans 1/cos(lat) as many px)
    const margin = Math.min((bounds.maxLon - bounds.minLon) / 4, (bounds.maxLat - bounds.minLat) / 4 / Math.cos(lat));
    minZoom = Math.max(minZoom, Math.log2((halfDiag * 360) / (512 * margin)));
  }
  // rounded up: rounding down would let the view reach past the margin
  return { ...b, minZoom: Math.ceil(minZoom * 100) / 100, maxZoom: Math.max(16.5, minZoom + 1) };
}

const frameOf = () => new Promise((r) => requestAnimationFrame(r));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const once = (map, ev) => new Promise((res) => map.once(ev, res));
const pct = (xs, p) => {
  if (!xs.length) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const r1 = (x) => Math.round(x * 10) / 10;
const dist = (xs) => ({ n: xs.length, avg: r1(xs.reduce((a, b) => a + b, 0) / Math.max(1, xs.length)), p50: r1(pct(xs, 50)), p95: r1(pct(xs, 95)), p99: r1(pct(xs, 99)), worst: r1(xs.length ? Math.max(...xs) : 0) });
const totalMb = async () => {
  const s = await invoke("dev_stats");
  return Math.round(s.memory.reduce((a, m) => a + m.pss_kb, 0) / 1024);
};

/** Mouse and wheel events sent the way a hand sends them: down on the canvas, moves and up on the
 *  document (they bubble to the window; sent to the window they would never reach a document listener,
 *  which is where MapLibre listens). */
async function sendDrag(canvas, op, button) {
  const b = canvas.getBoundingClientRect();
  const at = (k) => ({ clientX: b.left + (op.x + op.dx * k) * b.width, clientY: b.top + (op.y + op.dy * k) * b.height, button, buttons: button === 2 ? 2 : 1, bubbles: true });
  canvas.dispatchEvent(new MouseEvent("mousedown", at(0)));
  for (let i = 1; i <= op.steps; i++) {
    await frameOf();
    document.dispatchEvent(new MouseEvent("mousemove", at(i / op.steps)));
  }
  document.dispatchEvent(new MouseEvent("mouseup", { ...at(1), buttons: 0 }));
}
async function sendWheel(canvas, op) {
  const b = canvas.getBoundingClientRect();
  for (const deltaY of op.deltas) {
    canvas.dispatchEvent(new WheelEvent("wheel", { clientX: b.left + op.x * b.width, clientY: b.top + op.y * b.height, deltaY, deltaMode: 0, bubbles: true, cancelable: true }));
    await frameOf();
  }
}

/**
 * Run `script` against `map`. Returns the measurements; with the pixel check on, the worst frame is
 * saved as logs/snapshot-<name>.png.
 */
export async function runStress(map, stats, script, { name = "stress", pixelCheck = false } = {}) {
  const rec = { frame: [], render: [], labelMs: [], gray: 0, area: [], missing: 0, coarse: 0, rendered: 0 };
  let worst = { f: 0 };
  stats.onPixels = pixelCheck ? (f, data, gl) => {
    if (f <= worst.f) return;
    const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
    const flipped = new Uint8ClampedArray(w * h * 4);
    for (let y = 0; y < h; y++) flipped.set(data.subarray((h - 1 - y) * w * 4, (h - y) * w * 4), y * w * 4);
    worst = { f, w, h, flipped };
  } : null;
  stats.tap = (kind, v) => {
    if (kind !== "frame") return;
    rec.frame.push(v.interval);
    if (!v.rendered) return;
    rec.rendered += 1;
    rec.render.push(v.renderMs);
    if (map.lastDrawn?.labelMs !== undefined) rec.labelMs.push(map.lastDrawn.labelMs);
    if (v.missing) rec.missing += 1;
    if (map.lastDrawn?.ancestor) rec.coarse += 1;
    if (pixelCheck) {
      rec.area.push(v.pixelGray * 100);
      if (v.pixelGray > 0.001) rec.gray += 1;
    }
  };
  const errors0 = map.listenerErrors ?? 0;
  const uncaught0 = window.__uncaughtErrors ?? 0;
  const csp0 = window.__cspViolations ?? 0;
  const canvas = map.getCanvas();
  const memory = [{ t_s: 0, mb: await totalMb() }];
  const t0 = performance.now();
  let nextMem = 5000;
  const counts = {};
  const opErrors = [];
  let firstBadView = null;
  const noEffect = {}; // input events the map ignored: a synthetic event the handlers do not accept
  const view = () => {
    const c = map.getCenter();
    return [c.lng, c.lat, map.getZoom(), map.getBearing()];
  };
  for (const op of script) {
    counts[op.kind] = (counts[op.kind] ?? 0) + 1;
    const before = ["wheel", "drag", "rotdrag"].includes(op.kind) ? view() : null;
    try {
      // only the keys the operation sets: MapLibre tests `'bearing' in options`, so an explicit
      // undefined becomes a NaN bearing and every later frame throws
      const camera = Object.fromEntries(["center", "zoom", "bearing"].filter((k) => op[k] !== undefined).map((k) => [k, op[k]]));
      if (op.kind === "teleport") map.jumpTo(camera);
      else if (op.kind === "wheel") await sendWheel(canvas, op);
      else if (op.kind === "drag") await sendDrag(canvas, op, 0);
      else if (op.kind === "rotdrag") await sendDrag(canvas, op, 2);
      else map.easeTo({ ...camera, duration: op.duration, easing: (t) => t });
    } catch (e) {
      // a camera call that throws is a finding, not the end of the run: count it, keep the first few
      opErrors.push({ index: script.indexOf(op), op, view: view(), error: String(e?.message ?? e) });
    }
    if (before) {
      // an input event may only change one thing (a wheel the zoom, a right-drag the bearing)
      await frameOf();
      if (view().every((v, i) => Math.abs(v - before[i]) < 1e-9)) noEffect[op.kind] = (noEffect[op.kind] ?? 0) + 1;
    }
    await sleep(op.after);
    // the camera must stay finite; the first operation after which it is not is the one to look at
    if (!firstBadView && !view().every(Number.isFinite)) firstBadView = { index: script.indexOf(op), op, view: view() };
    if (performance.now() - t0 >= nextMem) {
      memory.push({ t_s: r1((performance.now() - t0) / 1000), mb: await totalMb() });
      nextMem += 5000;
    }
  }
  const chaosMs = performance.now() - t0;
  stats.tap = null;
  // settle: the last move ends, then everything in view loads
  const t1 = performance.now();
  await sleep(1000);
  const settled = await Promise.race([
    (async () => {
      if (!map.areTilesLoaded()) await once(map, "idle");
      return true;
    })(),
    sleep(30000).then(() => false),
  ]);
  const settleMs = performance.now() - t1;
  let rest = null;
  if (pixelCheck) {
    rest = 0;
    stats.tap = (kind, v) => {
      if (kind === "frame" && v.rendered) rest = Math.max(rest, v.pixelGray * 100);
    };
    map.triggerRepaint();
    await frameOf();
    await frameOf();
    stats.tap = null;
  }
  stats.onPixels = null;
  memory.push({ t_s: r1((performance.now() - t0) / 1000), mb: await totalMb() });
  await sleep(10000); // what the run added and the collector gives back
  memory.push({ t_s: r1((performance.now() - t0) / 1000), mb: await totalMb() });
  let saved = null;
  if (worst.flipped) {
    const c = document.createElement("canvas");
    c.width = worst.w;
    c.height = worst.h;
    c.getContext("2d").putImageData(new ImageData(worst.flipped, worst.w, worst.h), 0, 0);
    saved = await invoke("save_snapshot", { name, dataUrl: c.toDataURL("image/png") });
  }
  const budget = 1000 / 60;
  return {
    ops: script.length, op_counts: counts, input_no_effect: noEffect, chaos_s: r1(chaosMs / 1000),
    frames: rec.frame.length, rendered: rec.rendered,
    // over 1.5 budgets: a refresh really missed (WebKit rounds performance.now() to whole ms)
    dropped: rec.frame.filter((f) => f > 1.5 * budget).length,
    long_frames_over_50ms: rec.frame.filter((f) => f > 50).length,
    frame_interval: dist(rec.frame),
    render: dist(rec.render),
    labels: rec.labelMs.length ? dist(rec.labelMs) : null,
    stand_in_frames: rec.missing,
    coarse_frames: rec.coarse,
    pixel: pixelCheck ? { gray_frames: rec.gray, gray_pct: r1((100 * rec.gray) / Math.max(1, rec.rendered)), area_pct: dist(rec.area), worst_frame: saved, at_rest_pct: rest === null ? null : r1(rest) } : null,
    settled, settle_ms: Math.round(settleMs),
    listener_errors: (map.listenerErrors ?? 0) - errors0,
    op_errors: opErrors.length,
    first_bad_view: firstBadView,
    op_error_examples: opErrors.slice(0, 5),
    uncaught_errors: (window.__uncaughtErrors ?? 0) - uncaught0,
    csp_violations: (window.__cspViolations ?? 0) - csp0,
    memory_mb: memory,
    final_view: { center: map.getCenter(), zoom: r1(map.getZoom()), bearing: r1(map.getBearing()) },
  };
}
