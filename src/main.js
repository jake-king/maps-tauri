import * as maplibregl from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
import { invoke } from "@tauri-apps/api/core";
import initCoords, { parseCoords } from "./wasm/coords.js";

const status = document.getElementById("status");
const say = (msg) => {
  status.textContent = msg;
  // Mirror every status line to the backend's stdout so a run can be checked without looking.
  invoke("report", { msg }).catch(() => {});
};

// M0: no tiles yet. A background and one GeoJSON point are enough to prove WebGL renders here.
const map = new maplibregl.Map({
  container: "map",
  center: [-118.45, 34.02],
  zoom: 11,
  style: {
    version: 8,
    sources: {
      pt: { type: "geojson", data: { type: "Point", coordinates: [-118.45, 34.02] } },
    },
    layers: [
      { id: "bg", type: "background", paint: { "background-color": "#0b2239" } },
      { id: "pt", type: "circle", source: "pt", paint: { "circle-radius": 12, "circle-color": "#f5a623" } },
    ],
  },
});

map.on("error", (e) => say(`map error: ${e.error?.message ?? e}`));
map.once("idle", () => {
  const gl = map.painter.context.gl;
  const ext = gl.getExtension("WEBGL_debug_renderer_info");
  const renderer = ext ? gl.getParameter(ext.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
  const version = gl.getParameter(gl.VERSION);
  say(`map idle · ${version} · ${renderer}`);
});

// M2: the coordinate parser, compiled from crates/coords to WebAssembly. A startup self-check
// proves the module loaded and answers like the Rust tests say it should.
initCoords().then(() => {
  const probe = (q) => `${JSON.stringify(q)} → [${Array.from(parseCoords(q)).join(", ")}]`;
  invoke("report", { msg: `wasm ${probe("51.5074N 0.1278W")} · ${probe("-0.1278, 51.5074")} · ${probe("110 00")}` });
});
