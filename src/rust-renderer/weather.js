// The weather layer of the native renderer: NOAA radar (MRMS) as an animated overlay and G-AIRMET
// hazard areas as outlines. Rust fetches, decodes and caches (src-tauri/src/weather.rs); this side
// uploads each radar frame once as a one-byte-per-cell texture and draws it in a single full-screen
// pass: every pixel is taken back to latitude/longitude through the camera and Web Mercator, and
// looks its cell up, so the radar's own latitude/longitude grid is never reprojected on the CPU.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

const VS = `#version 300 es
// one triangle that covers the screen: vertex ids 0, 1, 2 -> (-1,-1), (3,-1), (-1,3)
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;

const FS = `#version 300 es
precision highp float;
uniform vec2 u_size;    // CSS px
uniform float u_dpr;
uniform vec2 u_center;  // world, [0,1)
uniform float u_scale;  // CSS px per world unit
uniform vec2 u_rot;     // cos, sin of the bearing
uniform vec4 u_grid;    // lat0, lon0, dlat, dlon of cell (0,0)'s centre
uniform vec2 u_dims;    // ni, nj
uniform float u_opacity;
uniform sampler2D u_radar; // R8: wx::radar codes
uniform sampler2D u_lut;   // 256 x 1 RGBA: colour per code
out vec4 o;
const float PI = 3.141592653589793;
void main() {
  vec2 p = vec2(gl_FragCoord.x, u_size.y * u_dpr - gl_FragCoord.y) / u_dpr; // CSS px, y down
  vec2 d = p - 0.5 * u_size;
  vec2 w = u_center + vec2(d.x * u_rot.x - d.y * u_rot.y, d.x * u_rot.y + d.y * u_rot.x) / u_scale;
  float lon = w.x * 360.0 - 180.0;
  float lat = degrees(atan(sinh(PI * (1.0 - 2.0 * w.y))));
  float i = floor((lon - u_grid.y) / u_grid.w + 0.5);
  float j = floor((u_grid.x - lat) / u_grid.z + 0.5);
  if (i < 0.0 || j < 0.0 || i >= u_dims.x || j >= u_dims.y) discard;
  int code = int(texelFetch(u_radar, ivec2(i, j), 0).r * 255.0 + 0.5);
  if (code == 0) discard;
  vec4 c = texelFetch(u_lut, ivec2(code, 0), 0);
  o = vec4(c.rgb * c.a, c.a) * u_opacity;
}`;

/** The NWS reflectivity scale, by lower bound in dBZ. */
export const DBZ_COLORS = [
  [5, "#04e9e7"], [10, "#019ff4"], [15, "#0300f4"], [20, "#02fd02"], [25, "#01c501"], [30, "#008e00"],
  [35, "#fdf802"], [40, "#e5bc00"], [45, "#fd9500"], [50, "#fd0000"], [55, "#d40000"], [60, "#bc0000"],
  [65, "#f800fd"], [70, "#9854c6"], [75, "#fdfdfd"],
];

/** Code (wx::radar: 0 = none, else 1 + 2 * (dBZ - 5)) to dBZ. */
export const codeToDbz = (code) => (code === 0 ? null : 5 + (code - 1) / 2);

/** 256 RGBA entries, one per code: the colour of the band the code's dBZ falls in. Weak echoes are
 *  more transparent so the map shows through drizzle. */
export function lut() {
  const out = new Uint8Array(256 * 4);
  for (let code = 1; code < 256; code++) {
    const dbz = codeToDbz(code);
    let hex = DBZ_COLORS[0][1];
    for (const [lo, c] of DBZ_COLORS) if (dbz >= lo) hex = c;
    const n = parseInt(hex.slice(1), 16);
    out.set([(n >> 16) & 255, (n >> 8) & 255, n & 255, dbz < 20 ? 150 : 210], code * 4);
  }
  return out;
}

export const HAZARD_STYLE = {
  "TURB-HI": { color: "#c026d3", label: "TURB" },
  "TURB-LO": { color: "#9333ea", label: "TURB" },
  ICE: { color: "#0284c7", label: "ICE" },
  IFR: { color: "#dc2626", label: "IFR" },
  MT_OBSC: { color: "#92400e", label: "MTN OBSC" },
  SFC_WND: { color: "#ea580c", label: "SFC WND" },
  LLWS: { color: "#ea580c", label: "LLWS" },
  M_FZLVL: { color: "#0891b2", label: "MULT FZL" },
  FZLVL: { color: "#0891b2", label: "FZL" },
};

/** The text drawn on a hazard area: "TURB MOD 180-400", "ICE FZL-230", "FZL 160". */
export function hazardLabel(h) {
  const s = HAZARD_STYLE[h.hazard] ?? { label: h.hazard };
  const parts = [s.label];
  if (h.severity) parts.push(h.severity);
  if (h.level) parts.push(h.level);
  else if (h.base || h.top) parts.push(`${h.base ?? "?"}-${h.top ?? "?"}`);
  return parts.join(" ");
}

/** The panel's status line for `s` (wx_state, or null before the first answer). */
export function status(s, { radar, hazards }) {
  if (!radar && !hazards) return "";
  if (!s) return "starting…";
  const parts = [];
  const l = s.loading ?? {};
  if (radar) {
    if (l.busy && l.wanted && l.held < l.wanted) parts.push(`radar: loading ${l.held} of ${l.wanted}…`);
    else if (s.frames.length) parts.push(`radar ${age(s.frames.at(-1), s.now)} (${s.frames.length} frames)`);
    else parts.push(l.busy || !s.radarOk ? "radar: loading…" : "radar: none");
  }
  if (hazards) {
    if (s.hazardsValid != null) parts.push(`hazards valid ${age(s.hazardsValid, s.now)}${l.hazards ? ", updating…" : ""}`);
    else parts.push(l.hazards || l.busy || !s.hazardsOk ? "hazards: loading…" : "hazards: none issued");
  }
  if (s.fromDisk) parts.push("offline: last fetched");
  if (s.errors?.length) parts.push(`${s.errors.length} fetch errors`);
  return parts.join(" · ");
}

/** "20:04Z, 3 min old" from Unix seconds. */
export function age(t, now) {
  if (t == null) return "none";
  const d = new Date(t * 1000);
  const hm = `${String(d.getUTCHours()).padStart(2, "0")}:${String(d.getUTCMinutes()).padStart(2, "0")}Z`;
  const m = Math.round((now - t) / 60);
  return m >= 0 ? `${hm}, ${m < 120 ? `${m} min` : `${Math.round(m / 60)} h`} old` : `${hm}, in ${Math.round(-m / 60 * 10) / 10} h`;
}

function program(gl) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, VS], [gl.FRAGMENT_SHADER, FS]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`weather shader: ${gl.getShaderInfoLog(s)}`);
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(`weather program: ${gl.getProgramInfoLog(p)}`);
  const u = {};
  for (const n of ["u_size", "u_dpr", "u_center", "u_scale", "u_rot", "u_grid", "u_dims", "u_opacity", "u_radar", "u_lut"]) u[n] = gl.getUniformLocation(p, n);
  return { p, u };
}

export class WeatherLayer {
  constructor(map, { onChange } = {}) {
    this.map = map;
    this.onChange = onChange;
    this.radarOn = false;
    this.looping = true; // animate the frames; false holds the newest
    this.hazardsOn = false;
    this.state = null;
    this.frames = new Map(); // time -> texture, on the GPU
    this.fetching = new Set(); // times being fetched from Rust
    this.loading = false;
    this.shown = null; // time of the frame drawn last
    this.loopStart = performance.now();
    this.gpuBytes = 0;
    /** performance.now() of: enabled, first frame on the GPU, hazards present, every frame held */
    this.marks = {};
    const gl = map.gl;
    this.prog = program(gl);
    this.vao = gl.createVertexArray();
    this.lutTex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, this.lutTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, 256, 1, 0, gl.RGBA, gl.UNSIGNED_BYTE, lut());
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    // hazard outlines: a 2D canvas between the map and the labels, so names stay readable on top
    const c = document.createElement("canvas");
    c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;";
    map.labelLayer.canvas.before(c);
    this.canvas = c;
    this.ctx = c.getContext("2d");
    map.overlays.push(this);
    map.on("render", () => this.drawHazards());
    // every frame: is a different radar frame due? (the loop as a pure function of time)
    map.loop.addUpdater((now) => this.radarOn && this.frameNow(now) !== this.shown);
  }

  /** Turn layers on or off. The first time either goes on, Rust starts fetching. */
  async set({ radar = this.radarOn, hazards = this.hazardsOn }) {
    this.marks.enabled ??= performance.now();
    this.radarOn = radar;
    this.hazardsOn = hazards;
    if ((radar || hazards) && !this.timer) {
      this.onChange?.(null); // "starting": shown before the first answer
      // Rust says when anything arrives; the slow poll is only a safety net
      await listen("wx-changed", () => this.refresh());
      this.state = await invoke("wx_enable");
      this.timer = setInterval(() => this.refresh(), 60000);
      await this.refresh();
    } else {
      this.onChange?.(this.state);
    }
    this.map.triggerRepaint();
  }

  /** Fetch the state and upload new frames. Signals arrive in bursts (one per frame): a refresh
   *  asked for while one runs is folded into a single rerun after it. */
  async refresh() {
    if (this.refreshing) {
      this.again = true;
      return;
    }
    this.refreshing = true;
    try {
      do {
        this.again = false;
        await this.refreshOnce();
      } while (this.again);
    } finally {
      this.refreshing = false;
    }
  }

  async refreshOnce() {
    this.state = await invoke("wx_state");
    if (this.state.hazards?.length) this.marks.hazards ??= performance.now();
    await this.sync();
    this.onChange?.(this.state);
    this.map.triggerRepaint();
  }

  /** Fetch the frames not yet held, one at a time (each is ~6 MB over IPC); each goes onto the GPU
   *  inside a frame through the map's loop, the newest first. Frames Rust no longer holds are freed
   *  there too: nothing here touches WebGL outside a frame. */
  async sync() {
    if (this.loading || !this.state) return;
    this.loading = true;
    try {
      const want = new Set(this.state.frames);
      const stale = [...this.frames.keys()].filter((t) => !want.has(t));
      if (stale.length) this.map.loop.arrive(() => this.free(stale), 0);
      const g = this.state.grid;
      const newest = this.state.frames.at(-1);
      for (const t of [...this.state.frames].reverse()) {
        if (this.frames.has(t) || this.fetching.has(t) || !g) continue;
        this.fetching.add(t);
        try {
          const buf = new Uint8Array(await invoke("wx_frame", { time: t }));
          if (buf.length !== g.ni * g.nj) throw new Error(`radar frame ${t}: ${buf.length} bytes for a ${g.ni} x ${g.nj} grid`);
          // the newest frame is what the user is waiting for: ahead of everything but visible tiles
          this.map.loop.arrive(() => this.uploadFrame(t, buf, g), t === newest ? 500 : 3000);
        } finally {
          this.fetching.delete(t);
        }
      }
    } finally {
      this.loading = false;
    }
  }

  /** Inside a frame. */
  uploadFrame(t, buf, g) {
    if (this.frames.has(t)) return;
    const gl = this.map.gl;
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, g.ni, g.nj, 0, gl.RED, gl.UNSIGNED_BYTE, buf);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    this.frames.set(t, { tex, bytes: buf.length, grid: g });
    this.gpuBytes += buf.length;
    this.marks.firstFrame ??= performance.now();
    const all = this.state?.frames ?? [];
    if (all.length >= 10 && all.every((x) => this.frames.has(x))) this.marks.allFrames ??= performance.now();
  }

  /** Inside a frame. */
  free(times) {
    for (const t of times) {
      const f = this.frames.get(t);
      if (!f) continue;
      this.map.gl.deleteTexture(f.tex);
      this.gpuBytes -= f.bytes;
      this.frames.delete(t);
    }
  }

  /** Which frame the loop is on: each held 400 ms, the newest 2 s, oldest to newest. */
  frameNow(now = performance.now()) {
    const times = [...this.frames.keys()].sort((a, b) => a - b);
    if (!times.length) return null;
    if (!this.looping) return times.at(-1);
    const period = (times.length - 1) * 400 + 2000;
    const k = (now - this.loopStart) % period;
    return times[Math.min(times.length - 1, Math.floor(k / 400))];
  }

  /** Called by the renderer after the map, before the labels. */
  draw(gl, map) {
    if (!this.radarOn) return;
    const t = this.frameNow();
    this.shown = t;
    const f = t == null ? null : this.frames.get(t);
    if (!f) return;
    const cam = map.cam;
    const { p, u } = this.prog;
    gl.useProgram(p);
    gl.uniform2f(u.u_size, cam.w, cam.h);
    gl.uniform1f(u.u_dpr, map.dpr);
    gl.uniform2f(u.u_center, cam.cx, cam.cy);
    gl.uniform1f(u.u_scale, cam.scale);
    gl.uniform2f(u.u_rot, Math.cos(cam.bearing), Math.sin(cam.bearing));
    gl.uniform4f(u.u_grid, f.grid.lat0, f.grid.lon0, f.grid.dlat, f.grid.dlon);
    gl.uniform2f(u.u_dims, f.grid.ni, f.grid.nj);
    gl.uniform1f(u.u_opacity, 0.85);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, f.tex);
    gl.uniform1i(u.u_radar, 0);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, this.lutTex);
    gl.uniform1i(u.u_lut, 1);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindVertexArray(this.vao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
  }

  drawHazards() {
    const { canvas: c, ctx, map } = this;
    const dpr = map.dpr, w = map.cam.w, h = map.cam.h;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    const show = this.hazardsOn && this.state?.hazards?.length > 0;
    if (!show && !this.painted) return; // weather off: nothing to clear, no cost per frame
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, c.width, c.height);
    this.painted = show;
    if (!show) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.font = "bold 11px 'Noto Sans',sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    const px = ([lng, lat]) => {
      const p = map.project([lng, lat]);
      return [p.x, p.y];
    };
    for (const hz of this.state.hazards) {
      if (hz.hazard === "FZLVL") continue; // freezing-level contours: lines across the whole map
      const style = HAZARD_STYLE[hz.hazard] ?? { color: "#444" };
      ctx.strokeStyle = style.color;
      ctx.fillStyle = `${style.color}22`;
      ctx.lineWidth = 2;
      ctx.beginPath();
      const rings = hz.kind === "polygon" ? hz.coords : [hz.coords];
      for (const ring of rings) {
        ring.forEach((ll, i) => (i ? ctx.lineTo(...px(ll)) : ctx.moveTo(...px(ll))));
        if (hz.kind === "polygon") ctx.closePath();
      }
      ctx.setLineDash(hz.kind === "line" ? [6, 4] : []);
      if (hz.kind === "polygon") ctx.fill("evenodd");
      ctx.stroke();
      // the label at the outer ring's (or line's) middle vertex average
      const r = rings[0];
      const [x, y] = r.map(px).reduce((a, b) => [a[0] + b[0], a[1] + b[1]]).map((v) => v / r.length);
      if (x > -50 && y > -20 && x < w + 50 && y < h + 20) {
        const text = hazardLabel(hz);
        ctx.lineWidth = 3;
        ctx.strokeStyle = "#ffffffd0";
        ctx.setLineDash([]);
        ctx.strokeText(text, x, y);
        ctx.fillStyle = style.color;
        ctx.fillText(text, x, y);
      }
    }
  }

  /** Registration check: a synthetic frame on a 0.001° grid around `ll`, drawn at zoom 15 twice,
   *  without and with the one cell holding `ll` lit. The pixels that differ are where the GPU put
   *  that cell; their centre must be where the map's own projection (geo.js) puts `ll`, within half
   *  a cell. Two separate implementations of the same math, checked against each other. */
  async probe(ll = [-118.4912, 34.0105]) {
    const gl = this.map.gl;
    const n = 41, d = 0.001;
    const grid = { ni: n, nj: n, lat0: ll[1] + 20 * d, lon0: ll[0] - 20 * d, dlat: d, dlon: d };
    const saved = { frames: this.frames, looping: this.looping, radarOn: this.radarOn };
    Object.assign(this, { looping: false, radarOn: true });
    this.map.jumpTo({ center: ll, zoom: 15, bearing: 0 });
    await new Promise((r) => (this.map.areTilesLoaded() ? r() : this.map.once("idle", r)));
    const shot = async (lit) => {
      const cells = new Uint8Array(n * n);
      if (lit) cells[20 * n + 20] = 255;
      const tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, n, n, 0, gl.RED, gl.UNSIGNED_BYTE, cells);
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
      // without these the texture is incomplete (the default filter wants mipmaps) and reads as 0
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      this.frames = new Map([[0, { tex, bytes: 0, grid }]]);
      const px = await new Promise((resolve) => {
        this.map.once("render", () => {
          const w = gl.drawingBufferWidth, h = gl.drawingBufferHeight;
          const out = new Uint8Array(w * h * 4);
          gl.readPixels(0, 0, w, h, gl.RGBA, gl.UNSIGNED_BYTE, out);
          resolve({ out, w, h });
        });
        this.map.triggerRepaint();
      });
      gl.deleteTexture(tex);
      return px;
    };
    const off = await shot(false), on = await shot(true);
    let x0 = Infinity, y0 = Infinity, x1 = -1, y1 = -1, changed = 0;
    for (let y = 0; y < on.h; y++) {
      for (let x = 0; x < on.w; x++) {
        const i = (y * on.w + x) * 4;
        if (on.out[i] !== off.out[i] || on.out[i + 1] !== off.out[i + 1] || on.out[i + 2] !== off.out[i + 2]) {
          changed++;
          x0 = Math.min(x0, x); x1 = Math.max(x1, x);
          y0 = Math.min(y0, y); y1 = Math.max(y1, y);
        }
      }
    }
    Object.assign(this, saved);
    this.map.triggerRepaint();
    const dpr = this.map.dpr;
    const p = this.map.project(ll);
    const east = this.map.project([ll[0] + d, ll[1]]);
    if (!changed) return { changed: 0 };
    // GL rows run bottom-up: flip to CSS y
    const cx = (x0 + x1 + 1) / 2 / dpr, cy = (on.h - (y0 + y1 + 1) / 2) / dpr;
    return {
      changed,
      box_css: [Math.round((x1 - x0 + 1) / dpr), Math.round((y1 - y0 + 1) / dpr)],
      cell_css: Math.round(east.x - p.x),
      off_by_css: [Math.round((cx - p.x) * 10) / 10, Math.round((cy - p.y) * 10) / 10],
    };
  }

  /** What the end-to-end check reports. */
  report() {
    const s = this.state ?? {};
    const since = (k) => (this.marks[k] == null ? null : Math.round(this.marks[k] - this.marks.enabled));
    return {
      ms_to: { first_frame: since("firstFrame"), hazards: since("hazards"), all_frames: since("allFrames") },
      frames: [...this.frames.keys()].length,
      shown: this.shown,
      gpu_bytes: this.gpuBytes,
      grid: s.grid,
      hazards: s.hazards?.length ?? 0,
      hazards_valid: s.hazardsValid,
      radar_ok: s.radarOk,
      from_disk: s.fromDisk,
      errors: s.errors,
    };
  }
}
