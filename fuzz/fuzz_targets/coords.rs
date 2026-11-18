//! What a user types in the search box: the coordinate parser, name folding, and the FTS5 query
//! built from it (a query that SQLite cannot parse would be an error, not a search).
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    for (lat, lon) in coords::parse_coords(s) {
        assert!(
            (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon),
            "{s:?}"
        );
    }
    let _ = coords::fold(s);
    let _ = coords::fts_query(s);
});
