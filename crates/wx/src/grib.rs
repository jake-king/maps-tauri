//! Just enough GRIB2 to read NOAA's MRMS radar mosaics: one message, a regular latitude/longitude
//! grid (template 3.0) scanned west to east and north to south, values packed as a PNG image
//! (template 5.41), no bitmap. Anything else is refused with an error naming what was found, so a
//! format change upstream shows up as a message rather than as a wrong picture.

use std::fmt;
use std::io::Cursor;

/// The largest grid accepted. MRMS CONUS is 7000 x 3500 = 24.5 M points; a corrupt header must not
/// make the reader allocate gigabytes.
pub const MAX_POINTS: u64 = 64_000_000;

/// A decoded field on a regular latitude/longitude grid. Row 0 is the northernmost; within a row
/// longitude increases eastward.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    pub ni: usize,
    pub nj: usize,
    /// latitude of row 0 and longitude of column 0 (degrees, longitude in -180..180)
    pub lat0: f64,
    pub lon0: f64,
    /// grid spacing in degrees (positive)
    pub dlat: f64,
    pub dlon: f64,
    /// discipline, parameter category and parameter number (MRMS reflectivity is 209/11/0 or 0/...)
    pub param: (u8, u8, u8),
    pub values: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "grib2: {}", self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error(msg.into()))
}

fn u16_at(b: &[u8], i: usize) -> Result<u16, Error> {
    b.get(i..i + 2)
        .map(|s| u16::from_be_bytes([s[0], s[1]]))
        .ok_or_else(|| Error(format!("short read at {i}")))
}

fn u32_at(b: &[u8], i: usize) -> Result<u32, Error> {
    b.get(i..i + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| Error(format!("short read at {i}")))
}

/// GRIB2 signed integers are sign and magnitude, not two's complement.
fn i32_sm_at(b: &[u8], i: usize) -> Result<i32, Error> {
    let v = u32_at(b, i)?;
    let m = (v & 0x7fff_ffff) as i32;
    Ok(if v & 0x8000_0000 != 0 { -m } else { m })
}

fn i16_sm_at(b: &[u8], i: usize) -> Result<i16, Error> {
    let v = u16_at(b, i)?;
    let m = (v & 0x7fff) as i16;
    Ok(if v & 0x8000 != 0 { -m } else { m })
}

fn byte_at(b: &[u8], i: usize) -> Result<u8, Error> {
    b.get(i)
        .copied()
        .ok_or_else(|| Error(format!("short read at {i}")))
}

struct GridDef {
    ni: usize,
    nj: usize,
    lat0: f64,
    lon0: f64,
    dlat: f64,
    dlon: f64,
}

struct Packing {
    points: u64,
    r: f32,
    e: i16,
    d: i16,
    bits: u8,
}

/// Decode the first (and only expected) message of a GRIB2 file.
pub fn decode(b: &[u8]) -> Result<Grid, Error> {
    if b.get(0..4) != Some(b"GRIB") {
        return err("not a GRIB file (no GRIB magic)");
    }
    let edition = byte_at(b, 7)?;
    if edition != 2 {
        return err(format!("edition {edition}, only 2 is read"));
    }
    let discipline = byte_at(b, 6)?;
    let mut grid: Option<GridDef> = None;
    let mut packing: Option<Packing> = None;
    let mut param = None;
    let mut bitmap = None;
    let mut data: Option<&[u8]> = None;
    let mut i = 16;
    loop {
        if b.get(i..i + 4) == Some(b"7777") {
            break;
        }
        let len = u32_at(b, i)? as usize;
        let sec = byte_at(b, i + 4)?;
        if len < 5 || i + len > b.len() {
            return err(format!(
                "section {sec} at {i} has length {len}, past the end ({})",
                b.len()
            ));
        }
        let s = &b[i..i + len];
        match sec {
            1 | 2 => {}
            3 => {
                let tmpl = u16_at(s, 12)?;
                if tmpl != 0 {
                    return err(format!(
                        "grid template 3.{tmpl}, only 3.0 (latitude/longitude) is read"
                    ));
                }
                let ni = u32_at(s, 30)? as usize;
                let nj = u32_at(s, 34)? as usize;
                let la1 = i32_sm_at(s, 46)? as f64 / 1e6;
                let lo1 = u32_at(s, 50)? as f64 / 1e6;
                let di = u32_at(s, 63)? as f64 / 1e6;
                let dj = u32_at(s, 67)? as f64 / 1e6;
                let scan = byte_at(s, 71)?;
                if scan != 0 {
                    return err(format!(
                        "scanning mode {scan:#010b}, only 0 (west to east, north to south) is read"
                    ));
                }
                if ni == 0 || nj == 0 || (ni as u64) * (nj as u64) > MAX_POINTS {
                    return err(format!(
                        "grid {ni} x {nj} is empty or larger than {MAX_POINTS} points"
                    ));
                }
                if !(di > 0.0 && dj > 0.0) {
                    return err(format!("grid spacing {di} x {dj}"));
                }
                let lon0 = if lo1 > 180.0 { lo1 - 360.0 } else { lo1 };
                grid = Some(GridDef {
                    ni,
                    nj,
                    lat0: la1,
                    lon0,
                    dlat: dj,
                    dlon: di,
                });
            }
            4 => {
                param = Some((discipline, byte_at(s, 9)?, byte_at(s, 10)?));
            }
            5 => {
                let tmpl = u16_at(s, 9)?;
                if tmpl != 41 {
                    return err(format!("data template 5.{tmpl}, only 5.41 (PNG) is read"));
                }
                let r = f32::from_bits(u32_at(s, 11)?);
                packing = Some(Packing {
                    points: u32_at(s, 5)? as u64,
                    r,
                    e: i16_sm_at(s, 15)?,
                    d: i16_sm_at(s, 17)?,
                    bits: byte_at(s, 19)?,
                });
            }
            6 => bitmap = Some(byte_at(s, 5)?),
            7 => data = Some(&s[5..]),
            other => return err(format!("unexpected section {other} at {i}")),
        }
        i += len;
        if data.is_some() {
            // one message per file: what follows the data section must be the end marker
            if b.get(i..i + 4) != Some(b"7777") {
                return err("more than one field in the message");
            }
            break;
        }
    }
    let g = grid.ok_or_else(|| Error("no grid definition (section 3)".into()))?;
    let p = packing.ok_or_else(|| Error("no data representation (section 5)".into()))?;
    let param = param.ok_or_else(|| Error("no product definition (section 4)".into()))?;
    match bitmap {
        Some(255) => {}
        Some(other) => return err(format!("bitmap indicator {other}, only 255 (none) is read")),
        None => return err("no bitmap section (section 6)"),
    }
    let data = data.ok_or_else(|| Error("no data section (section 7)".into()))?;
    let n = (g.ni as u64) * (g.nj as u64);
    if p.points != n {
        return err(format!(
            "{} data points for a {} x {} grid",
            p.points, g.ni, g.nj
        ));
    }
    if p.bits != 8 && p.bits != 16 {
        return err(format!("{} bits per value, only 8 or 16 are read", p.bits));
    }
    let raw = png_samples(data, g.ni, g.nj, p.bits)?;
    // value = (R + X * 2^E) / 10^D
    let scale = 2f32.powi(p.e as i32);
    let div = 10f32.powi(p.d as i32);
    let values = raw
        .into_iter()
        .map(|x| (p.r + x as f32 * scale) / div)
        .collect();
    Ok(Grid {
        ni: g.ni,
        nj: g.nj,
        lat0: g.lat0,
        lon0: g.lon0,
        dlat: g.dlat,
        dlon: g.dlon,
        param,
        values,
    })
}

fn png_samples(data: &[u8], ni: usize, nj: usize, bits: u8) -> Result<Vec<u32>, Error> {
    let limits = png::Limits {
        bytes: ni * nj * 2 + 1024,
    };
    let decoder = png::Decoder::new_with_limits(Cursor::new(data), limits);
    let mut reader = decoder
        .read_info()
        .map_err(|e| Error(format!("png: {e}")))?;
    let info = reader.info();
    if info.width as usize != ni || info.height as usize != nj {
        return err(format!(
            "png is {} x {}, grid is {ni} x {nj}",
            info.width, info.height
        ));
    }
    if info.color_type != png::ColorType::Grayscale || info.bit_depth as u8 != bits {
        return err(format!(
            "png is {:?} at {:?} bits, expected grayscale at {bits}",
            info.color_type, info.bit_depth
        ));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| Error("png: output too large".into()))?;
    let mut buf = vec![0u8; size];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| Error(format!("png: {e}")))?;
    let buf = &buf[..out.buffer_size()];
    Ok(if bits == 16 {
        buf.as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c) as u32)
            .collect()
    } else {
        buf.iter().map(|&x| x as u32).collect()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Encode samples as a grayscale PNG at `bits` per value.
    fn png(ni: usize, nj: usize, bits: u8, samples: &[u32]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, ni as u32, nj as u32);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(if bits == 16 {
                png::BitDepth::Sixteen
            } else {
                png::BitDepth::Eight
            });
            let mut w = enc.write_header().unwrap();
            let bytes: Vec<u8> = if bits == 16 {
                samples
                    .iter()
                    .flat_map(|&x| (x as u16).to_be_bytes())
                    .collect()
            } else {
                samples.iter().map(|&x| x as u8).collect()
            };
            w.write_image_data(&bytes).unwrap();
        }
        out
    }

    fn section(num: u8, body: &[u8]) -> Vec<u8> {
        let mut s = ((body.len() + 5) as u32).to_be_bytes().to_vec();
        s.push(num);
        s.extend_from_slice(body);
        s
    }

    fn sm32(v: i32) -> [u8; 4] {
        let m = v.unsigned_abs() & 0x7fff_ffff;
        (if v < 0 { m | 0x8000_0000 } else { m }).to_be_bytes()
    }

    fn sm16(v: i16) -> [u8; 2] {
        let m = v.unsigned_abs() & 0x7fff;
        (if v < 0 { m | 0x8000 } else { m }).to_be_bytes()
    }

    /// What can be varied in a synthetic message, defaulting to MRMS's layout.
    pub(crate) struct Spec {
        pub ni: usize,
        pub nj: usize,
        pub lat0: f64,
        pub lon0_east: f64,
        pub d: f64,
        pub r: f32,
        pub e: i16,
        pub dd: i16,
        pub bits: u8,
        pub grid_tmpl: u16,
        pub data_tmpl: u16,
        pub scan: u8,
        pub bitmap: u8,
        pub samples: Vec<u32>,
    }

    impl Spec {
        pub(crate) fn mrms(ni: usize, nj: usize, samples: Vec<u32>) -> Spec {
            Spec {
                ni,
                nj,
                lat0: 54.995,
                lon0_east: 230.005,
                d: 0.01,
                r: -9990.0,
                e: 0,
                dd: 1,
                bits: 16,
                grid_tmpl: 0,
                data_tmpl: 41,
                scan: 0,
                bitmap: 255,
                samples,
            }
        }
    }

    pub(crate) fn message(s: &Spec) -> Vec<u8> {
        let mut sec3 = vec![0u8; 67]; // body: section bytes 6..72
        sec3[0] = 0; // source of grid definition
        sec3[1..5].copy_from_slice(&((s.ni * s.nj) as u32).to_be_bytes());
        sec3[7..9].copy_from_slice(&s.grid_tmpl.to_be_bytes());
        // the body starts at section byte 5, so section offset k is body index k - 5
        sec3[30 - 5..34 - 5].copy_from_slice(&(s.ni as u32).to_be_bytes());
        sec3[34 - 5..38 - 5].copy_from_slice(&(s.nj as u32).to_be_bytes());
        sec3[46 - 5..50 - 5].copy_from_slice(&sm32((s.lat0 * 1e6).round() as i32));
        sec3[50 - 5..54 - 5].copy_from_slice(&(((s.lon0_east * 1e6).round()) as u32).to_be_bytes());
        sec3[63 - 5..67 - 5].copy_from_slice(&(((s.d * 1e6).round()) as u32).to_be_bytes());
        sec3[67 - 5..71 - 5].copy_from_slice(&(((s.d * 1e6).round()) as u32).to_be_bytes());
        sec3[71 - 5] = s.scan;
        let mut sec4 = vec![0u8; 29];
        sec4[9 - 5] = 15; // category
        sec4[10 - 5] = 0; // parameter
        let mut sec5 = vec![0u8; 16];
        sec5[0..4].copy_from_slice(&((s.ni * s.nj) as u32).to_be_bytes());
        sec5[4..6].copy_from_slice(&s.data_tmpl.to_be_bytes());
        sec5[6..10].copy_from_slice(&s.r.to_bits().to_be_bytes());
        sec5[10..12].copy_from_slice(&sm16(s.e));
        sec5[12..14].copy_from_slice(&sm16(s.dd));
        sec5[14] = s.bits;
        let mut body = Vec::new();
        body.extend(section(1, &[0u8; 16]));
        body.extend(section(3, &sec3));
        body.extend(section(4, &sec4));
        body.extend(section(5, &sec5));
        body.extend(section(6, &[s.bitmap]));
        body.extend(section(7, &png(s.ni, s.nj, s.bits, &s.samples)));
        let total = 16 + body.len() + 4;
        let mut m = b"GRIB".to_vec();
        m.extend_from_slice(&[0, 0, 209, 2]);
        m.extend_from_slice(&(total as u64).to_be_bytes());
        m.extend(body);
        m.extend_from_slice(b"7777");
        m
    }

    #[test]
    fn decodes_an_mrms_shaped_message() {
        // raw 9990 -> 0 dBZ, 10340 -> 35 dBZ, 9991 -> 0.1 dBZ, 0 -> -999 (no coverage)
        let g = decode(&message(&Spec::mrms(
            3,
            2,
            vec![9990, 10340, 9991, 0, 10000, 9990],
        )))
        .unwrap();
        assert_eq!((g.ni, g.nj), (3, 2));
        assert_eq!(g.param, (209, 15, 0));
        assert!(
            (g.lat0 - 54.995).abs() < 1e-9 && (g.lon0 - -129.995).abs() < 1e-9,
            "{} {}",
            g.lat0,
            g.lon0
        );
        assert!((g.dlat - 0.01).abs() < 1e-12 && (g.dlon - 0.01).abs() < 1e-12);
        let want = [0.0, 35.0, 0.1, -999.0, 1.0, 0.0];
        for (v, w) in g.values.iter().zip(want) {
            assert!((v - w).abs() < 1e-4, "{:?}", g.values);
        }
    }

    #[test]
    fn binary_and_decimal_scale_factors() {
        // value = (R + X * 2^E) / 10^D with R = 1, E = 2, D = -1: X = 3 -> (1 + 12) * 10 = 130
        let mut s = Spec::mrms(1, 1, vec![3]);
        (s.r, s.e, s.dd) = (1.0, 2, -1);
        assert_eq!(decode(&message(&s)).unwrap().values, vec![130.0]);
        (s.r, s.e, s.dd) = (1.0, -1, 0);
        assert_eq!(decode(&message(&s)).unwrap().values, vec![2.5]);
    }

    #[test]
    fn eight_bit_samples() {
        let mut s = Spec::mrms(2, 1, vec![7, 200]);
        (s.bits, s.r, s.dd) = (8, 0.0, 0);
        assert_eq!(decode(&message(&s)).unwrap().values, vec![7.0, 200.0]);
    }

    #[test]
    fn western_longitudes_and_southern_latitudes() {
        let mut s = Spec::mrms(1, 1, vec![9990]);
        (s.lat0, s.lon0_east) = (-33.5, 151.25);
        let g = decode(&message(&s)).unwrap();
        assert_eq!((g.lat0, g.lon0), (-33.5, 151.25));
    }

    fn refused(s: &Spec, needle: &str) {
        match decode(&message(s)) {
            Err(Error(m)) => assert!(m.contains(needle), "{m:?} does not mention {needle:?}"),
            Ok(_) => panic!("accepted; expected an error about {needle}"),
        }
    }

    #[test]
    fn refuses_what_it_does_not_read() {
        let base = || Spec::mrms(2, 2, vec![9990; 4]);
        let mut s = base();
        s.grid_tmpl = 30;
        refused(&s, "grid template 3.30");
        let mut s = base();
        s.data_tmpl = 40;
        refused(&s, "data template 5.40");
        let mut s = base();
        s.scan = 0b0100_0000;
        refused(&s, "scanning mode");
        let mut s = base();
        s.bitmap = 0;
        refused(&s, "bitmap indicator 0");
        let mut s = base();
        s.bits = 12;
        refused(&s, "12 bits");
    }

    #[test]
    fn refuses_a_png_of_the_wrong_size() {
        let mut m = message(&Spec::mrms(2, 2, vec![9990; 4]));
        // claim a 4 x 1 grid over the same 2 x 2 image: ni at section-3 offset 30
        let s3 = m
            .windows(5)
            .position(|w| w[4] == 3 && u32::from_be_bytes([w[0], w[1], w[2], w[3]]) == 72)
            .unwrap();
        m[s3 + 30..s3 + 34].copy_from_slice(&4u32.to_be_bytes());
        m[s3 + 34..s3 + 38].copy_from_slice(&1u32.to_be_bytes());
        match decode(&m) {
            Err(Error(e)) => assert!(e.contains("png is 2 x 2, grid is 4 x 1"), "{e}"),
            Ok(_) => panic!("accepted"),
        }
    }

    #[test]
    fn refuses_garbage_and_truncation_without_panicking() {
        assert!(decode(b"").is_err());
        assert!(decode(b"GRIB").is_err());
        assert!(decode(b"NOPE0000000000000000")
            .unwrap_err()
            .0
            .contains("magic"));
        let m = message(&Spec::mrms(3, 3, vec![10000; 9]));
        for cut in 0..m.len() {
            assert!(
                decode(&m[..cut]).is_err(),
                "a message cut at {cut} of {} decoded",
                m.len()
            );
        }
    }

    #[test]
    fn refuses_a_huge_grid_before_allocating() {
        let mut m = message(&Spec::mrms(2, 2, vec![9990; 4]));
        let s3 = m
            .windows(5)
            .position(|w| w[4] == 3 && u32::from_be_bytes([w[0], w[1], w[2], w[3]]) == 72)
            .unwrap();
        m[s3 + 30..s3 + 34].copy_from_slice(&100_000u32.to_be_bytes());
        m[s3 + 34..s3 + 38].copy_from_slice(&100_000u32.to_be_bytes());
        assert!(decode(&m).unwrap_err().0.contains("larger than"));
    }

    /// A real frame, when one has been downloaded: WX_FRAME=path/to/MRMS_...grib2 (not gzipped).
    #[test]
    fn a_real_mrms_frame() {
        let Ok(path) = std::env::var("WX_FRAME") else {
            return;
        };
        let g = decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!((g.ni, g.nj), (7000, 3500));
        assert!((g.lat0 - 54.995).abs() < 1e-6 && (g.lon0 - -129.995).abs() < 1e-6);
        let echo = g.values.iter().filter(|&&v| v >= 5.0).count();
        let missing = g.values.iter().filter(|&&v| v <= -999.0).count();
        let max = g.values.iter().cloned().fold(f32::MIN, f32::max);
        eprintln!("echo >= 5 dBZ: {echo}, no coverage: {missing}, max {max} dBZ");
        assert!(max < 100.0, "max {max} dBZ is not reflectivity");
        assert!(echo > 0 && echo < g.values.len() / 2);
    }
}
