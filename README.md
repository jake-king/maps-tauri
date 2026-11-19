# maps-tauri

A desktop map viewer in Tauri 2 and Rust, with aviation weather. It reads a whole-planet PMTiles
archive over HTTP range requests and keeps what it has seen in a SQLite cache, so places visited
once still draw with the network gone. It has two renderers: MapLibre GL, from the self-hosted web
map viewer it began as a port of (with a GeoNames search box), and a native one, where Rust decodes
the vector tiles and builds the meshes and a small WebGL2 renderer in the webview draws them. On the
same seeded stress run, the native renderer never showed unfinished map (MapLibre did in 10.6% of
frames) and peaked at 619 MB of memory to MapLibre's 1501 MB. Weather (radar, satellite, METARs,
winds aloft, SIGMETs and G-AIRMETs) runs on one time bar from 18 hours back to 12 ahead.

## Status

A personal project, not a product. In practice:

- One map area: the West LA extract. Other areas need an extract you cut yourself (see Data).
- Linux only as a package. macOS, Windows and an iOS simulator build run in CI but are not packaged.
- Search covers West LA only, from the published index.
- Weather comes from NOAA and is not for flight planning.
- `src/maplibre-patch.js` targets MapLibre 6.7.0 only.

## Documentation

`book/` is a short engineering book (mdBook): the architecture, the native renderer, weather, search,
how things were measured, and how they are checked. `tools/docs.sh` builds it with the Rust API
reference beside it under `api/`; every public item is documented, and the gate keeps it so.

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
- `src`: the frontend. `rust-renderer/` is the native renderer (the default), `maplibre-app.js`
  the MapLibre version, `devtools/` the profiler overlay and the benchmarks.

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
- Radar (US only: MRMS is built from US and Canadian radars, and the stamp says so): MRMS base reflectivity, a loop of five frames ten minutes apart ending at the chosen time.
  Frames for a past time are fetched when first needed from NOAA's 24-hour archive, decoded in Rust
  (0.7 MB of GRIB2 each, thinned to 3500 x 1750 one-byte cells) and kept compressed in memory and on
  disk; the GPU holds up to 15 as textures. The shader takes each pixel back to latitude and
  longitude through the camera and Web Mercator and looks its cell up.
- Satellite: GOES-East and West longwave infrared from NOAA's nowCOAST map server, every 5 minutes,
  about 6.5 hours of history; outside their view, nowCOAST's hourly global infrared mosaic, brought
  onto the GOES grey scale by a fit (GOES = 0.650 x global + 51.4, r = 0.976 over 169,000 matched
  pixels). The global layer gives way wherever GOES has data. Clear ground is transparent; colder,
  higher cloud tops are more opaque.
- METARs: airport observations as dots coloured by flight category (VFR, MVFR, IFR, LIFR); a click
  opens the station's report. The present comes from AWC's bulk cache file (the world, about 5,000
  stations); the API caps history at 400 observations a request, so history is what the app collected
  from that file every five minutes while it ran, kept on disk, and the bar shows where it begins.
- Winds: GFS winds at the chosen flight level (FL050 to FL450 mapped to standard pressure levels;
  FL340, 250 hPa, when none is chosen), as 5,000 particles that drift with the wind and leave fading
  trails, coloured by speed. The field (U and V for the whole world on a 0.5° grid, cut by NOAA's
  NOMADS filter, about 0.7 MB) comes from the newest run that serves the chosen time at the nearest
  3-hourly step, or the run before if that one is not published yet. Particles move a fixed number of screen pixels per m/s at any zoom; the simulation is
  JavaScript. Each particle keeps its last 16 positions on the map, and every frame those paths are
  projected through the camera and drawn as fading lines, so the trails stay put on the map through a
  pan, zoom or turn. With winds alone on, the map holds 60 frames a second, redraw p50 9 ms, p99 11 ms.
- SIGMETs, worldwide: international SIGMETs (every FIR's turbulence, icing, thunderstorms, mountain
  waves, tropical cyclones, volcanic ash) and US convective SIGMETs in force at the chosen time, with
  the hazards; a past time's are fetched as needed. A click inside one shows its text.
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
time with nothing uploaded for the old one; the time-lapse must buffer first, then step 10 minutes at a time to live
with no frame where a layer goes blank and no gap over 1.2 s between steps; the world view must
show the global clouds, SIGMETs and non-overlapping hazard labels, and a click inside a SIGMET must
open it; METARs must cover the present, a click on LAX must open
its report, and the flight-level filter must narrow the hazards; and a synthetic one-cell radar frame at a known point
must land where the map's own projection puts that point, one cell wide and 1/cos(latitude) cells
tall.

## Install (Linux)

A version tag publishes a Debian package on the
[Releases](https://github.com/jake-king/maps-tauri/releases) page (Ubuntu 24.04 or newer, amd64).
The package holds the app; the data is a separate download, published as the release
`map-westla-20261118`: a 25 MB map of West LA (Malibu to downtown) and a 1 MB place-name index for
the same area:

    sudo apt install ./maps-tauri_<version>_amd64.deb
    mkdir -p ~/.local/share/local.maps-tauri
    curl -fL -o ~/.local/share/local.maps-tauri/westla.pmtiles \
      https://github.com/jake-king/maps-tauri/releases/download/map-westla-20261118/westla.pmtiles
    curl -fL -o ~/.local/share/local.maps-tauri/geonames.db \
      https://github.com/jake-king/maps-tauri/releases/download/map-westla-20261118/geonames.db
    maps-tauri

The extract was cut with [go-pmtiles](https://github.com/protomaps/go-pmtiles) from Protomaps' daily
build of 2026-11-18 (the commands are under Data, below); the index holds the 6,465 GeoNames places
inside the map's bounds. The release notes give both files' sha256. An installed app reads its data
from `~/.local/share/local.maps-tauri` (`MAPS_DATA` sets another directory); without an archive
there it says so on stdout and shows an empty map. Weather needs no setup: tick a layer and it is
fetched from NOAA. Other platforms build from source (below).

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
  service and is not included; the West LA index under Install is cut from its output. Without an
  index, search answers coordinates only.
- `data/remote.url` (optional): one line, the URL of a PMTiles archive to read over HTTP.

`tools/fetch-data.sh` does all of this from my own map service (`MAPS_SRC=<base url>`). Built from
source, the app reads `data/` in the source tree; the extract steps under Install work for it too.

## Run

Needs Rust 1.90 or newer (the gate itself runs the 1.99.1 that `rust-toolchain.toml` pins), Node, [wasm-pack](https://rustwasm.github.io/wasm-pack/) and the Tauri 2 system
dependencies for your platform. Developed on Linux (WebKitGTK).

    npm install
    npx tauri dev                          # native renderer
    MAPS_RENDERER=maplibre npx tauri dev   # MapLibre renderer
    tools/check.sh                         # format, lint, tests, decoder and mesher checks

The benchmarks (`tools/bench.sh`, `tools/zoombench.sh`, `tools/stress.sh`) and the release-build
gates (`tools/offline.sh` for the content security policy, `tools/cache.sh` for the disk cache)
are described at the top of each script. F4 in the app opens the profiler overlay.

## CI

Every push and pull request runs one pipeline, the same on GitHub Actions and on the self-hosted
Forgejo forge the project is developed on (this repository is a published copy). Each job runs in a
bare Ubuntu 24.04 container through `tools/forge-job.sh`, and `tools/ci-local.sh` runs any of them
the same way on a laptop.

- `check`: `tools/check.sh`, the gate run before every commit: format, clippy, documentation,
  licences and banned dependencies (cargo-deny), unused dependencies, a secret scan (gitleaks),
  ESLint, Ruff, ShellCheck and the unit tests.
- Then, in parallel: `linux-e2e` (the release build driven end to end, below), `coverage` (fails
  under the floor in `tools/coverage-floor.txt`), `mutants` (cargo-mutants on the changed lines),
  `audit` (RustSec and npm advisories), `fuzz` (every parser's fuzz target), `perf` (instruction
  counts under Valgrind against the merge base; +3% fails) and `msrv` (builds with Rust 1.90).
- `verdict`: fails unless every job passed; it is the one check `main` requires, and merges are
  fast-forward only, so the commit CI tested is the commit on `main`.

On a schedule: advisories daily, a 10-minute fuzz of each target and a full mutation run weekly
(the missed count must not grow), and weekly `tools/update-e2e.sh`, which installs the app in a clean
container and offers it a tampered package, a package signed by another key and a replayed old
version: each must be refused and the genuine update installed.

Tauri draws with each platform's own web view: WebKitGTK on Linux, WKWebView on macOS, WebView2
(Chromium) on Windows. The renderer is WebGL2, where engines differ most, so `platforms.yml` builds
and tests on each, on demand for now: the unit tests, the release build, then the app's self-driving
end-to-end run (`MAPS_BENCH=1`) judged by `tools/e2e_check.py`. NOAA's answers come from a recorded
tape (`MAPS_TAPE_REPLAY`, `wx::tape`): every platform draws the same data, with no network, and a
request not on the tape is an error naming its URL. The tape and the map archive are a release asset
pinned by sha256 (`tools/fixtures.py`, `tools/fixtures.lock`). Speed limits apply only on the dev
box; the runners draw WebGL in software, so CI checks correctness. Each platform's snapshots are then
compared with Linux's in 8 px blocks (`tools/compare_snapshots.py`, limits in
`tools/snapshot-limits.json`). A job also builds for the iOS simulator.

A version tag runs the gate and builds the Debian package. On the forge the release also carries
SBOMs, the documentation site and SHA256SUMS, signed with the release key (`release-signing.pub`);
on GitHub it is the package alone.

Run the same end to end anywhere with `python3 tools/e2e.py --tape fixtures/tape`, and make a new
tape with `--record DIR`.

## Security

Report a vulnerability privately, as [SECURITY.md](SECURITY.md) describes: email, encrypted to the
project's key if it holds anything sensitive. Releases are signed; SECURITY.md also says how to check one.

## Licence

Code: MIT or Apache-2.0, at your option. Map data © OpenStreetMap contributors (ODbL), basemap
schema and assets by Protomaps. Place names from GeoNames (CC BY 4.0). Weather from NOAA: MRMS radar (NCEP) and G-AIRMETs
(aviationweather.gov); not for flight planning. `src/maplibre-patch.js` changes a private MapLibre 6.7.0 method and
does nothing on any other version.
