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
export class Camera {
  constructor(cx, cy, zoom, w, h) {
    Object.assign(this, { cx, cy, zoom, w, h });
  }
  get scale() {
    return TILE_PX * 2 ** this.zoom; // CSS px per world unit
  }
  worldToScreen(x, y) {
    return [(x - this.cx) * this.scale + this.w / 2, (y - this.cy) * this.scale + this.h / 2];
  }
  screenToWorld(px, py) {
    return [this.cx + (px - this.w / 2) / this.scale, this.cy + (py - this.h / 2) / this.scale];
  }
}

/** The lowest zoom at which the world (512 px at zoom 0) still fills a w x h viewport. */
export const worldFitZoom = (w, h) => Math.max(0, Math.log2(Math.max(w, h) / TILE_PX));

/**
 * Keep a camera on the world: zoom no lower than worldFitZoom, and the center where no edge of the
 * view passes the world's edge (no wrap-around). Returns the camera, adjusted in place.
 */
export function clampCamera(cam, maxZoom = 22) {
  cam.zoom = Math.min(maxZoom, Math.max(worldFitZoom(cam.w, cam.h), cam.zoom));
  const hw = cam.w / 2 / cam.scale;
  const hh = cam.h / 2 / cam.scale;
  cam.cx = Math.min(1 - hw, Math.max(hw, cam.cx));
  cam.cy = Math.min(1 - hh, Math.max(hh, cam.cy));
  return cam;
}

/** The zoom of the tiles a camera draws: floor(zoom), clamped to the archive's range. */
export const tileZoom = (zoom, minZ, maxZ) => Math.max(minZ, Math.min(maxZ, Math.floor(zoom)));

/** Tiles `[z, x, y]` covering the view, nearest the center first (the order to request them). */
export function visibleTiles(cam, minZ, maxZ) {
  const z = tileZoom(cam.zoom, minZ, maxZ);
  const n = 2 ** z;
  const [x0, y0] = cam.screenToWorld(0, 0);
  const [x1, y1] = cam.screenToWorld(cam.w, cam.h);
  const tx0 = Math.max(0, Math.floor(x0 * n));
  const ty0 = Math.max(0, Math.floor(y0 * n));
  const tx1 = Math.min(n - 1, Math.floor(x1 * n));
  const ty1 = Math.min(n - 1, Math.floor(y1 * n));
  const out = [];
  for (let y = ty0; y <= ty1; y++) for (let x = tx0; x <= tx1; x++) out.push([z, x, y]);
  const cx = cam.cx * n - 0.5;
  const cy = cam.cy * n - 0.5;
  out.sort((a, b) => (a[1] - cx) ** 2 + (a[2] - cy) ** 2 - ((b[1] - cx) ** 2 + (b[2] - cy) ** 2));
  return out;
}

/**
 * The ring of tiles `margin` tiles beyond the view at the same zoom (not the visible ones), nearest
 * the center first: what to fetch before it scrolls into view.
 */
export function ringTiles(cam, minZ, maxZ, margin = 1) {
  if (margin <= 0) return [];
  const z = tileZoom(cam.zoom, minZ, maxZ);
  const n = 2 ** z;
  const [x0, y0] = cam.screenToWorld(0, 0);
  const [x1, y1] = cam.screenToWorld(cam.w, cam.h);
  const vx0 = Math.floor(x0 * n), vy0 = Math.floor(y0 * n), vx1 = Math.floor(x1 * n), vy1 = Math.floor(y1 * n);
  const out = [];
  for (let y = Math.max(0, vy0 - margin); y <= Math.min(n - 1, vy1 + margin); y++) {
    for (let x = Math.max(0, vx0 - margin); x <= Math.min(n - 1, vx1 + margin); x++) {
      if (x >= vx0 && x <= vx1 && y >= vy0 && y <= vy1) continue;
      out.push([z, x, y]);
    }
  }
  const cx = cam.cx * n - 0.5;
  const cy = cam.cy * n - 0.5;
  out.sort((a, b) => (a[1] - cx) ** 2 + (a[2] - cy) ** 2 - ((b[1] - cx) ** 2 + (b[2] - cy) ** 2));
  return out;
}

/** A tile's rectangle on screen, CSS px: `{ x, y, w, h }`. */
export function tileRect(cam, z, x, y) {
  const n = 2 ** z;
  const [sx, sy] = cam.worldToScreen(x / n, y / n);
  const size = cam.scale / n;
  return { x: sx, y: sy, w: size, h: size };
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
