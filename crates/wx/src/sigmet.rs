//! SIGMETs: hazard areas issued as warnings, worldwide.
//!
//! International SIGMETs (every FIR's turbulence, icing, thunderstorms, mountain waves, tropical
//! cyclones, volcanic ash) and US convective SIGMETs, from aviationweather.gov's JSON. Each is
//! valid over a period, not at an instant.
//!
//! The feed is loose about types: the same field arrives as a number, a numeric string, `null` or
//! the string "None" depending on the request (measured 2026-11-16: with `date=` numbers and null,
//! without it strings and "None"), so every field is read as any of those. An area crossing the
//! 180° meridian comes as longitudes past 180 (176.5 to 201.75), kept continuous so it draws in one
//! piece.

use serde_json::Value;

/// One SIGMET, international or US domestic.
#[derive(Debug, Clone, PartialEq)]
pub struct Sigmet {
    /// `SIGMET` (international, or US non-convective) or `CONVECTIVE` (US convective SIGMET).
    pub kind: String,
    /// The hazard as published: `TS`, `TURB`, `ICE`, `MTW`, `VA`, `TC`, `CONVECTIVE`, ...
    pub hazard: String,
    /// The qualifier as published (`SEV`, `EMBD`, ...; `severity N` for a US domestic one), when
    /// given.
    pub qualifier: Option<String>,
    /// The issuing flight information region's name, when given.
    pub fir: Option<String>,
    /// The base in feet; `None` = not given (a missing base is the surface for display purposes).
    pub base_ft: Option<i64>,
    /// The top in feet; `None` = not given.
    pub top_ft: Option<i64>,
    /// Start of the valid period, Unix seconds.
    pub from: i64,
    /// End of the valid period, Unix seconds (not before `from`).
    pub to: i64,
    /// One ring per area, each a list of at least 3 `[lon, lat]` points in degrees; a single point
    /// is a SIGMET reported at a position ("SEV ICE OBS AT ..."), drawn as a marker.
    pub rings: Vec<Vec<[f64; 2]>>,
    /// The SIGMET's text as issued; empty if the feed gave none.
    pub raw: String,
}

impl Sigmet {
    /// Whether `t` (Unix seconds) falls within the valid period, both ends included.
    pub fn active_at(&self, t: i64) -> bool {
        self.from <= t && t <= self.to
    }
}

/// A number from a number, a numeric string, or nothing from null / "None" / "".
fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.is_empty() && s != "None" => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn ring(v: &Value) -> Option<Vec<[f64; 2]>> {
    let pts: Vec<[f64; 2]> = v
        .as_array()?
        .iter()
        .map(|p| Some([num(p.get("lon"))?, num(p.get("lat"))?]))
        .collect::<Option<_>>()?;
    ((pts.len() >= 3 || pts.len() == 1)
        && pts
            .iter()
            .all(|[lo, la]| (-360.0..=360.0).contains(lo) && (-90.0..=90.0).contains(la)))
    .then_some(pts)
}

/// AREA: a list of points; AREAS: a list of lists of points.
fn rings(coords: Option<&Value>) -> Option<Vec<Vec<[f64; 2]>>> {
    let c = coords?.as_array()?;
    if c.first()?.is_array() {
        let rs: Vec<_> = c.iter().filter_map(ring).collect();
        (!rs.is_empty()).then_some(rs)
    } else {
        Some(vec![ring(coords?)?])
    }
}

fn array(json: &str, what: &str) -> Result<Vec<Value>, String> {
    if json.trim().is_empty() {
        return Ok(Vec::new()); // a 204: nothing in force
    }
    match serde_json::from_str::<Value>(json).map_err(|e| format!("{what}: {e}"))? {
        Value::Array(a) => Ok(a),
        _ => Err(format!("{what}: not a list")),
    }
}

/// International SIGMETs, and the number of records skipped.
///
/// A record without a hazard, a valid period or a drawable area is skipped and counted. Empty text
/// (a 204: nothing in force) is no SIGMETs.
///
/// # Errors
///
/// The text is not JSON, or not a JSON list.
pub fn parse_international(json: &str) -> Result<(Vec<Sigmet>, usize), String> {
    let mut out = Vec::new();
    let mut skipped = 0;
    for r in array(json, "isigmet")? {
        let s = (|| {
            Some(Sigmet {
                kind: "SIGMET".into(),
                hazard: text(r.get("hazard"))?,
                qualifier: text(r.get("qualifier")),
                fir: text(r.get("firName")),
                base_ft: num(r.get("base")).map(|x| x as i64),
                top_ft: num(r.get("top")).map(|x| x as i64),
                from: num(r.get("validTimeFrom"))? as i64,
                to: num(r.get("validTimeTo"))? as i64,
                rings: rings(r.get("coords"))?,
                raw: text(r.get("rawSigmet")).unwrap_or_default(),
            })
        })();
        match s {
            Some(s) if s.to >= s.from => out.push(s),
            _ => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// US domestic SIGMETs (convective and non-convective), and the number of records skipped.
///
/// AIRMETs in this feed are left out, the G-AIRMET layer covers them. Empty text is no SIGMETs.
///
/// # Errors
///
/// The text is not JSON, or not a JSON list.
pub fn parse_domestic(json: &str) -> Result<(Vec<Sigmet>, usize), String> {
    let mut out = Vec::new();
    let mut skipped = 0;
    for r in array(json, "airsigmet")? {
        if text(r.get("airSigmetType")).as_deref() == Some("AIRMET") {
            continue;
        }
        let hazard = text(r.get("hazard"));
        let s = (|| {
            Some(Sigmet {
                kind: if hazard.as_deref() == Some("CONVECTIVE") {
                    "CONVECTIVE".into()
                } else {
                    "SIGMET".into()
                },
                hazard: hazard?,
                qualifier: text(r.get("severity")).map(|s| format!("severity {s}")),
                fir: None,
                base_ft: num(r.get("altitudeLow1")).map(|x| x as i64),
                top_ft: num(r.get("altitudeHi1")).map(|x| x as i64),
                from: num(r.get("validTimeFrom"))? as i64,
                to: num(r.get("validTimeTo"))? as i64,
                rings: rings(r.get("coords"))?,
                raw: text(r.get("rawAirSigmet")).unwrap_or_default(),
            })
        })();
        match s {
            Some(s) if s.to >= s.from => out.push(s),
            _ => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// A stable key for a SIGMET: its raw text identifies it (the feed repeats it until it lapses).
pub fn key(s: &Sigmet) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.raw.bytes().chain(s.from.to_le_bytes()) {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn international_records_whatever_the_types() {
        // the shape without date= (strings, "None") and with it (numbers, null), and an AREAS record
        let json = r#"[
          {"firName":"SEFG GUAYAQUIL","validTimeFrom":"1794851820","validTimeTo":"1794873420","hazard":"VA","qualifier":"ERUPTION MT REVENTADOR",
           "base":"0","top":"15000","geom":"AREA","coords":[{"lon":-77.8,"lat":-0.1},{"lon":-77.7,"lat":-0.2},{"lon":-77.9,"lat":-0.2},{"lon":-77.8,"lat":-0.1}],"rawSigmet":"WVEQ31 SEQU 161757"},
          {"firName":"EGGX SHANWICK","validTimeFrom":1794850000,"validTimeTo":1794864400,"hazard":"TURB","qualifier":"SEV",
           "base":null,"top":38000,"geom":"AREA","coords":[{"lon":-30,"lat":50},{"lon":-20,"lat":50},{"lon":-20,"lat":55},{"lon":-30,"lat":50}],"rawSigmet":"WSNT01 EGGX"},
          {"firName":"PAZA ANCHORAGE","validTimeFrom":1794850000,"validTimeTo":1794864400,"hazard":"ICE","base":"None","top":"None","geom":"AREAS",
           "coords":[[{"lon":176.5,"lat":55.75},{"lon":180,"lat":54.6},{"lon":180,"lat":59.25},{"lon":176.5,"lat":55.75}],
                     [{"lon":180,"lat":54.6},{"lon":190.25,"lat":51.25},{"lon":201.75,"lat":54},{"lon":180,"lat":54.6}]],"rawSigmet":"WSAK"},
          {"firName":"X","validTimeFrom":"None","validTimeTo":1794864400,"hazard":"TS","coords":[{"lon":0,"lat":0},{"lon":1,"lat":0},{"lon":0,"lat":1}]},
          {"firName":"Y","validTimeFrom":1,"validTimeTo":2,"hazard":"TS","coords":[{"lon":0,"lat":0}]}
        ]"#;
        let (s, skipped) = parse_international(json).unwrap();
        assert_eq!(skipped, 1, "no valid period");
        assert_eq!(s.len(), 4);
        assert_eq!(
            s[3].rings,
            vec![vec![[0.0, 0.0]]],
            "reported at a point: kept, as one point"
        );
        assert_eq!(
            (s[0].hazard.as_str(), s[0].base_ft, s[0].top_ft, s[0].from),
            ("VA", Some(0), Some(15000), 1_794_851_820)
        );
        assert_eq!(s[0].fir.as_deref(), Some("SEFG GUAYAQUIL"));
        assert_eq!(
            (s[1].base_ft, s[1].top_ft),
            (None, Some(38000)),
            "null base: not given"
        );
        assert_eq!(
            (s[2].base_ft, s[2].top_ft),
            (None, None),
            "\"None\": not given"
        );
        assert_eq!(s[2].rings.len(), 2, "AREAS: two polygons");
        assert_eq!(
            s[2].rings[1][2],
            [201.75, 54.0],
            "longitudes past 180 kept, so the area stays in one piece"
        );
    }

    #[test]
    fn domestic_convective_sigmets_and_not_airmets() {
        let json = r#"[
          {"airSigmetType":"SIGMET","hazard":"CONVECTIVE","severity":"5","altitudeLow1":"None","altitudeHi1":"24000","validTimeFrom":"1794873300","validTimeTo":"1794880500",
           "rawAirSigmet":"CONVECTIVE SIGMET 6W","coords":[{"lon":-122.24,"lat":40.9},{"lon":-121.0,"lat":39.7},{"lon":-120.9,"lat":40.5},{"lon":-122.24,"lat":40.9}]},
          {"airSigmetType":"AIRMET","hazard":"IFR","validTimeFrom":"1","validTimeTo":"2","coords":[{"lon":0,"lat":0},{"lon":1,"lat":0},{"lon":0,"lat":1}]}
        ]"#;
        let (s, skipped) = parse_domestic(json).unwrap();
        assert_eq!((s.len(), skipped), (1, 0));
        assert_eq!(
            (s[0].kind.as_str(), s[0].base_ft, s[0].top_ft),
            ("CONVECTIVE", None, Some(24000))
        );
    }

    #[test]
    fn empty_and_broken_answers() {
        assert_eq!(parse_international("").unwrap().0, vec![]);
        assert!(parse_international("{\"error\":1}").is_err());
        assert!(parse_domestic("<html>").is_err());
    }

    #[test]
    fn in_force_over_a_period_and_keyed_by_text_and_start() {
        let (s, _) = parse_international(r#"[{"validTimeFrom":100,"validTimeTo":200,"hazard":"TS","rawSigmet":"A","coords":[{"lon":0,"lat":0},{"lon":1,"lat":0},{"lon":0,"lat":1}]}]"#).unwrap();
        assert!(
            !s[0].active_at(99)
                && s[0].active_at(100)
                && s[0].active_at(200)
                && !s[0].active_at(201)
        );
        let mut t = s[0].clone();
        assert_eq!(key(&s[0]), key(&t));
        t.from = 101;
        assert_ne!(
            key(&s[0]),
            key(&t),
            "a reissue with a new period is a different SIGMET"
        );
    }

    /// Real responses: `WX_ISIGMET=file.json` (`WX_AIRSIGMET` for the domestic feed)
    #[test]
    fn real_responses() {
        if let Ok(p) = std::env::var("WX_ISIGMET") {
            let (s, skipped) = parse_international(&std::fs::read_to_string(p).unwrap()).unwrap();
            eprintln!("international: {} read, {skipped} skipped", s.len());
            assert!(s.len() > 20 && skipped * 10 < s.len());
        }
        if let Ok(p) = std::env::var("WX_AIRSIGMET") {
            let (s, skipped) = parse_domestic(&std::fs::read_to_string(p).unwrap()).unwrap();
            eprintln!("domestic: {} read, {skipped} skipped", s.len());
        }
    }
}
