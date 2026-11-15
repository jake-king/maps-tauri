// The native renderer's drawing half. Rust has already decoded and meshed every tile
// (crates/vtile); this file only fetches the meshes, uploads them as they come (no parsing beyond a
// 28-byte header) and draws two calls a tile. Each needed tile is drawn clipped to its own rectangle;
// if it is not loaded yet, its nearest loaded ancestor is drawn in its place, clipped the same way,
// so there is no gray box while an ancestor exists and no draw-order trouble between tiles.
//
// It also implements the small part of MapLibre's API that the dev tools use (events, _render,
// painter.context.gl, project/unproject, jumpTo/easeTo, areTilesLoaded), so F4, F6 and the pixel
// check measure both renderers the same way.

import { Camera, lngLatToWorld, worldToLngLat, visibleTiles, ringTiles, tileRect, ancestor, interpolate, parseMeshHeader } from "./geo.js";
import palette from "./palette.json";

const EXTENT = 4096;
const MAX_INFLIGHT = 8;
const FALLBACK_DEPTH = 3; // also request each needed tile's ancestor this many zooms up

const FILL_VS = `#version 300 es
in vec2 a_pos;
in float a_color;
uniform vec2 u_origin;   // tile's top-left, device px
uniform float u_scale;   // device px per tile unit
uniform vec2 u_viewport; // device px
uniform vec4 u_colors[32];
out vec4 v_color;
void main() {
  vec2 px = u_origin + a_pos * u_scale;
  vec2 clip = px / u_viewport * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_color = u_colors[int(a_color + 0.5)];
}`;
const LINE_VS = `#version 300 es
in vec2 a_pos;
in vec2 a_ext;
in float a_class;
uniform vec2 u_origin;
uniform float u_scale;
uniform vec2 u_viewport;
uniform float u_halfwidth[16]; // device px, for the current zoom
uniform vec4 u_colors[16];
out vec4 v_color;
void main() {
  int c = int(a_class + 0.5);
  vec2 px = u_origin + a_pos * u_scale + (a_ext / 32.0) * u_halfwidth[c];
  vec2 clip = px / u_viewport * 2.0 - 1.0;
  gl_Position = vec4(clip.x, -clip.y, 0.0, 1.0);
  v_color = u_halfwidth[c] > 0.0 ? u_colors[c] : vec4(0.0);
}`;
const FS = `#version 300 es
precision mediump float;
in vec4 v_color;
out vec4 o;
void main() { o = vec4(v_color.rgb * v_color.a, v_color.a); }`;

function program(gl, vs, fs) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p));
  const u = {};
  for (let i = 0; i < gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS); i++) {
    const name = gl.getActiveUniform(p, i).name.replace(/\[0\]$/, "");
    u[name] = gl.getUniformLocation(p, name);
  }
  return { p, u, a: (name) => gl.getAttribLocation(p, name) };
}

const rgbaF = (c) => [c[0] / 255, c[1] / 255, c[2] / 255, c[3] / 255];

export class RustMap {
  constructor({ container, baseUrl, center, zoom, minZoom = 0, maxZoom = 14, cacheSize = 240, background, fallback = true, prefetchRing = 1 }) {
    this.prefetchRing = prefetchRing; // tiles beyond the view to fetch ahead (0: off)
    this.fallback = fallback; // draw a loaded ancestor for a tile still loading
    this.baseUrl = baseUrl;
    this.minZ = minZoom;
    this.maxZ = maxZoom;
    this.cacheSize = cacheSize;
    this.listeners = new Map();
    this.tiles = new Map(); // key -> tile
    this.queue = [];
    this.inflight = 0;
    this.frame = 0;
    this.tilesVisible = true;
    this.anim = null;
    this.scheduled = false;
    this.idleFired = false;

    const canvas = document.createElement("canvas");
    canvas.style.cssText = "position:absolute;inset:0;width:100%;height:100%;";
    (typeof container === "string" ? document.getElementById(container) : container).appendChild(canvas);
    this.canvas = canvas;
    const gl = canvas.getContext("webgl2", { antialias: true, premultipliedAlpha: true });
    if (!gl) throw new Error("WebGL2 unavailable");
    this.gl = gl;
    this.painter = { context: { gl } }; // what the dev tools read
    this.fill = program(gl, FILL_VS, FS);
    this.line = program(gl, LINE_VS, FS);
    this.background = rgbaF(background ?? palette.background);
    this.fillColors = new Float32Array(32 * 4);
    palette.fill_colors.forEach((c, i) => this.fillColors.set(rgbaF(c), i * 4));
    this.lineColors = new Float32Array(16 * 4);
    palette.lines.forEach((l) => this.lineColors.set(rgbaF(l.color), l.class * 4));
    this.halfWidths = new Float32Array(16);

    const [cx, cy] = lngLatToWorld(center[0], center[1]);
    this.cam = new Camera(cx, cy, zoom, 1, 1);
    this.resize();
    window.addEventListener("resize", () => {
      this.resize();
      this.triggerRepaint();
    });
    this.bindInput();
    this.triggerRepaint();
  }

  // --- events (MapLibre-shaped) ---
  on(type, fn) {
    if (!this.listeners.has(type)) this.listeners.set(type, new Set());
    this.listeners.get(type).add(fn);
    return this;
  }
  off(type, fn) {
    this.listeners.get(type)?.delete(fn);
    return this;
  }
  once(type, fn) {
    const w = (e) => {
      this.off(type, w);
      fn(e);
    };
    return this.on(type, w);
  }
  /** Calls every listener; one that throws is counted and reported, and the rest still run. A
   *  listener's exception once escaped _render and stopped the frame loop exactly while tiles were
   *  loading, which hid those frames from every measurement. */
  fire(type, e = {}) {
    for (const fn of [...(this.listeners.get(type) ?? [])]) {
      try {
        fn({ type, target: this, ...e });
      } catch (err) {
        this.listenerErrors = (this.listenerErrors ?? 0) + 1;
        if (this.listenerErrors <= 3) console.error(`listener for "${type}" threw:`, err);
      }
    }
  }

  // --- camera (MapLibre-shaped) ---
  resize() {
    const dpr = window.devicePixelRatio || 1;
    this.dpr = dpr;
    this.cam.w = this.canvas.clientWidth || window.innerWidth;
    this.cam.h = this.canvas.clientHeight || window.innerHeight;
    this.canvas.width = Math.round(this.cam.w * dpr);
    this.canvas.height = Math.round(this.cam.h * dpr);
  }
  getCanvas() {
    return this.canvas;
  }
  getZoom() {
    return this.cam.zoom;
  }
  getCenter() {
    const [lng, lat] = worldToLngLat(this.cam.cx, this.cam.cy);
    return { lng, lat };
  }
  project(ll) {
    const [lng, lat] = Array.isArray(ll) ? ll : [ll.lng, ll.lat];
    const [x, y] = this.cam.worldToScreen(...lngLatToWorld(lng, lat));
    return { x, y };
  }
  unproject(p) {
    const [px, py] = Array.isArray(p) ? p : [p.x, p.y];
    const [lng, lat] = worldToLngLat(...this.cam.screenToWorld(px, py));
    return { lng, lat };
  }
  jumpTo({ center, zoom }) {
    this.anim = null;
    if (center) [this.cam.cx, this.cam.cy] = lngLatToWorld(...(Array.isArray(center) ? center : [center.lng, center.lat]));
    if (zoom !== undefined) this.cam.zoom = zoom;
    this.triggerRepaint();
    this.fire("moveend");
  }
  easeTo({ center, zoom, duration = 300, easing = (t) => t }) {
    const [tx, ty] = center ? lngLatToWorld(...(Array.isArray(center) ? center : [center.lng, center.lat])) : [this.cam.cx, this.cam.cy];
    this.anim = { from: [this.cam.cx, this.cam.cy, this.cam.zoom], to: [tx, ty, zoom ?? this.cam.zoom], t0: performance.now(), duration, easing };
    this.triggerRepaint();
  }
  triggerRepaint() {
    this.idleFired = false;
    if (this.scheduled) return;
    this.scheduled = true;
    requestAnimationFrame((now) => {
      this.scheduled = false;
      this._render(now);
    });
  }

  // --- what the dev tools use ---
  getStyle() {
    return { layers: [{ id: "background", type: "background" }, { id: "tiles", type: "fill" }] };
  }
  setLayoutProperty(id, prop, value) {
    if (id === "tiles" && prop === "visibility") this.tilesVisible = value !== "none";
    this.triggerRepaint();
  }
  setBackground(rgba) {
    this.background = rgba;
    this.triggerRepaint();
  }
  /** Every tile the current view needs is loaded (or known empty). */
  areTilesLoaded() {
    return this.needed().every(([z, x, y]) => {
      const t = this.tiles.get(`${z}/${x}/${y}`);
      return t && (t.state === "loaded" || t.state === "empty");
    });
  }
  /** Counts and bytes of what the cache holds, for the overlay. */
  cacheStats() {
    let loaded = 0, bytes = 0;
    for (const t of this.tiles.values()) if (t.state === "loaded") { loaded += 1; bytes += t.bytes; }
    return { tiles: this.tiles.size, loaded, bytes, inflight: this.inflight, queued: this.queue.length };
  }

  needed() {
    return visibleTiles(this.cam, this.minZ, this.maxZ);
  }

  // --- tiles ---
  request(z, x, y, priority) {
    const key = `${z}/${x}/${y}`;
    let t = this.tiles.get(key);
    if (t) {
      t.lastUsed = this.frame;
      if (t.state === "queued" && priority < t.priority) t.priority = priority; // a ring tile became visible
      return t;
    }
    t = { key, z, x, y, state: "queued", lastUsed: this.frame, priority };
    this.tiles.set(key, t);
    this.queue.push(t);
    return t;
  }
  pump() {
    this.queue.sort((a, b) => a.priority - b.priority);
    while (this.inflight < MAX_INFLIGHT && this.queue.length) {
      const t = this.queue.shift();
      if (t.state !== "queued") continue;
      this.load(t);
    }
  }
  async load(t) {
    t.state = "loading";
    this.inflight += 1;
    const t0 = performance.now();
    const tileID = { key: t.key };
    this.fire("dataloading", { sourceId: "protomaps", tile: { tileID } });
    try {
      const res = await fetch(`${this.baseUrl}/mesh/${t.z}/${t.x}/${t.y}`);
      if (res.status === 204) {
        t.state = "empty";
      } else if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      } else {
        const buf = await res.arrayBuffer();
        this.onFetched?.(performance.now() - t0);
        this.upload(t, buf); // the ArrayBuffer is dropped after this: only the GPU copy remains
        t.state = "loaded";
      }
      // the wait a user saw: from the moment the tile was needed on screen to now
      if (t.neededAt !== undefined && !t.waitReported) {
        t.waitReported = true;
        this.onVisibleWait?.(performance.now() - t.neededAt);
      }
    } catch (e) {
      t.state = "error";
      this.fire("error", { error: e });
    } finally {
      this.inflight -= 1;
      this.fire("data", { sourceId: "protomaps", tile: { tileID } });
      this.pump();
      this.triggerRepaint();
    }
  }
  upload(t, buf) {
    const gl = this.gl;
    const h = parseMeshHeader(buf);
    const mk = (target, view) => {
      const b = gl.createBuffer();
      gl.bindBuffer(target, b);
      gl.bufferData(target, view, gl.STATIC_DRAW);
      return b;
    };
    t.buffers = [];
    // fills: 6-byte vertices (i16 x, i16 y, u8 color, pad)
    t.fillVao = gl.createVertexArray();
    gl.bindVertexArray(t.fillVao);
    t.buffers.push(mk(gl.ARRAY_BUFFER, new Uint8Array(buf, h.fillVerts, h.fillV * 6)));
    gl.enableVertexAttribArray(this.fill.a("a_pos"));
    gl.vertexAttribPointer(this.fill.a("a_pos"), 2, gl.SHORT, false, 6, 0);
    gl.enableVertexAttribArray(this.fill.a("a_color"));
    gl.vertexAttribPointer(this.fill.a("a_color"), 1, gl.UNSIGNED_BYTE, false, 6, 4);
    t.buffers.push(mk(gl.ELEMENT_ARRAY_BUFFER, h.fillWide ? new Uint32Array(buf, h.fillIdx, h.fillI) : new Uint16Array(buf, h.fillIdx, h.fillI)));
    // lines: 8-byte vertices (i16 x, i16 y, i8 ex, i8 ey, u8 class, pad)
    t.lineVao = gl.createVertexArray();
    gl.bindVertexArray(t.lineVao);
    t.buffers.push(mk(gl.ARRAY_BUFFER, new Uint8Array(buf, h.lineVerts, h.lineV * 8)));
    gl.enableVertexAttribArray(this.line.a("a_pos"));
    gl.vertexAttribPointer(this.line.a("a_pos"), 2, gl.SHORT, false, 8, 0);
    gl.enableVertexAttribArray(this.line.a("a_ext"));
    gl.vertexAttribPointer(this.line.a("a_ext"), 2, gl.BYTE, false, 8, 4);
    gl.enableVertexAttribArray(this.line.a("a_class"));
    gl.vertexAttribPointer(this.line.a("a_class"), 1, gl.UNSIGNED_BYTE, false, 8, 6);
    t.buffers.push(mk(gl.ELEMENT_ARRAY_BUFFER, h.lineWide ? new Uint32Array(buf, h.lineIdx, h.lineI) : new Uint16Array(buf, h.lineIdx, h.lineI)));
    gl.bindVertexArray(null);
    t.fillCount = h.fillI;
    t.lineCount = h.lineI;
    t.fillType = h.fillWide ? gl.UNSIGNED_INT : gl.UNSIGNED_SHORT;
    t.lineType = h.lineWide ? gl.UNSIGNED_INT : gl.UNSIGNED_SHORT;
    t.bytes = buf.byteLength - 28;
  }
  evict(keep) {
    if (this.tiles.size <= this.cacheSize) return;
    const victims = [...this.tiles.values()]
      .filter((t) => !keep.has(t.key) && t.state !== "loading")
      .sort((a, b) => a.lastUsed - b.lastUsed);
    const gl = this.gl;
    for (const t of victims.slice(0, this.tiles.size - this.cacheSize)) {
      if (t.buffers) for (const b of t.buffers) gl.deleteBuffer(b);
      if (t.fillVao) gl.deleteVertexArray(t.fillVao);
      if (t.lineVao) gl.deleteVertexArray(t.lineVao);
      this.tiles.delete(t.key);
      const qi = this.queue.indexOf(t);
      if (qi >= 0) this.queue.splice(qi, 1);
    }
  }

  // --- drawing ---
  drawTile(t, rect) {
    const gl = this.gl;
    const d = this.dpr;
    const own = tileRect(this.cam, t.z, t.x, t.y);
    const origin = [own.x * d, own.y * d];
    const scale = (own.w * d) / EXTENT;
    // clip to the NEEDED tile's rectangle (scissor's origin is bottom-left)
    const x0 = Math.floor(rect.x * d), y0 = Math.floor(rect.y * d);
    const x1 = Math.ceil((rect.x + rect.w) * d), y1 = Math.ceil((rect.y + rect.h) * d);
    gl.scissor(x0, this.canvas.height - y1, x1 - x0, y1 - y0);
    for (const [prog, vao, count, type] of [[this.fill, t.fillVao, t.fillCount, t.fillType], [this.line, t.lineVao, t.lineCount, t.lineType]]) {
      if (!count) continue;
      gl.useProgram(prog.p);
      gl.uniform2f(prog.u.u_origin, origin[0], origin[1]);
      gl.uniform1f(prog.u.u_scale, scale);
      gl.uniform2f(prog.u.u_viewport, this.canvas.width, this.canvas.height);
      if (prog === this.fill) gl.uniform4fv(prog.u.u_colors, this.fillColors);
      else {
        gl.uniform4fv(prog.u.u_colors, this.lineColors);
        gl.uniform1fv(prog.u.u_halfwidth, this.halfWidths);
      }
      gl.bindVertexArray(vao);
      gl.drawElements(gl.TRIANGLES, count, type, 0);
    }
  }

  /** One frame: animate the camera, request what is needed, draw. Called through this._render so
   *  the dev tools can wrap it, as they wrap MapLibre's. */
  _render(now = performance.now()) {
    this.frame += 1;
    if (this.anim) {
      const a = this.anim;
      const k = Math.min(1, (now - a.t0) / Math.max(1, a.duration));
      const e = a.easing(k);
      this.cam.cx = a.from[0] + (a.to[0] - a.from[0]) * e;
      this.cam.cy = a.from[1] + (a.to[1] - a.from[1]) * e;
      this.cam.zoom = a.from[2] + (a.to[2] - a.from[2]) * e;
      if (k >= 1) {
        this.anim = null;
        this.fire("moveend");
      }
    }
    const zoom = this.cam.zoom;
    for (const l of palette.lines) {
      const w = zoom < l.minzoom ? 0 : interpolate(l.width.stops, l.width.base, zoom);
      this.halfWidths[l.class] = (w / 2) * this.dpr;
    }

    const needed = this.needed();
    const keep = new Set();
    const nowMs = performance.now();
    needed.forEach(([z, x, y], i) => {
      const t = this.request(z, x, y, i);
      keep.add(t.key);
      if (t.neededAt === undefined) {
        // first frame this tile is on screen: if it is ready, the user waited 0 ms for it
        t.neededAt = nowMs;
        if (t.state === "loaded" || t.state === "empty") {
          t.waitReported = true;
          this.onVisibleWait?.(0);
        }
      }
      const [pz, px, py] = ancestor(z, x, y, Math.max(this.minZ, z - FALLBACK_DEPTH));
      keep.add(this.request(pz, px, py, 1000 + i).key);
    });
    // prefetch: the ring just beyond the view, after everything visible and its fallbacks
    ringTiles(this.cam, this.minZ, this.maxZ, this.prefetchRing).forEach(([z, x, y], i) => {
      keep.add(this.request(z, x, y, 2000 + i).key);
    });
    this.pump();
    this.evict(keep);

    const gl = this.gl;
    gl.viewport(0, 0, this.canvas.width, this.canvas.height);
    gl.disable(gl.SCISSOR_TEST);
    gl.clearColor(...this.background);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.enable(gl.SCISSOR_TEST);
    const drawn = { self: 0, ancestor: 0, none: 0 };
    if (this.tilesVisible) {
      for (const [z, x, y] of needed) {
        let how = "none";
        const rect = tileRect(this.cam, z, x, y);
        for (let pz = z; pz >= (this.fallback ? Math.max(this.minZ, z - 10) : z); pz--) {
          const [az, ax, ay] = ancestor(z, x, y, pz);
          const t = this.tiles.get(`${az}/${ax}/${ay}`);
          if (t?.state === "loaded") {
            t.lastUsed = this.frame;
            this.drawTile(t, rect);
            how = pz === z ? "self" : "ancestor";
            break;
          }
          if (t?.state === "empty" && pz === z) {
            how = "self"; // no data here: background is correct
            break;
          }
        }
        drawn[how] += 1;
      }
    }
    /** How the last frame's needed tiles were drawn: by themselves, by a fallback ancestor, or not
     *  at all. A frame with any ancestor or none had tiles still loading. */
    this.lastDrawn = drawn;
    gl.disable(gl.SCISSOR_TEST);
    gl.bindVertexArray(null);
    this.fire("render");

    if (this.anim) this.triggerRepaint();
    else if (this.inflight === 0 && this.queue.length === 0 && !this.idleFired) {
      this.idleFired = true;
      this.fire("idle");
    }
  }

  // --- input: drag to pan, wheel to zoom about the cursor ---
  bindInput() {
    let drag = null;
    this.canvas.addEventListener("mousedown", (e) => {
      drag = { x: e.clientX, y: e.clientY };
      this.anim = null;
    });
    window.addEventListener("mousemove", (e) => {
      if (!drag) return;
      const s = this.cam.scale;
      this.cam.cx -= (e.clientX - drag.x) / s;
      this.cam.cy -= (e.clientY - drag.y) / s;
      drag = { x: e.clientX, y: e.clientY };
      this.triggerRepaint();
    });
    window.addEventListener("mouseup", () => {
      if (drag) this.fire("moveend");
      drag = null;
    });
    this.canvas.addEventListener("wheel", (e) => {
      e.preventDefault();
      const r = this.canvas.getBoundingClientRect();
      const px = e.clientX - r.left, py = e.clientY - r.top;
      const before = this.cam.screenToWorld(px, py);
      this.cam.zoom = Math.max(this.minZ, Math.min(20, this.cam.zoom - e.deltaY * 0.002));
      const after = this.cam.screenToWorld(px, py);
      this.cam.cx += before[0] - after[0];
      this.cam.cy += before[1] - after[1];
      this.triggerRepaint();
      this.fire("moveend");
    }, { passive: false });
  }
}
