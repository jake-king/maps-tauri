// The native renderer's app: Rust decodes and meshes the tiles, renderer.js draws them, labels.js
// places the names, search.js is the search box. The dev tools are the same as the MapLibre app's
// (src/devtools/harness.js), so the two can be compared number for number.

import { invoke } from "@tauri-apps/api/core";
import { RustMap } from "./renderer.js";
import { GpuLedger, installGpuLedger } from "../devtools/gpumem.js";
import { blockRows } from "../devtools/blocks.js";
import { snapshot } from "../devtools/snapshot.js";
import { say, memprobeBlank, startDevtools } from "../devtools/harness.js";
import { createSearchBox } from "./search.js";
import initCoords, { coordFeatures } from "../wasm/coords.js";
import { once } from "./once.js";
import { WeatherLayer, status as wxStatusText } from "./weather.js";

const flags = await invoke("dev_flags");
const gpu = installGpuLedger(WebGL2RenderingContext.prototype, new GpuLedger());

await memprobeBlank(flags, "rust");

// Everything awaited at startup is awaited BEFORE the map exists: an await between creating the map
// and listening for its first "idle" let that event fire unheard (the WASM load below did, ~1 run in 5).
const info = await invoke("archive_info");
const bounds = { minLon: info.minLon, minLat: info.minLat, maxLon: info.maxLon, maxLat: info.maxLat };
const TILES = navigator.userAgent.includes("Windows") ? "http://tiles.localhost" : "tiles://localhost";
const CACHE_TILES = flags.cacheZoomLevels ? flags.cacheZoomLevels * 12 : 240; // same tile budget as MapLibre's 20 levels

// A world-sized archive (the remote planet) starts at Santa Monica, not mid-Atlantic.
const worldSized = bounds.maxLon - bounds.minLon > 90;
const map = new RustMap({
  container: "map",
  baseUrl: TILES,
  center: worldSized ? [-118.4912, 34.0105] : [(bounds.minLon + bounds.maxLon) / 2, (bounds.minLat + bounds.maxLat) / 2],
  zoom: 12,
  minZoom: info.minZoom,
  maxZoom: info.maxZoom,
  cacheSize: CACHE_TILES,
  fallback: !flags.noFallback,
  prefetchRing: flags.prefetchRing ?? 1,
  labels: !flags.noLabels,
});
// The coordinate parser (WASM) loads on the first search, not at startup: compiled, it holds about
// 17 MB of the web process (measured 2026-11-15), which a map nobody searches should not pay for.
// A failed load is retried on the next search rather than kept.
const coordsReady = once(initCoords);
const coordFeaturesLazy = async (text, limit) => {
  await coordsReady();
  return coordFeatures(text, limit);
};
const search = createSearchBox(map, { invoke, coordFeatures: coordFeaturesLazy, container: document.getElementById("map") });
// The map data's licence (ODbL) requires the credit to be visible. Plain text: a link would navigate
// the app's only window, and the content security policy would refuse it anyway.
const credit = document.createElement("div");
credit.textContent = "© OpenStreetMap contributors, Protomaps";
credit.style.cssText = "position:absolute;right:0;bottom:0;z-index:20;padding:2px 6px;background:#fffc;" +
  "font:11px 'Noto Sans',sans-serif;color:#333;pointer-events:none;";
document.getElementById("map").appendChild(credit);
// what the end-to-end search reports: the text, and that it lies inside the window
const creditShown = () => {
  const r = credit.getBoundingClientRect();
  const inside = r.width > 0 && r.height > 0 && r.right <= innerWidth && r.bottom <= innerHeight;
  return { text: credit.textContent, inside };
};
// Weather: NOAA radar and G-AIRMET hazards, off until a box is ticked (nothing is fetched before).
const wxPanel = document.createElement("div");
wxPanel.style.cssText = "position:absolute;left:10px;bottom:10px;z-index:20;padding:6px 10px;background:#fffe;border-radius:6px;" +
  "box-shadow:0 1px 4px #0003;font:12px 'Noto Sans',sans-serif;color:#222;";
const wxBox = (label) => {
  const l = document.createElement("label");
  l.style.cssText = "margin-right:10px;cursor:pointer;";
  const b = document.createElement("input");
  b.type = "checkbox";
  b.style.cssText = "vertical-align:middle;margin:0 4px 0 0;";
  l.append(b, label);
  wxPanel.append(l);
  return b;
};
const radarBox = wxBox("Radar");
const hazardBox = wxBox("G-AIRMET hazards");
const wxStatus = document.createElement("div");
wxStatus.style.cssText = "margin-top:3px;color:#555;font-size:11px;";
wxPanel.append(wxStatus);
document.getElementById("map").appendChild(wxPanel);
const weather = new WeatherLayer(map, {
  onChange: (s) => (wxStatus.textContent = wxStatusText(s, { radar: radarBox.checked, hazards: hazardBox.checked })),
});
const wxSet = () => weather.set({ radar: radarBox.checked, hazards: hazardBox.checked }).catch((e) => say(`map error: weather: ${e.message ?? e}`));
radarBox.addEventListener("change", wxSet);
hazardBox.addEventListener("change", wxSet);
// pixel check: a magenta background, so any uncovered screen area is countable
if (flags.pixelCheck) map.setBackground([1, 0, 1, 1]);
map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));

const rows = () => {
  const c = map.cacheStats();
  return [...blockRows(map, gpu), { label: "mesh cache (GPU copies)", bytes: c.bytes, note: `${c.loaded} tiles` }];
};
const tools = startDevtools({
  map, flags, renderer: "rust", bounds, rows,
  describe: () => ({ cache_tiles: CACHE_TILES, prefetch_ring: flags.prefetchRing ?? 1, archive: info.source }),
});
map.onVisibleWait = (ms) => tools.stats.tap?.("visible", ms);
map.onFetched = (ms) => {
  if (!tools.active()) return;
  tools.stats.fetch.push(ms);
  tools.stats.tap?.("fetch", ms);
};

map.whenIdle().then(async () => {
  say(`native renderer · ${info.source} archive, zooms ${info.minZoom}-${info.maxZoom} · ${map.cacheStats().loaded} tiles · z${map.getZoom().toFixed(1)}`);
  await tools.runModes({
    // MAPS_SEARCH=<query>: type it, take the first result, settle, snapshot the result
    search: async () => {
      const names = await search.run(flags.search);
      await new Promise((r) => map.once("moveend", r));
      if (!map.areTilesLoaded()) await new Promise((r) => map.once("idle", r));
      await new Promise((r) => setTimeout(r, 200));
      const path = await snapshot(map, "search", { center: map.getCenter(), zoom: map.getZoom() }, (ctx, dpr) => search.paint(ctx, dpr));
      return invoke("bench_done", { result: { search: flags.search, results: names, box: search.boxStyle(), credit: creditShown(), center: map.getCenter(), snapshot: path } });
    },
    // MAPS_WEATHER=1: both layers on, wait for every frame and the hazards (90 s at most), then a
    // picture of the whole country with the loop held on the newest frame
    weather: async () => {
      radarBox.checked = hazardBox.checked = true;
      await wxSet();
      const t0 = performance.now();
      // wait on the layer's own updates, as a user would: no extra polling from here
      const statuses = new Set([wxStatus.textContent]);
      while ((weather.marks.allFrames == null || weather.marks.hazards == null) && performance.now() - t0 < 90000) {
        statuses.add(wxStatus.textContent);
        await new Promise((r) => setTimeout(r, 100));
      }
      statuses.add(wxStatus.textContent);
      weather.looping = false;
      const path = await snapshot(map, "weather", { center: [-97, 38.5], zoom: 3.6 }, (ctx) => ctx.drawImage(weather.canvas, 0, 0, ctx.canvas.width, ctx.canvas.height));
      const probe = await weather.probe();
      return invoke("bench_done", { result: { weather: weather.report(), wait_ms: Math.round(performance.now() - t0), statuses: [...statuses], probe, snapshot: path } });
    },
  });
});
