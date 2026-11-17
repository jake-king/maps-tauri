// The weather layer of the native renderer, synced to one chosen time as the Aviation Weather
// Center's GFA does it: NOAA radar (MRMS) as an animated loop ending at that time, GOES satellite
// clouds, and G-AIRMET hazard areas as outlines, filtered by flight level. Rust fetches, decodes and caches (src-tauri/src/weather.rs); this side
// uploads each radar frame once as a one-byte-per-cell texture and draws it in a single full-screen
// pass: every pixel is taken back to latitude/longitude through the camera and Web Mercator, and
// looks its cell up, so the radar's own latitude/longitude grid is never reprojected on the CPU.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { WindRenderer, decodeWind } from "./wind.js";

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

// Satellite clouds: a texture in Web Mercator over a fixed box (wx::satellite), grey levels from the
// infrared image; clear ground (grey below ~130) is transparent, colder cloud tops more opaque.
const SAT_FS = `#version 300 es
precision highp float;
uniform vec2 u_size;
uniform float u_dpr;
uniform vec2 u_center;
uniform float u_scale;
uniform vec2 u_rot;
uniform vec4 u_box;     // the image's world box: x0, y0 (top), x1, y1 (bottom), in [0,1)
uniform float u_opacity;
uniform sampler2D u_sat; // R8 grey, 0 = no data
// with the global mosaic: GOES (u_mask, over u_maskBox) blended in where it has data, from none at
// the edge of its coverage to all of it about 1.5 degrees inside, so the two meet without a seam
uniform sampler2D u_mask;
uniform vec4 u_maskBox;
uniform float u_useMask;
uniform float u_feather; // the blend's width, as a fraction of the GOES box's width
out vec4 o;
void main() {
  vec2 p = vec2(gl_FragCoord.x, u_size.y * u_dpr - gl_FragCoord.y) / u_dpr;
  vec2 d = p - 0.5 * u_size;
  vec2 w = u_center + vec2(d.x * u_rot.x - d.y * u_rot.y, d.x * u_rot.y + d.y * u_rot.x) / u_scale;
  vec2 uv = (w - u_box.xy) / (u_box.zw - u_box.xy);
  if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) discard;
  float g = texture(u_sat, uv).r;
  if (u_useMask > 0.5) {
    vec2 mv = (w - u_maskBox.xy) / (u_maskBox.zw - u_maskBox.xy);
    bool inBox = mv.x >= 0.0 && mv.y >= 0.0 && mv.x <= 1.0 && mv.y <= 1.0;
    vec2 sz = vec2(textureSize(u_mask, 0));
    // filtered, a pixel at the edge of GOES's coverage is mixed with no data and reads dimmer (a thin
    // light line along the edge): only where the pixel itself has data, then filtered within it
    float own = inBox ? texelFetch(u_mask, ivec2(min(mv * sz, sz - 1.0)), 0).r : 0.0;
    float here = 0.0;
    if (own > 0.0) {
      // bilinear over the four texels around, counting only those with data
      vec2 f = mv * sz - 0.5;
      vec2 fr = fract(f);
      ivec2 i0 = ivec2(floor(f));
      ivec2 top = ivec2(sz) - 1;
      float sum = 0.0, wsum = 0.0;
      for (int k = 0; k < 4; k++) {
        ivec2 o4 = ivec2(k & 1, k >> 1);
        float v = texelFetch(u_mask, clamp(i0 + o4, ivec2(0), top), 0).r;
        float wt = (o4.x == 1 ? fr.x : 1.0 - fr.x) * (o4.y == 1 ? fr.y : 1.0 - fr.y);
        if (v > 0.0) { sum += v * wt; wsum += wt; }
      }
      here = wsum > 0.0 ? sum / wsum : own;
    }
    if (here > 0.0) {
      // the share of a disc around this point that GOES covers: 0.5 at a straight edge, 1 deep inside
      vec2 rad = vec2(u_feather, u_feather * sz.x / sz.y);
      float covered = 0.0;
      for (int k = 0; k < 16; k++) {
        float ang = float(k) * 0.39269908;
        vec2 q = mv + vec2(cos(ang), sin(ang)) * rad * (k % 2 == 0 ? 1.0 : 0.5);
        if (q.x >= 0.0 && q.y >= 0.0 && q.x <= 1.0 && q.y <= 1.0 && texture(u_mask, q).r > 0.0) covered += 1.0;
      }
      float t = clamp((covered / 16.0 - 0.5) * 2.0, 0.0, 1.0);
      g = g > 0.0 ? mix(g, here, t) : here;
    }
  }
  if (g <= 0.0) discard;
  float cold = smoothstep(130.0 / 255.0, 215.0 / 255.0, g);
  if (cold <= 0.0) discard;
  // low cloud blue-grey, cold high tops near white: readable on the light basemap
  vec3 c = mix(vec3(0.42, 0.48, 0.58), vec3(0.97, 0.98, 1.0), cold);
  float a = (0.25 + 0.55 * cold) * u_opacity;
  o = vec4(c * a, a);
}`;

/** Web Mercator metres to world coordinates in [0, 1) (y south), as geo.js uses. */
export function mercToWorld(x, y) {
  const half = Math.PI * 6378137;
  return [(x + half) / (2 * half), (half - y) / (2 * half)];
}

/** "SFC" and "FZL" (the freezing level, which varies) count as the surface; "180" is FL180. */
export function flightLevel(s) {
  if (s == null) return null;
  if (s === "SFC" || s === "FZL") return 0;
  const n = Number(s);
  return Number.isFinite(n) ? n : null;
}

/** Whether a hazard applies at flight level `fl` (null = all levels): as GFA does, within 3,000 ft
 *  of its base or top. A missing base or top does not hide it. */
export function atLevel(h, fl) {
  if (fl == null) return true;
  const base = flightLevel(h.base) ?? 0;
  const top = flightLevel(h.top) ?? 999;
  return base <= fl + 30 && top >= fl - 30;
}

/** "18:40Z" */
export function zulu(t) {
  const d = new Date(t * 1000);
  return `${String(d.getUTCHours()).padStart(2, "0")}:${String(d.getUTCMinutes()).padStart(2, "0")}Z`;
}

/** "now", "-2 h 20 min", "+3 h" for a time relative to now (seconds). */
export function relative(t, now) {
  const m = Math.round((t - now) / 60);
  if (Math.abs(m) < 5) return "now";
  const sign = m < 0 ? "-" : "+";
  const h = Math.floor(Math.abs(m) / 60), mm = Math.abs(m) % 60;
  return `${sign}${h ? `${h} h` : ""}${h && mm ? " " : ""}${mm ? `${mm} min` : ""}`;
}

/** The valid-time line over the map: what each layer shows and when it is from. */
export function stamp(s, { radar, satellite, hazards, metars = false, winds = false }, m = null, w = null, sigmets = null) {
  if (!s) return "";
  const sel = s.selection ?? {};
  const parts = [];
  // MRMS is a US product (US and Canadian radars): said on the stamp so its absence elsewhere is not
  // read as clear sky
  if (radar) parts.push(sel.radar?.length ? `Radar ${zulu(sel.radar.at(-1))} obs (US)` : s.at > s.now + 300 ? "Radar: observations only" : "Radar: none at this time");
  if (satellite) {
    const g = sel.satelliteGlobal != null ? `world ${zulu(sel.satelliteGlobal)}` : null;
    if (sel.satellite != null) parts.push(`Satellite ${zulu(sel.satellite)} obs${g ? `, ${g}` : ""}`);
    else if (g) parts.push(`Satellite ${g} obs`);
    else parts.push(s.at > s.now + 300 ? "Satellite: observations only" : "Satellite: none at this time");
  }
  if (hazards) {
    parts.push(s.hazardsValid != null ? `Hazards ${zulu(s.hazardsValid)} ${s.hazardsValid > s.now ? `fcst ${relative(s.hazardsValid, s.now)}` : "issued"}` : "Hazards: none near this time");
    if (sigmets != null) parts.push(`${sigmets} SIGMETs`);
  }
  if (metars) parts.push(metarStamp(m, s.at, s.now));
  if (winds) parts.push(windStamp(w));
  return parts.join(" · ");
}

/** Flight category colours as aviation displays use them; index = wx::metar category. */
export const CATEGORY = [
  { name: "not reported", color: "#9ca3af" },
  { name: "VFR", color: "#16a34a" },
  { name: "MVFR", color: "#2563eb" },
  { name: "IFR", color: "#dc2626" },
  { name: "LIFR", color: "#c026d3" },
];

/** The METAR part of the valid-time line: how many stations report at the chosen time, or why none. */
export function metarStamp(m, at, now) {
  if (!m) return "METARs: loading…";
  if (m.stations.length) return `METARs ${m.stations.length} stations`;
  const [first] = m.range ?? [];
  if (first != null && at < first) return `METARs: collected from ${zulu(first)}`;
  return at > now + 300 ? "METARs: observations only" : "METARs: none";
}

/** The station nearest the screen point (x, y) within `radius` px, from [id, lon, lat, cat, t] rows. */
export function nearestStation(stations, x, y, radius, project) {
  let best = null, bestD = radius * radius;
  for (const s of stations) {
    const p = project([s[1], s[2]]);
    const d = (p.x - x) ** 2 + (p.y - y) ** 2;
    if (d <= bestD) {
      best = s;
      bestD = d;
    }
  }
  return best;
}

/** The wind part of the valid-time line: level, model run and forecast hour, valid time. */
export function windStamp(w) {
  if (!w) return "Winds: loading…";
  if (w.error) return "Winds: none (model not reached)";
  const fl = { 850: "FL050", 700: "FL100", 500: "FL180", 400: "FL240", 300: "FL300", 250: "FL340", 200: "FL390", 150: "FL450" }[w.hpa] ?? `${w.hpa} hPa`;
  return `Winds ${fl} GFS ${zulu(w.run).slice(0, 2)}Z +${w.fhr} h (${zulu(w.valid)})`;
}

/** Whether screen point (x, y) lies inside a ring of screen points (even-odd rule). */
export function pointInRing(x, y, ring) {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [xi, yi] = ring[i], [xj, yj] = ring[j];
    if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

/** The SIGMET under screen point (x, y): inside one of its areas, or within 10 px of a point one. */
export function sigmetAt(sigs, x, y, project) {
  for (const s of sigs) {
    for (const ring of s.rings) {
      const pts = ring.map((ll) => {
        const p = project(ll);
        return [p.x, p.y];
      });
      if (pts.length === 1 ? Math.hypot(pts[0][0] - x, pts[0][1] - y) <= 10 : pointInRing(x, y, pts)) return s;
    }
  }
  return null;
}

/** "SIGMET TURB SEV FL300-380 · SHANWICK", "CONVECTIVE SIGMET tops FL240". */
export function sigmetLabel(s) {
  const fl = (n) => `FL${String(n).padStart(3, "0")}`;
  const parts = [s.kind === "CONVECTIVE" ? "CONVECTIVE SIGMET" : `SIGMET ${s.hazard}`];
  if (s.kind !== "CONVECTIVE" && s.qualifier) parts.push(s.qualifier);
  if (s.base == null && s.top != null) parts.push(`tops ${fl(s.top)}`);
  else if (s.base != null && s.top != null) parts.push(s.base === 0 ? `SFC-${fl(s.top)}` : `${fl(s.base)}-${String(s.top).padStart(3, "0")}`);
  const fir = s.fir ? ` · ${s.fir.replace(/^\S+\s+/, "")}` : "";
  return parts.join(" ") + fir;
}

/** How much a SIGMET's label matters when space is short: volcanic ash and cyclones first, then
 *  thunderstorms, then severe turbulence or icing, then the rest. (G-AIRMET labels rank 1.) */
export function sigmetPriority(s) {
  if (s.hazard === "VA" || s.hazard === "TC") return 6;
  if (s.hazard === "TS" || s.hazard === "CONVECTIVE") return 5;
  if ((s.hazard === "TURB" || s.hazard === "ICE") && /SEV/.test(s.qualifier ?? "")) return 4;
  if (s.hazard === "MTW") return 3;
  return 2;
}

/** Where to put each label so none overlap, as the map's own labels are placed: the most important
 *  first, each at its own point if free, else a little above or below, else to a side; a label with no
 *  room is left out (its outline is still drawn, and a click still shows its text). `items` are
 *  { x, y, w, h, priority } centred boxes in CSS px; returns { x, y } or null for each. */
export function placeLabels(items, width, height) {
  const order = items.map((_, i) => i).sort((a, b) => items[b].priority - items[a].priority || a - b);
  const placed = [];
  const out = items.map(() => null);
  const pad = 2;
  for (const i of order) {
    const { x, y, w, h } = items[i];
    const candidates = [[0, 0], [0, -1.3 * h], [0, 1.3 * h], [0, -2.6 * h], [0, 2.6 * h], [0.6 * w, 0], [-0.6 * w, 0]];
    for (const [dx, dy] of candidates) {
      const b = { x: x + dx, y: y + dy, w, h };
      if (b.x - w / 2 < 0 || b.y - h / 2 < 0 || b.x + w / 2 > width || b.y + h / 2 > height) continue;
      const free = placed.every((p) => b.x - w / 2 - pad >= p.x + p.w / 2 || p.x - p.w / 2 - pad >= b.x + w / 2 || b.y - h / 2 - pad >= p.y + p.h / 2 || p.y - p.h / 2 - pad >= b.y + h / 2);
      if (free) {
        placed.push(b);
        out[i] = { x: b.x, y: b.y };
        break;
      }
    }
  }
  return out;
}

export const SIGMET_COLOR = { TS: "#b91c1c", CONVECTIVE: "#b91c1c", TURB: "#c2410c", ICE: "#1d4ed8", MTW: "#7e22ce", VA: "#78350f", TC: "#9f1239" };

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

/** The panel's status line for `s` (wx_state, or null before the first answer). `progress` is the
 *  layer's own: `radar` = how many of the selection's frames are on the GPU, `satellite` = whether
 *  the selection's image is. */
export function status(s, { radar, satellite, hazards }, progress = {}) {
  if (!radar && !satellite && !hazards) return "";
  if (!s) return "starting…";
  const parts = [];
  const l = s.loading ?? {};
  const sel = s.selection ?? {};
  // what will be shown: the whole loop, or only its newest frame when the loop is held
  const want = progress.radarWant ?? sel.radar?.length ?? 0;
  const past = s.at <= s.now + 300;
  if (radar && want && progress.radar != null && progress.radar < want) parts.push(`radar: loading ${progress.radar} of ${want}…`);
  else if (radar && !want && past && (l.busy || s.radarOk == null)) parts.push("radar: loading…");
  if (satellite && sel.satellite != null && progress.satellite === false) parts.push("satellite: loading…");
  else if (satellite && sel.satellite == null && past && (l.busy || s.satelliteOk == null)) parts.push("satellite: loading…");
  if (hazards && s.hazardsValid == null && (l.hazards || l.busy || s.hazardsOk == null)) parts.push("hazards: loading…");
  else if (hazards && l.hazards) parts.push("hazards: updating…");
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

function program(gl, fs, names) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, VS], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`weather shader: ${gl.getShaderInfoLog(s)}`);
    gl.attachShader(p, s);
  }
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(`weather program: ${gl.getProgramInfoLog(p)}`);
  const u = {};
  for (const n of names) u[n] = gl.getUniformLocation(p, n);
  return { p, u };
}

/** Radar textures kept on the GPU (6 MB each) and satellite textures (3 MB each): the selection's
 *  plus the most recently shown, so stepping back and forth does not fetch again. */
const RADAR_GPU = 15;
const SAT_GPU = 4;

export class WeatherLayer {
  constructor(map, { onChange } = {}) {
    this.map = map;
    this.onChange = onChange;
    this.radarOn = false;
    this.satOn = false;
    this.hazardsOn = false;
    this.metarOn = false;
    this.metars = null; // wx_metars for the chosen time
    this.windOn = false;
    this.wind = null; // the decoded field for the chosen time and level, or { error }
    this.sigmets = null; // those in force at the chosen time (with the hazards layer)
    this.windKey = null;
    this.looping = true; // animate the radar loop; false holds its newest frame
    this.level = null; // flight level for hazards (hundreds of feet), null = all
    this.at = null; // the chosen time (Unix seconds), null = live: the present, following it
    this.gen = 0; // bumped by every change of time: work for an older one stops
    this.state = null;
    this.frames = new Map(); // radar time -> { tex, bytes, grid, used }, on the GPU
    this.sats = new Map(); // satellite time -> { tex, bytes, used }
    this.globals = new Map(); // global mosaic time -> { tex, bytes, box, used }
    this.fetching = new Set(); // "r<time>" / "s<time>" being fetched from Rust
    this.override = null; // radar times to loop instead of the selection (the registration probe)
    this.planned = new Map(); // time-lapse: each step's selection, from buffering
    this.protect = new Set(); // textures prepared for the next step: not evicted
    this.shown = null; // radar frame drawn last
    this.shownSat = null;
    this.loopStart = performance.now();
    this.gpuBytes = 0;
    /** performance.now() of: enabled, first frame on the GPU, hazards present, every frame held */
    this.marks = {};
    const gl = map.gl;
    this.prog = program(gl, FS, ["u_size", "u_dpr", "u_center", "u_scale", "u_rot", "u_grid", "u_dims", "u_opacity", "u_radar", "u_lut"]);
    this.satProg = program(gl, SAT_FS, ["u_size", "u_dpr", "u_center", "u_scale", "u_rot", "u_box", "u_opacity", "u_sat", "u_mask", "u_maskBox", "u_useMask", "u_feather"]);
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
    // a click without a drag asks for the nearest station's report; any move closes the box
    let down = null;
    map.canvas.addEventListener("mousedown", (e) => (down = [e.clientX, e.clientY]));
    map.canvas.addEventListener("click", (e) => {
      if (!down || Math.hypot(e.clientX - down[0], e.clientY - down[1]) > 4) return;
      const r = map.canvas.getBoundingClientRect();
      this.clicked(e.clientX - r.left, e.clientY - r.top).catch((err) => (this.lastError = String(err?.message ?? err)));
    });
    // the report box stays on its station as the map moves
    map.on("render", () => this.placePopup());
    // every frame: is a different radar frame due, or a different satellite image?
    // the winds animate: every frame while they are on and have a field
    map.loop.addUpdater((now) => (this.radarOn && this.frameNow(now) !== this.shown) || (this.satOn && this.satNow() !== this.shownSat) || (this.windOn && !!this.wind?.uv));
  }

  get anyOn() {
    return this.radarOn || this.satOn || this.hazardsOn || this.metarOn || this.windOn;
  }

  /** Turn layers on or off. The first time any goes on, Rust starts fetching. */
  async set({ radar = this.radarOn, satellite = this.satOn, hazards = this.hazardsOn, metars = this.metarOn, winds = this.windOn }) {
    this.marks.enabled ??= performance.now();
    Object.assign(this, { radarOn: radar, satOn: satellite, hazardsOn: hazards, metarOn: metars, windOn: winds });
    if (!metars) this.closePopup();
    if (this.anyOn && !this.timer) {
      this.onChange?.(null); // "starting": shown before the first answer
      // Rust says when anything arrives; the slow poll is only a safety net (and moves "now")
      await listen("wx-changed", () => this.refresh());
      this.state = await invoke("wx_enable");
      this.timer = setInterval(() => this.refresh(), 60000);
    }
    await this.refresh();
    this.map.triggerRepaint();
  }

  /** Choose the time to show (Unix seconds), or null for live. */
  async setTime(at) {
    this.at = at;
    this.gen += 1;
    this.loopStart = performance.now();
    await this.refresh();
  }

  setLevel(fl) {
    this.level = fl;
    this.painted = true; // force a repaint of the outlines
    this.map.triggerRepaint();
    if (this.windOn) this.refresh(); // the winds are per level
  }

  /** Fetch the state for the chosen time and get what it needs onto the GPU. Signals arrive in
   *  bursts (one per frame): a refresh asked for while one runs is folded into one rerun after it. */
  async refresh() {
    if (!this.timer) return;
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
    const gen = this.gen;
    this.state = await invoke("wx_state", { at: this.at });
    if (this.metarOn) {
      const m = await invoke("wx_metars", { at: this.at });
      if (this.gen === gen) this.metars = m;
      if (m.stations.length) this.marks.metars ??= performance.now();
    }
    this.onChange?.(this.state);
    this.map.triggerRepaint();
    // the wind field alongside the radar and satellite, not ahead of them: NOMADS can be slow
    await Promise.all([this.sync(), this.syncWind(gen), this.syncSigmets(gen)]);
    this.onChange?.(this.state);
  }

  /** The selection's radar frames (newest first) and satellite image, fetched from Rust one at a time
   *  and put on the GPU inside a frame. Nothing here touches WebGL outside a frame. */
  async sync() {
    const s = this.state;
    if (!s) return;
    const gen = this.gen;
    const sel = s.selection;
    const jobs = [];
    if (this.satOn && sel.satellite != null && !this.sats.has(sel.satellite)) jobs.push(["s", sel.satellite, 400]);
    if (this.satOn && sel.satelliteGlobal != null && !this.globals.has(sel.satelliteGlobal)) jobs.push(["g", sel.satelliteGlobal, 450]);
    if (this.radarOn && s.grid) {
      // the loop's frames, or only its newest when the loop is held (a time-lapse plays one per step)
      [...(this.looping ? sel.radar : sel.radar.slice(-1))].reverse().forEach((t, i) => {
        if (!this.frames.has(t)) jobs.push(["r", t, i === 0 ? 500 : 3000]);
      });
    }
    // the newest radar frame first, then the satellite image, then the rest of the loop
    jobs.sort((a, b) => a[2] - b[2]);
    for (const [kind, t, priority] of jobs) {
      const key = `${kind}${t}`;
      if (this.fetching.has(key)) continue;
      this.fetching.add(key);
      try {
        const buf = new Uint8Array(await invoke({ r: "wx_frame", s: "wx_satellite", g: "wx_satellite_global" }[kind], { time: t }));
        if (kind === "r") {
          const g = s.grid;
          if (buf.length !== g.ni * g.nj) throw new Error(`radar frame ${t}: ${buf.length} bytes for a ${g.ni} x ${g.nj} grid`);
          // a frame for a time already left behind is not worth a GPU slot
          this.map.loop.arrive(() => this.gen === gen && this.uploadRadar(t, buf, g), priority);
        } else {
          const meta = kind === "s" ? s.satellite : s.satelliteGlobal;
          if (buf.length !== meta.width * meta.height) throw new Error(`satellite ${t}: ${buf.length} bytes for ${meta.width} x ${meta.height}`);
          this.map.loop.arrive(() => this.gen === gen && this.uploadSat(t, buf, meta, kind === "g" ? this.globals : this.sats), priority);
        }
      } catch (e) {
        this.lastError = String(e?.message ?? e);
      } finally {
        this.fetching.delete(key);
      }
      // the chosen time moved on while this was fetched: stop fetching for the old one (refresh()
      // has queued a rerun for the new time, which starts as soon as this returns)
      if (this.gen !== gen) break;
    }
  }

  texture(w, h, buf, filter) {
    const gl = this.map.gl;
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, buf);
    // set explicitly: the default minification filter wants mipmaps, and an incomplete texture reads 0
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, filter);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4);
    return tex;
  }

  /** Inside a frame. */
  uploadRadar(t, buf, g) {
    if (this.frames.has(t)) return;
    const tex = this.texture(g.ni, g.nj, buf, this.map.gl.NEAREST);
    this.frames.set(t, { tex, bytes: buf.length, grid: g, used: performance.now() });
    this.gpuBytes += buf.length;
    this.marks.firstFrame ??= performance.now();
    const want = this.state?.selection?.radar ?? [];
    if (want.length && want.every((x) => this.frames.has(x))) this.marks.allFrames ??= performance.now();
    this.evict(this.frames, RADAR_GPU, new Set(want));
    this.onChange?.(this.state);
  }

  /** Inside a frame. `into`: the GOES images or the global ones. */
  uploadSat(t, buf, meta, into = this.sats) {
    if (into.has(t)) return;
    const tex = this.texture(meta.width, meta.height, buf, this.map.gl.LINEAR);
    const [x0, y0, x1, y1] = meta.bbox3857;
    const [wx0, wy0] = mercToWorld(x0, y1); // top left: west, north
    const [wx1, wy1] = mercToWorld(x1, y0); // bottom right: east, south
    into.set(t, { tex, bytes: buf.length, box: [wx0, wy0, wx1, wy1], used: performance.now() });
    this.gpuBytes += buf.length;
    this.marks.firstSat ??= performance.now();
    const sel = this.state?.selection ?? {};
    this.evict(into, into === this.sats ? SAT_GPU : 2, new Set([into === this.sats ? sel.satellite : sel.satelliteGlobal]));
    this.onChange?.(this.state);
  }

  /** Free the least recently used textures past `keep`, never one the selection needs. */
  evict(map, keep, need) {
    if (map.size <= keep) return;
    // never the selection's, nor what the time-lapse has prepared for its next step
    const victims = [...map.entries()].filter(([t]) => !need.has(t) && !this.protect.has(t)).sort((a, b) => a[1].used - b[1].used);
    for (const [t, f] of victims.slice(0, map.size - keep)) {
      this.map.gl.deleteTexture(f.tex);
      this.gpuBytes -= f.bytes;
      map.delete(t);
    }
  }

  /** The radar loop's frames on the GPU, oldest first. */
  loopTimes() {
    if (this.override) return this.override;
    return (this.state?.selection?.radar ?? []).filter((t) => this.frames.has(t));
  }

  /** Which frame the loop is on: each held 400 ms, the newest 2 s, oldest to newest. */
  frameNow(now = performance.now()) {
    const times = this.loopTimes();
    if (!times.length) return null;
    if (!this.looping) return times.at(-1);
    const period = (times.length - 1) * 400 + 2000;
    const k = (now - this.loopStart) % period;
    return times[Math.min(times.length - 1, Math.floor(k / 400))];
  }

  /** The wind field for the chosen time and level: fetched when either changes, put on the GPU
   *  inside a frame. */
  async syncWind(gen) {
    // the wind field for this time and level: fetched when either changes, on the GPU inside a frame
    const key = `${this.at}|${this.level}`;
    if (this.windOn && key !== this.windKey) {
      this.windKey = key;
      try {
        const f = decodeWind(await invoke("wx_wind", { at: this.at, fl: this.level }));
        if (this.gen === gen && this.windKey === key) {
          this.wind = f;
          this.map.loop.arrive(() => this.windRenderer().setField(f), 600);
          this.marks.wind ??= performance.now();
        }
      } catch (e) {
        this.wind = { error: String(e?.message ?? e) };
        this.windKey = null; // try again on the next refresh
      }
    }
    if (this.state.hazards?.length) this.marks.hazards ??= performance.now();
  }

  /** The SIGMETs in force at the chosen time; a past time's may be fetched first, in Rust. */
  async syncSigmets(gen) {
    if (!this.hazardsOn) return;
    try {
      const list = await invoke("wx_sigmets", { at: this.at });
      if (this.gen !== gen) return;
      this.sigmets = list;
      this.painted = true;
      this.map.triggerRepaint();
    } catch (e) {
      this.lastError = String(e?.message ?? e);
    }
  }

  windRenderer() {
    this.windGl ??= new WindRenderer(this.map.gl);
    return this.windGl;
  }

  /** Time-lapse buffering: everything each time will need, fetched into Rust (network), three times at
   *  a time; nothing is sent to the webview yet. */
  async buffer(times, progress) {
    let done = 0;
    const queue = [...times];
    const worker = async () => {
      while (queue.length) {
        const at = queue.shift();
        try {
          this.planned.set(at, await invoke("wx_prefetch", { at, winds: this.windOn, fl: this.level }));
        } catch (e) {
          this.lastError = String(e?.message ?? e);
        }
        progress?.(++done, times.length, at);
      }
    };
    await Promise.all([worker(), worker(), worker()]);
  }

  /** The textures time `at` shows, on the GPU before the time-lapse steps to it (from Rust's memory,
   *  so quick), and kept from eviction until then. */
  async prepare(at) {
    if (at == null) return;
    const sel = this.planned.get(at) ?? (await invoke("wx_state", { at })).selection;
    this.planned.set(at, sel);
    const g = this.state?.grid, meta = this.state?.satellite, gmeta = this.state?.satelliteGlobal;
    const newest = sel.radar?.at(-1);
    this.protect = new Set([newest, sel.satellite, sel.satelliteGlobal].filter((t) => t != null));
    const arrived = (job, priority) => new Promise((resolve) => this.map.loop.arrive(() => resolve(job()), priority));
    const jobs = [];
    if (this.radarOn && newest != null && g && !this.frames.has(newest)) {
      jobs.push(invoke("wx_frame", { time: newest }).then((b) => arrived(() => this.uploadRadar(newest, new Uint8Array(b), g), 2500)));
    }
    if (this.satOn && sel.satellite != null && meta && !this.sats.has(sel.satellite)) {
      jobs.push(invoke("wx_satellite", { time: sel.satellite }).then((b) => arrived(() => this.uploadSat(sel.satellite, new Uint8Array(b), meta, this.sats), 2500)));
    }
    if (this.satOn && sel.satelliteGlobal != null && gmeta && !this.globals.has(sel.satelliteGlobal)) {
      jobs.push(invoke("wx_satellite_global", { time: sel.satelliteGlobal }).then((b) => arrived(() => this.uploadSat(sel.satelliteGlobal, new Uint8Array(b), gmeta, this.globals), 2600)));
    }
    await Promise.allSettled(jobs);
  }

  /** Whether time `at`'s textures are on the GPU (null: live, always ready). */
  isPrepared(at) {
    if (at == null) return true;
    const sel = this.planned.get(at);
    if (!sel) return false;
    const newest = sel.radar?.at(-1);
    return (!this.radarOn || newest == null || this.frames.has(newest)) && (!this.satOn || sel.satellite == null || this.sats.has(sel.satellite)) && (!this.satOn || sel.satelliteGlobal == null || this.globals.has(sel.satelliteGlobal));
  }

  /** For the time-lapse: is what the chosen time shows on screen? Its newest radar frame (the loop is
   *  held while playing) and its satellite image, when those layers are on and exist then. */
  displayReady() {
    const s = this.state;
    if (!s || this.refreshing) return false;
    const sel = s.selection ?? {};
    const radar = !this.radarOn || !sel.radar?.length || this.frames.has(sel.radar.at(-1));
    const sat = !this.satOn || sel.satellite == null || this.sats.has(sel.satellite);
    return radar && sat;
  }


  /** For status(): the selection's radar frames on the GPU, and whether its satellite image is. */
  progress() {
    const sel = this.state?.selection ?? {};
    const shown = this.looping ? sel.radar ?? [] : (sel.radar ?? []).slice(-1);
    return { radar: shown.filter((t) => this.frames.has(t)).length, radarWant: shown.length, satellite: sel.satellite == null ? null : this.sats.has(sel.satellite) };
  }

  satNow() {
    const t = this.state?.selection?.satellite;
    return t != null && this.sats.has(t) ? t : null;
  }

  /** Called by the renderer after the map, before the labels: the clouds, then the radar on them. */
  draw(gl, map) {
    this.drawn = { radar: false, sat: false };
    const cam = map.cam;
    const common = (u) => {
      gl.uniform2f(u.u_size, cam.w, cam.h);
      gl.uniform1f(u.u_dpr, map.dpr);
      gl.uniform2f(u.u_center, cam.cx, cam.cy);
      gl.uniform1f(u.u_scale, cam.scale);
      gl.uniform2f(u.u_rot, Math.cos(cam.bearing), Math.sin(cam.bearing));
    };
    gl.bindVertexArray(this.vao);
    if (this.satOn) {
      // the chosen time's image, or until it is on the GPU the last one shown: the picture holds
      // rather than going blank for the frames in between (the flash)
      const want = this.state?.selection?.satellite;
      const t = this.satNow() ?? (want != null && this.sats.has(this.shownSat) ? this.shownSat : null);
      this.shownSat = t;
      const s = t == null ? null : this.sats.get(t);
      // with the world's mosaic, one pass: it blends GOES in (SAT_FS); without it, GOES alone
      const gwant = this.state?.selection?.satelliteGlobal;
      const gt = gwant != null && this.globals.has(gwant) ? gwant : gwant != null && this.globals.has(this.shownGlobal) ? this.shownGlobal : null;
      const gs = gt == null ? null : this.globals.get(gt);
      this.shownGlobal = gs ? gt : null;
      if (gs) {
        gs.used = performance.now();
        const { p, u } = this.satProg;
        gl.useProgram(p);
        common(u);
        gl.uniform4f(u.u_box, ...gs.box);
        gl.uniform1f(u.u_opacity, 1);
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, gs.tex);
        gl.uniform1i(u.u_sat, 0);
        gl.activeTexture(gl.TEXTURE1);
        gl.bindTexture(gl.TEXTURE_2D, s ? s.tex : gs.tex);
        gl.uniform1i(u.u_mask, 1);
        gl.uniform4f(u.u_maskBox, ...(s ? s.box : [0, 0, 0, 0]));
        gl.uniform1f(u.u_useMask, s ? 1 : 0);
        gl.uniform1f(u.u_feather, 1.5 / 80); // 1.5 degrees of the GOES box's 80
        gl.activeTexture(gl.TEXTURE0);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
        if (s) {
          this.drawn.sat = true;
          s.used = performance.now();
        }
      }
      if (s && !gs) {
        this.drawn.sat = true;
        s.used = performance.now();
        const { p, u } = this.satProg;
        gl.useProgram(p);
        common(u);
        gl.uniform4f(u.u_box, ...s.box);
        gl.uniform1f(u.u_opacity, 1);
        gl.activeTexture(gl.TEXTURE0);
        gl.bindTexture(gl.TEXTURE_2D, s.tex);
        gl.uniform1i(u.u_sat, 0);
        gl.uniform1f(u.u_useMask, 0);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
      }
    }
    if (this.radarOn) {
      const has = (this.state?.selection?.radar?.length ?? 0) > 0 || this.override;
      const t = this.frameNow() ?? (has && this.frames.has(this.shown) ? this.shown : null);
      this.shown = t;
      const f = t == null ? null : this.frames.get(t);
      if (f) {
        this.drawn.radar = true;
        f.used = performance.now();
        const { p, u } = this.prog;
        gl.useProgram(p);
        common(u);
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
        gl.drawArrays(gl.TRIANGLES, 0, 3);
      }
    }
    gl.bindVertexArray(null);
    if (this.windOn && this.windGl?.field) this.windGl.draw(gl, map);
    // a frame where a layer that has something for the chosen time showed nothing: the flash
    if (this.countBlank) {
      const sel = this.state?.selection ?? {};
      if ((this.radarOn && sel.radar?.length && !this.drawn.radar) || (this.satOn && sel.satellite != null && !this.drawn.sat)) this.blankFrames = (this.blankFrames ?? 0) + 1;
      this.frames_ = (this.frames_ ?? 0) + 1;
    }
  }

  drawHazards() {
    const { canvas: c, ctx, map } = this;
    const dpr = map.dpr, w = map.cam.w, h = map.cam.h;
    if (c.width !== Math.round(w * dpr) || c.height !== Math.round(h * dpr)) {
      c.width = Math.round(w * dpr);
      c.height = Math.round(h * dpr);
    }
    const showHz = this.hazardsOn && (this.state?.hazards?.length > 0 || this.sigmets?.length > 0);
    const showMetars = this.metarOn && this.metars?.stations?.length > 0;
    const show = showHz || showMetars;
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
    // station dots first, so the hazard outlines and their labels stay readable on top
    if (showMetars) {
      this.drawMetars(ctx, w, h);
      ctx.font = "bold 11px 'Noto Sans',sans-serif";
      ctx.textAlign = "center";
    }
    const labels = []; // drawn last, placed so none overlap
    for (const hz of showHz ? this.state.hazards ?? [] : []) {
      if (hz.hazard === "FZLVL") continue; // freezing-level contours: lines across the whole map
      if (!atLevel(hz, this.level)) continue;
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
      labels.push({ text: hazardLabel(hz), x, y, color: style.color, priority: 1 });
    }
    if (showHz && this.sigmets?.length) this.drawSigmets(ctx, labels);
    this.drawLabels(ctx, labels, w, h);
  }

  /** SIGMETs over the G-AIRMETs: heavier outlines in their hazard's colour, labelled; a SIGMET
   *  reported at a point is a ringed marker. Filtered by the flight level like the G-AIRMETs. */
  drawSigmets(ctx, labels) {
    const map = this.map;
    const px = ([lng, lat]) => {
      const p = map.project([lng, lat]);
      return [p.x, p.y];
    };
    ctx.font = "bold 11px 'Noto Sans',sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.setLineDash([]);
    for (const s of this.sigmets) {
      if (!atLevel(s, this.level)) continue;
      const color = SIGMET_COLOR[s.hazard] ?? "#b91c1c";
      ctx.strokeStyle = color;
      ctx.fillStyle = `${color}26`;
      ctx.lineWidth = 2.5;
      let lx = 0, ly = 0, n = 0;
      for (const ring of s.rings) {
        const pts = ring.map(px);
        if (pts.length === 1) {
          ctx.beginPath();
          ctx.arc(pts[0][0], pts[0][1], 6, 0, 2 * Math.PI);
          ctx.fill();
          ctx.stroke();
          [lx, ly, n] = [pts[0][0], pts[0][1] - 14, 1];
          continue;
        }
        ctx.beginPath();
        pts.forEach(([x, y], i) => (i ? ctx.lineTo(x, y) : ctx.moveTo(x, y)));
        ctx.closePath();
        ctx.fill();
        ctx.stroke();
        for (const [x, y] of pts) {
          lx += x;
          ly += y;
          n += 1;
        }
      }
      if (!n) continue;
      labels.push({ text: sigmetLabel(s), x: lx / n, y: ly / n, color, priority: sigmetPriority(s), bold: true });
    }
  }

  /** The hazard labels, placed so none overlap (placeLabels). */
  drawLabels(ctx, labels, w, h) {
    ctx.font = "bold 11px 'Noto Sans',sans-serif";
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.setLineDash([]);
    const boxes = labels.map((l) => ({ x: l.x, y: l.y, w: ctx.measureText(l.text).width + 6, h: 15, priority: l.priority }));
    const at = placeLabels(boxes, w, h);
    this.labelsPlaced = at.filter(Boolean).length;
    this.labelsWanted = labels.length;
    labels.forEach((l, i) => {
      if (!at[i]) return;
      ctx.lineWidth = 3;
      ctx.strokeStyle = "#ffffffd0";
      ctx.strokeText(l.text, at[i].x, at[i].y);
      ctx.fillStyle = l.color;
      ctx.fillText(l.text, at[i].x, at[i].y);
    });
  }

  /** Station dots coloured by flight category, over the hazard outlines; their IDs once zoomed in. */
  drawMetars(ctx, w, h) {
    const map = this.map;
    const z = map.getZoom();
    const r = z < 5 ? 2.5 : z < 8 ? 3.5 : 5;
    ctx.setLineDash([]);
    ctx.lineWidth = 1;
    ctx.strokeStyle = "#ffffff";
    ctx.font = "10px 'Noto Sans',sans-serif";
    ctx.textAlign = "left";
    for (const s of this.metars.stations) {
      const p = map.project([s[1], s[2]]);
      if (p.x < -10 || p.y < -10 || p.x > w + 10 || p.y > h + 10) continue;
      ctx.fillStyle = CATEGORY[s[3]]?.color ?? CATEGORY[0].color;
      ctx.beginPath();
      ctx.arc(p.x, p.y, r, 0, 2 * Math.PI);
      ctx.fill();
      ctx.stroke();
      if (z >= 8) {
        ctx.fillStyle = "#222";
        ctx.fillText(s[0], p.x + r + 2, p.y);
      }
    }
  }

  /** A click on the map: the nearest station's report in a small box (no drag, METARs on). */
  async clicked(x, y) {
    const project = (ll) => this.map.project(ll);
    const s = this.metarOn && this.metars ? nearestStation(this.metars.stations, x, y, 10, project) : null;
    if (!s) {
      // not a station: a SIGMET under the click shows its text, as dispatch tools do
      const sg = this.hazardsOn && this.sigmets ? sigmetAt(this.sigmets.filter((v) => atLevel(v, this.level)), x, y, project) : null;
      if (!sg) return this.closePopup();
      this.showPopup(`${sigmetLabel(sg)}\nvalid ${zulu(sg.from)} to ${zulu(sg.to)}\n${sg.raw}`, this.map.unproject([x, y])); // anchored where it was clicked
      return;
    }
    const r = await invoke("wx_metar", { id: s[0], at: this.at });
    if (!r) return this.closePopup();
    const [raw, t] = r;
    this.showPopup(`${s[0]} · ${CATEGORY[s[3]].name} · ${zulu(t)}\n${raw}`, { lng: s[1], lat: s[2] });
  }

  /** The report box, anchored to a place on the map. */
  showPopup(text, at) {
    if (!this.popup) {
      this.popup = document.createElement("div");
      this.popup.style.cssText = "position:absolute;z-index:25;max-width:420px;padding:6px 8px;background:#fff;border-radius:6px;" +
        "box-shadow:0 2px 8px #0004;font:12px ui-monospace,monospace;color:#222;white-space:pre-wrap;pointer-events:none;";
      this.map.canvas.parentElement.append(this.popup);
    }
    this.popup.textContent = text;
    this.popupAt = [at.lng, at.lat];
    this.popup.style.display = "block";
    this.placePopup();
  }

  placePopup() {
    if (!this.popupAt || !this.popup) return;
    const p = this.map.project(this.popupAt);
    this.popup.style.left = `${Math.round(p.x + 12)}px`;
    this.popup.style.top = `${Math.round(p.y - 10)}px`;
  }

  closePopup() {
    this.popupAt = null;
    if (this.popup) this.popup.style.display = "none";
  }

  /** Registration check: a synthetic frame on a 0.001° grid around `ll`, drawn at zoom 15 twice,
   *  without and with the one cell holding `ll` lit. The pixels that differ are where the GPU put
   *  that cell; their centre must be where the map's own projection (geo.js) puts `ll`, within half
   *  a cell. Two separate implementations of the same math, checked against each other. */
  async probe(ll = [-118.4912, 34.0105]) {
    const gl = this.map.gl;
    const n = 41, d = 0.001;
    const grid = { ni: n, nj: n, lat0: ll[1] + 20 * d, lon0: ll[0] - 20 * d, dlat: d, dlon: d };
    // nothing else may change the picture between the two shots: satellite and winds off
    const saved = { frames: this.frames, looping: this.looping, radarOn: this.radarOn, satOn: this.satOn, windOn: this.windOn, override: this.override };
    Object.assign(this, { looping: false, radarOn: true, satOn: false, windOn: false, override: [0] });
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
      at: s.at,
      selection: s.selection,
      frames: [...this.frames.keys()].length,
      sats: [...this.sats.keys()].length,
      shown: this.shown,
      shown_sat: this.shownSat,
      shown_global: this.shownGlobal ?? null,
      metars: this.metars?.stations?.length ?? null,
      sigmets: this.sigmets?.length ?? null,
      labels: { placed: this.labelsPlaced ?? null, wanted: this.labelsWanted ?? null },
      wind: this.wind?.uv ? { run: this.wind.run, fhr: this.wind.fhr, hpa: this.wind.hpa, valid: this.wind.valid, segments: this.windGl?.lastSegments ?? 0, frames: this.windGl?.frames ?? 0 } : this.wind,
      metar_range: this.metars?.range ?? null,
      last_error: this.lastError ?? null,
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
