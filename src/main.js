import * as maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { Protocol, PMTiles } from "pmtiles";
import { layers, namedFlavor } from "@protomaps/basemaps";
import { invoke } from "@tauri-apps/api/core";
import MaplibreGeocoder from "@maplibre/maplibre-gl-geocoder";
import "@maplibre/maplibre-gl-geocoder/dist/maplibre-gl-geocoder.css";
import initCoords, { parseCoords, coordFeatures } from "./wasm/coords.js";

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

const protocol = new Protocol();
maplibregl.addProtocol("pmtiles", protocol.tile);
const archive = new PMTiles(ARCHIVE);
protocol.add(archive);

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
  layers: layers("protomaps", namedFlavor("light"), { lang: "en" }),
};

const header = await archive.getHeader();
const map = new maplibregl.Map({
  container: "map",
  style,
  center: [header.centerLon, header.centerLat],
  zoom: 12,
  maxBounds: [[header.minLon - 0.2, header.minLat - 0.2], [header.maxLon + 0.2, header.maxLat + 0.2]],
});
map.addControl(new maplibregl.NavigationControl(), "top-right");
map.addControl(new maplibregl.ScaleControl());

map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));
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
