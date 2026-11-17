# Architecture

## Processes and crates

Tauri runs the app as a Rust process that owns a native window with a web view: WebKitGTK on
Linux, WKWebView on macOS, WebView2 on Windows. The frontend is plain JavaScript built with Vite;
the backend is the `src-tauri` crate, with the work split into three library crates:

| crate | role |
|---|---|
| `crates/vtile` | PMTiles v3 reading (local file, or HTTP Range requests with a pluggable cache), Mapbox Vector Tile decoding, meshing |
| `crates/wx` | NOAA weather: GRIB2, METARs, SIGMETs, G-AIRMET hazards, satellite images, winds, the time bar's selection rules, the request tape |
| `crates/coords` | the search box's coordinate parser, name folding and FTS5 query builder; also compiled to WebAssembly for the frontend |
| `src-tauri` | the app: the `tiles://` scheme, the commands, the gazetteer, the disk cache, weather fetching and caching |

`main.rs` only calls `maps_tauri_lib::run`, which opens the tile archive, the gazetteer and the
weather sources, registers the commands and the scheme, and runs the event loop.

## How the webview gets data

The webview never touches the filesystem or the network directly. It has two ways in, both served
by Rust:

- **The `tiles://` scheme.** `tiles://localhost/basemap.pmtiles` serves the PMTiles archive with HTTP
  Range semantics (206 with `Content-Range` for a single range, 416 for anything else), so MapLibre
  reads it exactly as it would over HTTP. Only that one name is reachable. For the native renderer,
  `tiles://localhost/mesh/{z}/{x}/{y}` returns a tile already decoded and meshed. Both run on the
  blocking thread pool through Tauri's asynchronous protocol API, so file and network I/O never run
  on the UI thread.
- **Commands.** The frontend calls Rust functions by name (`search`, `archive_info`, the `wx_*`
  weather commands, a few for the dev tools). Weather arrivals also come back the other way, as
  events Rust emits when a frame or forecast hour lands.

## Permissions and the content security policy

Tauri 2 lets the frontend call any app command unless the commands are declared. `build.rs` declares
every command, which turns each into a permission (`allow-search`, `allow-wx-frame`, ...) that a
capability must grant; `capabilities/default.json` grants them to the main window. This was checked
both ways: with `allow-search` removed, the call is refused and the rest of the app keeps working.

The webview also runs under a strict content security policy: it may load its own assets and talk to
the IPC channel and `tiles://`, nothing else. Anything remote (the planet archive, NOAA) is fetched
by Rust, outside the page's reach. `tools/offline.sh` runs the release build through a stress run and
both kinds of search and fails on any policy refusal. Its control run makes one request the policy
must refuse, so a green result cannot be a pattern that matches nothing; that control is how a
broken pattern, and with it four vacuous green runs, was found.

## Data sources

- A local PMTiles extract (`data/westla.pmtiles` by default, `MAPS_ARCHIVE` to choose another).
- Optionally, a whole-planet archive read over HTTP Range requests (`MAPS_ARCHIVE_URL`, or a
  gitignored `data/remote.url`): PMTiles v3, zooms 0-15, 128 GB. `Archive::open_url` issues one Range
  request per directory or tile over a reused connection, refuses anything but a 206 of the exact
  length, and caches leaf directories. If the planet cannot be reached, the app falls back to the
  local extract and says which it uses.
- A GeoNames FTS5 index (`data/geonames.db`, optional; without it search answers coordinates only).
- NOAA's public services, only once a weather layer is switched on.

## Offline: the disk cache

The planet's byte ranges are kept in a SQLite file (`src-tauri/src/diskcache.rs`). vtile defines a
`RangeCache` trait, so the crate itself has no SQLite dependency. The header is always read from the
network when it can be; its ETag names the archive version, a new version drops the old one's ranges,
and every later request carries `If-Range`, so a planet replaced mid-session is refused rather than
mixed. A response whose ETag does not match is refused too, in case a server ignores `If-Range`.
Past a size cap (2 GB by default) the least recently used ranges go, never the header and root
directory. With the server unreachable, the app opens the planet from the cache alone and draws
what it has seen.

`tools/cache.sh` checks this against a local Range server it can stop and restart: 239 requests
cold, 1 warm, 0% background offline over the same seeded run, the old version dropped under a new
ETag, and a corrupt cache file costing only the cache.

Weather follows the same rule: whatever was fetched is kept (compressed in memory and on disk) and
shown again offline, marked as such.
