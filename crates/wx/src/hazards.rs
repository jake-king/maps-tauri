//! G-AIRMET hazard areas from aviationweather.gov's GeoJSON.
//!
//! A G-AIRMET package is a set of snapshots, each valid at one instant three hours apart
//! (`fore=0, 3, 6, 9, 12`); a display shows
//! the snapshot nearest the present. Features the renderer cannot draw are dropped, not guessed at.

use serde_json::Value;

/// A hazard's geometry, in degrees: each point `[lon, lat]`.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// An area: outer ring first, then any holes; each ring at least 4 points.
    Polygon(Vec<Vec<[f64; 2]>>),
    /// A line of at least 2 points (the freezing-level contours).
    Line(Vec<[f64; 2]>),
}

/// One G-AIRMET feature the renderer can draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Hazard {
    /// The hazard type as published: `TURB-HI`, `TURB-LO`, `ICE`, `IFR`, `MT_OBSC`, `SFC_WND`,
    /// `LLWS`, `FZLVL`, `M_FZLVL`.
    pub hazard: String,
    /// The severity as published (`MOD`, for example), when given.
    pub severity: Option<String>,
    /// The lower bound as published: flight level in hundreds of feet, or `SFC` / `FZL`.
    pub base: Option<String>,
    /// The upper bound, in the same terms as `base`.
    pub top: Option<String>,
    /// freezing level, for FZLVL lines
    pub level: Option<String>,
    /// seconds since the Unix epoch
    pub valid: i64,
    /// Where it applies.
    pub shape: Shape,
}

/// "2026-11-16T21:00:00.000Z" (or without the fraction) to seconds since the epoch, UTC only.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || !s.ends_with('Z')
    {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        num(0..4)?,
        num(5..7)?,
        num(8..10)?,
        num(11..13)?,
        num(14..16)?,
        num(17..19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // days from civil (Howard Hinnant's algorithm)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

fn point(v: &Value) -> Option<[f64; 2]> {
    let a = v.as_array()?;
    let (lon, lat) = (a.first()?.as_f64()?, a.get(1)?.as_f64()?);
    ((-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)).then_some([lon, lat])
}

fn points(v: &Value) -> Option<Vec<[f64; 2]>> {
    v.as_array()?.iter().map(point).collect()
}

fn shape(g: &Value) -> Option<Shape> {
    let coords = g.get("coordinates")?;
    match g.get("type")?.as_str()? {
        "Polygon" => {
            let rings: Vec<_> = coords
                .as_array()?
                .iter()
                .map(points)
                .collect::<Option<_>>()?;
            (!rings.is_empty() && rings.iter().all(|r| r.len() >= 4))
                .then_some(Shape::Polygon(rings))
        }
        "LineString" => points(coords).filter(|p| p.len() >= 2).map(Shape::Line),
        _ => None,
    }
}

fn text(p: &Value, key: &str) -> Option<String> {
    match p.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Every drawable feature of one response, and the number of features skipped.
///
/// A response that is not a `FeatureCollection` is an error; a feature without a known geometry or
/// valid time is skipped and counted.
///
/// # Errors
///
/// The text is not JSON, not a `FeatureCollection`, or has no `features` array.
pub fn parse(json: &str) -> Result<(Vec<Hazard>, usize), String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("gairmet: {e}"))?;
    if v.get("type").and_then(Value::as_str) != Some("FeatureCollection") {
        return Err("gairmet: not a GeoJSON FeatureCollection".into());
    }
    let features = v
        .get("features")
        .and_then(Value::as_array)
        .ok_or("gairmet: no features array")?;
    let mut out = Vec::new();
    let mut skipped = 0;
    for f in features {
        let p = f.get("properties").unwrap_or(&Value::Null);
        let h = (|| {
            Some(Hazard {
                hazard: text(p, "hazard")?,
                severity: text(p, "severity"),
                base: text(p, "base"),
                top: text(p, "top"),
                level: text(p, "level"),
                valid: parse_time(p.get("validTime")?.as_str()?)?,
                shape: shape(f.get("geometry")?)?,
            })
        })();
        match h {
            Some(h) => out.push(h),
            None => skipped += 1,
        }
    }
    Ok((out, skipped))
}

/// The snapshot to show at `now`: the features whose valid time is nearest it (the earlier one on a
/// tie). Empty when there are none.
pub fn nearest_snapshot(all: &[Hazard], now: i64) -> Vec<Hazard> {
    let Some(best) = all
        .iter()
        .map(|h| h.valid)
        .min_by_key(|&t| ((t - now).abs(), t))
    else {
        return Vec::new();
    };
    all.iter().filter(|h| h.valid == best).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"type":"FeatureCollection","features":[
      {"type":"Feature","properties":{"product":"TANGO","hazard":"TURB-HI","validTime":"2026-11-16T21:00:00.000Z","forecast":6,"severity":"MOD","top":"400","base":"180"},
       "geometry":{"type":"Polygon","coordinates":[[[-120.5,40.1],[-118.0,40.1],[-118.0,38.0],[-120.5,40.1]]]}},
      {"type":"Feature","properties":{"hazard":"FZLVL","validTime":"2026-11-16T21:00:00.000Z","level":"160"},
       "geometry":{"type":"LineString","coordinates":[[-120,41],[-110,42]]}},
      {"type":"Feature","properties":{"hazard":"ICE","validTime":"not a time"},
       "geometry":{"type":"Polygon","coordinates":[[[-1,1],[1,1],[1,-1],[-1,1]]]}},
      {"type":"Feature","properties":{"hazard":"ICE","validTime":"2026-11-16T21:00:00Z"},
       "geometry":{"type":"Point","coordinates":[-1,1]}},
      {"type":"Feature","properties":{"hazard":"ICE","validTime":"2026-11-16T21:00:00Z"},
       "geometry":{"type":"Polygon","coordinates":[[[-1,1],[1,1],[-1,1]]]}},
      {"type":"Feature","properties":{"hazard":"ICE","validTime":"2026-11-16T21:00:00Z"},
       "geometry":{"type":"LineString","coordinates":[[500,1],[1,1]]}}
    ]}"#;

    #[test]
    fn keeps_drawable_features_and_counts_the_rest() {
        let (h, skipped) = parse(SAMPLE).unwrap();
        assert_eq!(
            skipped, 4,
            "bad time, a point, a 3-point ring, a longitude of 500"
        );
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].hazard, "TURB-HI");
        assert_eq!(
            (
                h[0].severity.as_deref(),
                h[0].base.as_deref(),
                h[0].top.as_deref()
            ),
            (Some("MOD"), Some("180"), Some("400"))
        );
        assert_eq!(h[0].valid, parse_time("2026-11-16T21:00:00Z").unwrap());
        assert!(
            matches!(&h[0].shape, Shape::Polygon(r) if r.len() == 1 && r[0][0] == [-120.5, 40.1])
        );
        assert_eq!(h[1].level.as_deref(), Some("160"));
        assert!(matches!(&h[1].shape, Shape::Line(p) if p.len() == 2));
    }

    #[test]
    fn refuses_what_is_not_a_feature_collection() {
        assert!(parse("").is_err());
        assert!(parse(r#"{"type":"Feature"}"#).is_err());
        assert!(parse(r#"{"type":"FeatureCollection"}"#).is_err());
        assert_eq!(
            parse(r#"{"type":"FeatureCollection","features":[]}"#)
                .unwrap()
                .0,
            vec![]
        );
    }

    #[test]
    fn times_in_utc() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2000-03-01T00:00:00.000Z"), Some(951_868_800));
        assert_eq!(parse_time("2026-11-16T21:00:00.000Z"), Some(1_794_862_800));
        for bad in [
            "2026-11-16 21:00:00Z",
            "2026-13-16T21:00:00Z",
            "2026-11-16T21:00:00+01:00",
            "2026-11-16T25:00:00Z",
            "",
        ] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    fn at(t: i64) -> Hazard {
        Hazard {
            hazard: "ICE".into(),
            severity: None,
            base: None,
            top: None,
            level: None,
            valid: t,
            shape: Shape::Line(vec![[0.0, 0.0], [1.0, 1.0]]),
        }
    }

    #[test]
    fn nearest_snapshot_to_now() {
        let all = vec![at(0), at(0), at(10_800), at(21_600)];
        assert_eq!(nearest_snapshot(&all, 1000).len(), 2);
        assert!(nearest_snapshot(&all, 9000)
            .iter()
            .all(|h| h.valid == 10_800));
        // exactly between two: the earlier
        assert!(nearest_snapshot(&all, 16_200)
            .iter()
            .all(|h| h.valid == 10_800));
        assert!(nearest_snapshot(&all, 99_999)
            .iter()
            .all(|h| h.valid == 21_600));
        assert!(nearest_snapshot(&[], 0).is_empty());
    }

    /// Real responses, when downloaded: `WX_GAIRMET=file.json`
    #[test]
    fn a_real_response() {
        let Ok(path) = std::env::var("WX_GAIRMET") else {
            return;
        };
        let (h, skipped) = parse(&std::fs::read_to_string(path).unwrap()).unwrap();
        eprintln!("{} features, {skipped} skipped", h.len());
        assert!(!h.is_empty());
        assert_eq!(skipped, 0, "a real feature was not understood");
    }
}
