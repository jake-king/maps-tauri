// The pan benchmark: a fixed lawnmower path over the archive's bounds at zoom 15, at a hand-fast
// pan speed, run twice (cold, then warm with every tile cached) so a change to tile loading can be
// measured instead of eyeballed. F6 runs it; MAPS_BENCH=1 runs it at startup.

import { invoke } from "@tauri-apps/api/core";

const ZOOM = 15;
const SPEED_PX_S = 4000; // a fast hand pan
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
  const rec = { frame: [], render: [], missing: 0, rendered: 0, fetch: [], load: [], tiles: new Set() };
  const onLoading = (e) => e.tile && e.sourceId === "protomaps" && rec.tiles.add(e.tile.tileID.key);
  map.on("dataloading", onLoading);
  stats.tap = (kind, v) => {
    if (kind === "frame") {
      rec.frame.push(v.interval);
      if (v.rendered) {
        rec.rendered += 1;
        rec.render.push(v.renderMs);
        if (v.missing) rec.missing += 1;
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
    fetch: summary(rec.fetch),
    load: summary(rec.load),
    serve: { n: t.requests, avg: r1(t.requests ? t.total_ms / t.requests : 0), worst: r1(t.worst_ms), bytes: t.bytes },
    tiles_loaded: rec.load.length,
    unique_tiles: rec.tiles.size,
    cache_max: map.style?.tileManagers?.protomaps?._outOfViewCache?.max ?? null,
    memory_mb: Math.round(backend.memory.reduce((a, m) => a + m.pss_kb, 0) / 1024),
  };
}

/** Run cold then warm; returns both results. */
export async function runBench(map, stats, bounds) {
  const path = lawnmower(bounds);
  map.jumpTo({ center: path[0], zoom: ZOOM });
  if (!map.areTilesLoaded()) await once(map, "idle");
  const cold = await pass(map, stats, path);
  map.jumpTo({ center: path[0], zoom: ZOOM });
  if (!map.areTilesLoaded()) await once(map, "idle");
  const warm = await pass(map, stats, path);
  return { speed_px_s: SPEED_PX_S, zoom: ZOOM, cold, warm };
}
