// Labels for the native renderer. Rust has already chosen them (crates/vtile mesh.rs: place points,
// and each road name's longest straight segment in a tile); this file decides which to show this
// frame, without overlaps, and draws them on a 2D canvas over the map with the browser's text
// rendering (full Unicode shaping for free).

import palette from "./palette.json" with { type: "json" };

export const KIND = { City: 0, Suburb: 1, Neighbourhood: 2, RoadHighway: 3, RoadMajor: 4, RoadMinor: 5 };

// font and the lowest zoom a kind is shown at, on top of the label's own min_zoom from the data
const STYLE = [
  { font: (z) => `500 ${z < 10 ? 13 : z < 12 ? 15 : 16}px "Noto Sans", sans-serif`, upper: false, minZ: 0 },
  { font: () => `500 11px "Noto Sans", sans-serif`, upper: true, minZ: 0 },
  { font: () => `500 10px "Noto Sans", sans-serif`, upper: true, minZ: 0 },
  { font: () => `12px "Noto Sans", sans-serif`, upper: false, minZ: 10 },
  { font: () => `11px "Noto Sans", sans-serif`, upper: false, minZ: 12 },
  { font: () => `11px "Noto Sans", sans-serif`, upper: false, minZ: 14 },
];

const css = (c) => `rgba(${c[0]},${c[1]},${c[2]},${(c[3] / 255).toFixed(3)})`;
const COLORS = palette.labels.map((l) => ({ fill: css(l.color), halo: css(l.halo) }));

/** The axis-aligned box of a w x h rectangle centred at (x, y) and turned by `angle`, padded. */
export function rotatedBox(x, y, w, h, angle, pad = 2) {
  const c = Math.abs(Math.cos(angle));
  const s = Math.abs(Math.sin(angle));
  const hw = (w * c + h * s) / 2 + pad;
  const hh = (w * s + h * c) / 2 + pad;
  return { x0: x - hw, y0: y - hh, x1: x + hw, y1: y + hh };
}

/**
 * Greedy placement: candidates in order, each kept if its box overlaps no box already kept.
 * `cands` = [{ key, rank: [numbers...], box }], ranks compared element by element, lowest first.
 * Returns the kept candidates, in placement order. A uniform grid makes each check local.
 */
export function placeLabels(cands, cell = 64) {
  const order = [...cands].sort((a, b) => {
    for (let i = 0; i < Math.max(a.rank.length, b.rank.length); i++) {
      const d = (a.rank[i] ?? 0) - (b.rank[i] ?? 0);
      if (d !== 0) return d;
    }
    return a.key < b.key ? -1 : a.key > b.key ? 1 : 0;
  });
  const grid = new Map();
  const kept = [];
  const overlaps = (p, q) => p.x0 < q.x1 && q.x0 < p.x1 && p.y0 < q.y1 && q.y0 < p.y1;
  for (const c of order) {
    const b = c.box;
    const gx0 = Math.floor(b.x0 / cell), gx1 = Math.floor(b.x1 / cell);
    const gy0 = Math.floor(b.y0 / cell), gy1 = Math.floor(b.y1 / cell);
    let hit = false;
    for (let gy = gy0; gy <= gy1 && !hit; gy++) {
      for (let gx = gx0; gx <= gx1 && !hit; gx++) {
        for (const o of grid.get(`${gx},${gy}`) ?? []) if (overlaps(b, o)) { hit = true; break; }
      }
    }
    if (hit) continue;
    kept.push(c);
    for (let gy = gy0; gy <= gy1; gy++) {
      for (let gx = gx0; gx <= gx1; gx++) {
        const k = `${gx},${gy}`;
        if (!grid.has(k)) grid.set(k, []);
        grid.get(k).push(b);
      }
    }
  }
  return kept;
}

export class LabelLayer {
  constructor(container) {
    const c = document.createElement("canvas");
    c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;";
    container.appendChild(c);
    this.canvas = c;
    this.ctx = c.getContext("2d");
    this.widths = new Map(); // font|text -> px
    this.placedLast = new Set();
    this.visible = true;
    this.count = 0;
  }

  width(font, text) {
    const k = `${font}|${text}`;
    let w = this.widths.get(k);
    if (w === undefined) {
      this.ctx.font = font;
      w = this.ctx.measureText(text).width;
      if (this.widths.size > 20000) this.widths.clear();
      this.widths.set(k, w);
    }
    return w;
  }

  /**
   * Choose and draw this frame's labels. `tiles` = [{ tile, rect }]: tiles drawn as themselves this
   * frame, with their on-screen rectangles (CSS px). Stand-in tiles contribute no labels, so a label
   * never jumps when the real tile arrives.
   */
  draw(tiles, zoom, dpr, w, h) {
    const ctx = this.ctx;
    if (this.canvas.width !== Math.round(w * dpr) || this.canvas.height !== Math.round(h * dpr)) {
      this.canvas.width = Math.round(w * dpr);
      this.canvas.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    if (!this.visible) return;
    const cands = [];
    for (const { tile, rect } of tiles) {
      if (!tile.labels) continue;
      const scale = rect.w / 4096;
      for (const l of tile.labels) {
        const st = STYLE[l.kind];
        if (!st || zoom < Math.max(l.minZoom, st.minZ)) continue;
        const x = rect.x + l.x * scale;
        const y = rect.y + l.y * scale;
        if (x < -100 || y < -50 || x > w + 100 || y > h + 50) continue;
        const text = st.upper ? l.text.toUpperCase() : l.text;
        const font = st.font(zoom);
        const tw = this.width(font, text);
        const th = parseInt(font.match(/(\d+)px/)[1], 10) * 1.2;
        // a street name only where its straight run is long enough for the text
        if (l.run && l.run * scale < tw + 8) continue;
        const key = `${l.kind}|${text}|${tile.key}`;
        const box = rotatedBox(x, y, tw, th, l.angle);
        if (box.x0 < 0 || box.y0 < 0 || box.x1 > w || box.y1 > h) continue; // whole labels only
        cands.push({
          key, text, font, x, y, angle: l.angle, kind: l.kind,
          rank: [l.kind, this.placedLast.has(key) ? 0 : 1, l.priority],
          box,
        });
      }
    }
    const kept = placeLabels(cands);
    this.placedLast = new Set(kept.map((c) => c.key));
    this.count = kept.length;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.lineJoin = "round";
    for (const c of kept) {
      const col = COLORS[c.kind];
      ctx.font = c.font;
      ctx.save();
      ctx.translate(c.x, c.y);
      if (c.angle) ctx.rotate(c.angle);
      ctx.lineWidth = 3;
      ctx.strokeStyle = col.halo;
      ctx.strokeText(c.text, 0, 0);
      ctx.fillStyle = col.fill;
      ctx.fillText(c.text, 0, 0);
      ctx.restore();
    }
  }
}
