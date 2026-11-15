// Pure pieces of the native renderer: Web Mercator camera math, tile coverage, width interpolation,
// and the mesh wire format's header. No WebGL, no DOM: tested in tests/geo.test.js.

const TILE_PX = 512; // the vector pyramid's tile size: a zoom-z tile is 512 px at zoom z

/** Longitude/latitude to world coordinates in [0, 1) (x east, y south). */
export function lngLatToWorld(lng, lat) {
  const s = Math.sin((lat * Math.PI) / 180);
  return [(lng + 180) / 360, 0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)];
}

export function worldToLngLat(x, y) {
  const lat = (Math.atan(Math.sinh(Math.PI * (1 - 2 * y))) * 180) / Math.PI;
  return [x * 360 - 180, lat];
}

/** A camera: world center, fractional zoom, viewport size in CSS pixels. */
/**
 * A camera: world center, fractional zoom, viewport size in CSS pixels, and a bearing in radians (the
 * direction the top of the screen faces, clockwise from north, as MapLibre's bearing). World to
 * screen is: offset from the center, scale, turn by -bearing about the screen center.
 */
export class Camera {
  constructor(cx, cy, zoom, w, h, bearing = 0) {
    Object.assign(this, { cx, cy, zoom, w, h, bearing });
  }
  get scale() {
    return TILE_PX * 2 ** this.zoom; // CSS px per world unit
  }
  /** A world point as unrotated pixels from the screen center (the frame tiles are drawn in). */
  mapPx(x, y) {
    return [(x - this.cx) * this.scale, (y - this.cy) * this.scale];
  }
  worldToScreen(x, y) {
    const [dx, dy] = this.mapPx(x, y);
    const c = Math.cos(this.bearing), s = Math.sin(this.bearing);
    return [dx * c + dy * s + this.w / 2, -dx * s + dy * c + this.h / 2];
  }
  screenToWorld(px, py) {
    const dx = px - this.w / 2, dy = py - this.h / 2;
    const c = Math.cos(this.bearing), s = Math.sin(this.bearing);
    return [this.cx + (dx * c - dy * s) / this.scale, this.cy + (dx * s + dy * c) / this.scale];
  }
  /** The view's corners in world coordinates (top left, top right, bottom right, bottom left),
   *  grown by `pad` screen pixels on every side. */
  viewQuad(pad = 0) {
    return [[-pad, -pad], [this.w + pad, -pad], [this.w + pad, this.h + pad], [-pad, this.h + pad]].map(([x, y]) => this.screenToWorld(x, y));
  }
}

/** Half the world-axis extent of a w x h view turned by `bearing`, in screen pixels: [x, y]. */
const turnedExtent = (w, h, bearing) => {
  const c = Math.abs(Math.cos(bearing)), s = Math.abs(Math.sin(bearing));
  return [(w * c + h * s) / 2, (w * s + h * c) / 2];
};

/** The lowest zoom at which the world (512 px at zoom 0) still fills a w x h viewport turned by
 *  `bearing`. */
export const worldFitZoom = (w, h, bearing = 0) => {
  const [ex, ey] = turnedExtent(w, h, bearing);
  return Math.max(0, Math.log2((2 * Math.max(ex, ey)) / TILE_PX));
};

/**
 * Keep a camera on the world: zoom no lower than worldFitZoom, and the center where no corner of the
 * (possibly turned) view passes the world's edge (no wrap-around). Returns the camera, adjusted in place.
 */
export function clampCamera(cam, maxZoom = 22) {
  cam.zoom = Math.min(maxZoom, Math.max(worldFitZoom(cam.w, cam.h, cam.bearing), cam.zoom));
  const [ex, ey] = turnedExtent(cam.w, cam.h, cam.bearing);
  const hw = ex / cam.scale;
  const hh = ey / cam.scale;
  cam.cx = Math.min(1 - hw, Math.max(hw, cam.cx));
  cam.cy = Math.min(1 - hh, Math.max(hh, cam.cy));
  return cam;
}

/** The zoom of the tiles a camera draws: floor(zoom), clamped to the archive's range. */
export const tileZoom = (zoom, minZ, maxZ) => Math.max(minZ, Math.min(maxZ, Math.floor(zoom)));

/** Does the convex quad `q` (4 points) overlap the axis-aligned box [x0, x1] x [y0, y1]? (Separating
 *  axes: the box's two, and the quad's edge normals.) Touching edges do not count. */
export function quadIntersectsRect(q, x0, y0, x1, y1) {
  const box = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
  const axes = [[1, 0], [0, 1]];
  for (let i = 0; i < 4; i++) {
    const [a, b] = [q[i], q[(i + 1) % 4]];
    axes.push([a[1] - b[1], b[0] - a[0]]);
  }
  for (const [ax, ay] of axes) {
    let qmin = Infinity, qmax = -Infinity, bmin = Infinity, bmax = -Infinity;
    for (const [x, y] of q) {
      const d = x * ax + y * ay;
      qmin = Math.min(qmin, d);
      qmax = Math.max(qmax, d);
    }
    for (const [x, y] of box) {
      const d = x * ax + y * ay;
      bmin = Math.min(bmin, d);
      bmax = Math.max(bmax, d);
    }
    if (qmax <= bmin || bmax <= qmin) return false;
  }
  return true;
}

/** Tiles at zoom z overlapping the world quad `q`, nearest the camera's center first. */
function tilesInQuad(cam, z, q) {
  const n = 2 ** z;
  const xs = q.map((p) => p[0]), ys = q.map((p) => p[1]);
  const tx0 = Math.max(0, Math.floor(Math.min(...xs) * n)), tx1 = Math.min(n - 1, Math.floor(Math.max(...xs) * n));
  const ty0 = Math.max(0, Math.floor(Math.min(...ys) * n)), ty1 = Math.min(n - 1, Math.floor(Math.max(...ys) * n));
  const out = [];
  for (let y = ty0; y <= ty1; y++) {
    for (let x = tx0; x <= tx1; x++) {
      if (quadIntersectsRect(q, x / n, y / n, (x + 1) / n, (y + 1) / n)) out.push([z, x, y]);
    }
  }
  const cx = cam.cx * n - 0.5;
  const cy = cam.cy * n - 0.5;
  out.sort((a, b) => (a[1] - cx) ** 2 + (a[2] - cy) ** 2 - ((b[1] - cx) ** 2 + (b[2] - cy) ** 2));
  return out;
}

/** Tiles `[z, x, y]` covering the (possibly turned) view, nearest the center first (the order to
 *  request them). Only tiles that actually overlap the view, not its bounding box. */
export function visibleTiles(cam, minZ, maxZ) {
  return tilesInQuad(cam, tileZoom(cam.zoom, minZ, maxZ), cam.viewQuad());
}

/**
 * The ring of tiles about `margin` tiles beyond the view at the same zoom (not the visible ones),
 * nearest the center first: what to fetch before it scrolls into view.
 */
export function ringTiles(cam, minZ, maxZ, margin = 1) {
  if (margin <= 0) return [];
  const z = tileZoom(cam.zoom, minZ, maxZ);
  const tilePx = cam.scale / 2 ** z;
  const visible = new Set(visibleTiles(cam, minZ, maxZ).map((t) => t.join()));
  return tilesInQuad(cam, z, cam.viewQuad(margin * tilePx)).filter((t) => !visible.has(t.join()));
}

/** A tile's square in the unrotated frame, CSS px, relative to the screen's top left as if the
 *  bearing were 0: `{ x, y, w, h }`. The renderer turns this frame about the screen center. */
export function tileRect(cam, z, x, y) {
  const n = 2 ** z;
  const [mx, my] = cam.mapPx(x / n, y / n);
  const size = cam.scale / n;
  return { x: mx + cam.w / 2, y: my + cam.h / 2, w: size, h: size };
}

/** The ancestor of z/x/y at zoom `pz` (pz <= z). */
export const ancestor = (z, x, y, pz) => [pz, x >> (z - pz), y >> (z - pz)];

/** MapLibre's ["interpolate", ["exponential", base], ["zoom"], z0, v0, z1, v1, ...]. */
export function interpolate(stops, base, z) {
  if (z <= stops[0][0]) return stops[0][1];
  const last = stops[stops.length - 1];
  if (z >= last[0]) return last[1];
  let i = 0;
  while (z >= stops[i + 1][0]) i++;
  const [z0, v0] = stops[i];
  const [z1, v1] = stops[i + 1];
  const d = z1 - z0;
  const t = base === 1 ? (z - z0) / d : (base ** (z - z0) - 1) / (base ** d - 1);
  return v0 + t * (v1 - v0);
}

/**
 * The mesh wire format's header (crates/vtile/src/mesh.rs, version 5): counts, index widths and the
 * byte offset of each block. Throws on a wrong magic or version, or a size that does not add up.
 */
export function parseMeshHeader(buf) {
  const v = new DataView(buf);
  if (buf.byteLength < 36) throw new Error("mesh: shorter than its header");
  const magic = String.fromCharCode(v.getUint8(0), v.getUint8(1), v.getUint8(2), v.getUint8(3));
  if (magic !== "MESH") throw new Error(`mesh: bad magic ${magic}`);
  const version = v.getUint32(4, true);
  if (version !== 5) throw new Error(`mesh: version ${version}, expected 5`);
  const flags = v.getUint32(8, true);
  const [fillV, fillI, lineV, lineI, labelN, labelBytes] = [12, 16, 20, 24, 28, 32].map((o) => v.getUint32(o, true));
  const pad4 = (n) => (n + 3) & ~3;
  const fillWide = (flags & 1) !== 0;
  const lineWide = (flags & 2) !== 0;
  const fillVerts = 36;
  const fillIdx = fillVerts + pad4(fillV * 6);
  const lineVerts = fillIdx + pad4(fillI * (fillWide ? 4 : 2));
  const lineIdx = lineVerts + lineV * 8;
  const labels = lineIdx + pad4(lineI * (lineWide ? 4 : 2));
  const end = labels + labelBytes;
  if (end !== buf.byteLength) throw new Error(`mesh: ${buf.byteLength} bytes, header says ${end}`);
  return { fillV, fillI, lineV, lineI, labelN, fillWide, lineWide, fillVerts, fillIdx, lineVerts, lineIdx, labels };
}

const utf8 = new TextDecoder();

/** The label block: `[{ x, y, angle, run, kind, minZoom, icon, color, priority, text, path }]`, x/y/run
 *  in tile units; icon is a sprite index + 1 (0: none); path, for a street or stream name, the line
 *  the text follows ([[x, y], ...] in tile units), else null. */
export function parseLabels(buf, h) {
  const v = new DataView(buf);
  const out = new Array(h.labelN);
  let o = h.labels;
  for (let i = 0; i < h.labelN; i++) {
    const len = v.getUint8(o + 16);
    out[i] = {
      x: v.getInt16(o, true),
      y: v.getInt16(o + 2, true),
      angle: v.getInt16(o + 4, true) / 10000,
      run: v.getUint16(o + 6, true),
      kind: v.getUint8(o + 8),
      minZoom: v.getUint8(o + 9),
      icon: v.getUint8(o + 10),
      color: v.getUint8(o + 11),
      priority: v.getInt32(o + 12, true),
      text: utf8.decode(new Uint8Array(buf, o + 17, len)),
      path: null,
    };
    const np = v.getUint8(o + 17 + len);
    if (np) {
      const path = new Array(np);
      for (let k = 0; k < np; k++) {
        const q = o + 18 + len + 4 * k;
        path[k] = [v.getInt16(q, true), v.getInt16(q + 2, true)];
      }
      out[i].path = path;
    }
    o += 18 + len + 4 * np;
  }
  return out;
}
