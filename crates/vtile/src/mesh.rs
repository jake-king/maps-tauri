//! Turn a decoded tile into two GPU-ready meshes: fills (triangulated polygons) and lines (strips
//! with extrusion vectors).
//!
//! Colors and line classes live in the vertices, so the renderer draws a whole tile in two calls:
//! guile-2d's "one draw call per sheet".
//!
//! Wire format of [`Mesh::to_bytes`], version 5 (little-endian, every block padded to 4 bytes):
//!   u32 magic "MESH", u32 version, u32 flags (bit 0: fill indices are u32, bit 1: line indices
//!   are u32; otherwise u16), u32 fill vertex count, u32 fill index count, u32 line vertex count,
//!   u32 line index count, u32 label count, u32 label block bytes,
//!   fill vertices (6 B: i16 x, i16 y, u8 color index, u8 pad), fill indices,
//!   line vertices (8 B: i16 x, i16 y, i8 ex, i8 ey, u8 class, u8 pad), line indices,
//!   labels, each: i16 x, i16 y, i16 angle (radians x 10000), u16 run (tile units of straight line
//!   the text may sit on; 0 for a point), u8 kind ([`LabelKind`]), u8 min zoom, u8 icon (sprite index
//!   plus 1, 0 for none), u8 color (label color index, POIs/water/shields), i32 priority (lower
//!   first), u8 text length, UTF-8 text, u8 path point count, that many (i16 x, i16 y): the line a
//!   street or stream name follows (0 points for a point label); the block padded to 4.
//! x, y are tile coordinates (extent 4096). (ex, ey) / 32 is the unit extrusion; the shader scales
//! it by the class's half width in pixels for the current zoom. Colors are looked up in the
//! palette (fills by index, lines by class), so a vertex carries no color.
//! Version 1 (8 B fill and 16 B line vertices with full colors, u32 indices) was ~70% larger.

use crate::mvt::{self, Layer};
use crate::palette::{self, LineClass};

/// The wire format's first four bytes, `MESH`, read as a little-endian `u32`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"MESH");
/// The wire format version [`Mesh::to_bytes`] writes.
pub const VERSION: u32 = 5;
/// Douglas-Peucker tolerance for a label path, in tile units.
pub const PATH_TOLERANCE: f64 = 6.0;
/// The most points a label path carries: [`path_label`] simplifies harder until it fits, and
/// [`Mesh::to_bytes`] writes no more.
pub const PATH_MAX_POINTS: usize = 32;
/// Extrusion is stored as `i8`: the unit vector times 32, so a miter of up to [`MITER_LIMIT`] (2)
/// fits.
pub const EXTRUDE_SCALE: f64 = 32.0;
/// Miter joins longer than this (in half widths) are clamped, so sharp corners do not spike.
pub const MITER_LIMIT: f64 = 2.0;

/// One vertex of a filled (triangulated) polygon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillVertex {
    /// x in tile coordinates (extent 4096; may lie outside the tile, in its buffer).
    pub x: i16,
    /// y in tile coordinates, down from the tile's top edge.
    pub y: i16,
    /// Fill color index into the palette's `fill_colors`.
    pub color: u8,
}

/// One vertex of a line strip: a point on the line and the direction to push it out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineVertex {
    /// x of the point on the line, in tile coordinates.
    pub x: i16,
    /// y of the point on the line, in tile coordinates.
    pub y: i16,
    /// Extrusion x, scaled by [`EXTRUDE_SCALE`]; the shader multiplies it by the half width.
    pub ex: i8,
    /// Extrusion y, scaled by [`EXTRUDE_SCALE`].
    pub ey: i8,
    /// The [`LineClass`] number, which gives the line's color and width.
    pub class: u8,
}

/// What a label names; the frontend styles and ranks by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LabelKind {
    /// A city or town name.
    City = 0,
    /// A suburb name.
    Suburb = 1,
    /// A neighbourhood name.
    Neighbourhood = 2,
    /// A highway's name, along the road.
    RoadHighway = 3,
    /// A major road's name, along the road.
    RoadMajor = 4,
    /// A minor road's name, along the road.
    RoadMinor = 5,
    /// A point of interest, with its icon.
    Poi = 6,
    /// A sea, lake, river or stream name.
    Water = 7,
    /// A route number on a road shield.
    Shield = 8,
}

/// A label to place: where, what it says, and how it ranks against others.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// x of the anchor, in tile coordinates.
    pub x: i16,
    /// y of the anchor, in tile coordinates.
    pub y: i16,
    /// radians, already turned to read left to right (in -pi/2..pi/2)
    pub angle: f32,
    /// straight run the text may sit on, tile units (0 for a point label)
    pub run: u16,
    /// What the label names.
    pub kind: LabelKind,
    /// The lowest zoom the label is shown at, from the data (0..=24).
    pub min_zoom: u8,
    /// sprite index + 1 (0: no icon)
    pub icon: u8,
    /// label color index (for kinds whose color varies: POIs, water, shields)
    pub color: u8,
    /// Placement order among colliding labels: lower is placed first.
    pub priority: i32,
    /// The text, UTF-8 (the wire format keeps at most 255 bytes of it).
    pub text: String,
    /// the line the text follows, tile units (empty for a point label)
    pub path: Vec<[i16; 2]>,
}

/// One tile's geometry, ready for the GPU: fill triangles, line strips and labels.
#[derive(Default, Debug)]
pub struct Mesh {
    /// Fill vertices.
    pub fill: Vec<FillVertex>,
    /// Fill triangles, three indices into `fill` each.
    pub fill_idx: Vec<u32>,
    /// Line vertices.
    pub line: Vec<LineVertex>,
    /// Line triangles, three indices into `line` each.
    pub line_idx: Vec<u32>,
    /// Labels, in the order they were produced.
    pub labels: Vec<Label>,
}

impl Mesh {
    /// Empty every buffer, keeping its capacity.
    pub fn clear(&mut self) {
        self.fill.clear();
        self.fill_idx.clear();
        self.line.clear();
        self.line_idx.clear();
        self.labels.clear();
    }

    fn label_block_len(&self) -> usize {
        let raw: usize = self
            .labels
            .iter()
            .map(|l| 18 + utf8_prefix(&l.text, 255) + 4 * l.path.len().min(PATH_MAX_POINTS))
            .sum();
        (raw + 3) & !3
    }

    fn wide(vertices: usize) -> bool {
        vertices > usize::from(u16::MAX)
    }

    /// The exact length in bytes of [`Mesh::to_bytes`]'s output.
    pub fn byte_len(&self) -> usize {
        let pad4 = |n: usize| (n + 3) & !3;
        let idx = |n: usize, wide: bool| pad4(n * if wide { 4 } else { 2 });
        36 + pad4(self.fill.len() * 6)
            + idx(self.fill_idx.len(), Self::wide(self.fill.len()))
            + self.line.len() * 8
            + idx(self.line_idx.len(), Self::wide(self.line.len()))
            + self.label_block_len()
    }

    /// The wire format above, written into `out` (cleared first).
    pub fn to_bytes(&self, out: &mut Vec<u8>) {
        let pad4 = |out: &mut Vec<u8>| out.resize((out.len() + 3) & !3, 0);
        let (fw, lw) = (Self::wide(self.fill.len()), Self::wide(self.line.len()));
        out.clear();
        out.reserve(self.byte_len());
        let flags = u32::from(fw) | (u32::from(lw) << 1);
        let header = [
            MAGIC,
            VERSION,
            flags,
            self.fill.len() as u32,
            self.fill_idx.len() as u32,
            self.line.len() as u32,
            self.line_idx.len() as u32,
            self.labels.len() as u32,
            self.label_block_len() as u32,
        ];
        for v in header {
            out.extend_from_slice(&v.to_le_bytes());
        }
        for v in &self.fill {
            out.extend_from_slice(&v.x.to_le_bytes());
            out.extend_from_slice(&v.y.to_le_bytes());
            out.extend_from_slice(&[v.color, 0]);
        }
        pad4(out);
        let indices = |out: &mut Vec<u8>, idx: &[u32], wide: bool| {
            for &i in idx {
                if wide {
                    out.extend_from_slice(&i.to_le_bytes());
                } else {
                    out.extend_from_slice(&(i as u16).to_le_bytes());
                }
            }
        };
        indices(out, &self.fill_idx, fw);
        pad4(out);
        for v in &self.line {
            out.extend_from_slice(&v.x.to_le_bytes());
            out.extend_from_slice(&v.y.to_le_bytes());
            out.extend_from_slice(&[v.ex as u8, v.ey as u8, v.class, 0]);
        }
        indices(out, &self.line_idx, lw);
        pad4(out);
        for l in &self.labels {
            let text = &l.text.as_bytes()[..utf8_prefix(&l.text, 255)];
            out.extend_from_slice(&l.x.to_le_bytes());
            out.extend_from_slice(&l.y.to_le_bytes());
            out.extend_from_slice(&((l.angle * 10000.0).round() as i16).to_le_bytes());
            out.extend_from_slice(&l.run.to_le_bytes());
            out.extend_from_slice(&[l.kind as u8, l.min_zoom, l.icon, l.color]);
            out.extend_from_slice(&l.priority.to_le_bytes());
            out.push(text.len() as u8);
            out.extend_from_slice(text);
            let path = &l.path[..l.path.len().min(PATH_MAX_POINTS)];
            out.push(path.len() as u8);
            for p in path {
                out.extend_from_slice(&p[0].to_le_bytes());
                out.extend_from_slice(&p[1].to_le_bytes());
            }
        }
        pad4(out);
    }
}

/// Douglas-Peucker simplification: the fewest points within `tol` of the original polyline.
pub fn simplify(pts: &[[f64; 2]], tol: f64) -> Vec<[f64; 2]> {
    if pts.len() <= 2 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0, pts.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (pts[a], pts[b]);
        let (dx, dy) = (pb[0] - pa[0], pb[1] - pa[1]);
        let len = dx.hypot(dy);
        let mut far = (0.0, a);
        for (i, p) in pts.iter().enumerate().take(b).skip(a + 1) {
            let d = if len == 0.0 {
                (p[0] - pa[0]).hypot(p[1] - pa[1])
            } else {
                ((p[0] - pa[0]) * dy - (p[1] - pa[1]) * dx).abs() / len
            };
            if d > far.0 {
                far = (d, i);
            }
        }
        if far.0 > tol {
            keep[far.1] = true;
            stack.push((a, far.1));
            stack.push((far.1, b));
        }
    }
    pts.iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// The longest run of a polyline that stays inside the tile (with a small margin), as separate
/// pieces wherever it leaves; returns the longest piece and its length.
pub fn longest_inside(pts: &[[i32; 2]], extent: u32) -> Option<(Vec<[f64; 2]>, f64)> {
    let m = 32;
    let inside = |p: [i32; 2]| {
        p[0] >= -m && p[1] >= -m && p[0] < extent as i32 + m && p[1] < extent as i32 + m
    };
    let mut best: Option<(Vec<[f64; 2]>, f64)> = None;
    let mut cur: Vec<[f64; 2]> = Vec::new();
    let mut len = 0.0;
    let mut finish = |cur: &mut Vec<[f64; 2]>, len: &mut f64| {
        if cur.len() >= 2 && best.as_ref().is_none_or(|(_, l)| *len > *l) {
            best = Some((cur.clone(), *len));
        }
        cur.clear();
        *len = 0.0;
    };
    for &p in pts {
        if !inside(p) {
            finish(&mut cur, &mut len);
            continue;
        }
        let q = [f64::from(p[0]), f64::from(p[1])];
        if let Some(last) = cur.last() {
            len += (q[0] - last[0]).hypot(q[1] - last[1]);
        }
        cur.push(q);
    }
    finish(&mut cur, &mut len);
    best
}

/// The longest prefix of `s` that is at most `max` bytes and ends on a character boundary.
fn utf8_prefix(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    (0..=max)
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

/// MapLibre's calculateSignedArea: positive or negative by winding, zero for a degenerate ring.
pub fn signed_area(ring: &[[i32; 2]]) -> f64 {
    let mut sum = 0.0;
    for w in ring.windows(2) {
        sum +=
            (f64::from(w[1][0]) - f64::from(w[0][0])) * (f64::from(w[0][1]) + f64::from(w[1][1]));
    }
    if let (Some(last), Some(first)) = (ring.last(), ring.first()) {
        if last != first {
            sum += (f64::from(first[0]) - f64::from(last[0]))
                * (f64::from(last[1]) + f64::from(first[1]));
        }
    }
    sum
}

/// Group rings into polygons as MapLibre's `classifyRings` does.
///
/// The first ring's winding marks exteriors; each later ring of the same winding starts a polygon,
/// the other winding is a hole of the current one. Rings of zero area are skipped. Returns ranges
/// of ring indices, each `[exterior, holes...]`.
pub fn classify_rings(rings: &[Vec<[i32; 2]>]) -> Vec<std::ops::Range<usize>> {
    let mut polys = Vec::new();
    let mut start: Option<usize> = None;
    let mut ccw: Option<bool> = None;
    for (i, r) in rings.iter().enumerate() {
        let area = signed_area(r);
        if area == 0.0 {
            continue;
        }
        let is_ccw = area < 0.0;
        let c = *ccw.get_or_insert(is_ccw);
        if c == is_ccw {
            if let Some(s) = start {
                polys.push(s..i);
            }
            start = Some(i);
        }
    }
    if let Some(s) = start {
        polys.push(s..rings.len());
    }
    polys
}

fn to_i16(v: i32) -> i16 {
    v.clamp(i16::MIN.into(), i16::MAX.into()) as i16
}

/// Reusable scratch for meshing tile after tile without reallocating: the decoded rings, earcut's
/// flat input, and the output mesh all keep their capacity between tiles.
#[derive(Default)]
pub struct Mesher {
    rings: Vec<Vec<[i32; 2]>>,
    flat: Vec<f64>,
    holes: Vec<usize>,
    /// The mesh of the last tile meshed.
    pub mesh: Mesh,
    /// polygons earcut could not triangulate (degenerate or self-intersecting), skipped
    pub earcut_failures: u32,
}

impl Mesher {
    /// Triangulate one polygon (rings[range]: exterior then holes) into the fill mesh.
    fn fill_polygon(&mut self, range: std::ops::Range<usize>, color: u8) {
        self.flat.clear();
        self.holes.clear();
        for (n, r) in self.rings[range].iter().enumerate() {
            let pts = if r.len() > 1 && r.first() == r.last() {
                &r[..r.len() - 1]
            } else {
                &r[..]
            };
            if n > 0 {
                self.holes.push(self.flat.len() / 2);
            }
            for p in pts {
                self.flat.push(f64::from(p[0]));
                self.flat.push(f64::from(p[1]));
            }
        }
        let Ok(tri) = earcutr::earcut(&self.flat, &self.holes, 2) else {
            self.earcut_failures += 1;
            return;
        };
        let base = self.mesh.fill.len() as u32;
        for p in self.flat.as_chunks::<2>().0 {
            self.mesh.fill.push(FillVertex {
                x: to_i16(p[0] as i32),
                y: to_i16(p[1] as i32),
                color,
            });
        }
        self.mesh
            .fill_idx
            .extend(tri.iter().map(|&i| base + i as u32));
    }

    /// Every polygon of a feature, in one color.
    pub fn fill_feature(&mut self, f: &mvt::Feature, color: u8) {
        if f.geom_type != mvt::GeomType::Polygon as u8 {
            return;
        }
        mvt::geometry(&f.geometry, &mut self.rings);
        for range in classify_rings(&self.rings) {
            self.fill_polygon(range, color);
        }
    }

    /// One polyline as a strip: two vertices a point, pushed apart along the miter.
    pub fn line_strip(&mut self, pts: &[[i32; 2]], class: LineClass) {
        // drop repeated points: a zero-length segment has no direction
        let mut p: Vec<[f64; 2]> = Vec::with_capacity(pts.len());
        for q in pts {
            let q = [f64::from(q[0]), f64::from(q[1])];
            if p.last() != Some(&q) {
                p.push(q);
            }
        }
        if p.len() < 2 {
            return;
        }
        let normal = |a: [f64; 2], b: [f64; 2]| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len = (dx * dx + dy * dy).sqrt();
            [-dy / len, dx / len]
        };
        let base = self.mesh.line.len() as u32;
        for i in 0..p.len() {
            let e = if i == 0 {
                normal(p[0], p[1])
            } else if i == p.len() - 1 {
                normal(p[i - 1], p[i])
            } else {
                let (n0, n1) = (normal(p[i - 1], p[i]), normal(p[i], p[i + 1]));
                let m = [n0[0] + n1[0], n0[1] + n1[1]];
                let ml = (m[0] * m[0] + m[1] * m[1]).sqrt();
                if ml < 1e-9 {
                    n1 // a full reversal: no miter direction, fall back to the segment normal
                } else {
                    let m = [m[0] / ml, m[1] / ml];
                    let scale = (1.0 / (m[0] * n1[0] + m[1] * n1[1])).min(MITER_LIMIT);
                    [m[0] * scale, m[1] * scale]
                }
            };
            let (ex, ey) = (
                (e[0] * EXTRUDE_SCALE).round() as i8,
                (e[1] * EXTRUDE_SCALE).round() as i8,
            );
            let (x, y) = (to_i16(p[i][0] as i32), to_i16(p[i][1] as i32));
            self.mesh.line.push(LineVertex {
                x,
                y,
                ex,
                ey,
                class: class as u8,
            });
            self.mesh.line.push(LineVertex {
                x,
                y,
                ex: -ex,
                ey: -ey,
                class: class as u8,
            });
        }
        for s in 0..(p.len() as u32 - 1) {
            let a = base + 2 * s;
            self.mesh
                .line_idx
                .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }

    fn line_feature(&mut self, f: &mvt::Feature, class: LineClass, z: u8) {
        if f.geom_type != mvt::GeomType::LineString as u8 {
            return;
        }
        mvt::geometry(&f.geometry, &mut self.rings);
        let rings = std::mem::take(&mut self.rings);
        for r in &rings {
            if class == LineClass::Rail {
                // dashed: on/off lengths from the style, in this tile's zoom (+0.5) pixels
                let w = rail_width(f64::from(z) + 0.5).max(0.5);
                let px = 4096.0 / (512.0 * 2f64.sqrt()); // tile units per px at zoom z + 0.5
                for d in dash_polyline(
                    r,
                    palette::RAIL_DASH[0] * w * px,
                    palette::RAIL_DASH[1] * w * px,
                ) {
                    self.line_strip(&d, class);
                }
            } else {
                self.line_strip(r, class);
            }
        }
        self.rings = rings;
    }

    /// Build both meshes for a decoded tile shown at zoom `z`, in the style's draw order.
    pub fn build(&mut self, layers: &[Layer], z: u8) -> &Mesh {
        self.mesh.clear();
        let layer = |name: &str| layers.iter().find(|l| l.name == name);
        let kind = |l: &Layer, f: &mvt::Feature| {
            l.prop(f, "kind")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };

        // fills, bottom to top
        if let Some(l) = layer("earth") {
            for f in &l.features {
                self.fill_feature(f, palette::EARTH);
            }
        }
        // landuse in the style's order around water: most below it, a few (piers, pedestrian
        // areas, dams) above it, or the ocean polygon paints over them
        for above in [false, true] {
            if above {
                if let Some(l) = layer("water") {
                    for f in &l.features {
                        self.fill_feature(f, palette::WATER);
                    }
                }
            }
            if let Some(l) = layer("landuse") {
                // in the style's layer order, not the tile's feature order: a school drawn under an
                // overlapping park came out green
                let mut feats: Vec<(u16, &mvt::Feature, u8)> = l
                    .features
                    .iter()
                    .filter_map(|f| {
                        let k = kind(l, f)?;
                        (palette::landuse_above_water(&k) == above)
                            .then(|| {
                                palette::landuse(&k).map(|c| (palette::landuse_rank(&k), f, c))
                            })
                            .flatten()
                    })
                    .collect();
                feats.sort_by_key(|(r, _, _)| *r); // stable: equal ranks keep tile order
                for (_, f, c) in feats {
                    self.fill_feature(f, c);
                }
            }
        }
        if let Some(l) = layer("buildings") {
            for f in &l.features {
                self.fill_feature(f, palette::BUILDINGS);
            }
        }

        // lines, gathered then drawn class by class in the style's order
        let mut lines: Vec<(LineClass, &Layer, usize)> = Vec::new();
        if let Some(l) = layer("water") {
            for (i, f) in l.features.iter().enumerate() {
                match kind(l, f).as_deref() {
                    Some("river") => lines.push((LineClass::WaterRiver, l, i)),
                    Some("stream") => lines.push((LineClass::WaterStream, l, i)),
                    _ => {}
                }
            }
        }
        if let Some(l) = layer("roads") {
            for (i, f) in l.features.iter().enumerate() {
                if let Some(c) = road_class(l, f) {
                    lines.push((c, l, i));
                }
            }
        }
        if let Some(l) = layer("boundaries") {
            for (i, f) in l.features.iter().enumerate() {
                let detail = l
                    .prop(f, "kind_detail")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(99.0);
                lines.push((
                    if detail <= 2.0 {
                        LineClass::BoundaryCountry
                    } else {
                        LineClass::Boundary
                    },
                    l,
                    i,
                ));
            }
        }
        // every road with a casing also gets its casing strip, drawn below all road fills
        let casings: Vec<(LineClass, &Layer, usize)> = lines
            .iter()
            .filter_map(|&(c, l, i)| casing_of(c).map(|k| (k, l, i)))
            .collect();
        lines.extend(casings);
        lines.sort_by_key(|(c, _, _)| *c as u8);
        for (c, l, i) in lines {
            if z >= palette::LINE_MINZOOM[c as usize] {
                self.line_feature(&l.features[i], c, z);
            }
        }
        self.labels(layers);
        &self.mesh
    }

    /// Labels: place names at their points, and one label per road name per tile on that road's
    /// longest straight segment. Only anchors inside the tile's own extent are kept, so the same
    /// label is not sent by two neighbouring tiles (MVT geometry overlaps into a buffer).
    fn labels(&mut self, layers: &[Layer]) {
        let name = |l: &Layer, f: &mvt::Feature| {
            l.prop(f, "name:en")
                .or_else(|| l.prop(f, "name"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        let num = |l: &Layer, f: &mvt::Feature, k: &str| l.prop(f, k).and_then(|v| v.as_f64());
        let inside = |p: [i32; 2], extent: u32| {
            p[0] >= 0 && p[1] >= 0 && p[0] < extent as i32 && p[1] < extent as i32
        };
        if let Some(l) = layers.iter().find(|l| l.name == "places") {
            for f in &l.features {
                let Some(text) = name(l, f) else { continue };
                let kind = match l.prop(f, "kind").and_then(|v| v.as_str()) {
                    Some("locality") => LabelKind::City,
                    Some("macrohood") => LabelKind::Suburb,
                    Some("neighbourhood") => LabelKind::Neighbourhood,
                    _ => continue,
                };
                mvt::geometry(&f.geometry, &mut self.rings);
                let Some(&p) = self.rings.first().and_then(|r| r.first()) else {
                    continue;
                };
                if !inside(p, l.extent) {
                    continue;
                }
                self.mesh.labels.push(Label {
                    x: to_i16(p[0]),
                    y: to_i16(p[1]),
                    angle: 0.0,
                    run: 0,
                    kind,
                    min_zoom: num(l, f, "min_zoom").unwrap_or(0.0).clamp(0.0, 24.0) as u8,
                    icon: 0,
                    color: 0,
                    priority: num(l, f, "sort_key")
                        .unwrap_or(0.0)
                        .clamp(i32::MIN as f64, i32::MAX as f64)
                        as i32,
                    text,
                    path: Vec::new(),
                });
            }
        }
        if let Some(l) = layers.iter().find(|l| l.name == "roads") {
            // name -> (best segment length, label)
            let mut best: std::collections::HashMap<String, (f64, Label)> = Default::default();
            for f in &l.features {
                let Some(text) = name(l, f) else { continue };
                let kind = match road_class(l, f) {
                    Some(LineClass::RoadHighway) => LabelKind::RoadHighway,
                    Some(LineClass::RoadMajor) => LabelKind::RoadMajor,
                    Some(
                        LineClass::RoadMinor
                        | LineClass::RoadService
                        | LineClass::RoadOther
                        | LineClass::RoadLink,
                    ) => LabelKind::RoadMinor,
                    _ => continue,
                };
                mvt::geometry(&f.geometry, &mut self.rings);
                for r in &self.rings {
                    if let Some(label) = path_label(
                        r,
                        l.extent,
                        &text,
                        kind,
                        LabelExtra {
                            min_zoom: num(l, f, "min_zoom").unwrap_or(0.0).clamp(0.0, 24.0) as u8,
                            color: 0,
                            priority: 1_000_000 - num(l, f, "sort_rank").unwrap_or(0.0) as i32,
                        },
                    ) {
                        if best
                            .get(&text)
                            .is_none_or(|(bl, _)| f64::from(label.run) > *bl)
                        {
                            best.insert(text.clone(), (f64::from(label.run), label));
                        }
                    }
                }
            }
            let mut roads: Vec<Label> = best.into_values().map(|(_, l)| l).collect();
            roads.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.text.cmp(&b.text))); // stable output
            self.mesh.labels.extend(roads);

            // shields: one per route number per tile, upright, on that route's longest segment
            let mut shields: std::collections::HashMap<String, (f64, Label)> = Default::default();
            for f in &l.features {
                let Some(st) = l.prop(f, "shield_text").and_then(|v| v.as_str()) else {
                    continue;
                };
                let kind = l.prop(f, "kind").and_then(|v| v.as_str());
                if !matches!(kind, Some("highway" | "major_road")) || st.chars().count() > 5 {
                    continue;
                }
                let net = l.prop(f, "network").and_then(|v| v.as_str()).unwrap_or("");
                let icon = palette::shield_icon(net, st.chars().count());
                mvt::geometry(&f.geometry, &mut self.rings);
                for r in &self.rings {
                    for w in r.windows(2) {
                        let mid = [(w[0][0] + w[1][0]) / 2, (w[0][1] + w[1][1]) / 2];
                        if !inside(mid, l.extent) {
                            continue;
                        }
                        let len = f64::from(w[1][0] - w[0][0]).hypot(f64::from(w[1][1] - w[0][1]));
                        let key = format!("{net}|{st}");
                        if shields.get(&key).is_some_and(|(bl, _)| *bl >= len) {
                            continue;
                        }
                        shields.insert(
                            key,
                            (
                                len,
                                Label {
                                    x: to_i16(mid[0]),
                                    y: to_i16(mid[1]),
                                    angle: 0.0,
                                    run: 0,
                                    kind: LabelKind::Shield,
                                    min_zoom: num(l, f, "min_zoom").unwrap_or(0.0).clamp(0.0, 24.0)
                                        as u8,
                                    icon,
                                    color: palette::SHIELD_TEXT_COLOR,
                                    priority: if kind == Some("highway") { 0 } else { 1 },
                                    text: st.to_owned(),
                                    path: Vec::new(),
                                },
                            ),
                        );
                    }
                }
            }
            let mut shields: Vec<Label> = shields.into_values().map(|(_, l)| l).collect();
            shields.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.text.cmp(&b.text)));
            self.mesh.labels.extend(shields);
        }
        if let Some(l) = layers.iter().find(|l| l.name == "pois") {
            for f in &l.features {
                let Some(text) = name(l, f) else { continue };
                let Some((icon, color)) = l
                    .prop(f, "kind")
                    .and_then(|v| v.as_str())
                    .and_then(palette::poi)
                else {
                    continue;
                };
                mvt::geometry(&f.geometry, &mut self.rings);
                let Some(&p) = self.rings.first().and_then(|r| r.first()) else {
                    continue;
                };
                if !inside(p, l.extent) {
                    continue;
                }
                let min_zoom = num(l, f, "min_zoom").unwrap_or(0.0).clamp(0.0, 24.0) as u8;
                self.mesh.labels.push(Label {
                    x: to_i16(p[0]),
                    y: to_i16(p[1]),
                    angle: 0.0,
                    run: 0,
                    kind: LabelKind::Poi,
                    min_zoom,
                    icon,
                    color,
                    priority: i32::from(min_zoom),
                    text,
                    path: Vec::new(),
                });
            }
        }
        if let Some(l) = layers.iter().find(|l| l.name == "water") {
            let mut best: std::collections::HashMap<String, (f64, Label)> = Default::default();
            for f in &l.features {
                let Some(text) = name(l, f) else { continue };
                let min_zoom = num(l, f, "min_zoom").unwrap_or(0.0).clamp(0.0, 24.0) as u8;
                let kind = l.prop(f, "kind").and_then(|v| v.as_str()).unwrap_or("");
                mvt::geometry(&f.geometry, &mut self.rings);
                let mk = |x: i32, y: i32, angle: f64, run: f64, prio: i32| Label {
                    x: to_i16(x),
                    y: to_i16(y),
                    angle: angle as f32,
                    run: run.min(f64::from(u16::MAX)) as u16,
                    kind: LabelKind::Water,
                    min_zoom,
                    icon: 0,
                    color: palette::WATER_LABEL_COLOR,
                    priority: prio,
                    text: text.clone(),
                    path: Vec::new(),
                };
                match (f.geom_type, kind) {
                    // rivers and streams: along their longest straight segment, like streets
                    (2, "river" | "stream") => {
                        for r in &self.rings {
                            let extra = LabelExtra {
                                min_zoom,
                                color: palette::WATER_LABEL_COLOR,
                                priority: 2,
                            };
                            if let Some(label) =
                                path_label(r, l.extent, &text, LabelKind::Water, extra)
                            {
                                if best
                                    .get(&text)
                                    .is_none_or(|(bl, _)| f64::from(label.run) > *bl)
                                {
                                    best.insert(text.clone(), (f64::from(label.run), label));
                                }
                            }
                        }
                    }
                    // seas, oceans, bays, lakes: at the point, or the middle of the outer ring
                    (1, _) | (3, _)
                        if matches!(
                            kind,
                            "sea" | "ocean" | "bay" | "strait" | "lake" | "water"
                        ) =>
                    {
                        let Some(r) = self.rings.first() else {
                            continue;
                        };
                        let (sx, sy) = r.iter().fold((0i64, 0i64), |(a, b), p| {
                            (a + i64::from(p[0]), b + i64::from(p[1]))
                        });
                        let n = r.len().max(1) as i64;
                        let c = [(sx / n) as i32, (sy / n) as i32];
                        if inside(c, l.extent) && !best.contains_key(&text) {
                            best.insert(
                                text.clone(),
                                (
                                    f64::MAX,
                                    mk(c[0], c[1], 0.0, 0.0, if kind == "ocean" { 0 } else { 1 }),
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
            let mut water: Vec<Label> = best.into_values().map(|(_, l)| l).collect();
            water.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.text.cmp(&b.text)));
            self.mesh.labels.extend(water);
        }
    }
}

/// The fields of a path label that do not come from its geometry.
pub struct LabelExtra {
    /// [`Label::min_zoom`].
    pub min_zoom: u8,
    /// [`Label::color`].
    pub color: u8,
    /// [`Label::priority`].
    pub priority: i32,
}

/// A street or stream name along the longest piece of `ring` inside the tile.
///
/// Its path is simplified (Douglas-Peucker, at most [`PATH_MAX_POINTS`] points), oriented to read
/// left to right, and anchored at its middle. `run` is the path's length; `angle` the direction at the middle (kept for the
/// straight-label fallback).
pub fn path_label(
    ring: &[[i32; 2]],
    extent: u32,
    text: &str,
    kind: LabelKind,
    extra: LabelExtra,
) -> Option<Label> {
    let (piece, len) = longest_inside(ring, extent)?;
    if len < 1.0 {
        return None;
    }
    // Too many points: simplify the original harder, doubling the tolerance each pass. Doubling is
    // what guarantees an end (at worst the two endpoints remain); the first version derived the
    // tolerance from the point count, which a wiggly road could leave unchanged, so it looped forever.
    let mut tol = PATH_TOLERANCE;
    let mut path = simplify(&piece, tol);
    while path.len() > PATH_MAX_POINTS {
        tol *= 2.0;
        path = simplify(&piece, tol);
    }
    if path.last()?[0] < path.first()?[0] {
        path.reverse(); // read left to right
    }
    // the middle of the path, by length, and the direction there
    let half = len / 2.0;
    let mut acc = 0.0;
    let (mut mid, mut angle) = (path[0], 0.0);
    for w in path.windows(2) {
        let seg = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
        if acc + seg >= half && seg > 0.0 {
            let t = (half - acc) / seg;
            mid = [
                w[0][0] + (w[1][0] - w[0][0]) * t,
                w[0][1] + (w[1][1] - w[0][1]) * t,
            ];
            angle = (w[1][1] - w[0][1]).atan2(w[1][0] - w[0][0]);
            break;
        }
        acc += seg;
    }
    // only anchors inside the tile's own extent: a neighbouring tile sends its own
    if !(mid[0] >= 0.0 && mid[1] >= 0.0 && mid[0] < f64::from(extent) && mid[1] < f64::from(extent))
    {
        return None;
    }
    Some(Label {
        x: to_i16(mid[0] as i32),
        y: to_i16(mid[1] as i32),
        angle: angle as f32,
        run: len.min(f64::from(u16::MAX)) as u16,
        kind,
        min_zoom: extra.min_zoom,
        icon: 0,
        color: extra.color,
        priority: extra.priority,
        text: text.to_owned(),
        path: path
            .iter()
            .map(|p| [to_i16(p[0].round() as i32), to_i16(p[1].round() as i32)])
            .collect(),
    })
}

/// The casing class drawn under a road class, if it has one.
pub fn casing_of(c: LineClass) -> Option<LineClass> {
    Some(match c {
        LineClass::RoadService => LineClass::CasingService,
        LineClass::RoadMinor => LineClass::CasingMinor,
        LineClass::RoadLink => LineClass::CasingLink,
        LineClass::RoadMajor => LineClass::CasingMajor,
        LineClass::RoadHighway => LineClass::CasingHighway,
        _ => return None,
    })
}

/// The railway's width in px at zoom `z` (MapLibre's exponential interpolation of the style's stops).
pub fn rail_width(z: f64) -> f64 {
    let s = &palette::RAIL_WIDTH_STOPS;
    if z <= s[0][0] {
        return s[0][1];
    }
    if z >= s[s.len() - 1][0] {
        return s[s.len() - 1][1];
    }
    let i = s.windows(2).position(|w| z < w[1][0]).unwrap_or(0);
    let ([z0, v0], [z1, v1]) = (s[i], s[i + 1]);
    let b = palette::RAIL_WIDTH_BASE;
    let t = if b == 1.0 {
        (z - z0) / (z1 - z0)
    } else {
        (b.powf(z - z0) - 1.0) / (b.powf(z1 - z0) - 1.0)
    };
    v0 + t * (v1 - v0)
}

/// Cut a polyline into dashes `on` long with `off` gaps (tile units), the pattern running on across
/// vertices. Each dash keeps the polyline's corners that fall inside it.
pub fn dash_polyline(pts: &[[i32; 2]], on: f64, off: f64) -> Vec<Vec<[i32; 2]>> {
    let mut out = Vec::new();
    if pts.len() < 2 || on <= 0.0 {
        return out;
    }
    let period = on + off.max(0.0);
    let mut phase = 0.0; // distance into the current period
    let mut cur: Vec<[f64; 2]> = Vec::new();
    let flush = |cur: &mut Vec<[f64; 2]>, out: &mut Vec<Vec<[i32; 2]>>| {
        if cur.len() >= 2 {
            out.push(
                cur.iter()
                    .map(|p| [p[0].round() as i32, p[1].round() as i32])
                    .collect(),
            );
        }
        cur.clear();
    };
    for w in pts.windows(2) {
        let (a, b) = (
            [f64::from(w[0][0]), f64::from(w[0][1])],
            [f64::from(w[1][0]), f64::from(w[1][1])],
        );
        let len = (b[0] - a[0]).hypot(b[1] - a[1]);
        if len == 0.0 {
            continue;
        }
        let at = |d: f64| {
            [
                a[0] + (b[0] - a[0]) * d / len,
                a[1] + (b[1] - a[1]) * d / len,
            ]
        };
        let mut d = 0.0;
        while d < len {
            let in_dash = phase < on;
            let step = if in_dash { on - phase } else { period - phase };
            let take = step.min(len - d);
            if in_dash {
                if cur.is_empty() {
                    cur.push(at(d));
                }
                cur.push(at(d + take));
            }
            d += take;
            phase += take;
            if phase >= period - 1e-9 {
                phase = 0.0;
            }
            if in_dash && phase >= on - 1e-9 || phase == 0.0 {
                flush(&mut cur, &mut out);
            }
        }
    }
    flush(&mut cur, &mut out);
    out
}

/// The style's road classes, simplified: no tunnel or bridge variants.
pub fn road_class(l: &Layer, f: &mvt::Feature) -> Option<LineClass> {
    let s = |k: &str| l.prop(f, k).and_then(|v| v.as_str());
    let has = |k: &str| l.prop(f, k).is_some();
    if s("kind_detail") == Some("pier") {
        return None;
    }
    match s("kind")? {
        "rail" => Some(LineClass::Rail),
        _ if has("is_link") => Some(LineClass::RoadLink),
        "highway" => Some(LineClass::RoadHighway),
        "major_road" => Some(LineClass::RoadMajor),
        "minor_road" if s("kind_detail") == Some("service") => Some(LineClass::RoadService),
        "minor_road" => Some(LineClass::RoadMinor),
        "other" | "path" => Some(LineClass::RoadOther),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri_area(m: &Mesh) -> f64 {
        m.fill_idx
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let (a, b, c) = (
                    m.fill[t[0] as usize],
                    m.fill[t[1] as usize],
                    m.fill[t[2] as usize],
                );
                let (ax, ay, bx, by, cx, cy) = (
                    a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64,
                );
                ((bx - ax) * (cy - ay) - (cx - ax) * (by - ay)).abs() / 2.0
            })
            .sum()
    }

    #[test]
    fn rings_classify_into_polygons_with_holes() {
        let sq = |x: i32, y: i32, s: i32, cw: bool| {
            let mut r = vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s], [x, y]];
            if !cw {
                r.reverse();
            }
            r
        };
        let rings = vec![
            sq(0, 0, 100, true),
            sq(10, 10, 20, false),
            sq(200, 0, 50, true),
        ];
        assert_eq!(classify_rings(&rings), vec![0..2, 2..3]);
        // a degenerate ring is skipped, not taken as an exterior
        let rings = vec![vec![[0, 0], [5, 0], [0, 0]], sq(0, 0, 10, true)];
        assert_eq!(classify_rings(&rings), vec![1..2]);
    }

    #[test]
    fn a_polygon_with_a_hole_triangulates_to_its_area() {
        let mut m = Mesher {
            rings: vec![
                vec![[0, 0], [100, 0], [100, 100], [0, 100], [0, 0]],
                vec![[20, 20], [20, 40], [40, 40], [40, 20], [20, 20]],
            ],
            ..Default::default()
        };
        m.fill_polygon(0..2, 7);
        assert_eq!(tri_area(&m.mesh), 100.0 * 100.0 - 20.0 * 20.0);
        assert_eq!(m.mesh.fill.len(), 8, "closing duplicates dropped");
        assert!(m.mesh.fill.iter().all(|v| v.color == 7));
    }

    #[test]
    fn lines_extrude_by_unit_normals_and_clamped_miters() {
        let mut m = Mesher::default();
        // straight: both ends extrude along the unit normal (0, 1) * 32
        m.line_strip(&[[0, 0], [10, 0], [10, 0], [20, 0]], LineClass::RoadMinor);
        assert_eq!(m.mesh.line.len(), 6, "the repeated point is dropped");
        assert!(m
            .mesh
            .line
            .iter()
            .step_by(2)
            .all(|v| (v.ex, v.ey) == (0, 32)));
        assert_eq!(m.mesh.line_idx.len(), 12);
        // a right angle: the miter is sqrt(2) long, along the diagonal
        m.mesh.clear();
        m.line_strip(&[[0, 0], [10, 0], [10, 10]], LineClass::RoadMinor);
        let mid = m.mesh.line[2];
        assert_eq!((mid.ex, mid.ey), (-32, 32));
        // a near reversal is clamped to the miter limit
        m.mesh.clear();
        m.line_strip(&[[0, 0], [100, 0], [0, 1]], LineClass::RoadMinor);
        let v = m.mesh.line[2];
        let len = ((v.ex as f64).powi(2) + (v.ey as f64).powi(2)).sqrt() / EXTRUDE_SCALE;
        assert!(len <= MITER_LIMIT + 0.02, "miter {len}");
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn wire_format_header_layout_and_padding() {
        let mut m = Mesher {
            rings: vec![vec![[0, 0], [10, 0], [10, 10], [0, 0]]], // 3 fill vertices: 18 B, padded
            ..Default::default()
        };
        m.fill_polygon(0..1, 5);
        m.line_strip(&[[0, 0], [10, 0]], LineClass::RoadMajor);
        let mut out = Vec::new();
        m.mesh.to_bytes(&mut out);
        assert_eq!(out.len(), m.mesh.byte_len());
        assert_eq!(&out[0..4], b"MESH");
        assert_eq!(u32_at(&out, 4), 5);
        assert_eq!(u32_at(&out, 8), 0, "both index blocks u16");
        assert_eq!(
            [
                u32_at(&out, 12),
                u32_at(&out, 16),
                u32_at(&out, 20),
                u32_at(&out, 24)
            ],
            [3, 3, 4, 6]
        );
        assert_eq!([u32_at(&out, 28), u32_at(&out, 32)], [0, 0], "no labels");
        // fills at 36: 3 x 6 B = 18, padded to 20; fill indices 3 x 2 = 6, padded to 8
        assert_eq!(out[36 + 4], 5, "first fill vertex color index");
        let line_at = 36 + 20 + 8;
        assert_eq!(
            &out[line_at + 4..line_at + 7],
            &[0, 32, LineClass::RoadMajor as u8]
        );
        assert_eq!(out.len(), line_at + 4 * 8 + 12);
        assert_eq!(out.len() % 4, 0);
    }

    #[test]
    fn simplify_keeps_corners_and_drops_near_straight_points() {
        let p = [[0.0, 0.0], [50.0, 1.0], [100.0, 0.0], [100.0, 100.0]];
        assert_eq!(
            simplify(&p, 6.0),
            vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]]
        );
        assert_eq!(
            simplify(&p, 0.5),
            p.to_vec(),
            "within tolerance nothing is dropped"
        );
    }

    #[test]
    fn a_path_label_reads_left_to_right_and_is_anchored_mid_length() {
        // drawn right to left: reversed so the text is not upside down
        let ring = [[3000, 2000], [2000, 2000], [1000, 2000]];
        let l = path_label(
            &ring,
            4096,
            "X St",
            LabelKind::RoadMinor,
            LabelExtra {
                min_zoom: 0,
                color: 0,
                priority: 0,
            },
        )
        .unwrap();
        assert_eq!(
            l.path,
            vec![[1000, 2000], [3000, 2000]],
            "collinear middle point simplified away"
        );
        assert_eq!((l.x, l.y, l.run), (2000, 2000, 2000));
        // a run leaving the tile is cut where it leaves; the longest piece inside wins
        let out = [
            [100, 100],
            [600, 100],
            [9000, 100],
            [9000, 900],
            [700, 900],
            [100, 900],
        ];
        let (piece, len) = longest_inside(&out, 4096).unwrap();
        assert_eq!(piece, vec![[700.0, 900.0], [100.0, 900.0]]);
        assert_eq!(len, 600.0);
        // a long wiggly path is capped at PATH_MAX_POINTS
        let zig: Vec<[i32; 2]> = (0..400)
            .map(|i| [i * 10, if i % 2 == 0 { 0 } else { 60 }])
            .collect();
        let z = path_label(
            &zig,
            4096,
            "Zig",
            LabelKind::RoadMinor,
            LabelExtra {
                min_zoom: 0,
                color: 0,
                priority: 0,
            },
        )
        .unwrap();
        assert!(z.path.len() <= PATH_MAX_POINTS && z.path.len() >= 2);
    }

    #[test]
    fn a_path_that_one_tolerance_cannot_reduce_still_ends_under_the_cap() {
        // 40 points zigzagging 60 units: simplifying at the tolerance the first version computed
        // (6 x 2 x 40/32 = 15) keeps every point, and it looped forever on a canyon road
        let zig: Vec<[i32; 2]> = (0..40)
            .map(|i| [i * 50, if i % 2 == 0 { 1000 } else { 1060 }])
            .collect();
        let l = path_label(
            &zig,
            4096,
            "Topanga Canyon",
            LabelKind::RoadMajor,
            LabelExtra {
                min_zoom: 0,
                color: 0,
                priority: 0,
            },
        )
        .unwrap();
        assert!(
            l.path.len() <= PATH_MAX_POINTS && l.path.len() >= 2,
            "{} points",
            l.path.len()
        );
    }

    #[test]
    fn dashes_follow_the_pattern_across_corners() {
        // a straight 100-unit line, 10 on / 15 off: dashes at 0-10, 25-35, 50-60, 75-85
        let d = dash_polyline(&[[0, 0], [100, 0]], 10.0, 15.0);
        assert_eq!(
            d,
            vec![
                vec![[0, 0], [10, 0]],
                vec![[25, 0], [35, 0]],
                vec![[50, 0], [60, 0]],
                vec![[75, 0], [85, 0]]
            ]
        );
        // the pattern carries round a corner: a dash that starts before it keeps the corner point
        let c = dash_polyline(&[[0, 0], [6, 0], [6, 20]], 10.0, 5.0);
        assert_eq!(c[0], vec![[0, 0], [6, 0], [6, 4]]);
        assert_eq!(c[1], vec![[6, 9], [6, 19]]);
        // no dash is longer than `on`, nothing degenerate
        assert!(dash_polyline(&[[0, 0], [1000, 7]], 33.0, 41.0)
            .iter()
            .all(|d| d.len() >= 2));
        assert!(dash_polyline(&[[0, 0]], 5.0, 5.0).is_empty());
    }

    #[test]
    fn every_road_class_with_a_casing_has_one_drawn_before_it() {
        for c in [
            LineClass::RoadService,
            LineClass::RoadMinor,
            LineClass::RoadLink,
            LineClass::RoadMajor,
            LineClass::RoadHighway,
        ] {
            let k = casing_of(c).expect("casing");
            assert!(
                (k as u8) < (LineClass::RoadOther as u8),
                "{k:?} must come before every road fill"
            );
        }
        assert_eq!(casing_of(LineClass::Rail), None);
        assert_eq!(casing_of(LineClass::RoadOther), None);
    }

    #[test]
    fn labels_on_the_wire() {
        let mut m = Mesh::default();
        m.labels.push(Label {
            x: 10,
            y: -2,
            angle: 0.5,
            run: 300,
            kind: LabelKind::RoadMajor,
            min_zoom: 12,
            icon: 3,
            color: 4,
            priority: -7,
            text: "Ocean Ave".into(),
            path: vec![[0, 0], [300, -5]],
        });
        m.labels.push(Label {
            x: 1,
            y: 2,
            angle: 0.0,
            run: 0,
            kind: LabelKind::City,
            min_zoom: 8,
            icon: 0,
            color: 0,
            priority: 3,
            text: "Zürich".into(),
            path: vec![],
        });
        let mut out = Vec::new();
        m.to_bytes(&mut out);
        assert_eq!(out.len(), m.byte_len());
        assert_eq!(u32_at(&out, 28), 2);
        let block = u32_at(&out, 32) as usize;
        assert_eq!(
            block,
            (18 + 9 + 8 + 18 + 7 + 3) & !3,
            "Zürich is 7 bytes of UTF-8; Ocean Ave has 2 path points"
        );
        let b = &out[36..];
        assert_eq!(i16::from_le_bytes([b[0], b[1]]), 10);
        assert_eq!(i16::from_le_bytes([b[2], b[3]]), -2);
        assert_eq!(i16::from_le_bytes([b[4], b[5]]), 5000);
        assert_eq!(u16::from_le_bytes([b[6], b[7]]), 300);
        assert_eq!(
            (b[8], b[9], b[10], b[11]),
            (LabelKind::RoadMajor as u8, 12, 3, 4)
        );
        assert_eq!(i32::from_le_bytes(b[12..16].try_into().unwrap()), -7);
        assert_eq!(b[16], 9);
        assert_eq!(&b[17..26], b"Ocean Ave");
        assert_eq!(b[26], 2, "two path points");
        assert_eq!(i16::from_le_bytes([b[31], b[32]]), 300);
        assert_eq!(i16::from_le_bytes([b[33], b[34]]), -5);
        assert_eq!(b[35 + 16], 7, "the next label's text length");
    }

    #[test]
    fn long_text_is_cut_on_a_character_boundary() {
        let s = "é".repeat(200); // 400 bytes
        let n = utf8_prefix(&s, 255);
        assert!(n <= 255 && s.is_char_boundary(n));
        assert_eq!(n, 254);
    }

    #[test]
    fn indices_widen_past_65535_vertices() {
        let m = Mesh {
            fill: vec![
                FillVertex {
                    x: 0,
                    y: 0,
                    color: 0
                };
                65536
            ],
            fill_idx: vec![0, 1, 65535],
            ..Default::default()
        };
        let mut out = Vec::new();
        m.to_bytes(&mut out);
        assert_eq!(u32_at(&out, 8), 1, "fill indices u32");
        assert_eq!(out.len(), m.byte_len());
        let idx_at = 36 + 65536 * 6;
        assert_eq!(u32_at(&out, idx_at + 8), 65535);
    }
}
