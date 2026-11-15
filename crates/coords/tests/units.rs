//! Exact-output cases for the paths `cases.rs` does not reach: `fold`, `fts_query`,
//! `coord_feature`, and the parser's rarer branches (doubled or lowercase hemisphere letters,
//! Python-only whitespace, sign-only and mark-only components, dedupe, the length-cap boundary).

use coords::{coord_feature, fold, fts_query, parse_coords, MAX_QUERY};
use serde_json::json;

const G: (f64, f64) = (51.5074, -0.1278);
const GR: (f64, f64) = (-0.1278, 51.5074);

// ---- fold -------------------------------------------------------------------------------------

#[test]
fn fold_lowercases_and_strips_acute() {
    assert_eq!(fold("Reykjavík"), "reykjavik");
}

#[test]
fn fold_strips_ring_and_diaeresis_from_uppercase() {
    assert_eq!(fold("ÅNGSTRÖM"), "angstrom");
}

#[test]
fn fold_drops_the_dot_that_lowercasing_dotted_capital_i_adds() {
    // `İ`.to_lowercase() is `i` + U+0307; the combining dot must not survive.
    assert_eq!(fold("İstanbul"), "istanbul");
}

#[test]
fn fold_lowercases_but_does_not_casefold_sharp_s() {
    // Python's str.lower(), not str.casefold(): ß stays ß.
    assert_eq!(fold("Straße"), "straße");
}

#[test]
fn fold_leaves_hangul_decomposed_into_jamo() {
    // NFD splits each syllable into conjoining jamo (combining class 0), as Python's builder does.
    assert_eq!(fold("서울"), "\u{1109}\u{1165}\u{110B}\u{116E}\u{11AF}");
}

#[test]
fn fold_keeps_whitespace_and_punctuation() {
    assert_eq!(fold("  Saint-Étienne "), "  saint-etienne ");
}

// ---- fts_query --------------------------------------------------------------------------------

#[test]
fn fts_query_of_empty_text_is_none() {
    assert_eq!(fts_query(""), None);
}

#[test]
fn fts_query_of_whitespace_is_none() {
    assert_eq!(fts_query(" \t\n "), None);
}

#[test]
fn fts_query_of_operator_characters_only_is_none() {
    assert_eq!(fts_query("\"*()-^:+_"), None);
}

#[test]
fn fts_query_quotes_one_word_and_adds_a_prefix_star() {
    assert_eq!(fts_query("london").as_deref(), Some("\"london\"*"));
}

#[test]
fn fts_query_collapses_whitespace_runs_and_keeps_case() {
    assert_eq!(
        fts_query("  Santa   Mon ").as_deref(),
        Some("\"Santa Mon\"*")
    );
}

#[test]
fn fts_query_drops_a_stray_double_quote() {
    assert_eq!(fts_query("\"london").as_deref(), Some("\"london\"*"));
}

#[test]
fn fts_query_drops_an_embedded_double_quote() {
    assert_eq!(fts_query("lon\"don").as_deref(), Some("\"lon don\"*"));
}

#[test]
fn fts_query_drops_star_caret_and_parentheses() {
    assert_eq!(fts_query("^(london*").as_deref(), Some("\"london\"*"));
}

#[test]
fn fts_query_keeps_near_as_a_word_inside_the_phrase() {
    assert_eq!(fts_query("NEAR(a b)").as_deref(), Some("\"NEAR a b\"*"));
}

#[test]
fn fts_query_splits_on_hyphen_and_apostrophe() {
    assert_eq!(fts_query("o'brien-x").as_deref(), Some("\"o brien x\"*"));
}

#[test]
fn fts_query_splits_on_underscore_like_python_non_word_alnum() {
    assert_eq!(fts_query("snake_case").as_deref(), Some("\"snake case\"*"));
}

#[test]
fn fts_query_keeps_digits_accents_and_cjk() {
    assert_eq!(
        fts_query("Route 66 Reykjavík 東京").as_deref(),
        Some("\"Route 66 Reykjavík 東京\"*")
    );
}

// ---- coord_feature ----------------------------------------------------------------------------

#[test]
fn coord_feature_north_west_is_exact() {
    assert_eq!(
        coord_feature(51.5074, -0.1278),
        json!({
            "type": "Feature",
            "id": "coord.51.50740,-0.12780",
            "text": "51.50740, -0.12780",
            "place_name": "51.50740° N, 0.12780° W",
            "place_type": ["coordinate"],
            "center": [-0.1278, 51.5074],
            "geometry": {"type": "Point", "coordinates": [-0.1278, 51.5074]},
            "properties": {"source": "coordinate", "lat": 51.5074, "lon": -0.1278},
        })
    );
}

#[test]
fn coord_feature_south_east_label() {
    assert_eq!(
        coord_feature(-33.8688, 151.2093)["place_name"],
        "33.86880° S, 151.20930° E"
    );
}

#[test]
fn coord_feature_zero_is_north_and_east() {
    let f = coord_feature(0.0, 0.0);
    assert_eq!(f["place_name"], "0.00000° N, 0.00000° E");
    assert_eq!(f["id"], "coord.0.00000,0.00000");
}

#[test]
fn coord_feature_rounds_to_five_places() {
    let f = coord_feature(51.507389, -0.127806);
    assert_eq!(f["text"], "51.50739, -0.12781");
    assert_eq!(f["properties"]["lat"], 51.507389);
}

// ---- parse_coords: rarer branches -------------------------------------------------------------

#[test]
fn a_hemisphere_letter_on_both_sides_of_one_half_is_refused() {
    assert_eq!(parse_coords("N51.5074N 0.1278W"), vec![]);
}

#[test]
fn lowercase_hemisphere_letters_are_read() {
    assert_eq!(parse_coords("51.5074n 0.1278w"), vec![G]);
}

#[test]
fn long_s_is_read_as_south_like_python_upper() {
    assert_eq!(
        parse_coords("51.5074\u{17F} 0.1278w"),
        vec![(-51.5074, -0.1278)]
    );
}

#[test]
fn leading_letter_on_first_half_and_trailing_on_second() {
    assert_eq!(parse_coords("N 51.5074 0.1278 W"), vec![G]);
}

#[test]
fn ascii_information_separator_is_whitespace_like_python() {
    // U+001C is whitespace to Python's str.split() but not to Rust's char::is_whitespace.
    assert_eq!(parse_coords("51.5074\u{1C}-0.1278"), vec![G, GR]);
}

#[test]
fn semicolon_is_a_separator() {
    assert_eq!(parse_coords("51.5074; -0.1278"), vec![G, GR]);
}

#[test]
fn negative_zero_comes_back_as_positive_zero() {
    let got = parse_coords("-0.0, 0.0");
    assert_eq!(got, vec![(0.0, 0.0)]);
    assert!(got[0].0.is_sign_positive(), "lat is -0.0: {got:?}");
}

#[test]
fn identical_readings_are_returned_once() {
    assert_eq!(parse_coords("1.5, 1.5"), vec![(1.5, 1.5)]);
}

#[test]
fn a_degree_sign_alone_marks_an_integer_component() {
    assert_eq!(parse_coords("51° 0°"), vec![(51.0, 0.0), (0.0, 51.0)]);
}

#[test]
fn the_ordinal_indicator_is_a_degree_sign() {
    assert_eq!(parse_coords("51º 0º"), vec![(51.0, 0.0), (0.0, 51.0)]);
}

#[test]
fn a_minute_mark_alone_marks_an_integer_component() {
    assert_eq!(
        parse_coords("51 30' 0 7'"),
        vec![(51.5, 7.0 / 60.0), (7.0 / 60.0, 51.5)]
    );
}

#[test]
fn a_plus_sign_alone_marks_an_integer_component() {
    assert_eq!(parse_coords("+51 +0"), vec![(51.0, 0.0), (0.0, 51.0)]);
}

#[test]
fn doubled_apostrophe_is_a_seconds_mark() {
    assert_eq!(
        parse_coords("51°30'26.6''N 0°7'40.1''W"),
        vec![(
            51.0 + 30.0 / 60.0 + 26.6 / 3600.0,
            -(7.0 / 60.0 + 40.1 / 3600.0)
        )]
    );
}

#[test]
fn unicode_prime_and_double_prime_are_minute_and_second_marks() {
    assert_eq!(
        parse_coords("51°30′26.6″N 0°7′40.1″W"),
        vec![(
            51.0 + 30.0 / 60.0 + 26.6 / 3600.0,
            -(7.0 / 60.0 + 40.1 / 3600.0)
        )]
    );
}

#[test]
fn length_cap_boundary_is_inclusive() {
    let base = "51.5074, -0.1278";
    let at_cap = format!("{base}{}", " ".repeat(MAX_QUERY - base.len()));
    assert_eq!(at_cap.chars().count(), 120);
    assert_eq!(parse_coords(&at_cap), vec![G, GR]);
    let over = format!("{at_cap} ");
    assert_eq!(parse_coords(&over), vec![]);
}

#[test]
fn ninety_and_one_eighty_are_in_range() {
    assert_eq!(parse_coords("90.0N 180.0W"), vec![(90.0, -180.0)]);
}
