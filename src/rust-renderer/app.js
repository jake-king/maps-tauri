// The native renderer's app: the map, plus the same dev tools the MapLibre app has (F4 overlay, F6
// benchmark, memory probes, pixel check), so the two can be compared number for number.
// v1 has no labels and no search box.

import { invoke } from "@tauri-apps/api/core";
import { RustMap } from "./renderer.js";
import { createStats, instrument, pollBackend, createOverlay } from "../devtools/overlay.js";
import { runBench, runSoak, setBenchSpeed, runZoomBench } from "../devtools/bench.js";
import { GpuLedger, installGpuLedger } from "../devtools/gpumem.js";
import { blockRows } from "../devtools/blocks.js";
import { snapshot } from "../devtools/snapshot.js";

const status = document.getElementById("status");
const say = (msg) => {
  status.textContent = msg;
  invoke("report", { msg }).catch(() => {});
};

const flags = await invoke("dev_flags");
const gpu = installGpuLedger(WebGL2RenderingContext.prototype, new GpuLedger());

const memprobe = async (stage, map) => {
  await new Promise((r) => setTimeout(r, 3000));
  const s = await invoke("dev_stats");
  const blocks = map ? rows(map) : [];
  await invoke("bench_done", { result: { probe: stage, renderer: "rust", memory: s.memory, blocks } });
};
if (flags.memprobe === "blank") {
  say("memory probe: blank (native renderer, before any map)");
  await memprobe("blank");
  await new Promise(() => {});
}

setBenchSpeed(flags.benchSpeed ?? 0);
const info = await invoke("archive_info");
const bounds = { minLon: info.minLon, minLat: info.minLat, maxLon: info.maxLon, maxLat: info.maxLat };
const TILES = navigator.userAgent.includes("Windows") ? "http://tiles.localhost" : "tiles://localhost";
const CACHE_TILES = flags.cacheZoomLevels ? flags.cacheZoomLevels * 12 : 240; // same tile budget as MapLibre's 20 levels

const map = new RustMap({
  container: "map",
  baseUrl: TILES,
  center: [(bounds.minLon + bounds.maxLon) / 2, (bounds.minLat + bounds.maxLat) / 2],
  zoom: 12,
  minZoom: info.minZoom,
  maxZoom: info.maxZoom,
  cacheSize: CACHE_TILES,
  fallback: !flags.noFallback,
  prefetchRing: flags.prefetchRing ?? 1,
});
map.onVisibleWait = (ms) => stats.tap?.("visible", ms);
// pixel check: a magenta background, so any uncovered screen area is countable
if (flags.pixelCheck) map.setBackground([1, 0, 1, 1]);

function rows(m) {
  const c = m.cacheStats();
  return [...blockRows(m, gpu), { label: "mesh cache (GPU copies)", bytes: c.bytes, note: `${c.loaded} tiles` }];
}

const stats = createStats();
let benching = false;
let overlay = null;
const sample = instrument(map, stats, {
  pixelCheck: flags.pixelCheck,
  active: () => benching || (overlay?.isVisible() ?? false),
});
overlay = createOverlay(stats);
map.onFetched = (ms) => {
  if (!benching && !overlay.isVisible()) return;
  stats.fetch.push(ms);
  stats.tap?.("fetch", ms);
};
const frame = (now) => {
  sample(now);
  overlay.draw();
  requestAnimationFrame(frame);
};
requestAnimationFrame(frame);
setInterval(() => {
  if (!overlay.isVisible() || benching) return;
  pollBackend(stats);
  stats.blocks = rows(map);
}, 1000);

map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));

const bench = async () => {
  if (benching) return;
  benching = true;
  say("benchmark running… (native renderer)");
  const result = await runBench(map, stats, bounds, { pixelCheck: flags.pixelCheck });
  benching = false;
  Object.assign(result, {
    renderer: "rust", cache_tiles: CACHE_TILES, cores: navigator.hardwareConcurrency, prefetch_ring: flags.prefetchRing ?? 1,
    pixel_check: !!flags.pixelCheck, blocks: rows(map),
  });
  invoke("bench_done", { result });
  say(`bench (native): cold ${result.cold.missing_pct}% loading · tile fetch avg ${result.cold.fetch.avg} ms`);
};
window.addEventListener("keydown", (e) => {
  if (e.key === "F6") {
    e.preventDefault();
    bench();
  }
});

map.once("idle", async () => {
  say(`native renderer · ${map.cacheStats().loaded} tiles · z${map.getZoom().toFixed(1)}`);
  if (flags.zoomBench) {
    benching = true;
    const result = await runZoomBench(map, stats, [-118.4912, 34.0105], "zoombench-rust");
    benching = false;
    return invoke("bench_done", { result: { zoom_bench: true, renderer: "rust", ...result } });
  }
  if (flags.snapshot) {
    // MAPS_SNAPSHOT_ZOOM picks the zoom; the center stays the pier
    const view = flags.snapshotZoom != null ? { center: [-118.4912, 34.0105], zoom: flags.snapshotZoom } : undefined;
    const path = await snapshot(map, flags.snapshot, view);
    return invoke("bench_done", { result: { snapshot: path, renderer: "rust" } });
  }
  if (flags.memprobe === "idle") return memprobe("idle", map);
  if (flags.memprobe === "soak") {
    benching = true;
    const series = await runSoak(map, stats, bounds, 10);
    benching = false;
    const s = await invoke("dev_stats");
    return invoke("bench_done", { result: { probe: "soak", renderer: "rust", series_mb: series, memory: s.memory, blocks: rows(map) } });
  }
  if (flags.memprobe === "pan") {
    benching = true;
    await runBench(map, stats, bounds);
    benching = false;
    return memprobe("pan", map);
  }
  if (flags.bench) bench();
});
