import * as maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { Protocol, PMTiles } from "pmtiles";
import { layers, namedFlavor } from "@protomaps/basemaps";
import { invoke } from "@tauri-apps/api/core";
import initCoords, { parseCoords } from "./wasm/coords.js";

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

// The coordinate parser, compiled from crates/coords to WebAssembly.
initCoords().then(() => {
  const probe = (q) => `${JSON.stringify(q)} → [${Array.from(parseCoords(q)).join(", ")}]`;
  invoke("report", { msg: `wasm ${probe("51.5074N 0.1278W")} · ${probe("110 00")}` });
});
