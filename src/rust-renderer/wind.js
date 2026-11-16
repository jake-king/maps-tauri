// Winds aloft as moving particles, the way dispatch and weather displays show the jet stream: each
// particle drifts with the GFS wind at the chosen flight level and leaves a fading trail, so the
// streaks run along the wind and longer streaks mean stronger wind.
//
// The simulation is plain JavaScript on Float64 positions in longitude/latitude (pure, tested in
// tests/wind.test.js). Each particle keeps its last TRAIL positions in world coordinates, and every
// frame those paths are projected through the current camera and drawn as lines fading from head to
// tail. Nothing is kept in screen space, so a pan, zoom or turn moves the trails exactly with the map;
// an earlier version kept the trails in a texture and resampled it each frame, which blurred them and
// left ghost copies during a zoom. A particle moves a fixed number of screen pixels per m/s at any
// zoom, so the picture reads the same zoomed in or out.

/** Screen pixels a particle moves per frame for each m/s of wind: 50 m/s is 2 px a frame. */
export const PX_PER_MPS = 0.04;
const HEADER = 48;

/** The field Rust sends (wx::wind::encode). */
export function decodeWind(buf) {
  if (buf.byteLength < HEADER) throw new Error(`wind field: ${buf.byteLength} bytes, short of the header`);
  const d = new DataView(buf);
  const ni = d.getUint32(0, true), nj = d.getUint32(4, true);
  const [lat0, lon0, dlat, dlon] = [8, 12, 16, 20].map((o) => d.getFloat32(o, true));
  const run = Number(d.getBigInt64(24, true));
  const fhr = d.getUint32(32, true), hpa = d.getUint32(36, true);
  const n = (buf.byteLength - HEADER) / 4;
  if (n !== ni * nj * 2) throw new Error(`wind field: expected ${ni * nj * 2} values for ${ni} x ${nj}, got ${n}`);
  return { ni, nj, lat0, lon0, dlat, dlon, run, fhr, hpa, valid: run + fhr * 3600, uv: new Float32Array(buf, HEADER) };
}

/** [u, v] (m/s) at a position, bilinear between grid points; null outside the grid. Row 0 is north. */
export function sampleWind(f, lon, lat) {
  const x = (lon - f.lon0) / f.dlon, y = (f.lat0 - lat) / f.dlat;
  if (!(x >= 0 && y >= 0 && x <= f.ni - 1 && y <= f.nj - 1)) return null;
  const i = Math.min(Math.floor(x), f.ni - 2), j = Math.min(Math.floor(y), f.nj - 2);
  const fx = x - i, fy = y - j;
  const at = (ii, jj, c) => f.uv[(jj * f.ni + ii) * 2 + c];
  const lerp = (c) =>
    (at(i, j, c) * (1 - fx) + at(i + 1, j, c) * fx) * (1 - fy) + (at(i, j + 1, c) * (1 - fx) + at(i + 1, j + 1, c) * fx) * fy;
  return [lerp(0), lerp(1)];
}

/** Colour by speed (m/s), as [r, g, b, a] in 0..1: dark and faint when calm, magenta in the jet. */
export function speedColor(s) {
  if (s < 15) return [0.27, 0.3, 0.38, 0.55];
  if (s < 30) return [0.06, 0.46, 0.56, 0.75];
  if (s < 50) return [0.49, 0.23, 0.93, 0.85];
  return [0.75, 0.09, 0.36, 0.95];
}

/** Points of history a trail keeps: about a quarter of a second at 60 frames a second. */
export const TRAIL = 16;

const toWorldX = (lon) => (lon + 180) / 360;
const toWorldY = (lat) => {
  const t = Math.sin((lat * Math.PI) / 180);
  return 0.5 - Math.log((1 + t) / (1 - t)) / (4 * Math.PI);
};

export class Particles {
  constructor(n, rng = Math.random) {
    // the recent path, in world coordinates (Web Mercator, [0, 1)): a ring of TRAIL points each
    this.hist = new Float64Array(n * TRAIL * 2);
    this.histHead = new Uint16Array(n);
    this.histLen = new Uint16Array(n);
    this.n = n;
    this.rng = rng;
    this.lon = new Float64Array(n);
    this.lat = new Float64Array(n);
    this.prevLon = new Float64Array(n);
    this.prevLat = new Float64Array(n);
    this.age = new Float64Array(n).fill(Infinity); // all start by respawning
    this.maxAge = Float64Array.from({ length: n }, () => 40 + Math.floor(rng() * 60));
    this.speed = new Float32Array(n);
  }

  respawn(i, [w, s, e, n]) {
    this.lon[i] = this.prevLon[i] = w + this.rng() * (e - w);
    this.lat[i] = this.prevLat[i] = s + this.rng() * (n - s);
    this.age[i] = 0;
    this.speed[i] = 0;
    this.histLen[i] = 0;
    this.record(i);
  }

  /** Add the particle's position to the head of its path. */
  record(i) {
    const h = (this.histHead[i] + 1) % TRAIL;
    this.histHead[i] = h;
    this.hist[(i * TRAIL + h) * 2] = toWorldX(this.lon[i]);
    this.hist[(i * TRAIL + h) * 2 + 1] = toWorldY(this.lat[i]);
    if (this.histLen[i] < TRAIL) this.histLen[i] += 1;
  }

  /** Point `k` of particle `i`'s path (0 = where it is now), in world coordinates. */
  point(i, k) {
    const at = (i * TRAIL + ((this.histHead[i] - k + TRAIL) % TRAIL)) * 2;
    return [this.hist[at], this.hist[at + 1]];
  }

  /** One frame: each particle moves with the wind; one that has lived its life, left the view or left
   *  the field starts again somewhere in the view. `view` is [west, south, east, north] in degrees,
   *  `scale` the camera's CSS px per world unit. */
  step(f, view, scale) {
    const pxPerDeg = scale / 360;
    for (let i = 0; i < this.n; i += 1) {
      const lon = this.lon[i], lat = this.lat[i];
      const out = lon < view[0] || lon > view[2] || lat < view[1] || lat > view[3];
      const uv = out || this.age[i] > this.maxAge[i] ? null : sampleWind(f, lon, lat);
      if (!uv) {
        this.respawn(i, view);
        continue;
      }
      const [u, v] = uv;
      this.prevLon[i] = lon;
      this.prevLat[i] = lat;
      // Web Mercator stretches latitude by 1/cos: a degree of latitude spans more pixels
      this.lon[i] = lon + (u * PX_PER_MPS) / pxPerDeg;
      this.lat[i] = lat + (v * PX_PER_MPS * Math.cos((lat * Math.PI) / 180)) / pxPerDeg;
      this.speed[i] = Math.hypot(u, v);
      this.age[i] += 1;
      this.record(i);
    }
  }
}

/** The trails as line segments for the GPU: [x, y (CSS px), r, g, b, a] per vertex, two per segment,
 *  from each particle's head back along its path, fading to nothing at the tail. Projected through
 *  `cam` every frame, so a trail is where its places are on the map now, whatever the camera did:
 *  nothing is carried over in screen space. Returns the number of vertices written. */
export function trailVertices(p, cam, out) {
  const c = Math.cos(cam.bearing), s = Math.sin(cam.bearing);
  const scale = cam.scale, cx = cam.cx, cy = cam.cy, hw = cam.w / 2, hh = cam.h / 2;
  let v = 0;
  for (let i = 0; i < p.n; i += 1) {
    const len = p.histLen[i];
    if (len < 2) continue;
    const [r, g, b, a] = speedColor(p.speed[i]);
    let prevX = 0, prevY = 0;
    for (let k = 0; k < len; k += 1) {
      const at = (i * TRAIL + ((p.histHead[i] - k + TRAIL) % TRAIL)) * 2;
      const dx = (p.hist[at] - cx) * scale, dy = (p.hist[at + 1] - cy) * scale;
      const x = dx * c + dy * s + hw, y = -dx * s + dy * c + hh;
      if (k > 0) {
        const a0 = a * (1 - (k - 1) / (TRAIL - 1)), a1 = a * (1 - k / (TRAIL - 1));
        out.set([prevX, prevY, r, g, b, a0, x, y, r, g, b, a1], v * 6);
        v += 2;
      }
      prevX = x;
      prevY = y;
    }
  }
  return v;
}

const SEG_VS = `#version 300 es
in vec2 a_px;      // CSS px, y down
in vec4 a_color;
uniform vec2 u_css; // canvas size in CSS px
out vec4 v_color;
void main() {
  v_color = a_color;
  gl_Position = vec4(a_px.x / u_css.x * 2.0 - 1.0, 1.0 - a_px.y / u_css.y * 2.0, 0.0, 1.0);
}`;
const SEG_FS = `#version 300 es
precision mediump float;
in vec4 v_color;
out vec4 o;
void main() {
  o = vec4(v_color.rgb * v_color.a, v_color.a);
}`;

function program(gl, vs, fs, attribs = []) {
  const p = gl.createProgram();
  for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
    const s = gl.createShader(type);
    gl.shaderSource(s, src);
    gl.compileShader(s);
    if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(`wind shader: ${gl.getShaderInfoLog(s)}`);
    gl.attachShader(p, s);
  }
  attribs.forEach((a, i) => gl.bindAttribLocation(p, i, a));
  gl.linkProgram(p);
  if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(`wind program: ${gl.getProgramInfoLog(p)}`);
  return p;
}

/** The GPU side: each frame, every trail as fading line segments straight onto the screen. */
export class WindRenderer {
  constructor(gl, count = 5000) {
    this.gl = gl;
    this.particles = new Particles(count);
    this.field = null;
    this.prog = program(gl, SEG_VS, SEG_FS, ["a_px", "a_color"]);
    this.uCss = gl.getUniformLocation(this.prog, "u_css");
    this.vao = gl.createVertexArray();
    this.buf = gl.createBuffer();
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 24, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 4, gl.FLOAT, false, 24, 8);
    gl.bindVertexArray(null);
    this.verts = new Float32Array(count * TRAIL * 2 * 6);
    this.frames = 0;
  }

  setField(f) {
    this.field = f;
  }

  /** One frame: step the particles, draw their trails. Inside the map's frame, after the radar. */
  draw(gl, map) {
    if (!this.field) return;
    const cam = map.cam;
    const q = cam.viewQuad(0).map(([x, y]) => [x * 360 - 180, (Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * 180) / Math.PI]);
    const view = [Math.min(...q.map((p) => p[0])), Math.min(...q.map((p) => p[1])), Math.max(...q.map((p) => p[0])), Math.max(...q.map((p) => p[1]))];
    this.particles.step(this.field, view, cam.scale);
    const n = trailVertices(this.particles, cam, this.verts);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.useProgram(this.prog);
    gl.uniform2f(this.uCss, cam.w, cam.h);
    gl.bindVertexArray(this.vao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.buf);
    gl.bufferData(gl.ARRAY_BUFFER, this.verts.subarray(0, n * 6), gl.STREAM_DRAW);
    gl.drawArrays(gl.LINES, 0, n);
    gl.bindVertexArray(null);
    this.frames += 1;
    this.lastSegments = n / 2;
  }
}
