# Search

The search box answers two kinds of input, and the split mirrors the original Python service it was
ported from.

## Coordinates, in the frontend

`crates/coords` ports three functions: `parse_coords` (decimal, degrees-minutes-seconds and
degrees-decimal-minutes notations), `fold` (the index's accent stripping) and `fts_query` (user text to
a safe FTS5 query). It is compiled to WebAssembly, so a typed coordinate is answered in the frontend
in under a millisecond, and the same Rust function builds coordinate results on both sides.

`parse_coords` refuses on purpose: every component must carry a mark that says "coordinate" (a decimal
point, a sign, a hemisphere letter or a degree symbol), so postcodes and half-typed place names fall
through to the gazetteer. When the input does not say which number is latitude, both in-range
readings come back, conventional order first, so a pasted GeoJSON `[lon, lat]` pair is never silently
misread.

The port is checked against the original, not only against its own tests. `tools/diff.py` runs the
Rust and the Python over about 188,000 inputs (the original's case tables plus seeded random strings)
and fails on any disagreement it cannot explain. It caught a planted bug every unit test missed, and
found two real differences, both kept and documented: Python accepts non-ASCII digits (the port reads
ASCII only, which is the safe direction), and Python splits words at combining marks (FTS5's
tokenizer splits the same way, so both find the same rows).

Rust's `regex` crate runs in linear time, so the regular-expression denial of service the Python
version once had (71 s of CPU for a 303-byte query) cannot occur in the port at all.

## Place names, in Rust

Anything else goes to the `search` command (`src-tauri/src/search.rs`), which ports the service's
query to rusqlite with bundled SQLite, FTS5 included. The index is opened read-only and immutable, so
SQLite can skip locking. `tools/search_diff.py` runs the port and the production service over the
real 13.46-million-row index: 156 queries, 896 results, identical. Two planted bugs (the exact-match
ranking term removed, numeric region codes kept in labels) both turn it red. It is part of the gate
when the index is present.
