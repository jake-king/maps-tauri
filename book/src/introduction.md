# Introduction

maps-tauri is a desktop map viewer built with Tauri 2. It began as a port of a self-hosted web map
viewer (MapLibre GL over a Protomaps basemap, with a GeoNames search box) and grew a second, native
renderer. It also draws NOAA aviation weather: radar, satellite, METARs, winds aloft, SIGMETs and
G-AIRMET hazard areas, all synced to one time bar.

This book explains how it is built and why. It is short on purpose; `docs/JOURNAL.md` in the
repository is the long record, in the order things happened, including the measurements that turned
out to be wrong.

## Two renderers

The app can draw the map two ways, chosen at start-up (`MAPS_RENDERER=maplibre` selects MapLibre):

- **MapLibre.** MapLibre GL JS runs in the webview and does everything itself: it
  fetches tiles, decodes them in JavaScript workers, lays out labels and draws with WebGL.
- **Native** (the default). Rust reads the PMTiles archive, decodes the vector tiles and builds ready-to-draw
  triangle meshes and label positions. A small WebGL2 renderer in the webview only uploads and draws
  them. No tile is decoded in JavaScript, and the native app never loads MapLibre's code.

The native renderer exists because measuring MapLibre showed that most of what a pan costs is tile
decoding in JavaScript. Moving that work into Rust, where allocation is under our control, while
keeping the frontend as web code, is the experiment. On a seeded stress run it never showed
unloaded background where MapLibre did in 10.6% of frames, and it peaked at 619 MB against 1501 MB
([Measuring](measuring.md)).

## Where to start reading

| To understand | Read |
|---|---|
| how the pieces fit together | [Architecture](architecture.md), then `src-tauri/src/lib.rs` |
| tile decoding and meshing | `crates/vtile`: `pmtiles.rs`, `mvt.rs`, `mesh.rs` |
| the WebGL side | `src/rust-renderer/renderer.js` and `loop.js` |
| weather | `crates/wx` (decoding) and `src-tauri/src/weather.rs` (fetching, caching) |
| search | `crates/coords` and `src-tauri/src/search.rs` |
| what is checked, and how | `tools/check.sh` and [Testing, CI and releases](testing.md) |

The Rust API documentation is published beside this book ([API reference](api.md)).
