//! The coordinate test tables from the Python service's selftest (`maps-search-selftest.py`),
//! ported as-is. Every accepted notation names the Greenwich observatory, ~51.5074 N 0.1278 W.

use std::time::{Duration, Instant};

use coords::parse_coords;

const G: (f64, f64) = (51.5074, -0.1278);
const GR: (f64, f64) = (-0.1278, 51.5074);

fn accepted() -> Vec<(&'static str, Vec<(f64, f64)>)> {
    vec![
        // unlettered, so ambiguous: lat-lon first, the other reading offered
        ("51.5074, -0.1278", vec![G, GR]),
        ("51.5074 -0.1278", vec![G, GR]),
        ("51.5074,-0.1278", vec![G, GR]),
        ("  +51.5074 , -0.1278  ", vec![G, GR]),
        ("51.5074° -0.1278°", vec![G, GR]),
        ("51.5074 ,   -0.1278", vec![G, GR]),
        // a hemisphere letter names the axis: one reading
        ("51.5074N 0.1278W", vec![G]),
        ("51.5074° N, 0.1278° W", vec![G]),
        ("N 51.5074, W 0.1278", vec![G]),
        ("0.1278W 51.5074N", vec![G]),
        ("51°30'26.6\"N 0°7'40.1\"W", vec![(51.507389, -0.127806)]),
        (
            "51 30' 26.6\" N, 0 7' 40.1\" W",
            vec![(51.507389, -0.127806)],
        ),
        ("51°30.443'N 0°7.668'W", vec![(51.507383, -0.1278)]),
        ("-33.8688, 151.2093", vec![(-33.8688, 151.2093)]),
        ("0.0, 0.0", vec![(0.0, 0.0)]),
        // GeoJSON [lon, lat] pastes
        ("-118.4912, 34.0195", vec![(34.0195, -118.4912)]),
        ("-0.1278, 51.5074", vec![GR, G]),
        (
            "-74.006, 40.7128",
            vec![(-74.006, 40.7128), (40.7128, -74.006)],
        ),
        ("13.405, 52.52", vec![(13.405, 52.52), (52.52, 13.405)]),
    ]
}

const REFUSED: &[&str] = &[
    "51.5074",
    "London",
    "santa mon",
    "Lake Tahoe",
    "",
    "12345",
    "5130",
    "1 2 3",
    "51.5074, -0.1278, 12",
    "0.0, 181.0",
    "91.0, 181.0",
    "91N 0W",
    "51.5074N 0.1278N",
    "51.5074E 0.1278W",
    "-51.5074S 0.1278W",
    "51°70'N 0°7'W",
    "51°30'70\"N 0°7'W",
    "51.5°30'N 0°7'W",
    "1e3, 4",
    "1e400 1",
];

/// Postcodes and half-typed place names that the Python's first draft read as coordinates.
const REFUSED_REALISTIC: &[&str] = &[
    "110 00",
    "114 18",
    "100 0001",
    "75 008",
    "28 013",
    "602 00",
    "N1 9",
    "E1 7",
    "W1 1",
    "11/45",
    "1/2 Mile Well",
    "4 1/2 Mile Indian Reserve 2",
    "5/12",
    "7 11",
    "1 2",
    "12 5",
    "42 5",
    "51 30",
    "12,50",
    "48,86",
    "SW1A 1AA",
    "700 E 400 S",
    "1234 AB",
    "Route 66",
    "I 95",
    "Gate 5 12",
    "52,52 13,40",
];

/// The fixture's place names; every prefix of each must be refused, because a search box that
/// shows results while typing sends each one.
const NAMES: &[&str] = &[
    "Reykjavík",
    "Reykjavik",
    "Lake Tahoe",
    "Santa Monica",
    "Santa Maria a Monte",
    "London",
    "Londonderry",
];

#[test]
fn accepts_every_notation() {
    for (q, want) in accepted() {
        let got = parse_coords(q);
        assert_eq!(got.len(), want.len(), "{q:?}: got {got:?}, want {want:?}");
        for ((glat, glon), (wlat, wlon)) in got.iter().zip(&want) {
            assert!(
                (glat - wlat).abs() <= 1e-5 && (glon - wlon).abs() <= 1e-5,
                "{q:?}: got {got:?}, want {want:?}"
            );
        }
    }
}

#[test]
fn refuses_non_coordinates() {
    for q in REFUSED.iter().chain(REFUSED_REALISTIC) {
        assert!(
            parse_coords(q).is_empty(),
            "{q:?} parsed to {:?}",
            parse_coords(q)
        );
    }
}

#[test]
fn length_cap_is_checked_before_whitespace_collapse() {
    let q = format!("51.5074, -0.1278{}", " ".repeat(110));
    assert!(parse_coords(&q).is_empty());
    // the same text under the cap does parse, so the cap is what refused it above
    assert_eq!(parse_coords("51.5074, -0.1278").len(), 2);
}

#[test]
fn length_cap_counts_characters_not_bytes() {
    // 40 degree signs are 80 bytes but 40 characters: under the cap, so the regex decides (refused
    // there), whereas 61 two-byte characters would be 122 bytes and still under 120 characters.
    let q = format!("51.5074, -0.1278{}", "\u{a0}".repeat(100));
    assert_eq!(q.chars().count(), 116);
    assert_eq!(
        parse_coords(&q).len(),
        2,
        "a byte-counting cap would refuse this"
    );
}

#[test]
fn refuses_every_prefix_of_every_name() {
    for name in NAMES {
        for (i, _) in name.char_indices().skip(1).chain([(name.len(), ' ')]) {
            let prefix = &name[..i];
            assert!(
                parse_coords(prefix).is_empty(),
                "prefix {prefix:?} of {name:?} parsed"
            );
        }
    }
}

#[test]
fn hostile_inputs_are_fast_and_refused() {
    let hostile = [
        format!("{}°30' 1", "9".repeat(400)),
        format!("{} 1", "9".repeat(400)),
        format!("{}.5 1.0", "0".repeat(10_000)),
        format!("1{}", " ".repeat(600)),
        format!("1{}", " ".repeat(2000)),
        format!("1{}", ",".repeat(500)),
    ];
    for q in &hostile {
        let t0 = Instant::now();
        let got = parse_coords(q);
        let took = t0.elapsed();
        assert!(got.is_empty(), "{}… parsed to {got:?}", &q[..24]);
        assert!(
            took < Duration::from_millis(250),
            "{}… took {took:?}",
            &q[..24]
        );
    }
}

/// The `>= 60` boundary, which the Python suite's 70-minute and 70-second cases do not reach.
#[test]
fn sixty_minutes_or_seconds_is_refused_and_just_under_is_not() {
    assert!(parse_coords("51°60'N 0°7'W").is_empty());
    assert!(parse_coords("51°30'60\"N 0°7'W").is_empty());
    assert_eq!(parse_coords("51°59.999'N 0°7'W").len(), 1);
    assert_eq!(parse_coords("51°30'59.999\"N 0°7'W").len(), 1);
}

/// One letter on either half fixes both axes. Found by `tools/diff.py`: a port that read the
/// second half's letter backwards passed every case above.
#[test]
fn one_letter_on_either_half_fixes_both_axes() {
    assert_eq!(parse_coords("51.5074, 0.1278W"), vec![G]);
    assert_eq!(parse_coords("0.1278W, 51.5074"), vec![G]);
    assert_eq!(parse_coords("51.5074N, -0.1278"), vec![G]);
    assert_eq!(parse_coords("-0.1278, 51.5074N"), vec![G]);
}
