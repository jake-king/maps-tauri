import * as maplibregl from "maplibre-gl";
import maplibreWorkerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import "maplibre-gl/dist/maplibre-gl.css";
import { Protocol, PMTiles, FetchSource } from "pmtiles";
import { layers, namedFlavor } from "@protomaps/basemaps";
import { invoke } from "@tauri-apps/api/core";
import MaplibreGeocoder from "@maplibre/maplibre-gl-geocoder";
import "@maplibre/maplibre-gl-geocoder/dist/maplibre-gl-geocoder.css";
import initCoords, { coordFeatures } from "./wasm/coords.js";
import { TimedSource, createStats } from "./devtools/overlay.js";
import { retainCachedParents } from "./maplibre-patch.js";
import { GpuLedger, installGpuLedger } from "./devtools/gpumem.js";
import { blockRows } from "./devtools/blocks.js";
import { say, memprobeBlank, startDevtools } from "./devtools/harness.js";

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
await memprobeBlank(flags, "maplibre");

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
// prefetchZoomDelta (0-6), fadeDuration (0) and a startup warm-up pass were measured too and changed
// nothing (bench/2026-11-15-prefetch*, px-fade0, px-warm*.json); MapLibre's defaults stand.
const map = new maplibregl.Map({
  maxTileCacheZoomLevels: CACHE_ZOOM_LEVELS,
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

const bounds = { minLon: header.minLon, minLat: header.minLat, maxLon: header.maxLon, maxLat: header.maxLat };
const tools = startDevtools({
  map, flags, renderer: "maplibre", bounds, stats,
  rows: () => blockRows(map, gpu),
  describe: () => ({
    cache_zoom_levels: CACHE_ZOOM_LEVELS, workers: maplibregl.getWorkerCount(), labels: !flags.noLabels,
    cached_parents: patchState,
  }),
});
map.once("idle", () => {
  say(`MapLibre · z${map.getZoom().toFixed(1)} · ${style.layers.length} style layers`);
  tools.runModes();
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
