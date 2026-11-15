// The F4 overlay: frame and tile timings, per-process memory, a 120-frame graph. Modelled on
// guile-2d's profiler: a row per block (average, worst, a bar against one frame), the last 120
// frames as columns with a line at the budget, top right, dragged by the mouse.
//
// Measured here (the webview): frame interval, MapLibre's render CPU time, tile fetch round trip
// through tiles://, tile request-to-decoded time, frames drawn while tiles were still missing.
// Measured by the backend (dev_stats): the tiles:// handler's own time, process memory.

import { invoke } from "@tauri-apps/api/core";
import { Ring, bar, columns, budgetY, frameBudget, clampPosition, formatBytes } from "./stats.js";

const FRAMES = 120;
const W = 360;
const ROW = 15;
const GRAPH_H = 56;
const BAR_W = 90;

/** A pmtiles Source that times every range read and counts reads in flight. */
export class TimedSource {
  constructor(inner, stats) {
    this.inner = inner;
    this.stats = stats;
  }
  getKey() {
    return this.inner.getKey();
  }
  async getBytes(offset, length, signal, etag) {
    const t0 = performance.now();
    this.stats.fetchInFlight += 1;
    try {
      return await this.inner.getBytes(offset, length, signal, etag);
    } finally {
      this.stats.fetchInFlight -= 1;
      const ms = performance.now() - t0;
      this.stats.fetch.push(ms);
      this.stats.tap?.("fetch", ms);
    }
  }
}

export function createStats() {
  return {
    frame: new Ring(FRAMES),        // rAF interval, ms
    render: new Ring(FRAMES),       // MapLibre render CPU per rendered frame, ms (0 when idle)
    missing: new Ring(FRAMES),      // 1 when a rendered frame still had tiles loading
    fetch: new Ring(FRAMES),        // tiles:// round trip as the webview sees it, ms
    load: new Ring(FRAMES),         // tile request -> decoded, ms
    serve: { avg: 0, worst: 0, requests: 0, bytes: 0 },
    memory: [],
    fetchInFlight: 0,
    tilesPending: new Map(),        // tile key -> start time
    renderedThisSecond: 0,
    fps: 0,
    renderHooked: false,
    tap: null,                      // the benchmark's per-measurement listener, when running
  };
}

/** Wire the measurements into the map. Returns the per-frame sampler the overlay and bench use. */
export function instrument(map, stats) {
  let renderMs = 0;
  let rendered = false;
  // MapLibre's _render is private; if a version renames it the row reads "n/a" instead of failing.
  if (typeof map._render === "function") {
    const original = map._render.bind(map);
    map._render = (...args) => {
      const t0 = performance.now();
      const out = original(...args);
      renderMs += performance.now() - t0;
      rendered = true;
      return out;
    };
    stats.renderHooked = true;
  }
  map.on("dataloading", (e) => {
    if (e.tile && e.sourceId === "protomaps") stats.tilesPending.set(e.tile.tileID.key, performance.now());
  });
  const done = (e) => {
    if (!e.tile) return;
    const t0 = stats.tilesPending.get(e.tile.tileID.key);
    if (t0 !== undefined) {
      stats.tilesPending.delete(e.tile.tileID.key);
      const ms = performance.now() - t0;
      stats.load.push(ms);
      stats.tap?.("load", ms);
    }
  };
  map.on("data", done);
  map.on("dataabort", (e) => e.tile && stats.tilesPending.delete(e.tile.tileID.key));

  let last = performance.now();
  let secondStart = last;
  return function sample(now) {
    const interval = now - last;
    const missing = rendered && !map.areTilesLoaded();
    stats.frame.push(interval);
    last = now;
    stats.render.push(rendered ? renderMs : 0);
    stats.missing.push(missing ? 1 : 0);
    stats.tap?.("frame", { interval, rendered, renderMs, missing });
    if (rendered) stats.renderedThisSecond += 1;
    renderMs = 0;
    rendered = false;
    if (now - secondStart >= 1000) {
      stats.fps = (stats.renderedThisSecond * 1000) / (now - secondStart);
      stats.renderedThisSecond = 0;
      secondStart = now;
    }
  };
}

export async function pollBackend(stats) {
  try {
    const s = await invoke("dev_stats");
    const t = s.tiles;
    stats.serve = { avg: t.requests ? t.total_ms / t.requests : 0, worst: t.worst_ms, requests: t.requests, bytes: t.bytes };
    stats.memory = s.memory;
  } catch (e) {
    stats.memory = [{ pid: 0, name: `dev_stats failed: ${e}`, pss_kb: 0 }];
  }
}

/** The toggled, draggable canvas. */
export function createOverlay(stats) {
  const canvas = document.createElement("canvas");
  canvas.style.cssText = "position:absolute;z-index:10;cursor:move;display:none;";
  document.body.appendChild(canvas);
  const ctx = canvas.getContext("2d");
  let pos = null;
  let visible = localStorage.getItem("f4") === "1";
  let drag = null;

  canvas.addEventListener("mousedown", (e) => {
    drag = { dx: e.clientX - pos.x, dy: e.clientY - pos.y };
    e.preventDefault();
  });
  window.addEventListener("mousemove", (e) => {
    if (!drag) return;
    pos = clampPosition({ x: e.clientX - drag.dx, y: e.clientY - drag.dy }, size(), viewport());
    place();
  });
  window.addEventListener("mouseup", () => (drag = null));
  window.addEventListener("keydown", (e) => {
    if (e.key !== "F4") return;
    e.preventDefault();
    visible = !visible;
    localStorage.setItem("f4", visible ? "1" : "0");
    canvas.style.display = visible ? "block" : "none";
  });
  canvas.style.display = visible ? "block" : "none";

  const viewport = () => ({ w: window.innerWidth, h: window.innerHeight });
  const rowsCount = () => 5 + 2 + Math.max(1, stats.memory.length) + 1;
  const size = () => ({ w: W, h: 8 + rowsCount() * ROW + GRAPH_H + 14 });
  const place = () => {
    canvas.style.left = `${pos.x}px`;
    canvas.style.top = `${pos.y}px`;
  };

  function draw() {
    if (!visible) return;
    const { w, h } = size();
    const dpr = window.devicePixelRatio || 1;
    if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
      canvas.width = w * dpr;
      canvas.height = h * dpr;
      canvas.style.width = `${w}px`;
      canvas.style.height = `${h}px`;
    }
    if (!pos) pos = clampPosition({ x: window.innerWidth - w - 50, y: 10 }, { w, h }, viewport());
    place();
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = "rgba(15,18,24,0.88)";
    ctx.fillRect(0, 0, w, h);
    ctx.font = "11px ui-monospace, monospace";
    ctx.textBaseline = "top";

    const budget = frameBudget(stats.frame.values());
    let y = 6;
    const text = (s, x, color = "#d8dee9") => {
      ctx.fillStyle = color;
      ctx.fillText(s, x, y);
    };
    const row = (label, sum, extra = "") => {
      text(label, 8);
      text(`${sum.avg.toFixed(2).padStart(6)} ${sum.worst.toFixed(1).padStart(6)}`, 92);
      const b = bar(sum.avg, budget, BAR_W);
      const bw = bar(sum.worst, budget, BAR_W);
      ctx.fillStyle = "#3b4252";
      ctx.fillRect(196, y + 2, BAR_W, 8);
      ctx.fillStyle = bw.over ? "#bf616a55" : "#88c0d055";
      ctx.fillRect(196, y + 2, bw.w, 8);
      ctx.fillStyle = b.over ? "#bf616a" : "#88c0d0";
      ctx.fillRect(196, y + 2, b.w, 8);
      text(extra, 292, "#a3be8c");
      y += ROW;
    };
    text(`ms          avg  worst   vs ${budget.toFixed(1)} ms frame`, 8, "#81a1c1");
    y += ROW;
    row("frame", stats.frame.summary(), `${stats.fps.toFixed(0)} fps`);
    const renderRendered = new Ring(FRAMES);
    for (const v of stats.render.values()) if (v > 0) renderRendered.push(v);
    row("render", stats.renderHooked ? renderRendered.summary() : { avg: 0, worst: 0 }, stats.renderHooked ? "" : "n/a");
    row("tile fetch", stats.fetch.summary(), `${stats.fetchInFlight} in flight`);
    row("tile serve", stats.serve, `${stats.serve.requests}/s`);
    row("tile load", stats.load.summary(), `${stats.tilesPending.size} pending`);
    const missing = stats.missing.values().reduce((a, b) => a + b, 0);
    text(`frames drawn with tiles missing: ${missing}/${FRAMES}`, 8, missing ? "#ebcb8b" : "#a3be8c");
    y += ROW;
    text(`memory (PSS)`, 8, "#81a1c1");
    y += ROW;
    let total = 0;
    for (const m of stats.memory) {
      total += m.pss_kb;
      text(`${m.name.slice(0, 26).padEnd(26)} ${formatBytes(m.pss_kb * 1024).padStart(9)}`, 8);
      y += ROW;
    }
    if (stats.memory.length === 0) {
      text("waiting for dev_stats…", 8);
      y += ROW;
    }
    text(`${"total".padEnd(26)} ${formatBytes(total * 1024).padStart(9)}`, 8, "#ebcb8b");
    y += ROW + 4;

    // the graph: frame interval per column, MapLibre's render share darker, a red tick under
    // frames that were drawn with tiles missing, and the budget line
    const frames = stats.frame.values();
    const renders = stats.render.values();
    const miss = stats.missing.values();
    const cw = (w - 16) / FRAMES;
    const cols = columns(frames, budget, GRAPH_H);
    const rcols = columns(renders, budget, GRAPH_H);
    cols.forEach((c, i) => {
      const x = 8 + i * cw;
      ctx.fillStyle = c.over ? "#bf616a" : "#4c566a";
      ctx.fillRect(x, y + GRAPH_H - c.h, Math.max(1, cw - 0.5), c.h);
      ctx.fillStyle = "#88c0d0";
      ctx.fillRect(x, y + GRAPH_H - rcols[i].h, Math.max(1, cw - 0.5), rcols[i].h);
      if (miss[i]) {
        ctx.fillStyle = "#ebcb8b";
        ctx.fillRect(x, y + GRAPH_H + 2, Math.max(1, cw - 0.5), 4);
      }
    });
    ctx.fillStyle = "#ebcb8b";
    ctx.fillRect(8, y + budgetY(budget, GRAPH_H), w - 16, 1);
  }

  return { draw, isVisible: () => visible };
}
