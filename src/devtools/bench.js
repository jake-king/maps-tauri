// The pan benchmark: a fixed lawnmower path over the archive's bounds at zoom 15, at a hand-fast
// pan speed, run twice (cold, then warm with every tile cached) so a change to tile loading can be
// measured instead of eyeballed. F6 runs it; MAPS_BENCH=1 runs it at startup.

import { invoke } from "@tauri-apps/api/core";

const ZOOM = 15;
let SPEED_PX_S = 4000; // a fast hand pan; MAPS_BENCH_SPEED overrides it (a stress test)
export const setBenchSpeed = (v) => {
  if (v > 0) SPEED_PX_S = v;
};
const ROWS = 4;

const pct = (xs, p) => {
  if (xs.length === 0) return 0;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
};
const avg = (xs) => (xs.length ? xs.reduce((a, b) => a + b, 0) / xs.length : 0);
const r1 = (x) => Math.round(x * 10) / 10;
const summary = (xs) => ({ n: xs.length, avg: r1(avg(xs)), p95: r1(pct(xs, 95)), worst: r1(xs.length ? Math.max(...xs) : 0) });

const once = (map, ev) => new Promise((res) => map.once(ev, res));

/** The path's waypoints: ROWS passes west->east and back, inset from the bounds. */
export function lawnmower(b, rows = ROWS, inset = 0.15) {
  const dx = (b.maxLon - b.minLon) * inset;
  const dy = (b.maxLat - b.minLat) * inset;
  const pts = [];
  for (let i = 0; i < rows; i++) {
    const lat = b.maxLat - dy - ((b.maxLat - b.minLat - 2 * dy) * i) / Math.max(1, rows - 1);
    const [a, z] = i % 2 === 0 ? [b.minLon + dx, b.maxLon - dx] : [b.maxLon - dx, b.minLon + dx];
    pts.push([a, lat], [z, lat]);
  }
  return pts;
}

async function pass(map, stats, path) {
  const rec = { frame: [], render: [], labelMs: [], labels: [], missing: 0, rendered: 0, fetch: [], load: [], visible: [], tiles: new Set(),
                grayFrames: 0, grayTiles: 0, neededTiles: 0,
                pix: { frames: 0, area: [], both: 0, tileOnly: 0, pixelOnly: 0 } };
  const onLoading = (e) => e.tile && e.sourceId === "protomaps" && rec.tiles.add(e.tile.tileID.key);
  map.on("dataloading", onLoading);
  stats.tap = (kind, v) => {
    if (kind === "frame") {
      rec.frame.push(v.interval);
      if (v.rendered) {
        rec.rendered += 1;
        rec.render.push(v.renderMs);
        if (map.lastDrawn?.labelMs !== undefined) {
          rec.labelMs.push(map.lastDrawn.labelMs);
          rec.labels.push(map.lastDrawn.labels);
          (rec.labelFrames ??= []).push({ ms: map.lastDrawn.labelMs, misses: map.lastDrawn.labelMisses, firstSprite: map.lastDrawn.firstSprite, labels: map.lastDrawn.labels });
        }
        if (v.missing) rec.missing += 1;
        if (v.gray > 0) rec.grayFrames += 1;
        rec.grayTiles += v.gray;
        rec.neededTiles += v.needed;
        const pixGray = v.pixelGray > 0.001; // more than 0.1% of the frame shows background
        if (pixGray) rec.pix.frames += 1;
        rec.pix.area.push(v.pixelGray * 100);
        if (pixGray && v.gray > 0) rec.pix.both += 1;
        else if (v.gray > 0) rec.pix.tileOnly += 1;
        else if (pixGray) rec.pix.pixelOnly += 1;
      }
    } else rec[kind].push(v);
  };
  await invoke("dev_stats"); // resets the backend's tile window
  const t0 = performance.now();
  for (const p of path.slice(1)) {
    const from = map.project(map.getCenter());
    const to = map.project(p);
    const px = Math.hypot(to.x - from.x, to.y - from.y);
    map.easeTo({ center: p, zoom: ZOOM, duration: (px / SPEED_PX_S) * 1000, easing: (t) => t });
    await once(map, "moveend");
  }
  const movingMs = performance.now() - t0;
  if (!map.areTilesLoaded()) await once(map, "idle");
  const settleMs = performance.now() - t0 - movingMs;
  stats.tap = null;
  map.off("dataloading", onLoading);
  const backend = await invoke("dev_stats");
  const t = backend.tiles;
  return {
    moving_s: r1(movingMs / 1000),
    settle_ms: Math.round(settleMs),
    frames: summary(rec.frame),
    // A missed vsync, not jitter: WebKit rounds performance.now() to whole ms, so a perfect
    // 16.7 ms frame can measure 18. Over 1.5 budgets is a frame that really missed a refresh.
    dropped: rec.frame.filter((f) => f > 1.5 * (1000 / 60)).length,
    jitter_only: rec.frame.filter((f) => f > 1000 / 60 + 1 && f <= 1.5 * (1000 / 60)).length,
    render: summary(rec.render),
    rendered: rec.rendered,
    missing_frames: rec.missing,
    missing_pct: r1((100 * rec.missing) / Math.max(1, rec.rendered)),
    // the honest gray-box measure: frames where some needed tile had NOTHING drawn over it
    gray_frames: rec.grayFrames,
    gray_pct: r1((100 * rec.grayFrames) / Math.max(1, rec.rendered)),
    // ground truth when MAPS_PIXEL_CHECK=1: frames with background visible, and agreement with
    // the tile measure frame by frame
    pixel: {
      gray_frames: rec.pix.frames,
      gray_pct: r1((100 * rec.pix.frames) / Math.max(1, rec.rendered)),
      area_pct: summary(rec.pix.area),
      agree_gray: rec.pix.both,
      tile_says_gray_pixels_clean: rec.pix.tileOnly,
      pixels_gray_tile_says_clean: rec.pix.pixelOnly,
    },
    fetch: summary(rec.fetch),
    load: summary(rec.load),
    serve: { n: t.requests, avg: r1(t.requests ? t.total_ms / t.requests : 0), worst: r1(t.worst_ms), bytes: t.bytes },
    // the native renderer's mesh handler, by stage (ms per request, averaged)
    stages: (() => {
      const s = backend.stages;
      if (!s || !s.requests) return null;
      const per = (k) => Math.round((s[k] / s.requests) * 100) / 100;
      return { n: s.requests, wait: per("wait_ms"), read: per("read_ms"), decode: per("decode_ms"), mesh: per("mesh_ms"),
               serialize: per("serialize_ms"), copy: per("copy_ms"), inflight_avg: per("inflight_sum") / 1, inflight_max: s.inflight_max };
    })(),
    // every display frame should have been rendered while the camera moves; far fewer means the
    // render loop stalled (as it did when a listener threw), and every figure above is suspect
    rendered_ratio: r1(rec.rendered / Math.max(1, rec.frame.length)),
    listener_errors: map.listenerErrors ?? 0,
    // the native renderer: how long each tile was on screen before it could be drawn (0 = ready in time)
    visible_wait: rec.visible.length ? { ...summary(rec.visible), not_ready: rec.visible.filter((v) => v > 0).length } : null,
    mesh_cache: backend.mesh_cache ?? null,
    label_draw: rec.labelMs.length ? { ...summary(rec.labelMs), labels_avg: r1(avg(rec.labels)),
      slowest: [...(rec.labelFrames ?? [])].sort((a, b) => b.ms - a.ms).slice(0, 5).map((f) => ({ ...f, ms: r1(f.ms) })) } : null,
    tiles_loaded: rec.load.length,
    unique_tiles: rec.tiles.size,
    cache_max: map.style?.tileManagers?.protomaps?._outOfViewCache?.max ?? null,
    memory_mb: Math.round(backend.memory.reduce((a, m) => a + m.pss_kb, 0) / 1024),
  };
}

/**
 * The pixel check's own control: with every layer but the background hidden, the frame must read
 * as 100% background. Returns the magenta percentage measured, or 0 if the check is blind.
 */
async function pixelSelfTest(map, stats) {
  let worst = 0;
  stats.tap = (kind, v) => {
    if (kind === "frame" && v.rendered) worst = Math.max(worst, v.pixelGray);
  };
  const hidden = map.getStyle().layers.filter((l) => l.type !== "background").map((l) => l.id);
  for (const id of hidden) map.setLayoutProperty(id, "visibility", "none");
  await once(map, "idle");
  map.triggerRepaint();
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  for (const id of hidden) map.setLayoutProperty(id, "visibility", "visible");
  await once(map, "idle");
  stats.tap = null;
  return r1(worst * 100);
}

/**
 * Soak: the pan path `passes` times in a row, recording total memory (PSS, MB) after each. With a
 * cache too small to hold the path, every pass decodes every tile again. A high-water mark levels
 * off after the first pass or two; a leak keeps climbing.
 */
export async function runSoak(map, stats, bounds, passes) {
  const path = lawnmower(bounds);
  const mb = async () => {
    const s = await invoke("dev_stats");
    return Math.round(s.memory.reduce((a, m) => a + m.pss_kb, 0) / 1024);
  };
  const after = [await mb()];
  for (let i = 0; i < passes; i++) {
    map.jumpTo({ center: path[0], zoom: ZOOM });
    if (!map.areTilesLoaded()) await once(map, "idle");
    const r = await pass(map, stats, path);
    after.push(await mb());
    await invoke("report", { msg: `soak pass ${i + 1}/${passes}: ${after.at(-1)} MB, ${r.load.n} tiles decoded` });
  }
  return after;
}

/**
 * The zoom benchmark: at a fixed center, zoom out 15 -> 10, then in 10 -> 16, each over 1.2 s,
 * waiting for the map to settle after each leg. With the pixel check it reports, per leg, the
 * background visible during the motion (avg and worst share of the screen, frames with any) and at
 * rest after it, and saves the worst frame of the run as logs/snapshot-<name>.png.
 */
export async function runZoomBench(map, stats, center, name) {
  let worst = { f: 0, png: null };
  stats.onPixels = (f, data, gl) => {
    if (f <= worst.f) return;
    const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
    const flipped = new Uint8ClampedArray(w * h * 4);
    for (let y = 0; y < h; y++) flipped.set(data.subarray((h - 1 - y) * w * 4, (h - y) * w * 4), y * w * 4);
    worst = { f, w, h, flipped };
  };
  const legs = [];
  map.jumpTo({ center, zoom: 15 });
  if (!map.areTilesLoaded()) await once(map, "idle");
  for (const [from, to] of [[15, 10], [10, 16]]) {
    map.notReady = {};
    const rec = { area: [], frames: 0, gray: 0, fallback: 0, coarse: 0 };
    stats.tap = (kind, v) => {
      if (kind !== "frame" || !v.rendered) return;
      rec.frames += 1;
      rec.area.push(v.pixelGray * 100);
      if (v.pixelGray > 0.001) rec.gray += 1;
      if (v.missing) rec.fallback += 1;
      if (map.lastDrawn?.ancestor) rec.coarse += 1; // a coarser stand-in drawn: detail missing
    };
    map.easeTo({ center, zoom: to, duration: 1200, easing: (t) => t });
    await once(map, "moveend");
    stats.tap = null;
    if (!map.areTilesLoaded()) await once(map, "idle");
    // at rest: one more frame, read
    let rest = 0;
    stats.tap = (kind, v) => {
      if (kind === "frame" && v.rendered) rest = Math.max(rest, v.pixelGray * 100);
    };
    map.triggerRepaint();
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    stats.tap = null;
    legs.push({
      leg: `${from}->${to}`, frames: rec.frames,
      gray_frames: rec.gray, fallback_frames: rec.fallback, coarse_frames: rec.coarse,
      not_ready: map.notReady ? { ...map.notReady } : null,
      area_pct: summary(rec.area), at_rest_pct: r1(rest),
    });
  }
  stats.onPixels = null;
  let saved = null;
  if (worst.flipped) {
    const c = document.createElement("canvas");
    c.width = worst.w;
    c.height = worst.h;
    c.getContext("2d").putImageData(new ImageData(worst.flipped, worst.w, worst.h), 0, 0);
    saved = await invoke("save_snapshot", { name, dataUrl: c.toDataURL("image/png") });
  }
  return { legs, worst_frame_pct: r1(worst.f * 100), worst_frame: saved };
}

/** Run cold then warm; returns both results. */
export async function runBench(map, stats, bounds, opts = {}) {
  const outside_pct = opts.pixelCheck ? await pixelSelfTest(map, stats) : null;
  const path = lawnmower(bounds);
  map.jumpTo({ center: path[0], zoom: ZOOM });
  if (!map.areTilesLoaded()) await once(map, "idle");
  const cold = await pass(map, stats, path);
  map.jumpTo({ center: path[0], zoom: ZOOM });
  if (!map.areTilesLoaded()) await once(map, "idle");
  const warm = await pass(map, stats, path);
  return { speed_px_s: SPEED_PX_S, zoom: ZOOM, pixel_selftest_outside_pct: outside_pct, cold, warm };
}
