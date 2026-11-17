//! The gazetteer search, ported from `serve.py`'s `search()`: a coordinate is answered directly,
//! anything else becomes an FTS5 phrase-prefix query over the GeoNames index.
//!
//! The answers are Carmen GeoJSON features, the shape `@maplibre/maplibre-gl-geocoder` expects.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};

/// Results returned when the caller does not ask for a number.
pub const DEFAULT_LIMIT: usize = 8;
/// The most results one search returns; a larger request is clamped to this.
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

/// Open the index read-only and immutable.
///
/// Nothing writes through this file while it is open (a rebuild replaces it whole), so SQLite may
/// skip locking.
///
/// # Errors
///
/// SQLite's error when the file cannot be opened (missing or unreadable). A file that is not a
/// database is only found out by the first query.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    // a path in a URI: '%', '?' and '#' would start an escape, the query or a fragment
    let escaped = path
        .display()
        .to_string()
        .replace('%', "%25")
        .replace('?', "%3f")
        .replace('#', "%23");
    let uri = format!("file:{escaped}?mode=ro&immutable=1");
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
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
        return points
            .into_iter()
            .take(limit)
            .map(|(lat, lon)| coords::coord_feature(lat, lon))
            .collect();
    }
    let Some(matcher) = coords::fts_query(raw) else {
        return Vec::new();
    };
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

#[cfg(test)]
mod tests {
    //! A dozen-row gazetteer with the production schema (`place` + the external-content
    //! `place_fts`, `unicode61 remove_diacritics 2`), so nothing here reads `data/geonames.db`.
    //! `name_fold` is written as literals, not through `coords::fold`, so a broken fold cannot
    //! agree with itself.

    use super::*;
    use std::path::PathBuf;

    const SCHEMA: &str = "
CREATE TABLE place (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    name_fold  TEXT    NOT NULL,
    lat        REAL    NOT NULL,
    lon        REAL    NOT NULL,
    fclass     TEXT,
    fcode      TEXT,
    country    TEXT,
    admin1     TEXT,
    population INTEGER NOT NULL DEFAULT 0,
    score      INTEGER NOT NULL DEFAULT 0
);
CREATE VIRTUAL TABLE place_fts USING fts5(
    name,
    content      = 'place',
    content_rowid= 'id',
    tokenize     = \"unicode61 remove_diacritics 2\"
);";

    type Row = (
        i64,
        &'static str,
        &'static str,
        f64,
        f64,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        i64,
        i64,
    );

    /// Springfield 6 is inserted before 5, so an id tie-break is not just insertion order.
    const ROWS: &[Row] = &[
        (
            1,
            "London",
            "london",
            51.50853,
            -0.12574,
            Some("P"),
            Some("PPLC"),
            Some("GB"),
            Some("ENG"),
            8961989,
            100,
        ),
        (
            2,
            "Londonderry",
            "londonderry",
            54.9981,
            -7.30934,
            Some("P"),
            Some("PPLA2"),
            Some("GB"),
            Some("NIR"),
            85016,
            50,
        ),
        (
            3,
            "London",
            "london",
            42.98339,
            -81.23304,
            Some("P"),
            Some("PPL"),
            Some("CA"),
            Some("08"),
            346765,
            60,
        ),
        (
            4,
            "New London",
            "new london",
            41.35565,
            -72.09952,
            Some("P"),
            Some("PPL"),
            Some("US"),
            Some("CT"),
            27179,
            70,
        ),
        (
            6,
            "Springfield",
            "springfield",
            37.21533,
            -93.29824,
            Some("P"),
            Some("PPL"),
            Some("US"),
            Some("MO"),
            169176,
            40,
        ),
        (
            5,
            "Springfield",
            "springfield",
            39.80172,
            -89.64371,
            Some("P"),
            Some("PPLA"),
            Some("US"),
            Some("IL"),
            116250,
            40,
        ),
        (
            7,
            "Reykjavík",
            "reykjavik",
            64.13548,
            -21.89541,
            Some("P"),
            Some("PPLC"),
            Some("IS"),
            Some("39"),
            118918,
            30,
        ),
        (
            8,
            "Reykjavík Airport",
            "reykjavik airport",
            64.13,
            -21.94,
            Some("S"),
            Some("AIRP"),
            Some("IS"),
            Some("39"),
            0,
            90,
        ),
        (
            9,
            "Lake Tahoe",
            "lake tahoe",
            39.09,
            -120.04,
            Some("H"),
            Some("LK"),
            Some("US"),
            Some("CA"),
            0,
            20,
        ),
        (
            10,
            "Near Sawrey",
            "near sawrey",
            54.35,
            -2.97,
            Some("P"),
            Some("PPL"),
            Some("GB"),
            Some("ENG"),
            0,
            5,
        ),
        (
            11, "Nowhere", "nowhere", 0.5, 0.25, None, None, None, None, 0, 1,
        ),
        (
            12,
            "Mont Blanc",
            "mont blanc",
            45.83,
            6.86,
            Some("T"),
            Some("MT"),
            Some("FR"),
            Some(""),
            0,
            10,
        ),
    ];

    fn load(db: &Connection, rows: &[Row]) {
        db.execute_batch(SCHEMA).unwrap();
        for r in rows {
            db.execute(
                "INSERT INTO place VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                rusqlite::params![r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10],
            )
            .unwrap();
        }
        db.execute("INSERT INTO place_fts(place_fts) VALUES('rebuild')", [])
            .unwrap();
    }

    fn gazetteer() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        load(&db, ROWS);
        db
    }

    /// 30 rows named `Zed 01`..`Zed 30`, score = id, for the limit clamp.
    fn zeds() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(SCHEMA).unwrap();
        for i in 1..=30i64 {
            db.execute(
                "INSERT INTO place (id, name, name_fold, lat, lon, score) VALUES (?1, ?2, ?3, 0, 0, ?1)",
                rusqlite::params![i, format!("Zed {i:02}"), format!("zed {i:02}")],
            )
            .unwrap();
        }
        db.execute("INSERT INTO place_fts(place_fts) VALUES('rebuild')", [])
            .unwrap();
        db
    }

    fn ids(features: &[Value]) -> Vec<i64> {
        features
            .iter()
            .map(|f| {
                f["properties"]["geonameid"]
                    .as_i64()
                    .expect("a gazetteer feature")
            })
            .collect()
    }

    fn q(raw: &str) -> Vec<i64> {
        ids(&search(&gazetteer(), raw, DEFAULT_LIMIT))
    }

    // ---- ranking ------------------------------------------------------------------------------

    #[test]
    fn exact_name_first_then_score_descending() {
        // exact `london`: 1 (score 100), 3 (60); then prefix-only: New London (70), Londonderry (50)
        assert_eq!(q("london"), vec![1, 3, 4, 2]);
    }

    #[test]
    fn equal_exact_and_score_tie_breaks_on_id_ascending() {
        assert_eq!(q("springfield"), vec![5, 6]);
    }

    #[test]
    fn uppercase_query_is_still_an_exact_match() {
        assert_eq!(q("LONDON"), vec![1, 3, 4, 2]);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed_before_the_exact_match() {
        assert_eq!(q("  london \t"), vec![1, 3, 4, 2]);
    }

    #[test]
    fn exact_match_beats_a_higher_score_prefix_match() {
        // the airport scores 90, the city 30
        assert_eq!(q("reykjavik"), vec![7, 8]);
    }

    #[test]
    fn accented_query_folds_to_the_exact_match() {
        assert_eq!(q("Reykjavík"), vec![7, 8]);
    }

    #[test]
    fn accented_uppercase_query_folds_to_the_exact_match() {
        assert_eq!(q("REYKJAVÍK"), vec![7, 8]);
    }

    #[test]
    fn a_phrase_prefix_matches_mid_name() {
        assert_eq!(q("tahoe"), vec![9]);
    }

    #[test]
    fn a_two_word_prefix_matches() {
        assert_eq!(q("lake tah"), vec![9]);
    }

    #[test]
    fn a_word_order_the_name_lacks_matches_nothing() {
        assert_eq!(q("tahoe lake"), Vec::<i64>::new());
    }

    // ---- limit --------------------------------------------------------------------------------

    #[test]
    fn limit_truncates_after_ranking() {
        assert_eq!(ids(&search(&gazetteer(), "london", 2)), vec![1, 3]);
    }

    #[test]
    fn limit_zero_is_clamped_to_one() {
        assert_eq!(ids(&search(&gazetteer(), "london", 0)), vec![1]);
    }

    #[test]
    fn limit_above_max_is_clamped_to_max() {
        let got = ids(&search(&zeds(), "zed", 100));
        assert_eq!(got, (6..=30).rev().collect::<Vec<i64>>());
        assert_eq!(got.len(), MAX_LIMIT);
    }

    #[test]
    fn limit_exactly_max_is_not_clamped_lower() {
        assert_eq!(ids(&search(&zeds(), "zed", 25)).len(), 25);
    }

    // ---- nothing to search for ----------------------------------------------------------------

    #[test]
    fn empty_query_returns_nothing() {
        assert_eq!(search(&gazetteer(), "", DEFAULT_LIMIT), Vec::<Value>::new());
    }

    #[test]
    fn whitespace_query_returns_nothing() {
        assert_eq!(
            search(&gazetteer(), " \t\n ", DEFAULT_LIMIT),
            Vec::<Value>::new()
        );
    }

    #[test]
    fn operator_only_query_returns_nothing() {
        assert_eq!(
            search(&gazetteer(), "\"*()-^", DEFAULT_LIMIT),
            Vec::<Value>::new()
        );
    }

    // ---- FTS5 syntax in the query must not reach MATCH as syntax --------------------------------
    // Every case here matches rows, so an SQL error (swallowed into an empty list) cannot pass.
    // Punctuation is stripped from the MATCH but not from the exact-name comparison, so
    // `"london` is not an exact match for `london`: the score order applies (1, 4, 3, 2).

    #[test]
    fn leading_double_quote() {
        assert_eq!(q("\"london"), vec![1, 4, 3, 2]);
    }

    #[test]
    fn unbalanced_double_quote_mid_word() {
        assert_eq!(q("lake\" tahoe"), vec![9]);
    }

    #[test]
    fn trailing_star() {
        assert_eq!(q("london*"), vec![1, 4, 3, 2]);
    }

    #[test]
    fn leading_minus() {
        assert_eq!(q("-london"), vec![1, 4, 3, 2]);
    }

    #[test]
    fn unbalanced_open_parenthesis() {
        assert_eq!(q("(london"), vec![1, 4, 3, 2]);
    }

    #[test]
    fn unbalanced_close_parentheses() {
        assert_eq!(q("london))("), vec![1, 4, 3, 2]);
    }

    #[test]
    fn caret_and_column_filter_syntax() {
        assert_eq!(q("^name:london"), Vec::<i64>::new());
        assert_eq!(q("{name}: lake tahoe"), Vec::<i64>::new());
        assert_eq!(q("^lake tahoe"), vec![9]);
    }

    #[test]
    fn near_is_a_word_not_an_operator() {
        assert_eq!(q("NEAR"), vec![10]);
    }

    #[test]
    fn near_with_parenthesis_is_a_word_not_an_operator() {
        assert_eq!(q("NEAR(saw"), vec![10]);
    }

    #[test]
    fn uppercase_or_and_not_are_words_not_operators() {
        // as operators, `london OR new` would match 1-4; as a phrase it matches nothing
        assert_eq!(q("london OR new"), Vec::<i64>::new());
        assert_eq!(q("new AND london"), Vec::<i64>::new());
        assert_eq!(q("lake NOT tahoe"), Vec::<i64>::new());
    }

    // ---- the feature shape --------------------------------------------------------------------

    #[test]
    fn a_place_feature_is_exact() {
        let got = search(&gazetteer(), "london", 1);
        assert_eq!(
            got,
            vec![json!({
                "type": "Feature",
                "id": "geonames.1",
                "text": "London",
                "place_name": "London, ENG, GB",
                "place_type": ["place"],
                "center": [-0.12574, 51.50853],
                "geometry": {"type": "Point", "coordinates": [-0.12574, 51.50853]},
                "properties": {
                    "geonameid": 1, "feature_class": "P", "feature_code": "PPLC",
                    "country_code": "GB", "population": 8961989,
                },
            })]
        );
    }

    #[test]
    fn a_numeric_admin1_is_dropped_from_the_place_name() {
        assert_eq!(
            search(&gazetteer(), "london", 2)[1]["place_name"],
            "London, CA"
        );
    }

    #[test]
    fn an_empty_admin1_is_dropped_from_the_place_name() {
        assert_eq!(
            search(&gazetteer(), "mont", 1)[0]["place_name"],
            "Mont Blanc, FR"
        );
    }

    #[test]
    fn null_class_country_and_admin1_are_null_and_default_to_place() {
        let got = search(&gazetteer(), "nowhere", 1);
        assert_eq!(got[0]["place_name"], "Nowhere");
        assert_eq!(got[0]["place_type"], json!(["place"]));
        assert_eq!(
            got[0]["properties"],
            json!({
                "geonameid": 11, "feature_class": null, "feature_code": null,
                "country_code": null, "population": 0,
            })
        );
    }

    #[test]
    fn feature_class_labels_reach_the_feature() {
        assert_eq!(
            search(&gazetteer(), "lake", 1)[0]["place_type"],
            json!(["water"])
        );
        assert_eq!(
            search(&gazetteer(), "reykjavik airport", 1)[0]["place_type"],
            json!(["site"])
        );
        assert_eq!(
            search(&gazetteer(), "mont blanc", 1)[0]["place_type"],
            json!(["landform"])
        );
    }

    #[test]
    fn fclass_label_table() {
        let table = [
            ("A", "region"),
            ("H", "water"),
            ("L", "area"),
            ("P", "place"),
            ("R", "road"),
            ("S", "site"),
            ("T", "landform"),
            ("U", "undersea"),
            ("V", "forest"),
            ("", "place"),
            ("X", "place"),
            ("p", "place"),
        ];
        for (code, label) in table {
            assert_eq!(fclass_label(code), label, "fclass {code:?}");
        }
    }

    #[test]
    fn place_name_admin1_rules() {
        assert_eq!(place_name("A", Some("1"), Some("IS")), "A, IS");
        assert_eq!(place_name("A", Some("1A"), Some("IS")), "A, 1A, IS");
        assert_eq!(place_name("A", Some("ENG"), None), "A, ENG");
        assert_eq!(place_name("A", None, Some("")), "A");
        // a non-ASCII digit is not "a bare numeric code"
        assert_eq!(place_name("A", Some("١"), Some("EG")), "A, ١, EG");
    }

    // ---- coordinates are answered without the database ----------------------------------------

    /// No tables at all: any SQL would fail, so a coordinate answer proves no query ran.
    fn empty_db() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn an_ambiguous_coordinate_returns_both_readings_in_order() {
        assert_eq!(
            search(&empty_db(), "51.5074, -0.1278", DEFAULT_LIMIT),
            vec![
                coords::coord_feature(51.5074, -0.1278),
                coords::coord_feature(-0.1278, 51.5074)
            ]
        );
    }

    #[test]
    fn the_limit_applies_to_coordinate_readings() {
        assert_eq!(
            search(&empty_db(), "51.5074, -0.1278", 1),
            vec![coords::coord_feature(51.5074, -0.1278)]
        );
    }

    #[test]
    fn a_lettered_coordinate_returns_one_reading_even_with_a_gazetteer() {
        assert_eq!(
            search(&gazetteer(), "51.5074N 0.1278W", DEFAULT_LIMIT),
            vec![coords::coord_feature(51.5074, -0.1278)]
        );
    }

    // ---- fail closed --------------------------------------------------------------------------

    #[test]
    fn a_missing_index_returns_nothing_instead_of_panicking() {
        assert_eq!(
            search(&empty_db(), "london", DEFAULT_LIMIT),
            Vec::<Value>::new()
        );
    }

    // ---- open ---------------------------------------------------------------------------------

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../logs/search-tests")
            .join(format!("{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_db(path: &Path) {
        let _ = std::fs::remove_file(path);
        let db = Connection::open(path).unwrap();
        load(&db, ROWS);
    }

    #[test]
    fn open_reads_a_file_index() {
        let dir = scratch("open");
        let path = dir.join("g.db");
        write_db(&path);
        let db = open(&path).unwrap();
        assert_eq!(ids(&search(&db, "london", DEFAULT_LIMIT)), vec![1, 3, 4, 2]);
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_is_read_only() {
        let dir = scratch("ro");
        let path = dir.join("g.db");
        write_db(&path);
        let db = open(&path).unwrap();
        let err = db.execute("DELETE FROM place", []).unwrap_err();
        assert!(err.to_string().contains("readonly"), "{err}");
        drop(db);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_of_a_missing_file_is_an_error() {
        let dir = scratch("missing");
        assert!(open(&dir.join("absent.db")).is_err());
        assert!(!dir.join("absent.db").exists(), "open created the file");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `#`, `?` and `%` are legal in a Linux path but delimit or escape a URI; `open` formats the
    /// path into `file:{path}?mode=ro&immutable=1` unescaped.
    fn open_in_dir_named(name: &str) -> Option<Vec<i64>> {
        let dir = scratch(name);
        let path = dir.join("g.db");
        write_db(&path);
        let got = open(&path).map(|db| ids(&search(&db, "london", DEFAULT_LIMIT)));
        std::fs::remove_dir_all(&dir).unwrap();
        got.ok()
    }

    #[test]
    fn open_handles_a_space_in_the_path() {
        assert_eq!(open_in_dir_named("a b"), Some(vec![1, 3, 4, 2]));
    }

    #[test]
    fn open_handles_a_hash_in_the_path() {
        assert_eq!(open_in_dir_named("a#b"), Some(vec![1, 3, 4, 2]));
    }

    #[test]
    fn open_handles_a_question_mark_in_the_path() {
        assert_eq!(open_in_dir_named("a?b"), Some(vec![1, 3, 4, 2]));
    }

    #[test]
    fn open_handles_a_percent_escape_in_the_path() {
        assert_eq!(open_in_dir_named("a%41b"), Some(vec![1, 3, 4, 2]));
    }
}
