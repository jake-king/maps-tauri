//! Mapbox Vector Tile v2 decoding: protobuf messages Tile > Layer > Feature, and geometry as a
//! stream of MoveTo / LineTo / ClosePath commands with zigzag-encoded deltas.
//! Spec: https://github.com/mapbox/vector-tile-spec/tree/master/2.1

use std::io;

use crate::pmtiles::varint;

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Float(f32),
    Double(f64),
    Int(i64),
    Bool(bool),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Float(v) => Some(v as f64),
            Value::Double(v) => Some(v),
            Value::Int(v) => Some(v as f64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomType {
    Unknown = 0,
    Point = 1,
    LineString = 2,
    Polygon = 3,
}

#[derive(Debug, Default)]
pub struct Feature {
    pub id: u64,
    pub geom_type: u8,
    /// pairs of (key index, value index) into the layer's tables
    pub tags: Vec<u32>,
    /// the raw command stream
    pub geometry: Vec<u32>,
}

#[derive(Debug)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub keys: Vec<String>,
    pub values: Vec<Value>,
    pub features: Vec<Feature>,
}

impl Layer {
    /// The value of property `key` on feature `f`.
    pub fn prop<'a>(&'a self, f: &Feature, key: &str) -> Option<&'a Value> {
        let k = self.keys.iter().position(|x| x == key)? as u32;
        f.tags
            .as_chunks::<2>()
            .0
            .iter()
            .find(|p| p[0] == k)
            .and_then(|p| self.values.get(p[1] as usize))
    }
}

/// One field of a protobuf message: its number and its payload.
enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed64(u64),
    Fixed32(u32),
}

fn next_field<'a>(b: &'a [u8], p: &mut usize) -> io::Result<(u32, Field<'a>)> {
    let key = varint(b, p)?;
    let (num, wire) = ((key >> 3) as u32, key & 7);
    let f = match wire {
        0 => Field::Varint(varint(b, p)?),
        1 => {
            let s = b.get(*p..*p + 8).ok_or_else(|| bad("truncated fixed64"))?;
            *p += 8;
            Field::Fixed64(u64::from_le_bytes(s.try_into().expect("8")))
        }
        2 => {
            let n = varint(b, p)? as usize;
            let s = b
                .get(*p..*p + n)
                .ok_or_else(|| bad("truncated bytes field"))?;
            *p += n;
            Field::Bytes(s)
        }
        5 => {
            let s = b.get(*p..*p + 4).ok_or_else(|| bad("truncated fixed32"))?;
            *p += 4;
            Field::Fixed32(u32::from_le_bytes(s.try_into().expect("4")))
        }
        _ => return Err(bad("unsupported wire type")),
    };
    Ok((num, f))
}

fn packed_u32(b: &[u8], out: &mut Vec<u32>) -> io::Result<()> {
    let mut p = 0;
    while p < b.len() {
        out.push(varint(b, &mut p)? as u32);
    }
    Ok(())
}

fn utf8(b: &[u8]) -> io::Result<String> {
    String::from_utf8(b.to_vec()).map_err(|_| bad("string is not UTF-8"))
}

fn value(b: &[u8]) -> io::Result<Value> {
    let mut p = 0;
    let mut v = Value::Bool(false);
    while p < b.len() {
        v = match next_field(b, &mut p)? {
            (1, Field::Bytes(s)) => Value::String(utf8(s)?),
            (2, Field::Fixed32(x)) => Value::Float(f32::from_bits(x)),
            (3, Field::Fixed64(x)) => Value::Double(f64::from_bits(x)),
            (4, Field::Varint(x)) => Value::Int(x as i64),
            (5, Field::Varint(x)) => Value::Int(x as i64),
            (6, Field::Varint(x)) => Value::Int(((x >> 1) as i64) ^ -((x & 1) as i64)),
            (7, Field::Varint(x)) => Value::Bool(x != 0),
            _ => v,
        };
    }
    Ok(v)
}

fn feature(b: &[u8]) -> io::Result<Feature> {
    let mut f = Feature::default();
    let mut p = 0;
    while p < b.len() {
        match next_field(b, &mut p)? {
            (1, Field::Varint(x)) => f.id = x,
            (2, Field::Bytes(s)) => packed_u32(s, &mut f.tags)?,
            (3, Field::Varint(x)) => f.geom_type = x as u8,
            (4, Field::Bytes(s)) => packed_u32(s, &mut f.geometry)?,
            _ => {}
        }
    }
    Ok(f)
}

fn layer(b: &[u8]) -> io::Result<Layer> {
    let mut l = Layer {
        name: String::new(),
        extent: 4096,
        keys: vec![],
        values: vec![],
        features: vec![],
    };
    let mut p = 0;
    while p < b.len() {
        match next_field(b, &mut p)? {
            (1, Field::Bytes(s)) => l.name = utf8(s)?,
            (2, Field::Bytes(s)) => l.features.push(feature(s)?),
            (3, Field::Bytes(s)) => l.keys.push(utf8(s)?),
            (4, Field::Bytes(s)) => l.values.push(value(s)?),
            (5, Field::Varint(x)) => l.extent = x as u32,
            _ => {}
        }
    }
    Ok(l)
}

/// Decode a whole (decompressed) tile.
pub fn decode(b: &[u8]) -> io::Result<Vec<Layer>> {
    let mut layers = Vec::new();
    let mut p = 0;
    while p < b.len() {
        if let (3, Field::Bytes(s)) = next_field(b, &mut p)? {
            layers.push(layer(s)?);
        }
    }
    Ok(layers)
}

/// A feature's geometry as point lists in tile coordinates: one per MoveTo. ClosePath appends the
/// ring's first point again, so a polygon ring comes back closed, exactly as @mapbox/vector-tile's
/// loadGeometry returns it (checked over every tile by tools/mvt_diff.mjs). `out` is cleared and reused, so a caller decoding many features
/// allocates only when a feature is bigger than any before it.
pub fn geometry(cmds: &[u32], out: &mut Vec<Vec<[i32; 2]>>) {
    let mut used = 0;
    let (mut x, mut y) = (0i32, 0i32);
    let mut i = 0;
    let zz = |v: u32| ((v >> 1) as i32) ^ -((v & 1) as i32);
    while i < cmds.len() {
        let (id, count) = (cmds[i] & 7, cmds[i] >> 3);
        i += 1;
        match id {
            1 | 2 => {
                for _ in 0..count {
                    let (Some(&dx), Some(&dy)) = (cmds.get(i), cmds.get(i + 1)) else {
                        break;
                    };
                    i += 2;
                    x += zz(dx);
                    y += zz(dy);
                    if id == 1 {
                        if used == out.len() {
                            out.push(Vec::new());
                        }
                        out[used].clear();
                        used += 1;
                    }
                    if used > 0 {
                        out[used - 1].push([x, y]);
                    }
                }
            }
            7 => {
                // ClosePath: repeat the ring's first point, as @mapbox/vector-tile does
                if used > 0 {
                    if let Some(&first) = out[used - 1].first() {
                        out[used - 1].push(first);
                    }
                }
            }
            _ => break,
        }
    }
    for v in out.iter_mut().skip(used) {
        v.clear();
    }
    out.truncate(used);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_example_geometries() {
        let mut g = Vec::new();
        // a point at (25, 17)
        geometry(&[9, 50, 34], &mut g);
        assert_eq!(g, vec![vec![[25, 17]]]);
        // a line (2,2) -> (2,10) -> (10,10)
        geometry(&[9, 4, 4, 18, 0, 16, 16, 0], &mut g);
        assert_eq!(g, vec![vec![[2, 2], [2, 10], [10, 10]]]);
        // a polygon ring (3,6) (8,12) (20,34), closed: the first point comes back at the end
        geometry(&[9, 6, 12, 18, 10, 12, 24, 44, 15], &mut g);
        assert_eq!(g, vec![vec![[3, 6], [8, 12], [20, 34], [3, 6]]]);
        // multi-point: two MoveTos
        geometry(&[17, 10, 14, 3, 9], &mut g);
        assert_eq!(g, vec![vec![[5, 7]], vec![[3, 2]]]);
    }

    #[test]
    fn values_by_wire_type() {
        assert_eq!(
            value(&[0x0a, 2, b'h', b'i']).unwrap(),
            Value::String("hi".into())
        );
        assert_eq!(value(&[0x30, 3]).unwrap(), Value::Int(-2)); // sint64 zigzag 3 = -2
        assert_eq!(value(&[0x38, 1]).unwrap(), Value::Bool(true));
        assert_eq!(value(&[0x20, 7]).unwrap(), Value::Int(7));
    }
}
