//! Winds aloft from NOAA's GFS model: U and V at one pressure level over the box, cut out of a run
//! by NOMADS's GRIB filter (0.25°, 321 x 181 points, about 150 KB a level).
//!
//! A run starts every six hours (00, 06, 12, 18Z) and appears on NOMADS about four hours later, with
//! hourly forecast files out to 120 hours. For a chosen time the field comes from the newest run that
//! has started by then and is published, at the nearest forecast hour: the past is a short forecast
//! from the run before it, the future the newest run's forecast.

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
pub const RUN_STEP: i64 = 6 * 3600;
pub const MAX_FHR: i64 = 120;

/// The pressure level (hPa) nearest flight level `fl`.
pub fn pressure_for(fl: u32) -> u32 {
    LEVELS
        .iter()
        .min_by_key(|(f, _)| f.abs_diff(fl))
        .map(|(_, p)| *p)
        .expect("levels")
}

/// The runs to try for a field valid at `at`, best first: (run start, forecast hour). The newest run
/// started at or before `at` that should be published by `now`, then the two before it in case the
/// newest is late. Forecast hours are rounded to the nearest hour and kept within the model's range.
pub fn runs_for(at: i64, now: i64) -> Vec<(i64, i64)> {
    let newest_published = (now - PUBLISH_DELAY).div_euclid(RUN_STEP) * RUN_STEP;
    let started_by_at = at.div_euclid(RUN_STEP) * RUN_STEP;
    let first = newest_published.min(started_by_at);
    (0..3)
        .map(|k| first - k * RUN_STEP)
        .filter_map(|run| {
            let fhr = ((at - run) as f64 / 3600.0).round() as i64;
            (0..=MAX_FHR).contains(&fhr).then_some((run, fhr))
        })
        .collect()
}

/// The NOMADS filter request for U and V at `hpa` from run `run` (Unix seconds), forecast hour `fhr`,
/// over the box (latitude 15..60, longitude -135..-55, as 225..305 east).
pub fn url(run: i64, fhr: i64, hpa: u32) -> String {
    let iso = crate::satellite::iso(run); // 2026-11-16T12:00:00Z
    let (date, hh) = (iso[0..10].replace('-', ""), &iso[11..13]);
    format!(
        "https://nomads.ncep.noaa.gov/cgi-bin/filter_gfs_0p25.pl?dir=%2Fgfs.{date}%2F{hh}%2Fatmos&file=gfs.t{hh}z.pgrb2.0p25.f{fhr:03}\
         &var_UGRD=on&var_VGRD=on&lev_{hpa}_mb=on&subregion=&toplat=60&leftlon=225&rightlon=305&bottomlat=15"
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct Wind {
    pub ni: usize,
    pub nj: usize,
    /// latitude of row 0 (the northernmost) and longitude of column 0
    pub lat0: f64,
    pub lon0: f64,
    pub dlat: f64,
    pub dlon: f64,
    /// eastward and northward wind, m/s, rows north to south
    pub u: Vec<f32>,
    pub v: Vec<f32>,
}

/// U and V at `hpa` from a NOMADS response: two fields on the same grid at that isobaric level.
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

/// The field for the webview: a 48-byte header (ni, nj as u32; lat0, lon0, dlat, dlon as f32; run as
/// i64; forecast hour and hPa as u32; little-endian) then U and V interleaved as f32.
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
        // now 22:00Z: 18Z started at 18:00 but is not published until ~22:30, so 12Z is newest
        let now = T12 + 10 * 3600;
        assert_eq!(
            runs_for(now, now)[0],
            (T12, 10),
            "the present: 12Z, 10 hours on"
        );
        assert_eq!(
            runs_for(now + 6 * 3600, now)[0],
            (T12, 16),
            "the future: the newest run's forecast"
        );
        assert_eq!(
            runs_for(T12 - 5 * 3600, now)[0],
            (T12 - 6 * 3600, 1),
            "07Z: the 06Z run, an hour on"
        );
        assert_eq!(
            runs_for(T12 - 5 * 3600 + 1799, now)[0].1,
            1,
            "rounded to the nearest hour"
        );
        assert_eq!(runs_for(T12 - 5 * 3600 + 1800, now)[0].1, 2);
        // the fallbacks: the runs before, their hours further on
        assert_eq!(
            runs_for(now, now),
            vec![(T12, 10), (T12 - 6 * 3600, 16), (T12 - 12 * 3600, 22)]
        );
        // once 18Z is published (22:30Z), it serves times after 18Z
        assert_eq!(
            runs_for(T12 + 11 * 3600, T12 + 10 * 3600 + 1800)[0],
            (T12 + 6 * 3600, 5)
        );
    }

    #[test]
    fn the_request() {
        let u = url(T12, 9, 250);
        assert!(
            u.contains("dir=%2Fgfs.20261116%2F12%2Fatmos")
                && u.contains("file=gfs.t12z.pgrb2.0p25.f009")
                && u.contains("lev_250_mb=on"),
            "{u}"
        );
        assert!(u.contains("toplat=60&leftlon=225&rightlon=305&bottomlat=15"));
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

    /// The real NOMADS response: WX_GFS=file.grib2 (UGRD and VGRD at 250 hPa)
    #[test]
    fn a_real_response() {
        let Ok(path) = std::env::var("WX_GFS") else {
            return;
        };
        let w = from_grib(&std::fs::read(path).unwrap(), 250).unwrap();
        assert_eq!((w.ni, w.nj), (321, 181));
        let fastest =
            w.u.iter()
                .zip(&w.v)
                .map(|(u, v)| (u * u + v * v).sqrt())
                .fold(0.0f32, f32::max);
        eprintln!("fastest {fastest:.1} m/s ({:.0} kt)", fastest * 1.944);
        assert!(fastest > 20.0 && fastest < 120.0);
    }
}
