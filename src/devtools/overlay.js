// The F4 overlay: frame and tile timings, per-process memory, a 120-frame graph. Modelled on
// guile-2d's profiler: a row per block (average, worst, a bar against one frame), the last 120
// frames as columns with a line at the budget, top right, dragged by the mouse.
//
// Measured here (the webview): frame interval, MapLibre's render CPU time, tile fetch round trip
// through tiles://, tile request-to-decoded time, frames drawn while tiles were still missing.
// Measured by the backend (dev_stats): the tiles:// handler's own time, process memory.

import { invoke } from "@tauri-apps/api/core";
import { Ring, bar, columns, budgetY, frameBudget, clampPosition, formatBytes } from "./stats.js";
import { measureCoverage } from "./coverage.js";

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
    gray: new Ring(FRAMES),         // needed tiles with NOTHING drawn over them, per rendered frame
    fetch: new Ring(FRAMES),        // tiles:// round trip as the webview sees it, ms
    load: new Ring(FRAMES),         // tile request -> decoded, ms
    serve: { avg: 0, worst: 0, requests: 0, bytes: 0 },
    memory: [],
    blocks: [],                     // bytes per block: GPU ledger + tile CPU arrays (blocks.js)
    fetchInFlight: 0,
    tilesPending: new Map(),        // tile key -> start time
    renderedThisSecond: 0,
    fps: 0,
    renderHooked: false,
    tap: null,                      // the benchmark's per-measurement listener, when running
  };
}

/** Wire the measurements into the map. Returns the per-frame sampler the overlay and bench use. */
/** Fraction of the drawn frame that is pure magenta (#ff00ff), every 4th pixel each way. */
function magentaFraction(gl, buf) {
  const w = gl.drawingBufferWidth;
  const h = gl.drawingBufferHeight;
  if (buf.data.length !== w * h * 4) buf.data = new Uint8Array(w * h * 4);
  gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, buf.data);
  const d = buf.data;
  let hit = 0;
  let n = 0;
  for (let y = 0; y < h; y += 4) {
    for (let x = 0; x < w; x += 4) {
      const i = (y * w + x) * 4;
      n += 1;
      if (d[i] === 255 && d[i + 1] === 0 && d[i + 2] === 255) hit += 1;
    }
  }
  return n ? hit / n : 0;
}

/**
 * Wire the measurements into the map. Everything per frame runs only while `opts.active()` says
 * so (the overlay is open or the benchmark is running): closed, the tools cost nothing a frame.
 */
export function instrument(map, stats, opts = {}) {
  const active = opts.active ?? (() => true);
  const pixbuf = { data: new Uint8Array(0) };
  let framePix = 0;
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
  // Coverage is measured in MapLibre's own "render" event, at the end of _render: there the tile
  // manager has been updated for the camera that was just drawn, so the measure is of what reached
  // the screen. Measured from our requestAnimationFrame callback instead, it could run after the
  // camera moved but before the tile manager caught up, and count gaps a parent would fill.
  let frameCov = null;
  map.on("render", () => {
    if (!active()) return;
    // read in the render event, before the browser composites the frame, so the buffer is intact
    if (opts.pixelCheck) {
      const f = magentaFraction(map.painter.context.gl, pixbuf);
      framePix = Math.max(framePix, f);
      stats.onPixels?.(f, pixbuf.data, map.painter.context.gl);
    }
    if (map.areTilesLoaded()) return;
    const c = measureCoverage(map);
    if (c && (!frameCov || c.uncovered > frameCov.uncovered)) frameCov = c;
  });
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
    if (!active()) {
      last = now;
      renderMs = 0;
      rendered = false;
      return;
    }
    const interval = now - last;
    // The native renderer says how it drew each needed tile; MapLibre is asked areTilesLoaded().
    const missing = rendered && (map.lastDrawn ? map.lastDrawn.ancestor + (map.lastDrawn.children ?? 0) + map.lastDrawn.none > 0 : !map.areTilesLoaded());
    const cov = rendered ? (map.lastDrawn ? { uncovered: map.lastDrawn.none, needed: map.lastDrawn.self + map.lastDrawn.ancestor + (map.lastDrawn.children ?? 0) + map.lastDrawn.none } : frameCov) : null;
    frameCov = null;
    const pixelGray = rendered ? framePix : 0;
    framePix = 0;
    const gray = cov ? cov.uncovered : 0;
    const needed = cov ? cov.needed : 0;
    stats.gray.push(gray);
    stats.frame.push(interval);
    last = now;
    stats.render.push(rendered ? renderMs : 0);
    stats.missing.push(missing ? 1 : 0);
    stats.tap?.("frame", { interval, rendered, renderMs, missing, gray, needed, pixelGray });
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
  const rowsCount = () => 5 + 2 + Math.max(1, stats.memory.length) + 1 + (stats.blocks.length ? stats.blocks.length + 1 : 0);
  const size = () => ({ w: W, h: 8 + rowsCount() * ROW + GRAPH_H + 14 });
  const place = () => {
    canvas.style.left = `${pos.x}px`;
    canvas.style.top = `${pos.y}px`;
  };

  const frames0 = () => stats.frame.values();

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
    const dropped = frames0().filter((f) => f > 1.5 * budget).length;
    text(`ms          avg  worst   vs ${budget.toFixed(1)} ms frame`, 8, "#81a1c1");
    y += ROW;
    row("frame", stats.frame.summary(), `${stats.fps.toFixed(0)} fps, ${dropped} dropped`);
    const renderRendered = new Ring(FRAMES);
    for (const v of stats.render.values()) if (v > 0) renderRendered.push(v);
    row("render", stats.renderHooked ? renderRendered.summary() : { avg: 0, worst: 0 }, stats.renderHooked ? "" : "n/a");
    row("tile fetch", stats.fetch.summary(), `${stats.fetchInFlight} in flight`);
    row("tile serve", stats.serve, `${stats.serve.requests}/s`);
    row("tile load", stats.load.summary(), `${stats.tilesPending.size} pending`);
    const missing = stats.missing.values().reduce((a, b) => a + b, 0);
    const grayFrames = stats.gray.values().filter((g) => g > 0).length;
    text(`tiles loading: ${missing}/${FRAMES} frames · gray: ${grayFrames}/${FRAMES}`, 8, grayFrames ? "#bf616a" : missing ? "#ebcb8b" : "#a3be8c");
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
    y += ROW;
    if (stats.blocks.length) {
      text("bytes per block (estimate)", 8, "#81a1c1");
      y += ROW;
      for (const b of stats.blocks) {
        text(`${b.label.padEnd(26)} ${formatBytes(b.bytes).padStart(9)} ${b.note}`, 8);
        y += ROW;
      }
    }
    y += 4;

    // the graph: frame interval per column, MapLibre's render share darker, a red tick under
    // frames that were drawn with tiles missing, and the budget line
    const frames = stats.frame.values();
    const renders = stats.render.values();
    const miss = stats.missing.values();
    const grayv = stats.gray.values();
    const cw = (w - 16) / FRAMES;
    const cols = columns(frames, budget, GRAPH_H, 2 * budget, 1.5 * budget);
    const rcols = columns(renders, budget, GRAPH_H);
    cols.forEach((c, i) => {
      const x = 8 + i * cw;
      ctx.fillStyle = c.over ? "#bf616a" : "#4c566a";
      ctx.fillRect(x, y + GRAPH_H - c.h, Math.max(1, cw - 0.5), c.h);
      ctx.fillStyle = "#88c0d0";
      ctx.fillRect(x, y + GRAPH_H - rcols[i].h, Math.max(1, cw - 0.5), rcols[i].h);
      if (miss[i]) {
        ctx.fillStyle = grayv[i] > 0 ? "#bf616a" : "#ebcb8b";
        ctx.fillRect(x, y + GRAPH_H + 2, Math.max(1, cw - 0.5), 4);
      }
    });
    ctx.fillStyle = "#ebcb8b";
    ctx.fillRect(8, y + budgetY(budget, GRAPH_H), w - 16, 1);
  }

  return { draw, isVisible: () => visible };
}
