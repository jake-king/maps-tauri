//! Turn a decoded tile into two GPU-ready meshes: fills (triangulated polygons) and lines (strips
//! with extrusion vectors). Colors and line classes live in the vertices, so the renderer draws a
//! whole tile in two calls: guile-2d's "one draw call per sheet".
//!
//! Wire format of [`Mesh::to_bytes`], version 2 (little-endian, every block padded to 4 bytes):
//!   u32 magic "MESH", u32 version, u32 flags (bit 0: fill indices are u32, bit 1: line indices
//!   are u32; otherwise u16), u32 fill vertex count, u32 fill index count, u32 line vertex count,
//!   u32 line index count,
//!   fill vertices (6 B: i16 x, i16 y, u8 color index, u8 pad), fill indices,
//!   line vertices (8 B: i16 x, i16 y, i8 ex, i8 ey, u8 class, u8 pad), line indices.
//! x, y are tile coordinates (extent 4096). (ex, ey) / 32 is the unit extrusion; the shader scales
//! it by the class's half width in pixels for the current zoom. Colors are looked up in the
//! palette (fills by index, lines by class), so a vertex carries no color.
//! Version 1 (8 B fill and 16 B line vertices with full colors, u32 indices) was ~70% larger.

use crate::mvt::{self, Layer};
use crate::palette::{self, LineClass};

pub const MAGIC: u32 = u32::from_le_bytes(*b"MESH");
pub const VERSION: u32 = 2;
/// Extrusion is stored as i8: the unit vector times 32, so a miter of up to MITER_LIMIT (2) fits.
pub const EXTRUDE_SCALE: f64 = 32.0;
/// Miter joins longer than this (in half widths) are clamped, so sharp corners do not spike.
pub const MITER_LIMIT: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillVertex {
    pub x: i16,
    pub y: i16,
    pub color: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineVertex {
    pub x: i16,
    pub y: i16,
    pub ex: i8,
    pub ey: i8,
    pub class: u8,
}

#[derive(Default, Debug)]
pub struct Mesh {
    pub fill: Vec<FillVertex>,
    pub fill_idx: Vec<u32>,
    pub line: Vec<LineVertex>,
    pub line_idx: Vec<u32>,
}

impl Mesh {
    pub fn clear(&mut self) {
        self.fill.clear();
        self.fill_idx.clear();
        self.line.clear();
        self.line_idx.clear();
    }

    fn wide(vertices: usize) -> bool {
        vertices > usize::from(u16::MAX)
    }

    pub fn byte_len(&self) -> usize {
        let pad4 = |n: usize| (n + 3) & !3;
        let idx = |n: usize, wide: bool| pad4(n * if wide { 4 } else { 2 });
        28 + pad4(self.fill.len() * 6)
            + idx(self.fill_idx.len(), Self::wide(self.fill.len()))
            + self.line.len() * 8
            + idx(self.line_idx.len(), Self::wide(self.line.len()))
    }

    /// The wire format above, written into `out` (cleared first).
    pub fn to_bytes(&self, out: &mut Vec<u8>) {
        let pad4 = |out: &mut Vec<u8>| out.resize((out.len() + 3) & !3, 0);
        let (fw, lw) = (Self::wide(self.fill.len()), Self::wide(self.line.len()));
        out.clear();
        out.reserve(self.byte_len());
        let flags = u32::from(fw) | (u32::from(lw) << 1);
        for v in [MAGIC, VERSION, flags, self.fill.len() as u32, self.fill_idx.len() as u32, self.line.len() as u32, self.line_idx.len() as u32] {
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
    }
}

/// MapLibre's calculateSignedArea: positive or negative by winding, zero for a degenerate ring.
pub fn signed_area(ring: &[[i32; 2]]) -> f64 {
    let mut sum = 0.0;
    for w in ring.windows(2) {
        sum += (f64::from(w[1][0]) - f64::from(w[0][0])) * (f64::from(w[0][1]) + f64::from(w[1][1]));
    }
    if let (Some(last), Some(first)) = (ring.last(), ring.first()) {
        if last != first {
            sum += (f64::from(first[0]) - f64::from(last[0])) * (f64::from(last[1]) + f64::from(first[1]));
        }
    }
    sum
}

/// MapLibre's classifyRings: the first ring's winding marks exteriors; each later ring of the same
/// winding starts a polygon, the other winding is a hole of the current one. Returns ranges of
/// ring indices, each `[exterior, holes...]`.
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
            let pts = if r.len() > 1 && r.first() == r.last() { &r[..r.len() - 1] } else { &r[..] };
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
        for p in self.flat.chunks_exact(2) {
            self.mesh.fill.push(FillVertex { x: to_i16(p[0] as i32), y: to_i16(p[1] as i32), color });
        }
        self.mesh.fill_idx.extend(tri.iter().map(|&i| base + i as u32));
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
            let (ex, ey) = ((e[0] * EXTRUDE_SCALE).round() as i8, (e[1] * EXTRUDE_SCALE).round() as i8);
            let (x, y) = (to_i16(p[i][0] as i32), to_i16(p[i][1] as i32));
            self.mesh.line.push(LineVertex { x, y, ex, ey, class: class as u8 });
            self.mesh.line.push(LineVertex { x, y, ex: -ex, ey: -ey, class: class as u8 });
        }
        for s in 0..(p.len() as u32 - 1) {
            let a = base + 2 * s;
            self.mesh.line_idx.extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
        }
    }

    fn line_feature(&mut self, f: &mvt::Feature, class: LineClass) {
        if f.geom_type != mvt::GeomType::LineString as u8 {
            return;
        }
        mvt::geometry(&f.geometry, &mut self.rings);
        let rings = std::mem::take(&mut self.rings);
        for r in &rings {
            self.line_strip(r, class);
        }
        self.rings = rings;
    }

    /// Build both meshes for a decoded tile shown at zoom `z`, in the style's draw order.
    pub fn build(&mut self, layers: &[Layer], z: u8) -> &Mesh {
        self.mesh.clear();
        let layer = |name: &str| layers.iter().find(|l| l.name == name);
        let kind = |l: &Layer, f: &mvt::Feature| l.prop(f, "kind").and_then(|v| v.as_str()).map(str::to_owned);

        // fills, bottom to top
        if let Some(l) = layer("earth") {
            for f in &l.features {
                self.fill_feature(f, palette::EARTH);
            }
        }
        if let Some(l) = layer("landuse") {
            for f in &l.features {
                if let Some(c) = kind(l, f).as_deref().and_then(palette::landuse) {
                    self.fill_feature(f, c);
                }
            }
        }
        if let Some(l) = layer("water") {
            for f in &l.features {
                self.fill_feature(f, palette::WATER);
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
                let detail = l.prop(f, "kind_detail").and_then(|v| v.as_f64()).unwrap_or(99.0);
                lines.push((if detail <= 2.0 { LineClass::BoundaryCountry } else { LineClass::Boundary }, l, i));
            }
        }
        lines.sort_by_key(|(c, _, _)| *c as u8);
        for (c, l, i) in lines {
            if z >= palette::LINE_MINZOOM[c as usize] {
                self.line_feature(&l.features[i], c);
            }
        }
        &self.mesh
    }
}

/// The style's road classes, simplified: no tunnel, bridge or casing variants in v1.
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
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (m.fill[t[0] as usize], m.fill[t[1] as usize], m.fill[t[2] as usize]);
                let (ax, ay, bx, by, cx, cy) =
                    (a.x as f64, a.y as f64, b.x as f64, b.y as f64, c.x as f64, c.y as f64);
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
        let rings = vec![sq(0, 0, 100, true), sq(10, 10, 20, false), sq(200, 0, 50, true)];
        assert_eq!(classify_rings(&rings), vec![0..2, 2..3]);
        // a degenerate ring is skipped, not taken as an exterior
        let rings = vec![vec![[0, 0], [5, 0], [0, 0]], sq(0, 0, 10, true)];
        assert_eq!(classify_rings(&rings), vec![1..2]);
    }

    #[test]
    fn a_polygon_with_a_hole_triangulates_to_its_area() {
        let mut m = Mesher::default();
        m.rings = vec![
            vec![[0, 0], [100, 0], [100, 100], [0, 100], [0, 0]],
            vec![[20, 20], [20, 40], [40, 40], [40, 20], [20, 20]],
        ];
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
        assert!(m.mesh.line.iter().step_by(2).all(|v| (v.ex, v.ey) == (0, 32)));
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
        let mut m = Mesher::default();
        m.rings = vec![vec![[0, 0], [10, 0], [10, 10], [0, 0]]]; // 3 fill vertices: 18 B, padded
        m.fill_polygon(0..1, 5);
        m.line_strip(&[[0, 0], [10, 0]], LineClass::RoadMajor);
        let mut out = Vec::new();
        m.mesh.to_bytes(&mut out);
        assert_eq!(out.len(), m.mesh.byte_len());
        assert_eq!(&out[0..4], b"MESH");
        assert_eq!(u32_at(&out, 4), 2);
        assert_eq!(u32_at(&out, 8), 0, "both index blocks u16");
        assert_eq!([u32_at(&out, 12), u32_at(&out, 16), u32_at(&out, 20), u32_at(&out, 24)], [3, 3, 4, 6]);
        // fills at 28: 3 x 6 B = 18, padded to 20; fill indices 3 x 2 = 6, padded to 8
        assert_eq!(out[28 + 4], 5, "first fill vertex color index");
        let line_at = 28 + 20 + 8;
        assert_eq!(&out[line_at + 4..line_at + 7], &[0, 32, LineClass::RoadMajor as u8]);
        assert_eq!(out.len(), line_at + 4 * 8 + 12);
        assert_eq!(out.len() % 4, 0);
    }

    #[test]
    fn indices_widen_past_65535_vertices() {
        let mut m = Mesh::default();
        m.fill = vec![FillVertex { x: 0, y: 0, color: 0 }; 65536];
        m.fill_idx = vec![0, 1, 65535];
        let mut out = Vec::new();
        m.to_bytes(&mut out);
        assert_eq!(u32_at(&out, 8), 1, "fill indices u32");
        assert_eq!(out.len(), m.byte_len());
        let idx_at = 28 + 65536 * 6;
        assert_eq!(u32_at(&out, idx_at + 8), 65535);
    }
}
