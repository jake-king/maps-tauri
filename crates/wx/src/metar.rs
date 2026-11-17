//! METARs: surface observations at airports, coloured by flight category as dispatchers read them.
//!
//! The categories are VFR, MVFR, IFR and LIFR. The source is the Aviation Weather Center's bulk
//! cache file, every station's latest report in one 0.25 MB download, refreshed every few minutes.
//!
//! History is not in that file, and the API's history is capped at 400 observations a request: one
//! past hour over the US took 30 requests and 80 seconds (measured 2026-11-16). So history is what
//! this app collected from the cache file while it ran, kept on disk.

/// The Aviation Weather Center's bulk file: every station's latest METAR, gzipped CSV.
pub const CACHE_URL: &str = "https://aviationweather.gov/data/cache/metars.cache.csv.gz";
/// A station's report stands for this long after it was observed (METARs are hourly).
pub const VALID_FOR: i64 = 75 * 60;

/// The flight category as a number: 0 = not reported (or unrecognised), 1 = VFR, 2 = MVFR, 3 = IFR,
/// 4 = LIFR.
pub fn category(s: &str) -> u8 {
    match s {
        "VFR" => 1,
        "MVFR" => 2,
        "IFR" => 3,
        "LIFR" => 4,
        _ => 0,
    }
}

/// One station's report.
#[derive(Debug, Clone, PartialEq)]
pub struct Metar {
    /// The station's ICAO identifier (`KLAX`).
    pub id: String,
    /// When it was observed, Unix seconds.
    pub time: i64,
    /// Longitude, degrees east.
    pub lon: f64,
    /// Latitude, degrees north.
    pub lat: f64,
    /// The flight category as [`category`] numbers it.
    pub cat: u8,
    /// The report as issued, trimmed.
    pub raw: String,
}

/// One CSV record per line, RFC 4180 quoting (the raw report is quoted and holds commas).
fn fields(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Every report in the cache file's CSV with a time and a position: the whole world.
///
/// A file without the expected columns is an error; a row that cannot be read (no position, no
/// time, no station) is skipped and counted. Returns the reports and the number skipped.
///
/// # Errors
///
/// The text is empty, or the header lacks one of `raw_text`, `station_id`, `observation_time`,
/// `latitude`, `longitude`, `flight_category`.
pub fn parse_cache(csv: &str) -> Result<(Vec<Metar>, usize), String> {
    let mut lines = csv.lines();
    let header = fields(lines.next().ok_or("metar cache: empty")?);
    // some names repeat (sky_cover per layer): the first of each is the one wanted
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .ok_or(format!("metar cache: no {name} column"))
    };
    let (raw, id, time, lat, lon, cat) = (
        col("raw_text")?,
        col("station_id")?,
        col("observation_time")?,
        col("latitude")?,
        col("longitude")?,
        col("flight_category")?,
    );
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let f = fields(line);
        let get = |i: usize| f.get(i).map(String::as_str).unwrap_or("");
        let m = (|| {
            let (la, lo): (f64, f64) = (get(lat).parse().ok()?, get(lon).parse().ok()?);
            Some(Metar {
                id: get(id).to_string(),
                time: crate::hazards::parse_time(get(time))?,
                lon: lo,
                lat: la,
                cat: category(get(cat)),
                raw: get(raw).trim().to_string(),
            })
        })();
        match m {
            Some(m) if !m.id.is_empty() => out.push(m),
            _ => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// Each station's report in force at `at` (Unix seconds).
///
/// That is its latest observed at or before `at`, and no older than [`VALID_FOR`]. `by_station`
/// holds each station's reports, oldest first.
pub fn at_time<'a>(by_station: impl Iterator<Item = &'a [Metar]>, at: i64) -> Vec<&'a Metar> {
    by_station
        .filter_map(|reports| {
            let i = reports.partition_point(|m| m.time <= at);
            let m = reports.get(i.checked_sub(1)?)?;
            (at - m.time <= VALID_FOR).then_some(m)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "raw_text,station_id,observation_time,latitude,longitude,temp_c,sky_cover,cloud_base_ft_agl,sky_cover,flight_category,elevation_m";

    #[test]
    fn reads_the_cache_file_layout() {
        let csv = format!(
            "{HEAD}\n\"METAR KLAX 162153Z 25010KT 10SM FEW020 18/12 A3001 RMK AO2, \"\"quoted\"\"\",KLAX,2026-11-16T21:53:00.000Z,33.9382,-118.3866,18,FEW,2000,,VFR,38\n\
             \"SPECI KSFO 162201Z 1SM BR OVC004\",KSFO,2026-11-16T22:01:00.000Z,37.6197,-122.3647,12,OVC,400,,LIFR,3\n\
             \"METAR XXXX 1SM\",XXXX,2026-11-16T22:01:00.000Z,,,,,,,IFR,\n\
             \"METAR EGLL 162150Z\",EGLL,2026-11-16T21:50:00.000Z,51.47,-0.45,,,,,VFR,25\n\
             \"METAR KNUL 162150Z\",KNUL,2026-11-16T21:50:00.000Z,40.0,-100.0,,,,,null,1\n"
        );
        let (m, skipped) = parse_cache(&csv).unwrap();
        assert_eq!(skipped, 1, "the row without a position");
        assert_eq!(
            m.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            vec!["KLAX", "KSFO", "EGLL", "KNUL"],
            "the whole world: London too"
        );
        assert_eq!(
            m[0].raw,
            "METAR KLAX 162153Z 25010KT 10SM FEW020 18/12 A3001 RMK AO2, \"quoted\""
        );
        assert_eq!(
            (m[0].cat, m[1].cat, m[3].cat),
            (1, 4, 0),
            "\"null\" is not reported"
        );
        assert_eq!(
            m[1].time,
            crate::hazards::parse_time("2026-11-16T22:01:00Z").unwrap()
        );
        assert!((m[0].lat - 33.9382).abs() < 1e-9 && (m[0].lon + 118.3866).abs() < 1e-9);
    }

    #[test]
    fn refuses_a_file_that_is_not_the_cache() {
        assert!(parse_cache("").is_err());
        assert!(parse_cache("<html>error</html>\n")
            .unwrap_err()
            .contains("raw_text"));
    }

    fn rep(id: &str, t: i64) -> Metar {
        Metar {
            id: id.into(),
            time: t,
            lon: 0.0,
            lat: 0.0,
            cat: 1,
            raw: format!("{id} {t}"),
        }
    }

    #[test]
    fn the_report_in_force_at_a_time() {
        let a = vec![rep("A", 0), rep("A", 3600), rep("A", 7200)];
        let b = vec![rep("B", 100)];
        let stations = [a.as_slice(), b.as_slice()];
        let ids = |at: i64| {
            at_time(stations.iter().copied(), at)
                .iter()
                .map(|m| m.raw.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(3700), vec!["A 3600", "B 100"]);
        assert_eq!(
            ids(7199),
            vec!["A 3600"],
            "B's report from 100 is past 75 minutes"
        );
        assert_eq!(ids(7200), vec!["A 7200"]);
        assert_eq!(
            ids(-1),
            Vec::<String>::new(),
            "nothing before the first report"
        );
        assert_eq!(
            ids(7200 + VALID_FOR + 1),
            Vec::<String>::new(),
            "a station that stopped reporting goes dark"
        );
    }

    /// The real file, when downloaded and unzipped: `WX_METARS=metars.cache.csv`
    #[test]
    fn a_real_cache_file() {
        let Ok(path) = std::env::var("WX_METARS") else {
            return;
        };
        let (m, skipped) = parse_cache(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cats = (0..5)
            .map(|c| m.iter().filter(|x| x.cat == c).count())
            .collect::<Vec<_>>();
        eprintln!(
            "{} stations, {skipped} skipped, by category {cats:?}",
            m.len()
        );
        assert!(m.len() > 4000, "the whole world");
    }
}
