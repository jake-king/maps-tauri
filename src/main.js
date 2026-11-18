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
// The content security policy (src-tauri/tauri.conf.json) lets the page reach only its own assets,
// the Rust backend and tiles://. Anything it refuses is printed: tools/offline.sh fails on any.
window.__cspViolations = 0;
document.addEventListener("securitypolicyviolation", (e) => {
  window.__cspViolations++;
  invoke("report", { msg: `csp violation: ${e.effectiveDirective} ${e.blockedURI}` }).catch(() => {});
});

const flags = await invoke("dev_flags");
if (flags.cspProbe) fetch("http://192.0.2.1/csp-probe").catch(() => {});
if (flags.renderer === "rust") await import("./rust-renderer/app.js");
else await import("./maplibre-app.js");

// A newer release, when this build has an update URL (src-tauri/src/update.rs) and the release
// manifest offers one: a notice with one button, which installs it and restarts. Nothing happens
// without the user's click, and a failed check is reported, not shown.
invoke("update_check")
  .then((u) => u && showUpdate(u))
  .catch((e) => invoke("report", { msg: `update check failed: ${e}` }).catch(() => {}));

function showUpdate({ version }) {
  const box = document.createElement("div");
  box.id = "update";
  box.textContent = `Version ${version} is available. `;
  const button = document.createElement("button");
  button.textContent = "Install and restart";
  button.onclick = () => {
    button.disabled = true;
    button.textContent = "Installing…";
    invoke("update_install").catch((e) => {
      box.textContent = `The update was not installed: ${e}`;
    });
  };
  box.append(button);
  document.body.append(box);
}
