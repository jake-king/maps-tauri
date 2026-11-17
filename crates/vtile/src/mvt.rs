//! Mapbox Vector Tile v2 decoding.
//!
//! A tile is protobuf messages Tile > Layer > Feature, and a feature's geometry is a stream of
//! `MoveTo` / `LineTo` / `ClosePath` commands with zigzag-encoded deltas.
//! Spec: <https://github.com/mapbox/vector-tile-spec/tree/master/2.1>

use std::io;

use crate::pmtiles::varint;

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// A property value from a layer's value table.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `string_value`.
    String(String),
    /// `float_value`.
    Float(f32),
    /// `double_value`.
    Double(f64),
    /// `int_value`, `uint_value` (reinterpreted as `i64`) or zigzag-decoded `sint_value`.
    Int(i64),
    /// `bool_value`; also what a value message with no recognised field decodes to (`false`).
    Bool(bool),
}

impl Value {
    /// The string, if this is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    /// The number as `f64`, for [`Value::Float`], [`Value::Double`] and [`Value::Int`].
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Float(v) => Some(v as f64),
            Value::Double(v) => Some(v),
            Value::Int(v) => Some(v as f64),
            _ => None,
        }
    }
}

/// A feature's geometry type, as numbered in the spec ([`Feature::geom_type`] holds the number).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeomType {
    /// Unknown or unset.
    Unknown = 0,
    /// One or more points.
    Point = 1,
    /// One or more lines.
    LineString = 2,
    /// One or more polygons, as rings.
    Polygon = 3,
}

/// One feature of a layer, its geometry still encoded.
#[derive(Debug, Default)]
pub struct Feature {
    /// The feature id (0 when the tile gives none).
    pub id: u64,
    /// The geometry type number, a [`GeomType`] value (unchecked).
    pub geom_type: u8,
    /// pairs of (key index, value index) into the layer's tables
    pub tags: Vec<u32>,
    /// the raw command stream
    pub geometry: Vec<u32>,
}

/// One layer of a tile: its features and the key and value tables their tags index.
#[derive(Debug)]
pub struct Layer {
    /// The layer name (`water`, `roads`, ...).
    pub name: String,
    /// Tile coordinates per tile side (4096 when the tile gives none).
    pub extent: u32,
    /// Property names, indexed by the first of each of a feature's tag pairs.
    pub keys: Vec<String>,
    /// Property values, indexed by the second of each tag pair.
    pub values: Vec<Value>,
    /// The features, in tile order.
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
            // a declared length past the buffer, however large, is truncation (and may not overflow)
            let s = p
                .checked_add(n)
                .and_then(|end| b.get(*p..end))
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
///
/// # Errors
///
/// `InvalidData` when the protobuf is malformed: a truncated varint or field, a varint longer
/// than 10 bytes, an unsupported wire type, or a string that is not UTF-8.
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

/// A feature's geometry as point lists in tile coordinates, one per `MoveTo`.
///
/// `ClosePath` appends the ring's first point again, so a polygon ring comes back closed, exactly
/// as `@mapbox/vector-tile`'s `loadGeometry` returns it (checked over every tile by
/// `tools/mvt_diff.mjs`). `out` is cleared and reused, so a caller decoding many features
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
                    // wrapping: a corrupt delta past i32 gives a wrong point, never a panic
                    x = x.wrapping_add(zz(dx));
                    y = y.wrapping_add(zz(dy));
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

    // ---- builders: a protobuf writer just big enough for hand-made tiles ----

    fn enc_varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn key(field: u64, wire: u64, out: &mut Vec<u8>) {
        enc_varint(field << 3 | wire, out);
    }
    fn vfield(field: u64, v: u64, out: &mut Vec<u8>) {
        key(field, 0, out);
        enc_varint(v, out);
    }
    fn bfield(field: u64, b: &[u8], out: &mut Vec<u8>) {
        key(field, 2, out);
        enc_varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn packed(v: &[u32]) -> Vec<u8> {
        let mut b = Vec::new();
        for &x in v {
            enc_varint(x.into(), &mut b);
        }
        b
    }

    fn err_text<T>(r: io::Result<T>) -> String {
        let e = r.err().expect("expected an error");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e}");
        e.to_string()
    }

    /// A tile with one layer "roads" (extent 512, version 2), two features, and values of every
    /// kind; plus unknown fields at each level that the decoder must skip.
    fn hand_tile() -> Vec<u8> {
        let mut f1 = Vec::new();
        vfield(1, 42, &mut f1); // id
        bfield(2, &packed(&[0, 0, 1, 1]), &mut f1); // kind=primary, lanes=2
        vfield(3, 2, &mut f1); // LineString
        bfield(4, &packed(&[9, 4, 4, 18, 0, 16, 16, 0]), &mut f1);
        vfield(9, 7, &mut f1); // unknown: skipped
        let mut f2 = Vec::new();
        bfield(2, &packed(&[2, 2, 3, 3, 4, 4, 5, 5]), &mut f2);
        vfield(3, 1, &mut f2); // Point
        bfield(4, &packed(&[9, 50, 34]), &mut f2);

        let mut vals: Vec<Vec<u8>> = Vec::new();
        let mut v = Vec::new();
        bfield(1, b"primary", &mut v);
        vals.push(v);
        let mut v = Vec::new();
        vfield(5, 2, &mut v); // uint
        vals.push(v);
        let mut v = Vec::new();
        key(2, 5, &mut v); // float
        v.extend_from_slice(&1.5f32.to_bits().to_le_bytes());
        vals.push(v);
        let mut v = Vec::new();
        key(3, 1, &mut v); // double
        v.extend_from_slice(&(-0.25f64).to_bits().to_le_bytes());
        vals.push(v);
        let mut v = Vec::new();
        vfield(6, 5, &mut v); // sint: zigzag 5 = -3
        vals.push(v);
        let mut v = Vec::new();
        vfield(7, 0, &mut v); // bool false
        vals.push(v);

        let mut l = Vec::new();
        vfield(15, 2, &mut l); // version: not kept, skipped
        bfield(1, b"roads", &mut l);
        bfield(2, &f1, &mut l);
        bfield(2, &f2, &mut l);
        for k in ["kind", "lanes", "width", "ratio", "delta", "lit"] {
            bfield(3, k.as_bytes(), &mut l);
        }
        for v in &vals {
            bfield(4, v, &mut l);
        }
        vfield(5, 512, &mut l);

        let mut l2 = Vec::new();
        bfield(1, b"water", &mut l2);

        let mut t = Vec::new();
        vfield(1, 99, &mut t); // unknown tile field: skipped
        bfield(7, b"junk", &mut t); // unknown bytes field: skipped, not read as a layer
        bfield(3, &l, &mut t);
        bfield(3, &l2, &mut t);
        t
    }

    // ---- a whole tile ----

    #[test]
    fn hand_built_tile_decodes_to_exact_values() {
        let ls = decode(&hand_tile()).unwrap();
        assert_eq!(ls.len(), 2);
        let l = &ls[0];
        assert_eq!(l.name, "roads");
        assert_eq!(l.extent, 512);
        assert_eq!(l.keys, ["kind", "lanes", "width", "ratio", "delta", "lit"]);
        assert_eq!(
            l.values,
            vec![
                Value::String("primary".into()),
                Value::Int(2),
                Value::Float(1.5),
                Value::Double(-0.25),
                Value::Int(-3),
                Value::Bool(false),
            ]
        );
        assert_eq!(l.features.len(), 2);
        let f = &l.features[0];
        assert_eq!((f.id, f.geom_type), (42, GeomType::LineString as u8));
        assert_eq!(f.tags, [0, 0, 1, 1]);
        assert_eq!(f.geometry, [9, 4, 4, 18, 0, 16, 16, 0]);
        let mut g = Vec::new();
        geometry(&f.geometry, &mut g);
        assert_eq!(g, vec![vec![[2, 2], [2, 10], [10, 10]]]);
        let f = &l.features[1];
        assert_eq!((f.id, f.geom_type), (0, GeomType::Point as u8));
        assert_eq!(f.geometry, [9, 50, 34]);
    }

    #[test]
    fn layer_props_resolve_through_the_key_and_value_tables() {
        let ls = decode(&hand_tile()).unwrap();
        let (l, f1, f2) = (&ls[0], &ls[0].features[0], &ls[0].features[1]);
        assert_eq!(l.prop(f1, "kind"), Some(&Value::String("primary".into())));
        assert_eq!(l.prop(f1, "lanes"), Some(&Value::Int(2)));
        assert_eq!(l.prop(f1, "width"), None); // a key the feature has no tag for
        assert_eq!(l.prop(f1, "nope"), None); // a key the layer does not have
        assert_eq!(l.prop(f2, "width"), Some(&Value::Float(1.5)));
        assert_eq!(l.prop(f2, "ratio"), Some(&Value::Double(-0.25)));
        assert_eq!(l.prop(f2, "delta"), Some(&Value::Int(-3)));
        assert_eq!(l.prop(f2, "lit"), Some(&Value::Bool(false)));
    }

    #[test]
    fn layer_without_an_extent_defaults_to_4096() {
        let ls = decode(&hand_tile()).unwrap();
        assert_eq!(ls[1].name, "water");
        assert_eq!(ls[1].extent, 4096);
        assert!(ls[1].features.is_empty());
    }

    #[test]
    fn empty_tile_is_no_layers() {
        assert!(decode(&[]).unwrap().is_empty());
    }

    // ---- malformed input: an error or a defined skip, never a panic ----

    #[test]
    fn truncated_varints_are_errors() {
        assert_eq!(err_text(decode(&[0x9a])), "truncated varint"); // a key cut short
        assert_eq!(err_text(decode(&[0x1a])), "truncated varint"); // a length missing
        assert_eq!(err_text(decode(&[0x08, 0x80])), "truncated varint"); // a value cut short
                                                                         // inside a feature's packed geometry
        let mut f = Vec::new();
        bfield(4, &[0x09, 0x80], &mut f);
        let mut l = Vec::new();
        bfield(2, &f, &mut l);
        let mut t = Vec::new();
        bfield(3, &l, &mut t);
        assert_eq!(err_text(decode(&t)), "truncated varint");
    }

    #[test]
    fn truncated_fixed_and_length_fields_are_errors() {
        assert_eq!(err_text(decode(&[0x1a, 5, 1, 2])), "truncated bytes field");
        assert_eq!(
            err_text(value(&[0x19, 1, 2, 3, 4, 5, 6, 7])),
            "truncated fixed64"
        );
        assert_eq!(err_text(value(&[0x15, 1, 2, 3])), "truncated fixed32");
    }

    #[test]
    fn fixed_fields_of_exact_length_decode() {
        // the boundary of the two truncation checks: exactly 8 and 4 bytes left
        assert_eq!(
            value(&[0x19, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f]).unwrap(),
            Value::Double(1.0)
        );
        assert_eq!(value(&[0x15, 0, 0, 0x80, 0x3f]).unwrap(), Value::Float(1.0));
    }

    #[test]
    fn unknown_wire_types_are_errors() {
        for wire in [3u8, 4, 6, 7] {
            assert_eq!(
                err_text(decode(&[(3 << 3) | wire, 0])),
                "unsupported wire type",
                "wire type {wire}"
            );
        }
    }

    #[test]
    fn invalid_utf8_in_a_layer_name_is_an_error() {
        let mut l = Vec::new();
        bfield(1, &[0xff, 0xfe], &mut l);
        let mut t = Vec::new();
        bfield(3, &l, &mut t);
        assert_eq!(err_text(decode(&t)), "string is not UTF-8");
    }

    #[test]
    fn tag_indices_out_of_range_give_no_value() {
        let l = Layer {
            name: "x".into(),
            extent: 4096,
            keys: vec!["a".into(), "b".into()],
            values: vec![Value::Int(1)],
            features: vec![],
        };
        // value index 9 past the table, and a trailing unpaired key index
        let f = Feature {
            tags: vec![0, 9, 1],
            ..Default::default()
        };
        assert_eq!(l.prop(&f, "a"), None);
        assert_eq!(l.prop(&f, "b"), None);
        // a key index past the keys table never matches a key
        let f = Feature {
            tags: vec![7, 0],
            ..Default::default()
        };
        assert_eq!(l.prop(&f, "a"), None);
    }

    #[test]
    fn huge_bytes_field_length_is_an_error_not_a_panic() {
        let mut t = vec![0x1a];
        enc_varint(u64::MAX, &mut t);
        assert_eq!(err_text(decode(&t)), "truncated bytes field");
    }

    // ---- values ----

    #[test]
    fn value_with_several_fields_keeps_the_last_known_one() {
        // string, then an unknown field 9 (kept as is), then int 4
        let mut v = Vec::new();
        bfield(1, b"a", &mut v);
        vfield(9, 1, &mut v);
        assert_eq!(value(&v).unwrap(), Value::String("a".into()));
        vfield(4, 4, &mut v);
        assert_eq!(value(&v).unwrap(), Value::Int(4));
        assert_eq!(value(&[]).unwrap(), Value::Bool(false)); // no field at all
    }

    #[test]
    fn value_accessors() {
        assert_eq!(Value::String("s".into()).as_str(), Some("s"));
        assert_eq!(Value::Int(3).as_str(), None);
        assert_eq!(Value::Float(0.5).as_f64(), Some(0.5));
        assert_eq!(Value::Double(-2.5).as_f64(), Some(-2.5));
        assert_eq!(Value::Int(-7).as_f64(), Some(-7.0));
        assert_eq!(Value::Bool(true).as_f64(), None);
        assert_eq!(Value::String("1".into()).as_f64(), None);
    }

    // ---- geometry on malformed command streams ----

    #[test]
    fn geometry_command_count_past_the_buffer_stops_at_the_end() {
        let mut g = Vec::new();
        // MoveTo count 3, but only one (dx, dy) pair and half of a second
        geometry(&[(3 << 3) | 1, 2, 2, 4], &mut g);
        assert_eq!(g, vec![vec![[1, 1]]]);
        // LineTo count 5 with two pairs present
        geometry(&[9, 2, 2, (5 << 3) | 2, 2, 0, 0, 2], &mut g);
        assert_eq!(g, vec![vec![[1, 1], [2, 1], [2, 2]]]);
    }

    #[test]
    fn geometry_unknown_command_ends_the_stream() {
        let mut g = Vec::new();
        geometry(&[9, 2, 2, 3, 9, 4, 4], &mut g); // id 3 is no command
        assert_eq!(g, vec![vec![[1, 1]]]);
    }

    #[test]
    fn geometry_lineto_and_closepath_before_any_moveto_draw_nothing() {
        let mut g = vec![vec![[9, 9]]];
        geometry(&[15, 10, 2, 2], &mut g);
        assert_eq!(g, Vec::<Vec<[i32; 2]>>::new());
    }

    #[test]
    fn geometry_reuses_and_shrinks_its_output() {
        let mut g = vec![vec![[1, 1]; 5], vec![[2, 2]; 3], vec![[3, 3]]];
        geometry(&[9, 50, 34], &mut g);
        assert_eq!(g, vec![vec![[25, 17]]]);
    }

    #[test]
    fn geometry_coordinate_overflow_does_not_panic() {
        let mut g = Vec::new();
        // MoveTo count 2, each dx = zigzag(0xfffffffe) = i32::MAX
        geometry(&[(2 << 3) | 1, 0xffff_fffe, 0, 0xffff_fffe, 0], &mut g);
        assert_eq!(g.len(), 2);
    }
}
