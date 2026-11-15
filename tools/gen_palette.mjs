#!/usr/bin/env node
// Generate the native renderer's palette from the same Protomaps "light" style MapLibre uses, so
// the two renderers can be compared by eye and neither drifts by hand.
//   crates/vtile/src/palette.rs   fill colors by landuse kind, line classes (colors)
//   src/rust-renderer/palette.json  line width stops per class (the shader scales widths by zoom)
// Usage: node tools/gen_palette.mjs   (rerun after changing the basemaps version)
import { writeFileSync, mkdirSync, readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { layers, namedFlavor } from "@protomaps/basemaps";

const flavor = namedFlavor("light");
const ls = layers("protomaps", flavor, {});
const byId = Object.fromEntries(ls.map((l) => [l.id, l]));
// label layers exist only when the style is built with a language
const labelById = Object.fromEntries(layers("protomaps", flavor, { lang: "en" }).map((l) => [l.id, l]));

function rgba(c) {
  if (typeof c !== "string") throw new Error(`not a literal color: ${JSON.stringify(c)}`);
  let m = c.match(/^#([0-9a-f]{6})$/i);
  if (m) {
    const n = parseInt(m[1], 16);
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255, 255];
  }
  m = c.match(/^rgba?\(\s*(\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\s*\)$/);
  if (m) return [+m[1], +m[2], +m[3], Math.round(255 * (m[4] === undefined ? 1 : +m[4]))];
  throw new Error(`unparsed color ${c}`);
}
// A color that MapLibre interpolates over zoom: take its last stop (the street-level value).
function lastStop(c) {
  if (Array.isArray(c) && c[0] === "interpolate") return c[c.length - 1];
  return c;
}
const opacityOf = (l, key) => (typeof l.paint?.[key] === "number" ? l.paint[key] : 1);
const withOpacity = (c, o) => [c[0], c[1], c[2], Math.round(c[3] * o)];

// landuse kinds -> color: the layer filters say which kinds each landuse_* layer draws; its
// fill-color is a literal, or (landuse_park) a `case` over kind. `aboveWater` notes the kinds whose
// layer the style draws after the water layer (a pier must not be painted over by the ocean).
const landuse = {};
const aboveWater = new Set();
const landuseRank = {}; // kind -> the style layer's position: overlapping landuse drawn in that order
const waterIndex = ls.findIndex((l) => l.id === "water");
for (const l of ls.filter((x) => x["source-layer"] === "landuse" && x.type === "fill")) {
  const f = l.filter;
  let kinds = [];
  if (f[0] === "in" && f[1] === "kind") kinds = f.slice(2);
  else if (f[0] === "==" && f[1] === "kind") kinds = [f[2]];
  else if (f[0] === "any") for (const g of f.slice(1)) if (g[0] === "in" && g[1] === "kind") kinds.push(...g.slice(2));
  if (ls.indexOf(l) > waterIndex) for (const k of kinds) aboveWater.add(k);
  for (const k of kinds) if (!(k in landuseRank)) landuseRank[k] = ls.indexOf(l);
  const fc = l.paint["fill-color"];
  if (Array.isArray(fc) && fc[0] === "case") {
    for (let i = 1; i + 1 < fc.length; i += 2) {
      const cond = fc[i], color = rgba(fc[i + 1]);
      const ks = cond[0] === "in" ? cond[2][1] : cond[0] === "==" ? [cond[2]] : [];
      for (const k of ks) if (kinds.includes(k) && !(k in landuse)) landuse[k] = withOpacity(color, opacityOf(l, "fill-opacity"));
    }
  } else {
    for (const k of kinds) if (!(k in landuse)) landuse[k] = withOpacity(rgba(lastStop(fc)), opacityOf(l, "fill-opacity"));
  }
}

// line classes, in draw order; `id` is the style layer they mirror. Casings (a road's border) come
// before every road fill, as the style's "late" casing layers do.
const LINES = [
  ["WaterRiver", "water_river"], ["WaterStream", "water_stream"],
  ["CasingService", "roads_minor_service_casing"], ["CasingMinor", "roads_minor_casing"], ["CasingLink", "roads_link_casing"],
  ["CasingMajor", "roads_major_casing_late"], ["CasingHighway", "roads_highway_casing_late"],
  ["RoadOther", "roads_other"], ["RoadService", "roads_minor_service"], ["RoadMinor", "roads_minor"],
  ["RoadLink", "roads_link"], ["RoadMajor", "roads_major"], ["RoadHighway", "roads_highway"],
  ["Rail", "roads_rail"], ["Boundary", "boundaries"], ["BoundaryCountry", "boundaries_country"],
];
function stops(w) {
  if (typeof w === "number") return { base: 1, stops: [[0, w], [24, w]] };
  if (w[0] === "interpolate" && w[2][0] === "zoom") {
    const base = w[1][0] === "exponential" ? w[1][1] : 1;
    const s = [];
    for (let i = 3; i < w.length; i += 2) s.push([w[i], w[i + 1]]);
    return { base, stops: s };
  }
  throw new Error(`unparsed width ${JSON.stringify(w)}`);
}
// MapLibre's interpolate, to evaluate a casing's two width curves where the style gives them apart
const evalStops = ({ base, stops: s }, z) => {
  if (z <= s[0][0]) return s[0][1];
  if (z >= s[s.length - 1][0]) return s[s.length - 1][1];
  let i = 0;
  while (z >= s[i + 1][0]) i++;
  const [z0, v0] = s[i], [z1, v1] = s[i + 1], d = z1 - z0;
  const t = base === 1 ? (z - z0) / d : (base ** (z - z0) - 1) / (base ** d - 1);
  return v0 + t * (v1 - v0);
};
if (LINES.length > 16) throw new Error("the shader holds 16 line classes");
const lines = LINES.map(([name, id], i) => {
  const l = byId[id];
  let width = stops(l.paint["line-width"]);
  let minzoom = l.minzoom ?? 0;
  if (l.paint["line-gap-width"] !== undefined) {
    // a casing: total width = the gap (the road) + 2 x its own line width, sampled every 1/4 zoom
    const gap = stops(l.paint["line-gap-width"]);
    const s = [];
    for (let z = 0; z <= 22; z += 0.25) s.push([z, Math.round((evalStops(gap, z) + 2 * evalStops(width, z)) * 1000) / 1000]);
    width = { base: 1, stops: s };
    // only from the zoom where its own border is visible: no geometry for an invisible casing
    minzoom = Math.floor(s.find(([z]) => evalStops(stops(l.paint["line-width"]), z) > 0)?.[0] ?? 0);
  }
  return {
    name, id, class: i,
    color: withOpacity(rgba(lastStop(l.paint["line-color"])), opacityOf(l, "line-opacity")),
    width, minzoom,
  };
});
const rail = byId["roads_rail"];
const f64 = (n) => (Number.isInteger(n) ? `${n}.0` : `${n}`); // a Rust float literal
const railDash = rail.paint["line-dasharray"];

// Icons: the sprites the POI and shield layers use, by name; a label carries index+1 (0: none).
const sprites = JSON.parse(readFileSync("public/assets/sprites/v4/light.json", "utf8"));
const poiLayer = labelById["pois"];
const poiKinds = poiLayer.filter[1][2][1];
const poiIcon = (k) => (k === "station" ? "train_station" : k);
const icons = [];
const iconIndex = (name) => {
  if (!(name in sprites)) return 0;
  let i = icons.indexOf(name);
  if (i < 0) i = icons.push(name) - 1;
  return i + 1;
};
// POI text colors: the layer's `case` over kind groups, last value the default
const poiColor = {};
{
  const tc = poiLayer.paint["text-color"];
  for (let i = 1; i + 1 < tc.length; i += 2) for (const k of tc[i][2][1]) poiColor[k] ??= tc[i + 1];
  for (const k of poiKinds) poiColor[k] ??= tc[tc.length - 1];
}
// extra label colors (index into `label_colors`), shared by POI groups, water and shields
const labelColors = [];
const labelColorIndex = (c) => {
  const k = c.join();
  let i = labelColors.findIndex((x) => x.join() === k);
  if (i < 0) i = labelColors.push(c) - 1;
  return i;
};
const poiRows = poiKinds.map((k) => [k, iconIndex(poiIcon(k)), labelColorIndex(rgba(poiColor[k]))]);
const SHIELD_NETS = [["US:I", "US:I"], ["NL:S-road", "NL:S-road"]];
const shieldIcons = {};
for (const [net, prefix] of [...SHIELD_NETS, ["*", "generic_shield"]]) {
  shieldIcons[net] = [1, 2, 3, 4, 5].map((n) => iconIndex(`${prefix}-${n}char`));
}
const waterColor = labelColorIndex(rgba(labelById["water_waterway_label"].paint["text-color"]));
const waterHalo = rgba(labelById["water_waterway_label"].paint["text-halo-color"]);
const shieldTextColor = labelColorIndex(rgba(labelById["roads_shields"].paint["text-color"]));

const fill = (id) => withOpacity(rgba(byId[id].paint["fill-color"]), opacityOf(byId[id], "fill-opacity"));
// Fill colors are indexed: a vertex carries a u8 palette index, the shader looks the color up.
const fillColors = [fill("earth"), fill("water"), fill("buildings")];
const fillIndex = (c) => {
  const k = c.join();
  let i = fillColors.findIndex((x) => x.join() === k);
  if (i < 0) i = fillColors.push(c) - 1;
  return i;
};
const landuseIndex = Object.fromEntries(Object.entries(landuse).map(([k, c]) => [k, fillIndex(c)]));
if (fillColors.length > 255) throw new Error("more than 255 fill colors");
const out = `// GENERATED by tools/gen_palette.mjs from @protomaps/basemaps "light". Do not edit.
//! Draw rules for the native renderer, taken from the Protomaps light style MapLibre draws. The
//! colors themselves are in src/rust-renderer/palette.json: vertices carry indices, the shader looks
//! colors up.

pub const EARTH: u8 = 0;
pub const WATER: u8 = 1;
pub const BUILDINGS: u8 = 2;

/// Landuse polygon color index by \`kind\`; kinds not listed are not drawn (as in the style).
pub fn landuse(kind: &str) -> Option<u8> {
    match kind {
${Object.entries(landuseIndex).map(([k, i]) => `        ${JSON.stringify(k)} => Some(${i}),`).join("\n")}
        _ => None,
    }
}

/// A landuse kind's draw order: the position of its style layer (parks before schools, ...).
pub fn landuse_rank(kind: &str) -> u16 {
    match kind {
${Object.entries(landuseRank).map(([k, r]) => `        ${JSON.stringify(k)} => ${r},`).join("\n")}
        _ => u16::MAX,
    }
}

/// Landuse kinds the style draws above water (after the water layer), not below it.
pub fn landuse_above_water(kind: &str) -> bool {
    matches!(kind, ${[...aboveWater].map((k) => JSON.stringify(k)).join(" | ") || '""'})
}

/// POI kinds the style labels: (icon index + 1, or 0 when the sprite sheet has none; text color
/// index into palette.json's label_colors).
pub fn poi(kind: &str) -> Option<(u8, u8)> {
    match kind {
${poiRows.map(([k, i, c]) => `        ${JSON.stringify(k)} => Some((${i}, ${c})),`).join("\n")}
        _ => None,
    }
}

/// The shield sprite (index + 1) for a road's network and route-number length (1..=5).
pub fn shield_icon(network: &str, len: usize) -> u8 {
    let row: [u8; 5] = match network {
${SHIELD_NETS.map(([n]) => `        ${JSON.stringify(n)} => [${shieldIcons[n].join(", ")}],`).join("\n")}
        _ => [${shieldIcons["*"].join(", ")}],
    };
    if (1..=5).contains(&len) { row[len - 1] } else { 0 }
}

pub const WATER_LABEL_COLOR: u8 = ${waterColor};

/// The railway's dash pattern, in line widths (on, off), and its width curve (base, [zoom, px]).
pub const RAIL_DASH: [f64; 2] = [${railDash.join(", ")}];
pub const RAIL_WIDTH_BASE: f64 = ${stops(rail.paint["line-width"]).base};
pub const RAIL_WIDTH_STOPS: [[f64; 2]; ${stops(rail.paint["line-width"]).stops.length}] = [${stops(rail.paint["line-width"]).stops.map(([a, b]) => `[${f64(a)}, ${f64(b)}]`).join(", ")}];
pub const SHIELD_TEXT_COLOR: u8 = ${shieldTextColor};

/// Line classes in draw order. The class number is written into each line vertex; the frontend
/// turns it into a width for the current zoom (src/rust-renderer/palette.json).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LineClass {
${lines.map((l) => `    ${l.name} = ${l.class},`).join("\n")}
}


/// The zoom below which a class is not drawn (the style layer's minzoom).
pub const LINE_MINZOOM: [u8; ${lines.length}] = [${lines.map((l) => l.minzoom).join(", ")}];
`;
writeFileSync("crates/vtile/src/palette.rs", out);
// in rustfmt's form, so `cargo fmt --check` and a fresh regeneration agree
execFileSync("rustfmt", ["--edition", "2021", "crates/vtile/src/palette.rs"]);
mkdirSync("src/rust-renderer", { recursive: true });
writeFileSync("src/rust-renderer/palette.json", JSON.stringify({
  generated_by: "tools/gen_palette.mjs",
  background: rgba(flavor.background),
  fill_colors: fillColors,
  icons,
  label_colors: labelColors,
  water_halo: waterHalo,
  // label colors and halos, by the renderer's LabelKind order: city, suburb, neighbourhood, highway,
  // major road, minor road
  labels: [
    ["city_label", "city_label_halo"], ["subplace_label", "subplace_label_halo"], ["subplace_label", "subplace_label_halo"],
    ["roads_label_major", "roads_label_major_halo"], ["roads_label_major", "roads_label_major_halo"], ["roads_label_minor", "roads_label_minor_halo"],
  ].map(([c, h]) => ({ color: rgba(flavor[c]), halo: rgba(flavor[h]) })),
  lines: lines.map(({ name, id, class: c, color, width, minzoom }) => ({ name, id, class: c, color, width, minzoom })),
}, null, 1) + "\n");
console.log(`landuse kinds: ${Object.keys(landuse).length}, fill colors: ${fillColors.length}, line classes: ${lines.length}, icons: ${icons.length}, POI kinds: ${poiRows.length} (${poiRows.filter((r) => !r[1]).map((r) => r[0]).join(", ")} without a sprite)`);
