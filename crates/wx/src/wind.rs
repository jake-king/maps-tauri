//! Winds aloft from NOAA's GFS model: U and V at one pressure level for the whole world.
//!
//! They are cut out of a run by NOMADS's GRIB filter (0.5°, 720 x 361 points, about 0.7 MB a
//! level). Asked for as a subregion, even of the whole globe, NOMADS repacks the field simply
//! (template 5.0), which [`crate::grib`] reads; the plain global file uses complex packing (5.3),
//! which it does not.
//!
//! A run starts every six hours (00, 06, 12, 18Z) and appears on NOMADS about four hours later,
//! with forecast files every 3 hours at this resolution. For a chosen time the field comes from the
//! newest run that has started by then and is published, at the nearest forecast hour: the past is
//! a short forecast from the run before it, the future the newest run's forecast.

use crate::grib::{self, Grid};

/// Flight levels (hundreds of feet) and the standard-atmosphere pressure level (hPa) nearest each.
pub const LEVELS: [(u32, u32); 8] = [
    (50, 850),
    (100, 700),
    (180, 500),
    (240, 400),
    (300, 300),
    (340, 250),
    (390, 200),
    (450, 150),
];
/// The level shown when none is chosen: FL340, where airliners cruise.
pub const DEFAULT_FL: u32 = 340;
/// How long after a run starts it is assumed to be on NOMADS (seconds).
pub const PUBLISH_DELAY: i64 = 4 * 3600 + 30 * 60;
/// The time between GFS runs (seconds): one every six hours, at 00, 06, 12 and 18Z.
pub const RUN_STEP: i64 = 6 * 3600;
/// The furthest forecast hour asked for.
pub const MAX_FHR: i64 = 120;
/// The 0.5° files exist every 3 forecast hours (f007 is a 404; measured 2026-11-16).
pub const STEP_H: i64 = 3;

/// The pressure level (hPa) nearest flight level `fl`, from [`LEVELS`].
///
/// # Panics
///
/// Never: [`LEVELS`] is not empty.
///
/// # Examples
///
/// ```
/// assert_eq!(wx::wind::pressure_for(180), 500);
/// assert_eq!(wx::wind::pressure_for(60), 850); // FL060 is nearest FL050
/// ```
///
pub fn pressure_for(fl: u32) -> u32 {
    LEVELS
        .iter()
        .min_by_key(|(f, _)| f.abs_diff(fl))
        .map(|(_, p)| *p)
        .expect("levels")
}

/// The runs to try for a field valid at `at`, best first: `(run start, forecast hour)`.
///
/// Times are Unix seconds. The newest run started at or before `at` that should be published by
/// `now`, then the two before it in case the newest is late. Forecast hours are rounded to the
/// nearest [`STEP_H`] and kept within 0..=[`MAX_FHR`]; a run whose hour falls outside is left out.
pub fn runs_for(at: i64, now: i64) -> Vec<(i64, i64)> {
    let newest_published = (now - PUBLISH_DELAY).div_euclid(RUN_STEP) * RUN_STEP;
    let started_by_at = at.div_euclid(RUN_STEP) * RUN_STEP;
    let first = newest_published.min(started_by_at);
    (0..3)
        .map(|k| first - k * RUN_STEP)
        .filter_map(|run| {
            let fhr = ((at - run) as f64 / (STEP_H * 3600) as f64).round() as i64 * STEP_H;
            (0..=MAX_FHR).contains(&fhr).then_some((run, fhr))
        })
        .collect()
}

/// The NOMADS filter request for U and V at `hpa` from run `run` (Unix seconds), forecast hour
/// `fhr`, for the whole world.
pub fn url(run: i64, fhr: i64, hpa: u32) -> String {
    let iso = crate::satellite::iso(run); // 2026-11-16T12:00:00Z
    let (date, hh) = (iso[0..10].replace('-', ""), &iso[11..13]);
    format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_gfs_0p50.pl?dir=%2Fgfs.{date}%2F{hh}%2Fatmos&file=gfs.t{hh}z.pgrb2full.0p50.f{fhr:03}\
         &var_UGRD=on&var_VGRD=on&lev_{hpa}_mb=on&subregion=&toplat=90&leftlon=0&rightlon=359.5&bottomlat=-90"
    )
}

/// The wind at one pressure level on a regular latitude/longitude grid.
#[derive(Debug, Clone, PartialEq)]
pub struct Wind {
    /// Points per row (west to east).
    pub ni: usize,
    /// Rows (north to south).
    pub nj: usize,
    /// Latitude of row 0 (the northernmost), degrees north.
    pub lat0: f64,
    /// Longitude of column 0, degrees east.
    pub lon0: f64,
    /// Row spacing in degrees.
    pub dlat: f64,
    /// Column spacing in degrees.
    pub dlon: f64,
    /// Eastward wind, m/s, `ni * nj` values, rows north to south.
    pub u: Vec<f32>,
    /// Northward wind, m/s, laid out as `u`.
    pub v: Vec<f32>,
}

/// U and V at `hpa` from a NOMADS response: two fields on the same grid at that isobaric level.
///
/// # Errors
///
/// The response is not GRIB (NOMADS answers a missing file with an HTML page; the message quotes
/// it), it does not decode, U or V at `hpa` is missing, or the two are on different grids.
pub fn from_grib(bytes: &[u8], hpa: u32) -> Result<Wind, String> {
    if !bytes.starts_with(b"GRIB") {
        // NOMADS answers a missing file with an HTML page and a 200
        return Err(format!(
            "not GRIB: {}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(160)])
        ));
    }
    let fields = grib::decode_all(bytes).map_err(|e| e.to_string())?;
    let at_level = |param: u8| -> Result<&Grid, String> {
        fields
            .iter()
            .find(|g| g.param.1 == 2 && g.param.2 == param && g.level == (100, hpa * 100))
            .ok_or(format!(
                "no {} wind at {hpa} hPa in the response",
                if param == 2 { "U" } else { "V" }
            ))
    };
    let (u, v) = (at_level(2)?, at_level(3)?);
    if (u.ni, u.nj, u.lat0, u.lon0) != (v.ni, v.nj, v.lat0, v.lon0) {
        return Err("U and V on different grids".into());
    }
    Ok(Wind {
        ni: u.ni,
        nj: u.nj,
        lat0: u.lat0,
        lon0: u.lon0,
        dlat: u.dlat,
        dlon: u.dlon,
        u: u.values.clone(),
        v: v.values.clone(),
    })
}

/// The field for the webview: a 48-byte header (ni, nj as u32; lat0, lon0, dlat, dlon as f32; run
/// as i64; forecast hour and hPa as u32; little-endian) then U and V interleaved as f32.
pub fn encode(w: &Wind, run: i64, fhr: i64, hpa: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(48 + w.u.len() * 8);
    out.extend((w.ni as u32).to_le_bytes());
    out.extend((w.nj as u32).to_le_bytes());
    for x in [w.lat0, w.lon0, w.dlat, w.dlon] {
        out.extend((x as f32).to_le_bytes());
    }
    out.extend(run.to_le_bytes());
    out.extend((fhr as u32).to_le_bytes());
    out.extend(hpa.to_le_bytes());
    out.extend([0u8; 8]); // reserved, to 48
    for (u, v) in w.u.iter().zip(&w.v) {
        out.extend(u.to_le_bytes());
        out.extend(v.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const T12: i64 = 1_794_830_400; // 2026-11-16T12:00:00Z

    #[test]
    fn flight_levels_to_pressure_levels() {
        assert_eq!(pressure_for(340), 250);
        assert_eq!(pressure_for(350), 250);
        assert_eq!(
            pressure_for(370),
            200,
            "FL370 is nearer FL390's 200 hPa than FL340's 250"
        );
        assert_eq!(pressure_for(30), 850);
        assert_eq!(pressure_for(450), 150);
    }

    #[test]
    fn which_run_and_hour_serve_a_time() {
        // now 22:00Z: 18Z started at 18:00 but is not published until ~22:30, so 12Z is newest; the
        // 0.5° files are every 3 hours, so the hour is the nearest multiple of 3
        let now = T12 + 10 * 3600;
        assert_eq!(
            runs_for(now, now)[0],
            (T12, 9),
            "the present: 12Z, 9 hours on (valid 21Z)"
        );
        assert_eq!(
            runs_for(now + 6 * 3600, now)[0],
            (T12, 15),
            "the future: the newest run's forecast"
        );
        assert_eq!(
            runs_for(T12 - 5 * 3600, now)[0],
            (T12 - 6 * 3600, 0),
            "07Z: the 06Z run's analysis"
        );
        assert_eq!(
            runs_for(T12 - 6 * 3600 + 5399, now)[0].1,
            0,
            "rounded to the nearest 3 hours"
        );
        assert_eq!(runs_for(T12 - 6 * 3600 + 5400, now)[0].1, 3);
        // the fallbacks: the runs before, their hours further on
        assert_eq!(
            runs_for(now, now),
            vec![(T12, 9), (T12 - 6 * 3600, 15), (T12 - 12 * 3600, 21)]
        );
        // once 18Z is published (22:30Z), it serves times after 18Z
        assert_eq!(
            runs_for(T12 + 11 * 3600, T12 + 10 * 3600 + 1800)[0],
            (T12 + 6 * 3600, 6)
        );
        for (_, fhr) in runs_for(now + 7 * 3600 + 1234, now) {
            assert_eq!(fhr % 3, 0, "only hours the 0.5° files exist for");
        }
    }

    #[test]
    fn the_request() {
        let u = url(T12, 9, 250);
        assert!(
            u.contains("filter_gfs_0p50.pl")
                && u.contains("dir=%2Fgfs.20261116%2F12%2Fatmos")
                && u.contains("file=gfs.t12z.pgrb2full.0p50.f009")
                && u.contains("lev_250_mb=on"),
            "{u}"
        );
        // the whole world, asked for as a subregion: NOMADS then repacks it simply (template 5.0)
        assert!(
            u.contains("subregion=&toplat=90&leftlon=0&rightlon=359.5&bottomlat=-90"),
            "{u}"
        );
    }

    #[test]
    fn refuses_an_html_answer() {
        assert!(from_grib(b"<!DOCTYPE html><html>no such file</html>", 250)
            .unwrap_err()
            .starts_with("not GRIB"));
    }

    #[test]
    fn encoding_for_the_webview() {
        let w = Wind {
            ni: 2,
            nj: 1,
            lat0: 60.0,
            lon0: -135.0,
            dlat: 0.25,
            dlon: 0.25,
            u: vec![1.5, -2.0],
            v: vec![3.0, 4.5],
        };
        let b = encode(&w, T12, 9, 250);
        assert_eq!(b.len(), 48 + 16);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 2);
        assert_eq!(f32::from_le_bytes(b[8..12].try_into().unwrap()), 60.0);
        assert_eq!(i64::from_le_bytes(b[24..32].try_into().unwrap()), T12);
        assert_eq!(u32::from_le_bytes(b[36..40].try_into().unwrap()), 250);
        let f = |i: usize| f32::from_le_bytes(b[48 + 4 * i..52 + 4 * i].try_into().unwrap());
        assert_eq!(
            [f(0), f(1), f(2), f(3)],
            [1.5, 3.0, -2.0, 4.5],
            "u, v interleaved"
        );
    }

    /// The real NOMADS response: `WX_GFS=file.grib2` (UGRD and VGRD at 250 hPa, the world at 0.5°)
    #[test]
    fn a_real_response() {
        let Ok(path) = std::env::var("WX_GFS") else {
            return;
        };
        let w = from_grib(&std::fs::read(path).unwrap(), 250).unwrap();
        assert_eq!((w.ni, w.nj), (720, 361), "the world at 0.5°");
        let fastest =
            w.u.iter()
                .zip(&w.v)
                .map(|(u, v)| (u * u + v * v).sqrt())
                .fold(0.0f32, f32::max);
        eprintln!("fastest {fastest:.1} m/s ({:.0} kt)", fastest * 1.944);
        assert!(fastest > 20.0 && fastest < 120.0);
    }
}
