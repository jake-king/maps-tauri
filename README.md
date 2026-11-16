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
- `crates/wx`: NOAA weather. A GRIB2 reader for the MRMS radar mosaic and GFS model fields (a regular
  latitude/longitude grid, rows either way, values PNG-packed or simply packed, several fields to a
  file; anything else is refused by name), the reduction of a
  frame to one byte per cell for the GPU, and the G-AIRMET hazard GeoJSON.
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

## Weather

Three layers for the native renderer, synced to one chosen time the way the Aviation Weather
Center's GFA does it, and off until their boxes are ticked: nothing is fetched from NOAA before that.

- A time bar from 18 hours back to 12 hours ahead, in UTC, in 10-minute steps. Radar and satellite
  are observations, so they exist up to the present; the hazards are forecasts. Under the slider:
  radar and satellite history as strips, the hazard snapshot times as marks, and now. The valid time
  of what each layer shows is stamped on the map.
- Radar: MRMS base reflectivity, a loop of five frames ten minutes apart ending at the chosen time.
  Frames for a past time are fetched when first needed from NOAA's 24-hour archive, decoded in Rust
  (0.7 MB of GRIB2 each, thinned to 3500 x 1750 one-byte cells) and kept compressed in memory and on
  disk; the GPU holds up to 15 as textures. The shader takes each pixel back to latitude and
  longitude through the camera and Web Mercator and looks its cell up.
- Satellite: GOES-East and West longwave infrared from NOAA's nowCOAST map server, every 5 minutes,
  about 6.5 hours of history (older imagery exists, from NASA GIBS, but about 5 hours behind real
  time and colour-enhanced; not used yet). Clear ground is transparent; colder, higher cloud tops are
  more opaque.
- METARs: airport observations as dots coloured by flight category (VFR, MVFR, IFR, LIFR); a click
  opens the station's report. The present comes from AWC's bulk cache file (about 2,900 stations in
  the box); the API caps history at 400 observations a request, so history is what the app collected
  from that file every five minutes while it ran, kept on disk, and the bar shows where it begins.
- Winds: GFS winds at the chosen flight level (FL050 to FL450 mapped to standard pressure levels;
  FL340, 250 hPa, when none is chosen), as 5,000 particles that drift with the wind and leave fading
  trails, coloured by speed. The field (U and V on a 0.25° grid, cut by NOAA's NOMADS filter, about
  150 KB) comes from the newest run that serves the chosen time, or the run before if that one is not
  published yet. Particles move a fixed number of screen pixels per m/s at any zoom; the simulation is
  JavaScript, the trails a pair of textures on the GPU that fade a little each frame. With winds alone
  on, the map holds 60 frames a second, redraw p50 7 ms, p99 13 ms.
- Hazards: the G-AIRMET snapshot valid nearest the chosen time (turbulence, icing, IFR, mountain
  obscuration, surface wind), outlined and labelled, filtered by flight level as GFA does: an area
  shows if the level is within 3,000 ft of its base or top. A snapshot more than 90 minutes from the
  chosen time is not shown.
- Play steps the bar 10 minutes at a time (from live, over the last three hours) and moves on only
  once a step's radar frame and satellite image are on screen; the next step is fetched while the
  current one shows.

Rust keeps the present warm in the background (every two minutes: the newest radar frame first,
then the hazards, the newest satellite image, then the rest of the loop) and tells the webview as
each piece arrives. Whatever was fetched is shown again offline, marked as such.

`tools/weather.sh` checks it end to end against NOAA: the present and three hours back must each load
fully with nothing shown from after the chosen time; a change of time mid-load must settle on the new
time with nothing uploaded for the old one; the time-lapse must step 10 minutes at a time to live
without leaving a step before it is on screen; METARs must cover the present, a click on LAX must open
its report, and the flight-level filter must narrow the hazards; and a synthetic one-cell radar frame at a known point
must land where the map's own projection puts that point, one cell wide and 1/cos(latitude) cells
tall.

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
schema and assets by Protomaps. Weather from NOAA: MRMS radar (NCEP) and G-AIRMETs
(aviationweather.gov); not for flight planning. `src/maplibre-patch.js` changes a private MapLibre 6.7.0 method and
does nothing on any other version.
