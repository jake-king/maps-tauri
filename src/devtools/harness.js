// The dev tools both apps share, wired once: the status line, the frame loop that samples for the
// F4 overlay, the overlay's once-a-second backend poll, the F6 pan benchmark, the memory probes, and
// the flag-driven modes a scripted run asks for (MAPS_SNAPSHOT, MAPS_ZOOM_BENCH, MAPS_MEMPROBE,
// MAPS_BENCH). Each app passes in only what differs: its renderer's name, its own result fields, and
// its rows of the bytes-per-block view.

import { invoke } from "@tauri-apps/api/core";
import { createStats, instrument, pollBackend, createOverlay } from "./overlay.js";
import { runBench, runSoak, runZoomBench, setBenchSpeed } from "./bench.js";
import { snapshot } from "./snapshot.js";

const PIER = [-118.4912, 34.0105];

/** Show a status line, and mirror it to the backend's stdout so a run can be checked without looking. */
export function say(msg) {
  const el = document.getElementById("status");
  if (el) el.textContent = msg;
  invoke("report", { msg }).catch(() => {});
}

/** Per-process memory after 3 s, reported as the run's result (the app then exits). */
async function memprobe(stage, renderer, blocks) {
  await new Promise((r) => setTimeout(r, 3000));
  const s = await invoke("dev_stats");
  await invoke("bench_done", { result: { probe: stage, renderer, memory: s.memory, blocks } });
}

/** MAPS_MEMPROBE=blank: the floor before any map exists. Call before creating the map; never returns
 *  when it applies. */
export async function memprobeBlank(flags, renderer) {
  if (flags.memprobe !== "blank") return;
  say(`memory probe: blank (${renderer}, before any map)`);
  await memprobe("blank", renderer, []);
  await new Promise(() => {});
}

/**
 * Start the shared dev tools for `map`. `rows()` gives the bytes-per-block rows; `describe()` the
 * renderer's own fields for a benchmark result; `stats`, if given, is one already being fed (the
 * MapLibre app's timed tile source writes into it before the map exists). Returns `{ stats, active, runModes }`: call
 * `runModes(extra)` once the map is first idle; `extra.search`, if given, runs for MAPS_SEARCH.
 */
export function startDevtools({ map, flags, renderer, bounds, rows, describe = () => ({}), stats = createStats() }) {
  setBenchSpeed(flags.benchSpeed ?? 0);
  let benching = false;
  let overlay = null;
  const active = () => benching || (overlay?.isVisible() ?? false);
  const sample = instrument(map, stats, { pixelCheck: flags.pixelCheck, active });
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
    stats.blocks = rows(); // the tile walk runs here, once a second, never per frame
  }, 1000);

  /** Run `fn` with the per-frame tools on, whatever the overlay says. */
  const exclusive = async (fn) => {
    benching = true;
    try {
      return await fn();
    } finally {
      benching = false;
    }
  };

  const bench = async () => {
    if (benching) return;
    say(`benchmark running… (${renderer})`);
    const result = await exclusive(() => runBench(map, stats, bounds, { pixelCheck: flags.pixelCheck }));
    Object.assign(result, describe(), {
      renderer, pixel_check: !!flags.pixelCheck, cores: navigator.hardwareConcurrency,
      user_agent: navigator.userAgent, blocks: rows(),
    });
    invoke("bench_done", { result });
    say(`bench (${renderer}): cold ${result.cold.missing_pct}% of frames with tiles loading · tile fetch avg ${result.cold.fetch.avg} ms`);
  };
  window.addEventListener("keydown", (e) => {
    if (e.key === "F6") {
      e.preventDefault();
      bench();
    }
  });

  async function runModes(extra = {}) {
    if (flags.search && extra.search) return extra.search();
    if (flags.zoomBench) {
      const result = await exclusive(() => runZoomBench(map, stats, PIER, `zoombench-${renderer}`));
      return invoke("bench_done", { result: { zoom_bench: true, renderer, ...result } });
    }
    if (flags.snapshot) {
      const view = flags.snapshotZoom != null ? { center: flags.snapshotCenter ?? PIER, zoom: flags.snapshotZoom } : undefined;
      const path = await snapshot(map, flags.snapshot, view);
      return invoke("bench_done", { result: { snapshot: path, renderer } });
    }
    if (flags.memprobe === "idle") return memprobe("idle", renderer, rows());
    if (flags.memprobe === "soak") {
      const series = await exclusive(() => runSoak(map, stats, bounds, 10));
      const s = await invoke("dev_stats");
      return invoke("bench_done", { result: { probe: "soak", renderer, series_mb: series, memory: s.memory, blocks: rows() } });
    }
    if (flags.memprobe === "pan") {
      // the pan benchmark's two passes, then 20 s more: does memory the pan added come back on its
      // own (garbage not yet collected) or stay (held)?
      await exclusive(() => runBench(map, stats, bounds));
      const s = await invoke("dev_stats");
      const rightAfter = s.memory.reduce((a, m) => a + m.pss_kb, 0);
      await new Promise((r) => setTimeout(r, 20000));
      await invoke("report", { msg: `pan probe: ${Math.round(rightAfter / 1024)} MB right after the pan` });
      return memprobe("pan", renderer, rows());
    }
    if (flags.bench) bench();
  }

  return { stats, active, runModes };
}
