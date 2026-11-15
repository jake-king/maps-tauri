//! The maps search box's input handling, ported from the Infrastructure repo's
//! `containers/epoch/maps/gazetteer/serve.py` (ADR 0195/0196 there).
//!
//! Three functions:
//! * [`parse_coords`]: read a typed coordinate as zero, one or two (lat, lon) readings.
//! * [`fold`]: lowercase, decompose, strip combining marks; the gazetteer's `name_fold` column.
//! * [`fts_query`]: turn arbitrary text into a safe FTS5 phrase-prefix query.
//!
//! Behaviour is meant to match the Python exactly; `tests/differential.rs` checks that against the
//! real `serve.py` and lists every input where the two disagree.
//!
//! One property the port gets for free: Rust's `regex` crate runs in linear time and never
//! backtracks, so the polynomial ReDoS the Python's first draft had (71 s of CPU for a 303-byte
//! query) cannot be written here. The length cap and bounded digit classes are kept anyway, so
//! the accepted language is the same.

use std::sync::OnceLock;

use regex::{Captures, Regex};
use unicode_normalization::{char::canonical_combining_class, UnicodeNormalization};

#[cfg(feature = "wasm")]
mod wasm;

/// A coordinate is never this long (the longest accepted case is 26 characters). Counted in
/// characters, as Python's `len()` counts them, not bytes.
pub const MAX_QUERY: usize = 120;

const DEG: &str = r"[0-9]{1,3}(?:\.[0-9]{1,12})?";
const MINSEC: &str = r"[0-9]{1,2}(?:\.[0-9]{1,12})?";

/// One half of a coordinate: an optional hemisphere letter on either side, an optional sign,
/// degrees, and minutes and seconds only when their unit marks are present. Every gap is a single
/// optional space; whitespace runs are collapsed before matching.
fn component(i: u8) -> String {
    format!(
        r#"
        (?:(?P<hema{i}>[NSEW])\ ?)?
        (?P<sign{i}>[+-])?\ ?
        (?P<deg{i}>{DEG})\ ?(?P<degmark{i}>[°º])?\ ?
        (?:(?P<min{i}>{MINSEC})\ ?['′]\ ?
           (?:(?P<sec{i}>{MINSEC})\ ?(?:"|″|'')\ ?)?
        )?
        (?P<hemb{i}>[NSEW])?
        "#
    )
}

fn coord_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // The separator is never empty (so `5130` is not 51°, 30°), and `/` is not one.
        let pat = format!(r"(?xi)^{}(?:\ ?[,;]\ ?|\ ){}$", component(1), component(2));
        Regex::new(&pat).expect("coordinate pattern compiles")
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Lat,
    Lon,
}

fn axis(hem: Option<char>) -> Option<Axis> {
    match hem? {
        'N' | 'S' => Some(Axis::Lat),
        'E' | 'W' => Some(Axis::Lon),
        _ => None,
    }
}

/// Uppercase a matched hemisphere letter. The match is case-insensitive with Unicode simple case
/// folding, so `ſ` (long s) arrives here too and must come out as `S`, as Python's `.upper()` does.
fn upper_hem(s: &str) -> Option<char> {
    s.chars().flat_map(char::to_uppercase).next()
}

/// One component as (signed degrees, hemisphere, carries-a-coordinate-mark), or `None` when a
/// human would have to guess what was meant.
fn value(m: &Captures, i: u8) -> Option<(f64, Option<char>, bool)> {
    let g = |name: &str| m.name(&format!("{name}{i}")).map(|x| x.as_str());
    let (hema, hemb) = (g("hema"), g("hemb"));
    if hema.is_some() && hemb.is_some() {
        return None;
    }
    let hem = hema.or(hemb).and_then(upper_hem);
    let sign = g("sign");
    if sign.is_some() && hem.is_some() {
        return None;
    }
    let raw_deg = g("deg")?;
    let mut deg: f64 = raw_deg.parse().ok()?;
    let (minutes, seconds) = (g("min"), g("sec"));
    if let Some(min) = minutes {
        if deg.fract() != 0.0 {
            return None;
        }
        let min: f64 = min.parse().ok()?;
        if min >= 60.0 {
            return None;
        }
        deg += min / 60.0;
    }
    if let Some(sec) = seconds {
        let sec: f64 = sec.parse().ok()?;
        if sec >= 60.0 {
            return None;
        }
        deg += sec / 3600.0;
    }
    if sign == Some("-") || matches!(hem, Some('S' | 'W')) {
        deg = -deg;
    }
    let marked = sign.is_some()
        || hem.is_some()
        || g("degmark").is_some()
        || minutes.is_some()
        || raw_deg.contains('.');
    Some((deg, hem, marked))
}

/// Read a typed coordinate as a list of (lat, lon) readings, best first.
///
/// Empty means "not a coordinate, ask the gazetteer". One reading means the input said which
/// number was which. Two means it did not, and both in-range readings come back, conventional
/// lat-lon order first, so a GeoJSON `[lon, lat]` paste is never silently misread.
///
/// Every component must carry a mark that says "coordinate" (a decimal point, a sign, a
/// hemisphere letter, or a degree/minute/second symbol), so postcodes like `110 00` and
/// half-typed names like `N1 9` are refused.
pub fn parse_coords(raw: &str) -> Vec<(f64, f64)> {
    if raw.chars().count() > MAX_QUERY {
        return Vec::new();
    }
    let collapsed = split_whitespace_like_python(raw).join(" ");
    let Some(m) = coord_re().captures(&collapsed) else {
        return Vec::new();
    };
    let (Some((v1, h1, marked1)), Some((v2, h2, marked2))) = (value(&m, 1), value(&m, 2)) else {
        return Vec::new();
    };
    if !(marked1 && marked2) {
        return Vec::new();
    }

    let orders: Vec<(f64, f64)> = match (axis(h1), axis(h2)) {
        (Some(a1), Some(a2)) if a1 == a2 => return Vec::new(),
        (Some(a1), Some(_)) => vec![if a1 == Axis::Lat { (v1, v2) } else { (v2, v1) }],
        (Some(a1), None) => vec![if a1 == Axis::Lat { (v1, v2) } else { (v2, v1) }],
        (None, Some(a2)) => vec![if a2 == Axis::Lon { (v1, v2) } else { (v2, v1) }],
        (None, None) => vec![(v1, v2), (v2, v1)],
    };

    let mut readings: Vec<(f64, f64)> = Vec::with_capacity(2);
    for (lat, lon) in orders {
        if lat.abs() > 90.0 || lon.abs() > 180.0 {
            continue;
        }
        let point = (lat + 0.0, lon + 0.0); // + 0.0 turns -0.0 into 0.0
        if !readings.contains(&point) {
            readings.push(point);
        }
    }
    readings
}

/// Python's `str.split()` with no argument. It treats a few more characters as whitespace than
/// Unicode's White_Space property (and so Rust's `split_whitespace`): the ASCII information
/// separators U+001C..U+001F.
fn split_whitespace_like_python(s: &str) -> Vec<&str> {
    s.split(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .filter(|w| !w.is_empty())
        .collect()
}

/// The gazetteer's `name_fold`: lowercase, canonical decomposition, combining marks removed.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .nfd()
        .filter(|&c| canonical_combining_class(c) == 0)
        .collect()
}

/// Turn arbitrary user text into one quoted FTS5 phrase with a prefix star, or `None` when there
/// is nothing to search for. Only alphanumeric runs survive, so no operator (`"`, `*`, `^`,
/// `NEAR`, `OR`) can reach the match expression as syntax.
pub fn fts_query(raw: &str) -> Option<String> {
    let tokens: Vec<&str> = raw
        .split(|c: char| !is_python_word_alnum(c))
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    Some(format!("\"{}\"*", tokens.join(" ")))
}

/// Python's `[^\W_]` under `re.UNICODE`: a character that is alphanumeric by Python's `isalnum()`.
/// Rust's `char::is_alphanumeric` is close but counts Other_Alphabetic marks (e.g. Indic vowel
/// signs) as letters where Python does not; the differential test measures that gap.
fn is_python_word_alnum(c: char) -> bool {
    c.is_alphanumeric()
}

/// A parsed coordinate as the same feature shape a place returns. The labels state the reading,
/// so two readings of one input are told apart in the result list.
pub fn coord_feature(lat: f64, lon: f64) -> serde_json::Value {
    let label = format!(
        "{:.5}° {}, {:.5}° {}",
        lat.abs(),
        if lat >= 0.0 { 'N' } else { 'S' },
        lon.abs(),
        if lon >= 0.0 { 'E' } else { 'W' }
    );
    serde_json::json!({
        "type": "Feature",
        "id": format!("coord.{lat:.5},{lon:.5}"),
        "text": format!("{lat:.5}, {lon:.5}"),
        "place_name": label,
        "place_type": ["coordinate"],
        "center": [lon, lat],
        "geometry": {"type": "Point", "coordinates": [lon, lat]},
        "properties": {"source": "coordinate", "lat": lat, "lon": lon},
    })
}
