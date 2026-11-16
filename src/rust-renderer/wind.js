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

/** Whether a field goes all the way round the world (then it wraps at the 0/360 seam). */
export const isGlobal = (f) => Math.abs(f.ni * f.dlon - 360) < 1e-6;

/** [u, v] (m/s) at a position, bilinear between grid points; null outside the grid. Row 0 is north.
 *  A global field wraps: longitude is taken round the world, and the last column meets the first. */
export function sampleWind(f, lon, lat) {
  const y = (f.lat0 - lat) / f.dlat;
  if (!(y >= 0 && y <= f.nj - 1)) return null;
  let x = (lon - f.lon0) / f.dlon, i, i1;
  if (isGlobal(f)) {
    x = ((x % f.ni) + f.ni) % f.ni;
    i = Math.min(Math.floor(x), f.ni - 1);
    i1 = (i + 1) % f.ni;
  } else {
    if (!(x >= 0 && x <= f.ni - 1)) return null;
    i = Math.min(Math.floor(x), f.ni - 2);
    i1 = i + 1;
  }
  const j = Math.min(Math.floor(y), f.nj - 2);
  const fx = x - i, fy = y - j;
  const at = (ii, jj, c) => f.uv[(jj * f.ni + ii) * 2 + c];
  const lerp = (c) =>
    (at(i, j, c) * (1 - fx) + at(i1, j, c) * fx) * (1 - fy) + (at(i, j + 1, c) * (1 - fx) + at(i1, j + 1, c) * fx) * fy;
  return [lerp(0), lerp(1)];
}

/** The part of `view` the field has wind for, [west, south, east, north], or null. */
export function covered(f, view) {
  const north = f.lat0, south = f.lat0 - (f.nj - 1) * f.dlat;
  const s = Math.max(view[1], south), n = Math.min(view[3], north);
  if (isGlobal(f)) return n > s ? [view[0], s, view[2], n] : null;
  const w = Math.max(view[0], f.lon0), e = Math.min(view[2], f.lon0 + (f.ni - 1) * f.dlon);
  return n > s && e > w ? [w, s, e, n] : null;
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
/** A trail segment longer than this (CSS px) was stretched by the camera, not drawn by the wind (100
 *  m/s moves 4 px a frame): the trail is cut there. */
export const MAX_SEGMENT_PX = 8;

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

  /** Start particle `i` again somewhere in the view; outside `avoid` (the previous view) if given,
   *  which puts it in the part of the view just revealed. */
  respawn(i, [w, s, e, n], avoid = null) {
    let lon, lat;
    for (let tries = 0; tries < 30; tries++) {
      lon = w + this.rng() * (e - w);
      lat = s + this.rng() * (n - s);
      if (!avoid || lon < avoid[0] || lon > avoid[2] || lat < avoid[1] || lat > avoid[3]) break;
    }
    this.lon[i] = this.prevLon[i] = lon;
    this.lat[i] = this.prevLat[i] = lat;
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

  /** Whether particle `i` is in use this frame (see step). */
  active(i) {
    return i < (this.activeCount ?? this.n);
  }

  /** Point `k` of particle `i`'s path (0 = where it is now), in world coordinates. */
  point(i, k) {
    const at = (i * TRAIL + ((this.histHead[i] - k + TRAIL) % TRAIL)) * 2;
    return [this.hist[at], this.hist[at + 1]];
  }

  /** One frame: each particle moves with the wind; one that has lived its life, left the view or left
   *  the field starts again. `view` is [west, south, east, north] in degrees, `scale` the camera's CSS
   *  px per world unit, `prevView` the last frame's view. When the view moved, the restarts go to the
   *  part just revealed, and on a zoom out a share of the rest follows them, so the particles cover
   *  the new view evenly at once instead of staying bunched where the old view was. */
  step(f, view, scale, prevView = null) {
    const pxPerDeg = scale / 360;
    const area = (b) => Math.max(0, b[2] - b[0]) * Math.max(0, b[3] - b[1]);
    const shown = prevView
      ? 1 - area([Math.max(view[0], prevView[0]), Math.max(view[1], prevView[1]), Math.min(view[2], prevView[2]), Math.min(view[3], prevView[3])]) / area(view)
      : 0;
    const avoid = shown > 0.01 ? prevView : null;
    // only as many particles as the part of the view with wind deserves, and only there: otherwise a
    // strip of wind at the field's edge holds every particle and the streaks pile on each other
    const cov = covered(f, view);
    const active = cov ? Math.round((this.n * area(cov)) / area(view)) : 0;
    this.activeCount = active;
    const where = cov ?? view;
    const u = (this.u ??= new Float32Array(this.n)), v = (this.v ??= new Float32Array(this.n));
    const leave = (this.leave ??= new Uint8Array(this.n));
    let leaving = 0;
    for (let i = 0; i < this.n; i += 1) {
      if (i >= active) {
        // idle: not simulated, not drawn; it starts afresh when it is needed again
        this.histLen[i] = 0;
        this.age[i] = Infinity;
        leave[i] = 0;
        continue;
      }
      const lon = this.lon[i], lat = this.lat[i];
      const out = lon < view[0] || lon > view[2] || lat < view[1] || lat > view[3];
      const uv = out || this.age[i] > this.maxAge[i] ? null : sampleWind(f, lon, lat);
      leave[i] = uv ? 0 : 1;
      if (uv) [u[i], v[i]] = uv;
      else leaving += 1;
    }
    // the revealed part's fair share of particles, beyond those already restarting into it
    const extra = avoid ? Math.max(0, shown * active - leaving) : 0;
    const q = extra > 0 ? extra / Math.max(1, active - leaving) : 0;
    for (let i = 0; i < active; i += 1) {
      if (leave[i] || (q > 0 && this.rng() < q)) {
        this.respawn(i, where, avoid);
        continue;
      }
      const lon = this.lon[i], lat = this.lat[i];
      this.prevLon[i] = lon;
      this.prevLat[i] = lat;
      // Web Mercator stretches latitude by 1/cos: a degree of latitude spans more pixels
      this.lon[i] = lon + (u[i] * PX_PER_MPS) / pxPerDeg;
      this.lat[i] = lat + (v[i] * PX_PER_MPS * Math.cos((lat * Math.PI) / 180)) / pxPerDeg;
      this.speed[i] = Math.hypot(u[i], v[i]);
      this.age[i] += 1;
      this.record(i);
    }
  }
}

/** How a frame's wind looks, for the dev tools: how evenly the particle heads cover the screen (the
 *  coefficient of variation of their count over a 4 x 4 grid: 0 is even, 1 is badly bunched), and how
 *  many trail segments are longer than `maxPx` (no particle moves that far in one frame: a longer
 *  segment is a trail stretched by the camera, not by the wind). */
export function frameStats(p, cam, verts, nVerts, maxPx = 12) {
  const cells = new Float64Array(16);
  const c = Math.cos(cam.bearing), s = Math.sin(cam.bearing);
  let heads = 0;
  for (let i = 0; i < p.n; i += 1) {
    if (!p.active(i)) continue;
    const [wx, wy] = p.point(i, 0);
    const dx = (wx - cam.cx) * cam.scale, dy = (wy - cam.cy) * cam.scale;
    const x = dx * c + dy * s + cam.w / 2, y = -dx * s + dy * c + cam.h / 2;
    if (x < 0 || y < 0 || x >= cam.w || y >= cam.h) continue;
    cells[Math.floor((y / cam.h) * 4) * 4 + Math.floor((x / cam.w) * 4)] += 1;
    heads += 1;
  }
  const mean = heads / 16;
  const sd = Math.sqrt(cells.reduce((a, n) => a + (n - mean) ** 2, 0) / 16);
  let long = 0;
  for (let v = 0; v < nVerts; v += 2) {
    const ax = verts[v * 6], ay = verts[v * 6 + 1], bx = verts[(v + 1) * 6], by = verts[(v + 1) * 6 + 1];
    if (Math.hypot(bx - ax, by - ay) > maxPx) long += 1;
  }
  return { cv: mean ? Math.round((sd / mean) * 1000) / 1000 : null, heads, long, segments: nVerts / 2 };
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
      // a long segment is the camera's doing (a zoom since it was recorded): the trail ends here
      if (k > 0 && (x - prevX) ** 2 + (y - prevY) ** 2 > MAX_SEGMENT_PX ** 2) break;
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
    this.particles.step(this.field, view, cam.scale, this.prevView);
    this.prevView = view;
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
    if (this.measure) this.lastStats = frameStats(this.particles, cam, this.verts, n);
  }
}
