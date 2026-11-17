# API reference

The rustdoc output for every crate is published beside this book. Every public item is documented,
and the documentation build is part of the gate ([Testing, CI and releases](testing.md)).

| crate | |
|---|---|
| [coords](api/coords/index.html) | coordinate parsing, name folding and the FTS5 query builder |
| [vtile](api/vtile/index.html) | PMTiles v3 reading, Mapbox Vector Tile decoding, meshing and the wire format |
| [wx](api/wx/index.html) | NOAA weather: GRIB2, radar, satellite, METARs, SIGMETs, hazards, winds, the time bar, the tape |
| [maps_tauri_lib](api/maps_tauri_lib/index.html) | the app: `run` and the gazetteer search |

To build it locally: `tools/docs.sh` writes the book and the API reference to `target/site/`.
