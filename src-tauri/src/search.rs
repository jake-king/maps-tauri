//! The gazetteer search, ported from `serve.py`'s `search()`: a coordinate is answered directly,
//! anything else becomes an FTS5 phrase-prefix query over the GeoNames index.
//!
//! The answers are Carmen GeoJSON features, the shape `@maplibre/maplibre-gl-geocoder` expects.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 25;

/// Exact folded-name match first, then the precomputed notability score, then the id so equal
/// rows never reorder between identical queries. Both ranking terms are columns the index
/// builder computed.
const SEARCH_SQL: &str = "
SELECT p.id, p.name, p.lat, p.lon, p.fclass, p.fcode, p.country, p.admin1, p.population
  FROM place_fts f JOIN place p ON p.id = f.rowid
 WHERE place_fts MATCH ?1
 ORDER BY (p.name_fold = ?2) DESC, p.score DESC, p.id ASC
 LIMIT ?3";

/// Open the index read-only and immutable: nothing writes through this file while it is open
/// (a rebuild replaces it whole), so SQLite may skip locking.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let uri = format!("file:{}?mode=ro&immutable=1", path.display());
    Connection::open_with_flags(uri, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI)
}

fn fclass_label(fclass: &str) -> &'static str {
    match fclass {
        "A" => "region",
        "H" => "water",
        "L" => "area",
        "P" => "place",
        "R" => "road",
        "S" => "site",
        "T" => "landform",
        "U" => "undersea",
        "V" => "forest",
        _ => "place",
    }
}

/// `Reykjavík, 1, IS` reads badly: drop an admin1 that is a bare numeric code.
fn place_name(name: &str, admin1: Option<&str>, country: Option<&str>) -> String {
    let mut parts = vec![name];
    if let Some(a) = admin1.filter(|a| !a.is_empty() && !a.chars().all(|c| c.is_ascii_digit())) {
        parts.push(a);
    }
    if let Some(c) = country.filter(|c| !c.is_empty()) {
        parts.push(c);
    }
    parts.join(", ")
}

/// Answer one search-box query, at most `limit` features.
pub fn search(db: &Connection, raw: &str, limit: usize) -> Vec<Value> {
    let limit = limit.clamp(1, MAX_LIMIT);
    let points = coords::parse_coords(raw);
    if !points.is_empty() {
        return points.into_iter().take(limit).map(|(lat, lon)| coords::coord_feature(lat, lon)).collect();
    }
    let Some(matcher) = coords::fts_query(raw) else { return Vec::new() };
    let folded = coords::fold(raw.trim());
    let run = || -> rusqlite::Result<Vec<Value>> {
        let mut stmt = db.prepare_cached(SEARCH_SQL)?;
        let rows = stmt.query_map((&matcher, &folded, limit as i64), |r| {
            let (gid, name): (i64, String) = (r.get(0)?, r.get(1)?);
            let (lat, lon): (f64, f64) = (r.get(2)?, r.get(3)?);
            let fclass: Option<String> = r.get(4)?;
            let fcode: Option<String> = r.get(5)?;
            let country: Option<String> = r.get(6)?;
            let admin1: Option<String> = r.get(7)?;
            let population: i64 = r.get(8)?;
            Ok(json!({
                "type": "Feature",
                "id": format!("geonames.{gid}"),
                "text": name,
                "place_name": place_name(&name, admin1.as_deref(), country.as_deref()),
                "place_type": [fclass_label(fclass.as_deref().unwrap_or(""))],
                "center": [lon, lat],
                "geometry": {"type": "Point", "coordinates": [lon, lat]},
                "properties": {
                    "geonameid": gid, "feature_class": fclass, "feature_code": fcode,
                    "country_code": country, "population": population,
                },
            }))
        })?;
        rows.collect()
    };
    // fts_query admits no operator syntax, so a refused MATCH should be impossible; if one ever
    // happens, an empty list beats an error that blanks the search box.
    run().unwrap_or_else(|e| {
        eprintln!("gazetteer: MATCH refused {matcher:?}: {e}");
        Vec::new()
    })
}
