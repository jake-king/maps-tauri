// Labels for the native renderer. Rust has already chosen them (crates/vtile mesh.rs: place points,
// and each road name's longest straight segment in a tile); this file decides which to show this
// frame, without overlaps, and draws them on a 2D canvas over the map with the browser's text
// rendering (full Unicode shaping for free).

import palette from "./palette.json" with { type: "json" };

const KIND = { City: 0, Suburb: 1, Neighbourhood: 2, RoadHighway: 3, RoadMajor: 4, RoadMinor: 5, Poi: 6, Water: 7, Shield: 8 };

// placement order by kind, as MapLibre places the style's symbol layers: places, then points of
// interest, roads and shields, water, and minor streets last
const ORDER = [0, 1, 2, 4, 4, 7, 3, 6, 5];

// font and the lowest zoom a kind is shown at, on top of the label's own min_zoom from the data
const STYLE = [
  { font: (z) => `500 ${z < 10 ? 13 : z < 12 ? 15 : 16}px "Noto Sans", sans-serif`, upper: false, minZ: 0 },
  { font: () => `500 11px "Noto Sans", sans-serif`, upper: true, minZ: 0 },
  { font: () => `500 10px "Noto Sans", sans-serif`, upper: true, minZ: 0 },
  { font: () => `12px "Noto Sans", sans-serif`, upper: false, minZ: 10 },
  { font: () => `11px "Noto Sans", sans-serif`, upper: false, minZ: 12 },
  { font: () => `11px "Noto Sans", sans-serif`, upper: false, minZ: 14 },
  // Poi: 10 px to zoom 17, growing to 16 at 19 (the style's interpolation)
  { font: (z) => `${Math.round(10 + Math.max(0, Math.min(1, (z - 17) / 2)) * 6)}px "Noto Sans", sans-serif`, upper: false, minZ: 0 },
  // Water: streams and rivers from zoom 13; seas and lakes whenever the data says
  { font: () => `italic 12px "Noto Sans", sans-serif`, upper: false, minZ: 0 },
  // Shield: route numbers inside their sprite
  { font: () => `500 8px "Noto Sans", sans-serif`, upper: false, minZ: 0 },
];

const css = (c) => `rgba(${c[0]},${c[1]},${c[2]},${(c[3] / 255).toFixed(3)})`;
const COLORS = palette.labels.map((l) => ({ fill: css(l.color), halo: css(l.halo) }));
const EXTRA = palette.label_colors.map(css); // POI, water and shield text colors, by the label's color index
const POI_HALO = css(palette.fill_colors[0]); // the style's POI halo is the land color
const WATER_HALO = css(palette.water_halo);

/** The sprite sheet the style uses: one image plus a JSON index of icon rectangles. */
class Sprites {
  constructor(base, onLoad) {
    this.ready = false;
    const ratio = (window.devicePixelRatio || 1) > 1 ? "@2x" : "";
    Promise.all([fetch(`${base}${ratio}.json`).then((r) => r.json()), new Promise((res, rej) => {
      const img = new Image();
      img.onload = () => res(img);
      img.onerror = rej;
      img.src = `${base}${ratio}.png`;
    })]).then(([index, img]) => {
      Object.assign(this, { index, img, ready: true });
      onLoad?.();
    }).catch(() => {});
  }
  /** `{ x, y, width, height, pixelRatio }` for an icon by palette index (1-based), or null. */
  get(i) {
    if (!this.ready || !i) return null;
    return this.index[palette.icons[i - 1]] ?? null;
  }
}

/**
 * Lay text along a path: `pts` in screen px, `advances` each character's width. The text is centred
 * at the path's middle (by length); each character sits at its own distance along the path, turned
 * to the direction there. Returns `{ glyphs: [{ x, y, angle }], straight }`, or null when the path
 * is too short, when neighbouring characters would turn more than `maxAngle` (MapLibre's rule: text
 * on a hairpin is unreadable), or when a character would stand upside down. `straight` is true when
 * the whole text lies on one direction, so it can be drawn as one string (keeping its kerning).
 */
export function layoutOnPath(pts, advances, { maxAngle = Math.PI / 4, pad = 8, anchors = [0.5] } = {}) {
  if (pts.length < 2) return null;
  const cum = [0];
  for (let i = 1; i < pts.length; i++) cum.push(cum[i - 1] + Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]));
  const total = cum[cum.length - 1];
  const tw = advances.reduce((a, b) => a + b, 0);
  if (total < tw + pad) return null;
  let seg = 0;
  const at = (s) => {
    while (seg < pts.length - 2 && cum[seg + 1] < s) seg++;
    const [a, b] = [pts[seg], pts[seg + 1]];
    const len = cum[seg + 1] - cum[seg] || 1;
    const t = (s - cum[seg]) / len;
    return { x: a[0] + (b[0] - a[0]) * t, y: a[1] + (b[1] - a[1]) * t, angle: Math.atan2(b[1] - a[1], b[0] - a[0]) };
  };
  // try each anchor (a fraction of the path's length for the text's centre) until one reads well:
  // the middle of a road may be a hairpin where a stretch further along is not (MapLibre likewise
  // tries several anchors along a line)
  for (const f of anchors) {
    const start = Math.min(Math.max(f * total - tw / 2, pad / 2), total - tw - pad / 2);
    const lay = layoutFrom(start);
    if (lay) return lay;
  }
  return null;

  function layoutFrom(start) {
    seg = 0;
    const glyphs = [];
    let s = start;
    let prev = null;
    let min = Infinity, max = -Infinity;
    for (const adv of advances) {
      const g = at(s + adv / 2);
      if (Math.abs(g.angle) > Math.PI / 2 + 0.35) return null; // would stand upside down
      if (prev !== null) {
        let d = Math.abs(g.angle - prev);
        if (d > Math.PI) d = 2 * Math.PI - d;
        if (d > maxAngle) return null;
      }
      prev = g.angle;
      min = Math.min(min, g.angle);
      max = Math.max(max, g.angle);
      glyphs.push(g);
      s += adv;
    }
    return { glyphs, straight: max - min < 0.03 };
  }
}

/**
 * The on-screen angle of a straight label with the map turned by `bearing` (radians). A label along
 * a straight run turns with the map and is then kept readable left to right, in (-pi/2, pi/2]; a
 * point label (a place, an area: no run) stays level with the screen, as MapLibre keeps it.
 */
export function screenAngle(l, bearing) {
  if (!l.run) return 0;
  let a = l.angle - bearing;
  a = Math.atan2(Math.sin(a), Math.cos(a));
  if (a > Math.PI / 2) a -= Math.PI;
  else if (a <= -Math.PI / 2) a += Math.PI;
  return a;
}

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
 * A candidate may instead offer `options: [{ box, ... }]`, tried in order (a POI's name right of its
 * icon, else left); the first that fits is kept, its fields merged into the result as `chosen`.
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
  const fitsOne = (b) => {
    const gx0 = Math.floor(b.x0 / cell), gx1 = Math.floor(b.x1 / cell);
    const gy0 = Math.floor(b.y0 / cell), gy1 = Math.floor(b.y1 / cell);
    for (let gy = gy0; gy <= gy1; gy++) {
      for (let gx = gx0; gx <= gx1; gx++) {
        for (const o of grid.get(`${gx},${gy}`) ?? []) if (overlaps(b, o)) return false;
      }
    }
    return true;
  };
  // a label may claim several boxes (a curved name: one per character); all must fit
  const boxesOf = (o) => o.boxes ?? [o.box];
  for (const c of order) {
    const chosen = (c.options ?? [c]).find((o) => boxesOf(o).every(fitsOne));
    if (!chosen) continue;
    kept.push(c.options ? { ...c, chosen } : c);
    for (const b of boxesOf(chosen)) {
      const gx0 = Math.floor(b.x0 / cell), gx1 = Math.floor(b.x1 / cell);
      const gy0 = Math.floor(b.y0 / cell), gy1 = Math.floor(b.y1 / cell);
      for (let gy = gy0; gy <= gy1; gy++) {
        for (let gx = gx0; gx <= gx1; gx++) {
          const k = `${gx},${gy}`;
          if (!grid.has(k)) grid.set(k, []);
          grid.get(k).push(b);
        }
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
    this.sprites = new Sprites(`${location.origin}/assets/sprites/v4/light`, () => this.onSprites?.());
  }

  width(font, text) {
    const k = `${font}|${text}`;
    let w = this.widths.get(k);
    if (w === undefined) {
      this.misses = (this.misses ?? 0) + 1;
      // setting ctx.font makes the browser parse and resolve the font: only when it changes
      if (this.measureFont !== font) {
        this.ctx.font = font;
        this.measureFont = font;
      }
      w = this.ctx.measureText(text).width;
      if (this.widths.size > 20000) this.widths.clear();
      this.widths.set(k, w);
    }
    return w;
  }

  /**
   * Measure a newly loaded tile's label texts now, when the tile arrives, rather than in the frame
   * that first shows them: a burst of new measurements was the cause of the slow label frames.
   * Fonts that depend on zoom are measured for the zoom levels around `zoom`.
   */
  prepare(labels, zoom) {
    const before = this.misses ?? 0;
    for (const l of labels) {
      const st = STYLE[l.kind];
      if (!st) continue;
      const text = st.upper ? l.text.toUpperCase() : l.text;
      for (const z of [Math.floor(zoom), Math.floor(zoom) + 1]) {
        this.width(st.font(z), text);
        if (l.path) for (const ch of text) this.width(st.font(z), ch); // a curved name is laid out by character
      }
    }
    this.misses = before; // measured here, not in a frame
    this.measureFont = null; // drawing sets fonts too: do not trust the cached font across frames
  }

  /**
   * Choose and draw this frame's labels. `tiles` = [{ tile, rect }]: tiles drawn as themselves this
   * frame, with their on-screen rectangles (CSS px). Stand-in tiles contribute no labels, so a label
   * never jumps when the real tile arrives.
   */
  draw(tiles, zoom, dpr, w, h, project = null, bearing = 0) {
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
      const scale = rect.w / 4096; // screen px per tile unit (a turn does not change it)
      const toScreen = project ? (px, py) => project(tile, px, py) : (px, py) => [rect.x + px * scale, rect.y + py * scale];
      for (const l of tile.labels) {
        const st = STYLE[l.kind];
        if (!st || zoom < Math.max(l.minZoom, st.minZ)) continue;
        const [x, y] = toScreen(l.x, l.y);
        if (x < -100 || y < -50 || x > w + 100 || y > h + 50) continue;
        if (l.kind === KIND.Water && l.run && zoom < 13) continue; // streams and rivers from 13
        const text = st.upper ? l.text.toUpperCase() : l.text;
        const font = st.font(zoom);
        const tw = this.width(font, text);
        const th = parseInt(font.match(/(\d+)px/)[1], 10) * 1.2;
        // a straight-run label (no path) only where its run is long enough for the text
        if (!l.path && l.run && l.run * scale < tw + 8) continue;
        const key = `${l.kind}|${text}|${tile.key}`;
        const onScreen = (b) => b.x0 >= 0 && b.y0 >= 0 && b.x1 <= w && b.y1 <= h; // whole labels only
        const base = { key, text, font, x, y, angle: l.angle, kind: l.kind, color: l.color,
                       rank: [ORDER[l.kind] ?? 9, this.placedLast.has(key) ? 0 : 1, l.priority] };
        const icon = this.sprites.get(l.icon);
        if (l.kind === KIND.Shield) {
          if (!icon) continue;
          const iw = (icon.width / icon.pixelRatio) * 0.8, ih = (icon.height / icon.pixelRatio) * 0.8;
          const box = { x0: x - iw / 2 - 1, y0: y - ih / 2 - 1, x1: x + iw / 2 + 1, y1: y + ih / 2 + 1 };
          if (onScreen(box)) cands.push({ ...base, icon, iw, ih, box });
        } else if (l.kind === KIND.Poi) {
          const iw = icon ? icon.width / icon.pixelRatio : 0, ih = icon ? icon.height / icon.pixelRatio : 0;
          const gap = icon ? iw / 2 + 0.6 * th : 0;
          const iconBox = { x0: x - iw / 2, y0: y - ih / 2, x1: x + iw / 2, y1: y + ih / 2 };
          const options = (icon ? ["right", "left"] : ["center"]).map((side) => {
            const tx = side === "right" ? x + gap : side === "left" ? x - gap - tw : x - tw / 2;
            const tb = { x0: tx - 2, y0: y - th / 2 - 2, x1: tx + tw + 2, y1: y + th / 2 + 2 };
            const box = icon ? { x0: Math.min(iconBox.x0, tb.x0), y0: Math.min(iconBox.y0, tb.y0), x1: Math.max(iconBox.x1, tb.x1), y1: Math.max(iconBox.y1, tb.y1) } : tb;
            return { box, tx };
          }).filter((o) => onScreen(o.box));
          if (options.length) cands.push({ ...base, icon, iw, ih, options });
        } else if (l.path) {
          // along its path: one box per character, so a curve does not claim a whole rectangle
          const pts = l.path.map(([px, py]) => toScreen(px, py));
          // turned with the map, a path may now run right to left on screen: read it the other way
          if (pts[pts.length - 1][0] < pts[0][0]) pts.reverse();
          const chars = [...text];
          const lay = layoutOnPath(pts, chars.map((ch) => this.width(font, ch)), { anchors: [0.5, 0.35, 0.65, 0.2, 0.8] });
          if (!lay) continue;
          const boxes = lay.straight
            ? [rotatedBox(lay.glyphs.reduce((a, g) => a + g.x, 0) / lay.glyphs.length, lay.glyphs.reduce((a, g) => a + g.y, 0) / lay.glyphs.length, tw, th, lay.glyphs[0].angle)]
            : lay.glyphs.map((g, i) => rotatedBox(g.x, g.y, this.width(font, chars[i]), th, g.angle, 1));
          if (boxes.every(onScreen)) cands.push({ ...base, boxes, chars, lay });
        } else {
          const a = screenAngle(l, bearing);
          const box = rotatedBox(x, y, tw, th, a);
          if (onScreen(box)) cands.push({ ...base, angle: a, box });
        }
      }
    }
    this.frameMisses = this.misses ?? 0;
    this.misses = 0;
    const kept = placeLabels(cands);
    this.placedLast = new Set(kept.map((c) => c.key));
    this.count = kept.length;
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.lineJoin = "round";
    const sprite = this.sprites.img;
    this.measureFont = null; // drawing changes the font
    let font = null;
    for (const c of kept) {
      if (c.font !== font) {
        ctx.font = c.font;
        font = c.font;
      }
      if (c.kind === KIND.Shield) {
        const i = c.icon;
        ctx.drawImage(sprite, i.x, i.y, i.width, i.height, c.x - c.iw / 2, c.y - c.ih / 2, c.iw, c.ih);
        ctx.fillStyle = EXTRA[c.color];
        ctx.textAlign = "center";
        ctx.fillText(c.text, c.x, c.y + 0.5);
        continue;
      }
      if (c.kind === KIND.Poi) {
        if (c.icon) {
          const i = c.icon;
          ctx.drawImage(sprite, i.x, i.y, i.width, i.height, c.x - c.iw / 2, c.y - c.ih / 2, c.iw, c.ih);
        }
        ctx.textAlign = "left";
        ctx.lineWidth = 2;
        ctx.strokeStyle = POI_HALO;
        ctx.strokeText(c.text, c.chosen.tx, c.y);
        ctx.fillStyle = EXTRA[c.color];
        ctx.fillText(c.text, c.chosen.tx, c.y);
        ctx.textAlign = "center";
        continue;
      }
      const water = c.kind === KIND.Water;
      if (c.lay && !c.lay.straight) {
        // curved: character by character, each turned to the path
        ctx.lineWidth = water ? 2 : 3;
        ctx.strokeStyle = water ? WATER_HALO : COLORS[c.kind].halo;
        ctx.fillStyle = water ? EXTRA[c.color] : COLORS[c.kind].fill;
        for (const pass of ["stroke", "fill"]) {
          c.lay.glyphs.forEach((g, i) => {
            ctx.save();
            ctx.translate(g.x, g.y);
            ctx.rotate(g.angle);
            if (pass === "stroke") ctx.strokeText(c.chars[i], 0, 0);
            else ctx.fillText(c.chars[i], 0, 0);
            ctx.restore();
          });
        }
        continue;
      }
      if (c.lay) {
        // straight along its path: one string at the middle of the laid-out characters
        const n = c.lay.glyphs.length;
        c.x = c.lay.glyphs.reduce((a, g) => a + g.x, 0) / n;
        c.y = c.lay.glyphs.reduce((a, g) => a + g.y, 0) / n;
        c.angle = c.lay.glyphs[0].angle;
      }
      ctx.save();
      ctx.translate(c.x, c.y);
      if (c.angle) ctx.rotate(c.angle);
      ctx.lineWidth = water ? 2 : 3;
      ctx.strokeStyle = water ? WATER_HALO : COLORS[c.kind].halo;
      ctx.strokeText(c.text, 0, 0);
      ctx.fillStyle = water ? EXTRA[c.color] : COLORS[c.kind].fill;
      ctx.fillText(c.text, 0, 0);
      ctx.restore();
    }
  }
}
