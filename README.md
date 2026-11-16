# maps-tauri

A desktop map viewer in Tauri 2, built to learn Tauri. It started as a port of my self-hosted web
map viewer (MapLibre GL over a Protomaps basemap, with a GeoNames search box) and grew a second,
native renderer: Rust reads the PMTiles archive, decodes the vector tiles and builds the meshes,
and a small WebGL2 renderer in the webview draws them. It can read a whole-planet archive over
HTTP range requests and keeps what it has seen in a SQLite cache, so places visited once still draw
with the network gone.

## How this was built

I built this in November 2026 with an AI coding agent. I set the goals, chose what to measure and
what to try next, tested the app by hand, and reviewed the changes; the agent wrote most of the
code and kept the journal. `docs/JOURNAL.md` is that working log, in the order it happened: every
problem, the measurements that turned out to be wrong, and what fixed them.

## What is in it

- `crates/vtile`: PMTiles v3 reading (local file, or HTTP range requests with a pluggable cache),
  Mapbox Vector Tile decoding, and meshing: polygons triangulated with earcut, lines as mitered
  strips, label positions including street names along curved paths. Its decoder output is
  checked against `@mapbox/vector-tile` over every tile of an extract.
- `crates/coords`: the search box's coordinate parser, name folding and FTS5 query builder, ported
  from the Python service and compiled to WebAssembly for the frontend. `tools/diff.py` runs the
  port and the original over about 190,000 inputs and lists every disagreement.
- `src-tauri`: the backend. Tiles are served through a `tiles://` URI scheme with range support;
  meshes go to the webview in a compact binary format; `search` queries a GeoNames FTS5 index with
  rusqlite; `diskcache.rs` is the SQLite cache for the remote archive. Commands need capability
  grants, and a strict content security policy keeps the webview from reaching anything but the
  app itself.
- `src`: the frontend. `maplibre-app.js` is the MapLibre version (the default), `rust-renderer/`
  the native one, `devtools/` the profiler overlay and the benchmarks.

## Measured

A seeded stress script of 150 camera operations (interrupted pans, zooms, rotations, wheel bursts,
drags and teleports), the same input for both renderers, on a West LA extract. "Background
showing" counts frames where any of the screen was not yet drawn, read back from the GPU:

|                              | native            | MapLibre           |
|------------------------------|-------------------|--------------------|
| frames with background       | 0 of 2380         | 325 of 3066 (10.6%) |
| render p50 / p99             | 2 / 7 ms          | 5 / 13 ms          |
| peak memory                  | 619 MB            | 1501 MB            |

Against the remote planet over a LAN, the disk cache took frames with background from 7.8% to
0.4%. The journal has how each number was measured and what was wrong with the first attempts.

## Data

The app expects data the repository does not include (`data/` and `public/assets/` are
gitignored):

- `data/westla.pmtiles` and `data/basemap.pmtiles`: Protomaps basemap extracts (v4 schema). With
  [go-pmtiles](https://github.com/protomaps/go-pmtiles), from a daily build at
  `https://build.protomaps.com/<YYYYMMDD>.pmtiles`:

      go-pmtiles extract <build url> data/westla.pmtiles --bbox=-118.70,33.85,-118.15,34.20 --maxzoom=14
      go-pmtiles extract <build url> data/basemap.pmtiles --bbox=-118.55,33.95,-118.35,34.10 --maxzoom=14

- `public/assets/fonts` and `public/assets/sprites/v4`: from
  [protomaps/basemaps-assets](https://github.com/protomaps/basemaps-assets).
- `data/geonames.db` (optional): a GeoNames FTS5 index. The builder lives with the private Python
  service and is not included; without the index, search answers coordinates only.
- `data/remote.url` (optional): one line, the URL of a PMTiles archive to read over HTTP.

`tools/fetch-data.sh` does all of this from my own map service (`MAPS_SRC=<base url>`). I have not
run the public-source steps above end to end from a fresh clone.

## Run

Needs Rust, Node, [wasm-pack](https://rustwasm.github.io/wasm-pack/) and the Tauri 2 system
dependencies for your platform. Developed on Linux (WebKitGTK).

    npm install
    npx tauri dev                          # MapLibre renderer
    MAPS_RENDERER=rust npx tauri dev       # native renderer
    tools/check.sh                         # format, lint, tests, decoder and mesher checks

The benchmarks (`tools/bench.sh`, `tools/zoombench.sh`, `tools/stress.sh`) and the release-build
gates (`tools/offline.sh` for the content security policy, `tools/cache.sh` for the disk cache)
are described at the top of each script. F4 in the app opens the profiler overlay.

## Licence

Code: MIT or Apache-2.0, at your option. Map data © OpenStreetMap contributors (ODbL), basemap
schema and assets by Protomaps. `src/maplibre-patch.js` changes a private MapLibre 6.7.0 method and
does nothing on any other version.
