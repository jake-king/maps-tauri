// Entry point: pick a renderer. Only the chosen one is imported, so a native-renderer run never
// loads MapLibre's code (which matters for the memory comparison).
//   default            MapLibre GL (src/maplibre-app.js)
//   MAPS_RENDERER=rust  Rust decodes and meshes, a small WebGL2 renderer draws (src/rust-renderer)
import { invoke } from "@tauri-apps/api/core";

// Uncaught errors go to the webview's console, which no terminal run can see: report them to stdout.
// (A frontend hang once left an empty log; this is how its cause became visible.)
let reported = 0;
const report = (kind, e) => {
  window.__uncaughtErrors = (window.__uncaughtErrors ?? 0) + 1; // the stress benchmark counts these
  if (reported++ < 20) invoke("report", { msg: `${kind}: ${e?.message ?? e}${e?.stack ? `\n${e.stack}` : ""}` }).catch(() => {});
};
window.addEventListener("error", (e) => report("uncaught error", e.error ?? e.message));
window.addEventListener("unhandledrejection", (e) => report("unhandled rejection", e.reason));

const flags = await invoke("dev_flags");
if (flags.renderer === "rust") await import("./rust-renderer/app.js");
else await import("./maplibre-app.js");
