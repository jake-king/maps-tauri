import * as maplibregl from "maplibre-gl";
import maplibreWorkerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import "maplibre-gl/dist/maplibre-gl.css";
import { Protocol, PMTiles, FetchSource } from "pmtiles";
import { layers, namedFlavor } from "@protomaps/basemaps";
import { invoke } from "@tauri-apps/api/core";
import MaplibreGeocoder from "@maplibre/maplibre-gl-geocoder";
import "@maplibre/maplibre-gl-geocoder/dist/maplibre-gl-geocoder.css";
import initCoords, { parseCoords, coordFeatures } from "./wasm/coords.js";
import { TimedSource, createStats, instrument, pollBackend, createOverlay } from "./devtools/overlay.js";
import { runBench, runSoak } from "./devtools/bench.js";
import { retainCachedParents } from "./maplibre-patch.js";
import { GpuLedger, installGpuLedger } from "./devtools/gpumem.js";
import { snapshot } from "./devtools/snapshot.js";
import { blockRows } from "./devtools/blocks.js";

const status = document.getElementById("status");
const say = (msg) => {
  status.textContent = msg;
  // Mirror every status line to the backend's stdout so a run can be checked without looking.
  invoke("report", { msg }).catch(() => {});
};

// The archive comes from the Rust backend's tiles:// scheme, never the filesystem. Tauri spells
// a custom scheme differently per platform: tiles://localhost on Linux and macOS,
// http://tiles.localhost on Windows (WebView2 only routes http(s) schemes).
const TILES = navigator.userAgent.includes("Windows") ? "http://tiles.localhost" : "tiles://localhost";
const ARCHIVE = `${TILES}/basemap.pmtiles`;

// MapLibre 6 derives its worker's URL from import.meta.url, but only for http(s) pages: served from
// tauri://localhost in a release build it gets "" and starts no workers, so the map never loads.
// It worked in dev only because Vite serves over http. Vite bundles the worker (with its shared
// chunk) through this import, and setWorkerUrl points MapLibre at it in both modes.
maplibregl.setWorkerUrl(maplibreWorkerUrl);

const protocol = new Protocol();
maplibregl.addProtocol("pmtiles", protocol.tile);
const stats = createStats();
const archive = new PMTiles(new TimedSource(new FetchSource(ARCHIVE), stats));
protocol.add(archive);

const flags = await invoke("dev_flags");

// Memory probe: per-process memory at a stage, then exit. "blank" stops here, before any map,
// source or worker exists (MapLibre's code is loaded, nothing more): the floor every Tauri +
// WebKitGTK window pays. "idle" measures the map at rest after its first load.
const memprobe = async (stage) => {
  await new Promise((r) => setTimeout(r, 3000));
  const s = await invoke("dev_stats");
  const blocks = typeof map === "undefined" ? [] : blockRows(map, gpu);
  await invoke("bench_done", { result: { probe: stage, memory: s.memory, blocks } });
};
if (flags.memprobe === "blank") {
  say("memory probe: blank");
  await memprobe("blank");
  await new Promise(() => {}); // the app exits from bench_done
}

// Glyphs and sprites are Vite public assets, served from the page's own origin. Plain
// concatenation: new URL() would percent-encode the {fontstack}/{range} template tokens.
const style = {
  version: 8,
  glyphs: `${location.origin}/assets/fonts/{fontstack}/{range}.pbf`,
  sprite: `${location.origin}/assets/sprites/v4/light`,
  sources: {
    protomaps: {
      type: "vector",
      url: `pmtiles://${ARCHIVE}`,
      attribution: '<a href="https://openstreetmap.org/copyright">© OpenStreetMap contributors</a>, Protomaps',
    },
  },
  // MAPS_NO_LABELS=1 drops every label layer (the basemaps module adds labels only with `lang`):
  // a diagnostic for how much of a tile's decode time is label layout.
  layers: layers("protomaps", namedFlavor("light"), flags.noLabels ? {} : { lang: "en" }),
};

// MAPS_PIXEL_CHECK=1 paints the background pure magenta, so any screen area no tile covered (a
// "gray box") can be counted exactly from the drawn pixels. Ground truth for the tile-based
// measure; it stalls the GPU every frame, so a pixel-check run's timings mean nothing.
if (flags.pixelCheck) style.layers[0].paint = { "background-color": "#ff00ff" };
// MAPS_EMPTY_STYLE=1, a memory control: the map, its WebGL context and its worker, with nothing to
// draw but a background. Blank -> empty style is the engine; empty style -> full style is the data.
if (flags.emptyStyle) {
  style.sources = {};
  style.layers = [style.layers[0]];
  delete style.glyphs;
  delete style.sprite;
}

// The GPU ledger counts every WebGL allocation from here on, so it must be installed before the
// map creates its context. Recording is a Map write per allocation call, nothing per frame.
const gpu = installGpuLedger(WebGL2RenderingContext.prototype, new GpuLedger());

const header = await archive.getHeader();
// MapLibre keeps tiles that leave the view in a cache of about (tiles across + 1) x (tiles down + 1)
// x maxTileCacheZoomLevels. Its default of 5 is 60 tiles for a 1200x800 window: a pan out and
// back re-decodes everything. Note maxTileCacheSize can only LOWER that figure (it is a Math.min).
// Measured with the pan benchmark: 5 and 10 levels reload all 161 tiles on a revisit (30% of
// frames drawn with tiles missing); 20 and 40 reload 3 (0%), with no measurable memory cost.
const CACHE_ZOOM_LEVELS = flags.cacheZoomLevels ?? 20; // bench/2026-11-15-cache*.json
// MapLibre runs one worker unless it detects Safari, and WebKitGTK's user agent says Safari, so it
// would run 3. Each worker is a JavaScript engine of its own, about 45-50 MB, and on the pan
// benchmark 3 workers decoded tiles no faster than 1 (bench/2026-11-15-budget-*.json): 1 worker
// saves ~130 MB for nothing measurable. MAPS_WORKERS overrides it; setWorkerCount must run before
// the first map is created.
maplibregl.setWorkerCount(flags.workers ?? 1);
const PREFETCH = flags.prefetch ?? 4; // MapLibre's default prefetchZoomDelta
const map = new maplibregl.Map({
  maxTileCacheZoomLevels: CACHE_ZOOM_LEVELS,
  prefetchZoomDelta: PREFETCH,
  // MapLibre fades newly loaded content in over 300 ms by default; MAPS_FADE_MS overrides it
  fadeDuration: flags.fadeMs ?? 300,
  container: "map",
  style,
  center: [header.centerLon, header.centerLat],
  zoom: 12,
  maxBounds: [[header.minLon - 0.2, header.minLat - 0.2], [header.maxLon + 0.2, header.maxLat + 0.2]],
});
map.addControl(new maplibregl.NavigationControl(), "top-right");
map.addControl(new maplibregl.ScaleControl());

map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));

// See src/maplibre-patch.js. The tile manager exists once the style has loaded.
let patchState = "off";
if (flags.cachedParents) {
  map.once("styledata", () => {
    patchState = retainCachedParents(maplibregl, map, "protomaps");
    invoke("report", { msg: `cached-parents patch: ${patchState}` });
  });
}

// Dev tools: F4 toggles the overlay, F6 runs the pan benchmark (MAPS_BENCH=1 runs it at startup).
let benching = false;
let overlay = null;
const sample = instrument(map, stats, {
  pixelCheck: flags.pixelCheck,
  active: () => benching || (overlay?.isVisible() ?? false),
});
overlay = createOverlay(stats);
const frame = (now) => {
  sample(now);
  overlay.draw();
  requestAnimationFrame(frame);
};
requestAnimationFrame(frame);
setInterval(() => {
  if (!overlay.isVisible() || benching) return;
  pollBackend(stats);
  stats.blocks = blockRows(map, gpu); // the tile walk runs here, once a second, never per frame
}, 1000);
// Cache warm-up: before anyone pans, visit the whole region once at a low zoom so its tiles sit in
// the out-of-view cache. Panning into a new area at high zoom then finds a loaded PARENT to draw,
// blurry, for the ~30 ms its own tile takes, instead of nothing. MapLibre does not do this by
// itself: it requests a tile only when the tile enters the view. MAPS_WARM_ZOOM sets the zoom
// (0 turns warm-up off).
const WARM_ZOOM = flags.warmZoom ?? 0;
async function warmUp(b, zoom) {
  if (!zoom) return 0;
  const home = { center: map.getCenter(), zoom: map.getZoom() };
  map.jumpTo({ center: [(b.minLon + b.maxLon) / 2, (b.minLat + b.maxLat) / 2], zoom });
  // step one viewport at a time across the bounds, waiting for each view's tiles
  const nw = map.project([b.minLon, b.maxLat]);
  const se = map.project([b.maxLon, b.minLat]);
  const w = map.getCanvas().clientWidth;
  const h = map.getCanvas().clientHeight;
  let views = 0;
  for (let y = nw.y + h / 2; y < se.y + h / 2; y += h) {
    for (let x = nw.x + w / 2; x < se.x + w / 2; x += w) {
      map.jumpTo({ center: map.unproject([x, y]), zoom });
      views += 1;
      if (!map.areTilesLoaded()) await new Promise((r) => map.once("idle", r));
    }
  }
  map.jumpTo(home);
  return views;
}

const bounds = { minLon: header.minLon, minLat: header.minLat, maxLon: header.maxLon, maxLat: header.maxLat };
const bench = async () => {
  if (benching) return;
  benching = true;
  say("benchmark running…");
  const result = await runBench(map, stats, bounds, { pixelCheck: flags.pixelCheck });
  result.cache_zoom_levels = CACHE_ZOOM_LEVELS;
  result.workers = maplibregl.getWorkerCount();
  result.prefetch_zoom_delta = PREFETCH;
  result.labels = !flags.noLabels;
  result.warm_zoom = WARM_ZOOM;
  result.pixel_check = !!flags.pixelCheck;
  result.fade_ms = flags.fadeMs ?? 300;
  result.cached_parents = patchState;
  result.blocks = blockRows(map, gpu);
  result.cores = navigator.hardwareConcurrency;
  result.user_agent = navigator.userAgent;
  benching = false;
  invoke("bench_done", { result });
  const c = result.cold;
  const w = result.warm;
  say(`bench: cold ${c.missing_pct}% of frames missing tiles, warm ${w.missing_pct}% · ` +
      `tile fetch cold avg ${c.fetch.avg} ms`);
};
window.addEventListener("keydown", (e) => {
  if (e.key === "F6") {
    e.preventDefault();
    bench();
  }
});
map.once("idle", async () => {
  if (flags.snapshot) {
    const path = await snapshot(map, flags.snapshot);
    return invoke("bench_done", { result: { snapshot: path, renderer: "maplibre" } });
  }
  if (flags.memprobe === "idle") return memprobe("idle");
  if (flags.memprobe === "soak") {
    benching = true;
    const series = await runSoak(map, stats, bounds, 10);
    benching = false;
    const s = await invoke("dev_stats");
    return invoke("bench_done", { result: { probe: "soak", series_mb: series, memory: s.memory, blocks: blockRows(map, gpu) } });
  }
  if (flags.memprobe === "pan") {
    // the pan benchmark's two passes, then the usual 3 s settle, then 20 s more: does memory the
    // pan added come back on its own (garbage not yet collected) or stay (held)?
    benching = true;
    await runBench(map, stats, bounds);
    benching = false;
    const s = await invoke("dev_stats");
    const right_after = s.memory.reduce((a, m) => a + m.pss_kb, 0);
    await new Promise((r) => setTimeout(r, 20000));
    await invoke("report", { msg: `pan probe: ${Math.round(right_after / 1024)} MB right after the pan` });
    return memprobe("pan");
  }
  const t0 = performance.now();
  const views = await warmUp(bounds, WARM_ZOOM);
  if (views) invoke("report", { msg: `warm-up: ${views} views at z${WARM_ZOOM} in ${Math.round(performance.now() - t0)} ms` });
  if (flags.bench) bench();
});
map.once("idle", () => {
  const roads = map.querySourceFeatures("protomaps", { sourceLayer: "roads" }).length;
  const places = map.querySourceFeatures("protomaps", { sourceLayer: "places" }).length;
  say(`basemap idle · z${map.getZoom().toFixed(1)} · ${style.layers.length} layers · ` +
      `${roads} road and ${places} place features decoded`);
});

// Search. A coordinate is answered in the frontend by the WASM build of crates/coords, with no
// round trip; anything else goes to the backend's `search` command over the GeoNames index. Both
// build their features with the same Rust coord_feature, so there is one definition of the answer.
const wasmReady = initCoords();
const geocoderApi = {
  forwardGeocode: async (config) => {
    const limit = config.limit ?? 8;
    try {
      await wasmReady;
      const local = JSON.parse(coordFeatures(config.query, limit));
      if (local.length) return { features: local };
      return { features: await invoke("search", { query: config.query, limit }) };
    } catch (e) {
      // A dead gazetteer must not take the map down with it.
      say(`search unavailable: ${e?.message ?? e} (the map still works)`);
      return { features: [] };
    }
  },
};
map.addControl(new MaplibreGeocoder(geocoderApi, {
  maplibregl,
  placeholder: "Search places or type a coordinate",
  showResultsWhileTyping: true,
  minLength: 2,
  limit: 8,
  marker: true,
  collapsed: false,
}), "top-left");

// Startup self-check through the same forwardGeocode the search box calls, timed.
const probe = async (q) => {
  const t0 = performance.now();
  const { features } = await geocoderApi.forwardGeocode({ query: q, limit: 3 });
  const ms = (performance.now() - t0).toFixed(1);
  return `${JSON.stringify(q)} ${ms} ms → ${features.map((f) => f.place_name).join(" | ") || "(none)"}`;
};
wasmReady.then(async () => {
  invoke("report", { msg: `wasm parseCoords("110 00") → [${Array.from(parseCoords("110 00"))}]` });
  for (const q of ["santa mon", "51.5074N 0.1278W", "-0.1278, 51.5074", "110 00"]) {
    invoke("report", { msg: `search ${await probe(q)}` });
  }
});
