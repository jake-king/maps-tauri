# Engineering journal

A running record of building maps-tauri: what was done, what broke, why, and what the
measurements said. Newest entries at the bottom. Commits are named where they exist.

The project started on 2026-11-15 as a way to learn Tauri. It rebuilds the offline map viewer
from my homelab (MapLibre GL over a PMTiles basemap, with a GeoNames search box served by a
small Python service) as a Tauri 2 desktop app, with the search-box parsing ported to Rust and
compiled to WebAssembly.

## Setup (2026-11-15)

Rust 1.99 with the `wasm32-unknown-unknown` target, WebKitGTK 4.1, Node 22 and SQLite 3.54 were
already on the machine. Missing were the Tauri CLI (installed as an npm dev dependency, pinned to
2.12.4; Tauri 3 is still alpha), `wasm-pack`, and `go-pmtiles`.

The JavaScript dependencies are pinned to the versions the homelab viewer already ships:
MapLibre GL 6.7.0, pmtiles 4.5.0, @protomaps/basemaps 5.7.2, maplibre-gl-geocoder 1.9.4.
I first typed `maplibre-gl@5.7.2`, mixing it up with the basemaps version, and `npm audit`
flagged a critical XSS advisory against MapLibre <= 6.4.0. Pinning the intended 6.7.0 cleared it.

All map data comes from the homelab's map service over the LAN, never the internet
(`tools/fetch-data.sh`): a 6.2 MB Los Angeles cutout of the planet archive, extracted with
`go-pmtiles extract` in 33 HTTP Range requests; the Protomaps glyphs and sprites; and the
production GeoNames index (1.6 GB, 13,464,063 places).

## M0: a window that renders (289776e)

The first goal was to settle the riskiest unknown before building anything on it: does
MapLibre's WebGL work inside WebKitGTK at all? A minimal map (a background layer and one point)
reached `idle` over WebGL 2.0. WebKit reports the GPU as "Apple GPU" whatever the hardware,
which looks like its fingerprinting protection rather than a real reading.

To check runs without watching the screen, the frontend sends its status lines to a `report`
command that prints them on the backend's stdout. Every later milestone was verified this way.

Problems:
- **MapLibre 6 has no default export.** `import maplibregl from "maplibre-gl"` failed with
  "Importing binding name 'default' cannot be resolved". Fixed with `import * as maplibregl`.
- **Vite's dependency pre-bundler breaks MapLibre 6.** MapLibre loads its worker from a sibling
  file next to its entry module; the pre-bundler moves the entry into `.vite/deps` without that
  file. Fixed by excluding `maplibre-gl` from `optimizeDeps`.

## M2: the search-box parsing in Rust and WebAssembly (96975c8)

`crates/coords` ports three functions from the Python service: `parse_coords` (decimal, DMS and
DDM coordinates, with deliberate refusals for postcodes and half-typed place names), `fold`
(the index's accent-stripping) and `fts_query` (turning user text into a safe FTS5 query).

Tests came over from the Python suite's tables: 19 accepted notations, 47 refusals, 6 hostile
inputs. All passed on the first run, so I planted bugs to check they could fail. Removing the
"must look like a coordinate" check and counting the length cap in bytes were both caught.
Allowing exactly 60 minutes was not: neither the Python suite nor the port tested the boundary.
I added minute and second boundary cases.

A differential test (`tools/diff.py`) runs the Rust and the Python over the same ~188,000 inputs
(the case tables plus seeded random strings) and fails on any disagreement it cannot explain.
It caught a planted bug that every unit test missed (a hemisphere letter on the second number
read backwards), which then became a unit test too. It also found two real differences:
- **Python accepts non-ASCII digits** (`١٢`, `１２`): its `\d` and `float()` are Unicode-aware.
  The port reads ASCII digits only. Refusing is the safe direction by the original's own rule
  (an unparsed coordinate falls through to the place search), so this is kept and documented.
- **Python splits words at combining marks** (Hindi vowel signs, Arabic harakat, Hebrew niqqud).
  Rather than guess which side was right, I checked SQLite: FTS5's `unicode61` tokenizer splits
  the same way, and it re-tokenizes the query phrase, so both versions find the same rows. The
  diff now compares queries by the tokens FTS5 sees.

Rust's `regex` crate runs in linear time, so the Python version's original ReDoS (71 s of CPU
for a 303-byte query, fixed there by rewriting the pattern) cannot occur in the port at all.

Problems:
- **Python's `splitlines()` broke the diff's input.** It splits on U+2028 and U+0085 as well as
  `\n`, and serde_json leaves those characters unescaped, so JSON rows were cut in half. Split
  on `\n` only, the way the producer wrote them.
- The WASM module is 462 KB, mostly Unicode tables. Dropping the regex crate's `unicode-perl`
  feature (no longer needed once the pattern used `[0-9]`) saved 36 KB.

## M1: the basemap through a Rust URI scheme (ac086cb)

The webview never touches the filesystem. A `tiles://` URI scheme in Rust serves the PMTiles
archive with HTTP Range semantics: 206 with `Content-Range` for a single range, 416 for anything
else, and only listed file names are reachable. It uses the asynchronous protocol API and reads
on the blocking thread pool, so file I/O never runs on the UI thread. The first run read the
16 KB header and six tile ranges, and MapLibre decoded 466 road and 189 place features.

Problem:
- **The auto-mode permission classifier blocked internet downloads** (`go install` of
  go-pmtiles, a fetch from build.protomaps.com). Rather than work around it, the data now comes
  from the homelab's own map service, which already serves every file with Range support.

## M3: place search as a Tauri command (eb2e8ab)

`search.rs` ports the Python service's `search()` to rusqlite with bundled SQLite (FTS5
included), opening the index read-only and immutable. A parity test (`tools/search_diff.py`)
runs both over the real 13.46M-row index: 156 queries, 896 results, identical. Two planted bugs
(the exact-match ranking term removed, numeric region codes kept in labels) both turned it red.

The search box answers coordinates in the frontend through the WASM build, in under a
millisecond, and sends place names to the `search` command (about 20 ms on a debug build). Both
sides build coordinate results with the same Rust function, so there is one definition.

Tauri 2 lets the frontend call any app command unless the commands are declared. `build.rs` now
declares them, so each needs a grant in `capabilities/default.json`. Checked both ways: with
`allow-search` removed, the call is refused ("search not allowed") and the map and coordinate
search keep working.

A finding in the production data, left unchanged in the port: "santa mon" ranks Santa Monica,
Philippines above Santa Monica, California, because the Philippine entry has the higher
precomputed notability score.

Problems:
- **`wasm-pack` was not on PATH for npm scripts** (it lives in `~/.cargo/bin`); the script sets
  PATH itself.
- **A global commit hook refuses commits on `main`.** Work happens on `feat/tauri-port`.

## Dev tools: the F4 overlay and the pan benchmark (ad9ef67)

Panning fast at high zoom showed gray boxes where tiles were still loading. Before changing
anything I built tools to measure it, modelled on the F4 profiler in my guile-2d engine.

F4 toggles a canvas overlay: a row per measurement (average, worst, a bar against one frame):
frame interval, MapLibre's render CPU time, tile fetch round trip through `tiles://`, the Rust
handler's own time, and tile request-to-decoded time. Below that, memory for every process in
the app's tree, read by the backend from `/proc` (WebKitGTK renders in separate web and network
processes, so the app's own RSS is a small part of the total), and a 120-frame graph with the
budget line. The layout and statistics are pure functions with tests (7 planted bugs, all
caught after adding one missing case).

F6, or `MAPS_BENCH=1` at startup, runs a fixed benchmark: a lawnmower path over the cutout at
zoom 15 and 4000 px/s, run cold and then warm, reported as one JSON line. `tools/bench.sh` wraps
it and saves results to `bench/`.

First baseline (debug build): tiles missing in 34% of frames cold and still 30% warm. Rust's
handler took 0.2 ms per request and the `tiles://` round trip about 3 ms, so neither file I/O nor
IPC was the problem. Almost all of the ~35 ms from request to drawable tile came after the bytes
arrived.

Problems:
- **Benchmark runs left app windows open.** `timeout` signalled the `systemd-run` wrapper and
  the app outlived it. Under `MAPS_BENCH=1` the app now exits itself when the benchmark is done,
  and `timeout` runs inside the scope as the app's parent.
- **The benchmark ran twice in one session.** Probably Vite reloading the page because it was
  watching the log file being written (not confirmed). Vite now ignores `logs/`, `data/`,
  `target/` and `crates/`.
- **Starting the benchmark failed when I had closed my own dev server** (nothing served the
  frontend, so the app showed a blank page). `bench.sh` now starts its own Vite when none is
  running and stops its whole process group afterwards.

## The tile cache (f157654)

The warm pass re-fetched and re-decoded all 161 tiles. MapLibre sizes its out-of-view cache as
(tiles across + 1) x (tiles down + 1) x `maxTileCacheZoomLevels`: 60 tiles for a 1200x800
window at the default of 5, against 161 on the path. The obvious option, `maxTileCacheSize`,
cannot raise it: MapLibre takes the minimum of the two figures.

| maxTileCacheZoomLevels | cache | warm: frames with tiles missing | warm: tiles reloaded |
|---|---|---|---|
| 5 (default) | 60 | 30% | 161 |
| 10 | 120 | 30% | 161 |
| 20 | 240 | 0% | 3 |
| 40 | 480 | 0% | 3 |

20 is now the default. Memory stayed between 860 and 970 MB in every run, within run-to-run
noise. `MAPS_CACHE_ZOOMS` overrides it for comparisons.

## The release build, and a measurement correction (435e9a2)

**The release build drew no map.** MapLibre 6 works out its worker script's URL from
`import.meta.url`, but only when the page is served over http(s). A release build serves the
page from `tauri://localhost`, so MapLibre got an empty URL and started no workers. The dev
build worked only because Vite serves over http. Fixed by bundling the worker through Vite's
`?worker&url` import and passing it to `maplibregl.setWorkerUrl()`. I had expected the dev
build to load MapLibre's development bundle; it does not (the package exports one build), so
that hypothesis was wrong.

**My "frames over budget" count was mostly timer rounding.** WebKit rounds `performance.now()`
to whole milliseconds, so a perfect 16.7 ms frame can measure 18 ms. Counting frames over
17.7 ms gave 69-95 of ~487 per pass, and I reported that as a separate source of jank. Counting
frames over 1.5 budgets (a refresh actually missed) gives 1-4. The frame rate was smooth all
along; the remaining problem is tiles on first visit. The benchmark now reports real drops and
rounding separately, and F4's graph marks only real drops.

| build | frames that missed a refresh (cold / warm) | frames with tiles missing (cold / warm) |
|---|---|---|
| release | 2 / 1 of ~487 | 34.6% / 0.2% |
| debug | 4 / 1 of ~487 | 33.1% / 0.2% |

## First-visit gray boxes: finding the real cause

After the cache change, the remaining problem was tiles on a first visit. This took longer than
the fix itself, mostly because the first two measures were wrong in ways that took controls to
expose.

**Workers and prefetch did nothing.** MapLibre runs one worker by default, except on Safari,
where it uses half the cores up to three. WebKitGTK's user agent says Safari, so the app already
had 3 workers (and the webview reports 8 cores on a 16-thread machine). 1, 2, 3 and 6 workers
gave 12.9-16.6% of frames with gray tiles by the tile measure, with 6 the worst, and
`prefetchZoomDelta` at 0, 2, 4 and 6 changed nothing: every run loaded exactly the same 161
tiles, so prefetch was not fetching anything during a pan.

**Labels cost about a quarter of decode time.** With every label layer removed, request to
drawable fell from about 38 ms to 28 ms. Real, but not the main cost.

**A better measure, then a correct one.** "Frames with tiles loading" overstated the problem: a
loading tile is often drawn over by a parent placeholder. I wrote a tile-coverage measure (needed
tiles that no drawable tile overlaps, from the public `coveringTiles()` and the tile manager's
renderable set), with tests. A delay control validated it in one direction: slowing every tile by
50 and 150 ms raised it from 16% to 31% and 61%. But it disagreed with MapLibre's code, which
suggested parents should be filling those gaps. I suspected it was sampled at the wrong moment
(from my animation-frame callback, before MapLibre's tile manager caught up with the camera) and
moved it into MapLibre's `render` event. The numbers did not change, so that suspicion was wrong.

The deciding measure was the screen itself. With `MAPS_PIXEL_CHECK=1` the style's background is
pure magenta, and every rendered frame is read back with `readPixels` and the magenta pixels
counted: that is the gray-box area exactly. It stalls the GPU, so those runs are for coverage only,
never timing. The pixel check showed gray in **30.5% of frames** on a first visit, 2.6% of the
screen on average and up to 34%. The tile measure had undercounted: every frame it flagged was
really gray, but it missed 81 more (its "a child covers its parent" rule counts partial coverage
as full).

Problems on the way:
- **The pixel check was blind at first,** reading 0% everywhere. Its first self-test was flawed: I
  looked just outside the archive's bounds expecting background, but the cutout keeps whole
  low-zoom tiles that extend past the bounds, so a blurry parent was drawn there too. The second
  self-test hides every layer but the background, so the frame must read 100%. It read 0%, and a
  diagnostic showed `readPixels` worked (204,204,204) but the background was still `#cccccc`:
  **my edit that set it to magenta had never been applied.** I edit files with small Python
  scripts, and that one used a plain string replace whose target text had changed after an
  earlier edit, so it did nothing and said nothing. Edits now go through a helper that asserts its
  target appears exactly once.
- A backdrop layer (a second source capped at zoom 10, drawing land and water under everything) was
  my first idea for a fix using only public API. I dropped it on reasoning, without measuring:
  MapLibre lays out an overzoomed copy of a low-zoom tile separately for each 512 px patch of the
  view, so the backdrop would have the same first-visit delay.

## The cause, and the fix: parents from the cache

The cause is in MapLibre's `TileManager._updateRetainedTiles`. A needed tile without data walks up
its parents to find something to draw in its place, but it pulls a parent out of the out-of-view
cache only if the needed tile `wasRequested()`, which is false while the tile's first load is in
flight. A tile that has just entered the view, which is every new tile during a pan, only finds
parents already in view, and at zoom 15 the low-zoom parents never are. That is why a bigger
cache, more prefetch and a warm-up pass at zoom 10 or 12 (27.8% and 30.7% gray by the pixel
check) all failed: the parents were in the cache, and MapLibre did not look there.

`src/maplibre-patch.js` adds a second pass after MapLibre's own: for each needed tile still
without data, retain the first parent with data, in view or in the cache. Taking it from the cache
is MapLibre's own `_addTile`, so no new request is made. It overrides a private method, so it
applies only to MapLibre 6.7.0 and otherwise does nothing and reports why. `MAPS_CACHED_PARENTS=0`
turns it off for comparison.

| first visit, pixel check | frames with background showing | worst frame |
|---|---|---|
| without the patch | 29.5-30.5% | 30-34% of the screen |
| with the patch | 0% | 0% |
| with the patch, tiles slowed by 150 ms | 0% | 0% |

Timing runs show no cost: 5 dropped frames of ~490 and about 800 MB, as before. While a detailed
tile decodes, the view now shows its parent scaled up, blurry for a frame or two, instead of gray.
