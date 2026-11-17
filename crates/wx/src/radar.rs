//! A radar mosaic reduced for the GPU: reflectivity quantized to one byte per cell.
//!
//! The grid is thinned by an integer factor keeping the strongest echo of each block (thinning by
//! averaging would wash out a storm's core, the part that matters).
//!
//! Byte code: 0 = nothing to draw (no coverage, or below [`MIN_DBZ`]); otherwise
//! `1 + round((dBZ - MIN_DBZ) * 2)`, capped at 255: half-dBZ steps from 5 to 132 dBZ.

use crate::grib::Grid;

/// Echoes weaker than this are not drawn (the usual radar display floor).
pub const MIN_DBZ: f32 = 5.0;

/// A reduced mosaic: the grid of [`Grid`], thinned, with one byte code per cell.
#[derive(Debug, Clone, PartialEq)]
pub struct Radar {
    /// Cells per row (west to east).
    pub ni: usize,
    /// Rows (north to south).
    pub nj: usize,
    /// Latitude of row 0's cell centres, degrees north.
    pub lat0: f64,
    /// Longitude of column 0's cell centres, degrees east.
    pub lon0: f64,
    /// Row spacing in degrees.
    pub dlat: f64,
    /// Column spacing in degrees.
    pub dlon: f64,
    /// `ni * nj` byte codes (see the module), row by row from row 0.
    pub codes: Vec<u8>,
}

/// The byte code of a reflectivity in dBZ: 0 below [`MIN_DBZ`], for NaN and for the fill values.
///
/// # Examples
///
/// ```
/// use wx::radar::{code, dbz};
///
/// assert_eq!(code(-999.0), 0); // the no-data fill value
/// assert_eq!(code(5.0), 1);
/// assert_eq!(dbz(code(42.5)), Some(42.5)); // half-dBZ steps survive the round trip
/// assert_eq!(dbz(0), None);
/// ```
pub fn code(dbz: f32) -> u8 {
    if dbz.is_nan() || dbz < MIN_DBZ {
        return 0; // also the -999 / -99 fill values
    }
    (1.0 + ((dbz - MIN_DBZ) * 2.0).round()).min(255.0) as u8
}

/// The reflectivity in dBZ a byte code stands for, to the nearest half dBZ; `None` for 0.
pub fn dbz(code: u8) -> Option<f32> {
    (code > 0).then(|| MIN_DBZ + (code - 1) as f32 / 2.0)
}

/// Quantize and thin by `factor` (1 = keep every cell; 0 is taken as 1).
///
/// Each output cell is the strongest code of its block. Partial blocks at the east and south edges
/// are kept. The thinned grid's origin is its first block's centre.
pub fn reduce(g: &Grid, factor: usize) -> Radar {
    let f = factor.max(1);
    let (ni, nj) = (g.ni.div_ceil(f), g.nj.div_ceil(f));
    let mut codes = vec![0u8; ni * nj];
    for j in 0..g.nj {
        let row = &g.values[j * g.ni..(j + 1) * g.ni];
        let out = &mut codes[(j / f) * ni..(j / f + 1) * ni];
        for (i, &v) in row.iter().enumerate() {
            let c = code(v);
            let o = &mut out[i / f];
            if c > *o {
                *o = c;
            }
        }
    }
    // the centre of a block of f cells is (f - 1) / 2 cells in from its first cell
    let shift = (f as f64 - 1.0) / 2.0;
    Radar {
        ni,
        nj,
        lat0: g.lat0 - shift * g.dlat,
        lon0: g.lon0 + shift * g.dlon,
        dlat: g.dlat * f as f64,
        dlon: g.dlon * f as f64,
        codes,
    }
}

/// The MRMS directory listing's frame files, oldest first, with their times (seconds since the
/// epoch).
///
/// A frame file is named like `MRMS_MergedBaseReflectivityQC_00.50_20261116-195611.grib2.gz`. Other
/// links are ignored.
pub fn frames_in_index(html: &str) -> Vec<(String, i64)> {
    const PREFIX: &str = "MRMS_MergedBaseReflectivityQC_00.50_";
    const SUFFIX: &str = ".grib2.gz";
    let mut out: Vec<(String, i64)> = html
        .split('"')
        .filter(|s| {
            s.starts_with(PREFIX)
                && s.ends_with(SUFFIX)
                && s.len() == PREFIX.len() + 15 + SUFFIX.len()
        })
        .filter_map(|s| {
            let t = &s[PREFIX.len()..PREFIX.len() + 15]; // 20261116-195611
            let iso = format!(
                "{}-{}-{}T{}:{}:{}Z",
                &t[0..4],
                &t[4..6],
                &t[6..8],
                &t[9..11],
                &t[11..13],
                &t[13..15]
            );
            Some((s.to_string(), crate::hazards::parse_time(&iso)?))
        })
        .collect();
    out.sort_by_key(|f| f.1);
    out.dedup();
    out
}

/// Gunzip, refusing output past `max` bytes.
///
/// A frame is ~0.7 MB; a corrupt or hostile stream must not fill memory.
///
/// # Errors
///
/// The stream is not valid gzip, or it unpacks to more than `max` bytes.
pub fn gunzip(gz: &[u8], max: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(gz)
        .take(max as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("gunzip: {e}"))?;
    if out.len() > max {
        return Err(format!("gunzip: more than {max} bytes"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_from_a_directory_listing() {
        let html = r#"<a href="?C=N;O=D">Name</a> <a href="MRMS_MergedBaseReflectivityQC_00.50_20261116-195611.grib2.gz">x</a>
<a href="MRMS_MergedBaseReflectivityQC_00.50_20261116-194958.grib2.gz">x</a>
<a href="MRMS_MergedBaseReflectivityQC_00.50.latest.grib2.gz">latest</a>
<a href="MRMS_MergedBaseReflectivityQC_00.50_20261116-199999.grib2.gz">bad time</a>
<a href="MRMS_MergedBaseReflectivityQC_00.50_20261116-194958.grib2.gz">again</a>"#;
        let f = frames_in_index(html);
        assert_eq!(
            f.iter().map(|x| &x.0[36..51]).collect::<Vec<_>>(),
            vec!["20261116-194958", "20261116-195611"]
        );
        assert_eq!(f[1].1 - f[0].1, 6 * 60 + 13);
    }

    #[test]
    fn gunzip_with_a_ceiling() {
        use std::io::Write;
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&[7u8; 1000]).unwrap();
        let gz = e.finish().unwrap();
        assert_eq!(gunzip(&gz, 1000).unwrap().len(), 1000);
        assert!(gunzip(&gz, 999).unwrap_err().contains("more than 999"));
        assert!(gunzip(b"not gzip", 10).is_err());
    }

    fn grid(ni: usize, nj: usize, values: Vec<f32>) -> Grid {
        Grid {
            ni,
            nj,
            lat0: 50.0,
            lon0: -120.0,
            dlat: 0.01,
            dlon: 0.01,
            param: (209, 15, 0),
            level: (0, 0),
            values,
        }
    }

    #[test]
    fn codes_round_trip_in_half_dbz_steps() {
        assert_eq!(code(5.0), 1);
        assert_eq!(code(5.5), 2);
        assert_eq!(code(35.0), 61);
        assert_eq!(dbz(61), Some(35.0));
        assert_eq!(code(500.0), 255);
        assert_eq!(dbz(0), None);
    }

    #[test]
    fn below_the_floor_missing_and_nan_draw_nothing() {
        for v in [4.9, 0.0, -30.0, -99.0, -999.0, f32::NAN] {
            assert_eq!(code(v), 0, "{v}");
        }
    }

    #[test]
    fn thinning_keeps_the_strongest_echo_of_each_block() {
        // 3 x 3 thinned by 2: blocks {0,1}x{0,1}, {2}x{0,1}, {0,1}x{2}, {2}x{2}
        let g = grid(
            3,
            3,
            vec![10.0, 40.0, -999.0, 5.0, 20.0, 60.0, 0.0, 30.0, 7.0],
        );
        let r = reduce(&g, 2);
        assert_eq!((r.ni, r.nj), (2, 2));
        assert_eq!(r.codes, vec![code(40.0), code(60.0), code(30.0), code(7.0)]);
    }

    #[test]
    fn thinned_grid_is_georeferenced_at_block_centres() {
        let r = reduce(&grid(4, 4, vec![0.0; 16]), 2);
        assert!(
            (r.lat0 - 49.995).abs() < 1e-9 && (r.lon0 - -119.995).abs() < 1e-9,
            "{} {}",
            r.lat0,
            r.lon0
        );
        assert!((r.dlat - 0.02).abs() < 1e-12 && (r.dlon - 0.02).abs() < 1e-12);
        let same = reduce(&grid(2, 1, vec![12.0, 0.0]), 1);
        assert_eq!(
            (same.ni, same.nj, same.lat0, same.lon0),
            (2, 1, 50.0, -120.0)
        );
        assert_eq!(same.codes, vec![code(12.0), 0]);
    }
}
