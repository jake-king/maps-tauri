// Winds aloft as moving particles, the way dispatch and weather displays show the jet stream: each
// particle drifts with the GFS wind at the chosen flight level and leaves a fading trail, so the
// streaks run along the wind and longer streaks mean stronger wind.
//
// The simulation is plain JavaScript on Float64 positions in longitude/latitude (pure, tested in
// tests/wind.test.js); each frame the moved segments are projected to screen pixels and drawn on the
// GPU into a trail texture that fades a little every frame, then composited over the map. A particle
// moves a fixed number of screen pixels per m/s at any zoom, so the picture reads the same zoomed in
// or out; the trails are cleared whenever the camera moves.

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

export class Particles {
  constructor(n, rng = Math.random) {
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
    }
  }
}

const QUAD_VS = `#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`;
// the previous trails, a little fainter each frame
const FADE_FS = `#version 300 es
precision mediump float;
uniform sampler2D u_tex;
uniform vec2 u_size;
uniform float u_fade;
out vec4 o;
void main() {
  o = texture(u_tex, gl_FragCoord.xy / u_size) * u_fade;
}`;
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

/** The GPU side: trails in two textures used in turn, segments drawn into the newer one. */
export class WindRenderer {
  constructor(gl, count = 5000) {
    this.gl = gl;
    this.particles = new Particles(count);
    this.field = null;
    this.fadeProg = program(gl, QUAD_VS, FADE_FS);
    this.segProg = program(gl, SEG_VS, SEG_FS, ["a_px", "a_color"]);
    this.quadVao = gl.createVertexArray();
    this.segVao = gl.createVertexArray();
    this.segBuf = gl.createBuffer();
    gl.bindVertexArray(this.segVao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.segBuf);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 24, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 4, gl.FLOAT, false, 24, 8);
    gl.bindVertexArray(null);
    this.verts = new Float32Array(count * 2 * 6);
    this.targets = null;
    this.camKey = null;
    this.frames = 0;
  }

  setField(f) {
    this.field = f;
    this.clear();
  }

  /** Two colour textures the size of the canvas, each with a framebuffer. */
  ensureTargets(w, h) {
    const gl = this.gl;
    if (this.targets && this.targets.w === w && this.targets.h === h) return;
    if (this.targets) for (const t of this.targets.list) {
      gl.deleteTexture(t.tex);
      gl.deleteFramebuffer(t.fb);
    }
    const make = () => {
      const tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA8, w, h, 0, gl.RGBA, gl.UNSIGNED_BYTE, null);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
      const fb = gl.createFramebuffer();
      gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
      return { tex, fb };
    };
    this.targets = { w, h, list: [make(), make()], cur: 0 };
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    this.clear();
  }

  clear() {
    this.cleared = true;
  }

  /** One frame: step the particles, fade the old trails, draw the new segments, put it on screen.
   *  Called inside the map's frame, after the map and the radar. */
  draw(gl, map) {
    if (!this.field) return;
    const cam = map.cam;
    const W = gl.drawingBufferWidth, H = gl.drawingBufferHeight;
    this.ensureTargets(W, H);
    // trails belong to one view: any camera change starts them again
    const key = `${cam.cx},${cam.cy},${cam.zoom},${cam.bearing},${W},${H}`;
    if (key !== this.camKey) {
      this.camKey = key;
      this.cleared = true;
    }
    const q = cam.viewQuad(0).map(([x, y]) => {
      const lng = x * 360 - 180;
      const lat = (Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * 180) / Math.PI;
      return [lng, lat];
    });
    const view = [Math.min(...q.map((p) => p[0])), Math.min(...q.map((p) => p[1])), Math.max(...q.map((p) => p[0])), Math.max(...q.map((p) => p[1]))];
    const P = this.particles;
    P.step(this.field, view, cam.scale);
    let n = 0;
    const v = this.verts;
    for (let i = 0; i < P.n; i += 1) {
      if (P.prevLon[i] === P.lon[i] && P.prevLat[i] === P.lat[i]) continue; // just respawned
      const a = map.project([P.prevLon[i], P.prevLat[i]]), b = map.project([P.lon[i], P.lat[i]]);
      const c = speedColor(P.speed[i]);
      v.set([a.x, a.y, ...c, b.x, b.y, ...c], n * 12);
      n += 1;
    }
    const t = this.targets;
    const prev = t.list[t.cur], next = t.list[1 - t.cur];
    gl.bindFramebuffer(gl.FRAMEBUFFER, next.fb);
    gl.viewport(0, 0, W, H);
    gl.disable(gl.BLEND);
    if (this.cleared) {
      gl.clearColor(0, 0, 0, 0);
      gl.clear(gl.COLOR_BUFFER_BIT);
      this.cleared = false;
    } else {
      gl.useProgram(this.fadeProg);
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, prev.tex);
      gl.uniform1i(gl.getUniformLocation(this.fadeProg, "u_tex"), 0);
      gl.uniform2f(gl.getUniformLocation(this.fadeProg, "u_size"), W, H);
      gl.uniform1f(gl.getUniformLocation(this.fadeProg, "u_fade"), 0.94);
      gl.bindVertexArray(this.quadVao);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
    gl.useProgram(this.segProg);
    gl.uniform2f(gl.getUniformLocation(this.segProg, "u_css"), cam.w, cam.h);
    gl.bindVertexArray(this.segVao);
    gl.bindBuffer(gl.ARRAY_BUFFER, this.segBuf);
    gl.bufferData(gl.ARRAY_BUFFER, v.subarray(0, n * 12), gl.STREAM_DRAW);
    gl.lineWidth(1);
    gl.drawArrays(gl.LINES, 0, n * 2);
    // the trails onto the screen, over the map and the radar
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, W, H);
    gl.useProgram(this.fadeProg);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, next.tex);
    // set here too: after a clear the fade pass did not run, and its uniforms may never have been set
    gl.uniform1i(gl.getUniformLocation(this.fadeProg, "u_tex"), 0);
    gl.uniform2f(gl.getUniformLocation(this.fadeProg, "u_size"), W, H);
    gl.uniform1f(gl.getUniformLocation(this.fadeProg, "u_fade"), 1.0);
    gl.bindVertexArray(this.quadVao);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    gl.bindVertexArray(null);
    t.cur = 1 - t.cur;
    this.frames += 1;
    this.lastSegments = n;
  }
}
