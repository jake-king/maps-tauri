// Entry point: pick a renderer. Only the chosen one is imported, so a native-renderer run never
// loads MapLibre's code (which matters for the memory comparison).
//   default            MapLibre GL (src/maplibre-app.js)
//   MAPS_RENDERER=rust  Rust decodes and meshes, a small WebGL2 renderer draws (src/rust-renderer)
import { invoke } from "@tauri-apps/api/core";

const flags = await invoke("dev_flags");
if (flags.renderer === "rust") await import("./rust-renderer/app.js");
else await import("./maplibre-app.js");
