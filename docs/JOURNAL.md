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

## Memory, and what transfers from guile-2d

The app felt heavy next to my guile-2d engine, about 800 MB after a pan. guile-2d works from a
fixed budget: one block of memory carved up at startup, frame scratch wiped every frame, nothing
allocated per frame, a profiler counting bytes per block. Its test arenas are kilobytes.

The first step was finding where the 800 MB lives. `MAPS_MEMPROBE=blank|idle` (`tools/memprobe.sh`)
records memory (PSS) for every process in the app's tree at a stage, then exits:

| stage | total | app process | WebKit network process | WebKit web process |
|---|---|---|---|---|
| blank window, MapLibre's code loaded, no map | 293 MB | 96 | 32 | 164 |
| map at rest after its first load | 599 MB | 101 | 32 | 464 |
| after the pan benchmark | ~830 MB | | | |

About 290 MB is the floor any Tauri window on WebKitGTK pays before drawing anything, and no
technique from guile-2d reaches it: SDL and OpenGL are a thin layer, a browser engine is not. The
rest is the map, all inside the web process.

At rest, each MapLibre worker costs about 45-50 MB (1, 3 and 6 workers: 494, 599 and 694 MB),
because each is a JavaScript engine of its own, and labels cost about 80 MB. On the pan benchmark:

| workers | tile cache | memory after the pan | gray, first visit / revisit | dropped frames |
|---|---|---|---|---|
| 3 (the default here) | 240 tiles | ~830 MB | 0% / 0% | 2-4 |
| 1 | 240 tiles | ~700 MB | 0% / 0% (pixel check) | 1-2 |
| 1 | 120 tiles | ~705 MB | 0% / 2.5% | 2-4 |
| 1 | 60 tiles | ~695 MB | 7.6% / 13.8% (pixel check) | 2-3 |

One worker decodes tiles no slower than three on this workload, so the app now runs one: about
130 MB saved for nothing measurable. Shrinking the cache saves almost nothing and brings gray boxes
back, because the parent patch needs parents still in the cache. The cache stays at 240 tiles.

The dev tools themselves broke guile-2d's per-frame rule: the sampler ran every frame with F4
closed, each ring buffer allocated through `Array.shift()`, and tile coverage was measured on every
render while tiles loaded. The rings are now one preallocated `Float64Array` each, written in a
circle (3 more planted bugs, all caught), and nothing runs per frame unless the overlay is open or
the benchmark is running.

Result: 500 MB at rest instead of 599, about 700 MB after a pan instead of ~830.

Problem:
- **My first sweep was invalid.** I looped over `"1 20"`-style pairs with `set -- $cfg`, but my
  shell is zsh, which does not split an unquoted variable into words, so `MAPS_WORKERS` got
  `"1 20"` (unparseable, so the default) and every run measured the defaults. The giveaway was
  `cache max 240` in runs meant to have a smaller cache. Rerun with explicit values.

## Bytes per block

guile-2d's profiler counts bytes per block, so F4 now does too, at three levels, refreshed once a
second while it is open and never per frame:

- **The GPU ledger** (`src/devtools/gpumem.js`) wraps WebGL's allocation calls (`bufferData`,
  `texImage2D`, `texStorage2D`, `renderbufferStorage`, their deletes, and `linkProgram`) and keeps
  live bytes per buffer, texture and renderbuffer. Counting at the call is the only exact place:
  MapLibre frees each buffer's CPU copy after upload, and an index buffer does not keep its size.
- **Tile CPU arrays** (`src/devtools/tilemem.js`) walks each loaded tile, in view and cached, and
  adds up the ArrayBuffers it reaches by the tile field that holds them, each buffer counted once.
- **Process memory by mapping kind** (backend, `/proc/<pid>/smaps`): anonymous (JavaScript heaps,
  malloc, JIT), GPU-mapped, shared memory, and files.

Tests cover both meters' bookkeeping (12 tests; 8 planted bugs, all caught). The live control:
GPU and tile bytes must follow the cache size, and they do. After the pan with a 240-tile cache,
26 MB of GPU buffers and 13 MB of tile arrays (167 tiles); with a 60-tile cache, 9 MB and 4.8 MB
(68 tiles).

What the blocks show:

| | web process, anonymous | map data the meters see |
|---|---|---|
| blank window | 58 MB | none |
| map with an empty style (map, WebGL context, one worker) | 104 MB | none |
| full map at rest | 226-231 MB | ~3 MB GPU, ~1 MB tile arrays, 12 shader programs |
| right after the pan | 437 MB | ~30 MB GPU, ~13 MB tile arrays |

So at rest about 120 MB comes from the style itself (71 layers, fonts, sprites, label layout),
not from tile data and not from shaders (12 programs). During a pan the web process grows by
about 210 MB, of which the map holds about 45 MB.

**The rest is a high-water mark, not a leak.** Memory did not come down 20 s after the pan
(710 -> 702 MB), and earlier sweeps showed the total barely depends on cache size, so it is not
data being held either. A soak run settles it: ten passes in a row with a cache too small to keep
the path, so every pass decodes all ~160 tiles again (about 1,600 decodes in all). Memory went
from 507 MB at rest to 685 MB after the first pass, then stayed between 670 and 695 MB for the
other nine, with no upward trend. Decoding a burst of tiles takes about 180 MB of working memory
that the JavaScript engine and malloc keep for reuse. For a tool left open all day, that it levels
off matters more than its size.

What would lower it: less decoding work per tile (fewer style layers; labels alone are about
80 MB at rest), or a renderer that does not decode in JavaScript at all. Neither is a setting.

## A renderer that does not decode in JavaScript: R0, reading tiles in Rust

The memory work showed that most of what a pan costs is MapLibre decoding tiles in JavaScript. The
next step is a second renderer, side by side with MapLibre: Rust reads and decodes the tiles and
turns them into ready-to-draw triangle buffers, and a small WebGL2 renderer in the webview only
uploads and draws. JavaScript does no decoding. The webview stays, so the frontend remains web
code (DX's argument for the iPad), and decoding moves to Rust, where allocation is ours to control.
The first version draws fills and lines only: labels (shaping, placement, collision) are a large
share of what MapLibre does and come later, if at all.

`crates/vtile` holds a PMTiles v3 reader and a Mapbox Vector Tile decoder, both hand-rolled (the
formats are a few pages of spec each); `flate2` does the gzip. Spec examples are unit tests (tile
ids, varints, directory offsets, the MVT geometry examples, value types).

The real check is a differential test (`tools/mvt_diff.mjs`): every tile in the archive read and
decoded by two independent pipelines, Rust on one side, pmtiles.js and `@mapbox/vector-tile` (the
decoder MapLibre 6.7.0 itself uses) on the other, compared layer by layer: features, geometry
types, rings, vertices, a coordinate checksum, extent and `kind` values. All 142 tiles and 926
layers are identical. A planted bug (coordinate deltas read without their sign) turns 833 of them
red.

Problems:
- **I assumed `@mapbox/vector-tile` leaves polygon rings open.** The first diff agreed on
  everything except vertex counts, always a few more on the JavaScript side. Its source closes a
  ring by repeating the first point on ClosePath. The Rust decoder now does the same; outlines need
  the closing segment anyway.
- `pbf` 5 has no default export (`import { PbfReader }`), and I had first pinned 4.0.1, a different
  major from the 5.1.2 `@mapbox/vector-tile` itself uses.

## R1: meshes in Rust

`crates/vtile/src/mesh.rs` turns a decoded tile into two GPU-ready meshes. Fills: rings grouped
into polygons with holes by winding (MapLibre's classifyRings), triangulated with `earcutr` (a
port of the earcut MapLibre uses). Lines: two vertices per point, pushed apart along the miter
(capped at 2 half widths so sharp corners do not spike), with the width applied in the shader, so a
mesh never needs rebuilding when the zoom changes. Colors and line classes live in the vertices
(indexed), so a tile draws in two calls, one for fills and one for lines. Scratch buffers are
reused from tile to tile.

The palette is generated (`tools/gen_palette.mjs`) from the same Protomaps light style MapLibre
draws: 18 fill colors (32 landuse kinds share them), 11 line classes with their real zoom width
stops. Two details of that style: landcover fades to transparent by zoom 7 (so it is skipped), and
buildings are 50% transparent. v1 simplifies roads: no casings, no tunnel or bridge styling.

Checks: unit tests (ring classification, a square with a hole triangulating to its exact area,
miters on a straight line, a right angle and a near-reversal, the wire format byte by byte, index
widening past 65,535 vertices), 7 planted bugs all caught. On the whole archive
(`examples/meshcheck.rs`): 25,300 of 25,302 polygon features triangulate to exactly their area; two
are off by up to 0.5%, which earcut does not promise for self-touching polygons. Decode + mesh takes
1.7 ms a tile.

The first wire format was too big: 415 KB per zoom-14 tile, against about 160 KB of MapLibre GPU
buffers per tile measured by the GPU ledger. 32-bit indices, full colors in every vertex and 16-bit
extrusion vectors were the waste. Version 2 uses 16-bit indices (32-bit only past 65,535 vertices,
which no tile here reaches), palette indices instead of colors, and `i8` extrusion: 6-byte fill and
8-byte line vertices, 235 KB per zoom-14 tile.

## R2: the WebGL renderer, and R3: measured against MapLibre

The backend serves `tiles://localhost/mesh/{z}/{x}/{y}`: the tile decoded and meshed in Rust on
request (about 1.7 ms), each blocking-pool thread reusing one mesher and one output buffer.
`src/main.js` now picks a renderer, and only the chosen one is imported, so a `MAPS_RENDERER=rust`
run never loads MapLibre's code. `archive_info` reads the archive's bounds in Rust, so the native
app loads no JavaScript tile library either.

`src/rust-renderer/renderer.js` (about 450 lines) fetches meshes, uploads them as they arrive (the
only parsing is a 28-byte header) and draws two calls a tile. Each needed tile is drawn clipped to
its own rectangle (WebGL scissor); a tile still loading is replaced by its nearest loaded ancestor,
clipped the same way, and each needed tile's ancestor three zooms up is requested as a backstop.
That is the behaviour MapLibre needed a patch for, built in, with no draw-order problems between
tiles. The renderer also implements the part of MapLibre's API the dev tools use, so F4, F6, the
memory probes and the pixel check measure both renderers the same way. The pure parts (Mercator
math, tile coverage, MapLibre's exponential interpolation, the mesh header) have 16 tests, among
them that the camera's center tile is the archive's own tile under Santa Monica.

To check the output without a screen, `MAPS_SNAPSHOT=<name>` settles at a fixed view, reads the
frame and saves a PNG through a `save_snapshot` command. At the Santa Monica Pier at zoom 15 the
native renderer draws the same coastline, beach, water, parks, buildings and roads as MapLibre,
almost line for line. Missing, as planned: labels and POI icons, road casings, dashed rail. The pier
comes out as an outline rather than a filled shape, a classification gap still to fix.

**A bug in my own tools invalidated the first comparison.** The first native runs showed 0% of
frames with tiles loading, which cannot be right with 150 ms tile delays, and only 201 of 504 frames
rendered. Recording each tile's state the first time a render saw it needed showed 44 already loaded,
though they had been requested only as needed: the frames where they were missing never reached the
render listeners. The cause: the dev tools' render listener calls `measureCoverage(map)`, which
called `map.getSource()`, a MapLibre method the native renderer does not have. It threw, the
exception escaped `_render`, and the next frame was never scheduled, exactly while tiles were
loading. Those frames vanished from every measure and the pan stalled until a tile arrived. Fixed
three ways: `measureCoverage` returns null for a map without MapLibre's internals; the renderer
catches and counts a listener's exception and keeps rendering; and the benchmark now reports the
share of frames rendered and listener errors, and warns when frames were not rendered.

Controls after the fix: with the ancestor fallback turned off (`MAPS_NO_FALLBACK=1`), background
shows in 8.4% of first-visit frames by the pixel check, and the renderer's own count agrees (41
frames both, 1 flagged by the count only, none by pixels only). With the fallback, 0%, and still 0%
with every tile slowed by 150 ms (62.7% of frames then draw an ancestor). Pixel self-test 100%.

| | MapLibre (1 worker, cache 240, parent patch) | native renderer |
|---|---|---|
| memory at rest | ~500 MB | **296 MB** |
| memory after the pan | ~700 MB | **340 MB** |
| 10-pass soak | flat at 670-695 MB | **flat at 325-331 MB** |
| web process at rest | ~355 MB | **157 MB** (the blank-window floor) |
| render CPU per frame, avg / p95 | 4.7 / 8 ms | **0.5 / 1 ms** |
| tile request to drawable, avg / p95 | 37 / 60 ms | **14 / 24 ms** |
| tiles loaded on the first pass | 158 | **43** |
| gray on screen, first visit / revisit | 0% / 0% (with the patch) | **0% / 0%** |
| dropped frames | 1 | **0** |

The tile count differs because past zoom 14 MapLibre lays out each zoom-14 tile again as four
separate overzoomed tiles; the native renderer scales the zoom-14 mesh. At rest the web process is
at the blank-window floor: everything the map costs now lives in the app process (Rust meshing
scratch) and the GPU.

What it costs: no labels, no search box yet in native mode, a simplified road style, and about 900
lines of renderer and mesher to own.

## Profiling the mesh handler: the CPU, not the code

In the app, a native tile took 14 ms from request to drawable, 10.3 ms of it in the Rust handler.
The same decode and mesh took 1.7 ms a tile in the offline check, so the question was where the
other 8.5 ms came from.

Stage timers in the live handler (`MeshStages`, reported through `dev_stats` and the benchmark):

| stage | offline, back to back | in the app |
|---|---|---|
| read + gunzip | 0.21 ms | 1.56 ms |
| decode | 0.14 ms | 1.12 ms |
| mesh | 1.37 ms | 7.50 ms |
| serialize | 0.06 ms | 0.31 ms |
| wait for a pool thread | | 0.04 ms |
| copy into the response | | 0.07 ms |

Not queueing, not concurrency (1.4 requests at once on average, 3 at most), not the extra copy:
every stage was uniformly 5-8 times slower. `examples/stages.rs` reproduces it offline:

| offline run (z14 tiles) | per tile |
|---|---|
| back to back | 1.74 ms |
| 16 ms sleep between tiles (the app's rhythm) | 7.75 ms |
| the same, pinned to a performance core | 7.53 ms |
| pinned to an efficiency core, back to back | 2.55 ms |
| a fresh thread per tile, back to back | 5.67 ms |
| 16 ms gap spent busy-waiting instead of sleeping | 1.76 ms |

The busy-wait run separates the two explanations: spinning keeps the core awake and clocked
without touching the caches, and the tile is fast again. So it is the CPU going idle between
requests (the i5-12600KF under the `powersave` governor swung between 1.2 and 4.2 GHz while
gapping), not cold caches. The hybrid chip's efficiency cores and thread hopping are smaller
factors.

This is how a real machine behaves, a laptop with power saving more so, and a benchmark that runs
tiles back to back would never show it. Keeping the CPU awake would waste power. The fix is to take
meshing off the critical path: a mesh cache and background prefetch, so a tile is ready before it
is visible however slowly a resting CPU builds it.

Problem:
- zsh again: `B="cmd args"; $B` does not split into words in zsh. A shell function does.

## Prefetch, and a cache that turned out not to be needed

Profiling showed the per-request time is mostly a resting CPU building the mesh. Two ways to take
it off the critical path, each measured by a new figure: for every tile, how long it was on screen
before it could be drawn (`visible_wait`; 0 when it was ready in time).

**A prefetch ring.** The renderer also requests the ring of tiles one beyond the view, at a lower
priority than anything visible (`ringTiles` in geo.js, tested; `MAPS_PREFETCH_RING`). At zoom 15 a
zoom-14 tile is 1,024 px wide, so at 4,000 px/s the ring gives about 256 ms of lead, against a fetch
of about 16 ms.

| pan benchmark, first visit | no ring | ring of 1 |
|---|---|---|
| tiles not ready when they came into view | 40 of 40 | **0 of 40** |
| wait while visible, avg / p95 | 15.3 / 27 ms | **0 / 0 ms** |
| frames drawing a fallback ancestor | 9.3% | **0%** |
| tiles fetched | 43 | 77 |
| memory | ~354 MB | ~364 MB (Rust cache off) |

**A Rust mesh cache** (`MeshCache`: an LRU by bytes, `MAPS_MESH_CACHE_MB`, tested). In the
benchmark it had zero hits: the renderer's GPU-side cache holds every tile, so the Rust cache only
duplicated it, at about 20 MB. Shrinking the GPU cache to 12 tiles forces re-fetches; there the Rust
cache halved request time (6.7 against 15.9 ms) but changed nothing visible, because the ring kept
every tile ahead of the view. A stress run at 16,000 px/s (`MAPS_BENCH_SPEED`; the pan took 2.1 s
instead of 8, every frame rendered) gave the same: every tile ready, cache or not. So the cache is
off by default and kept for a case that needs it.

The question was whether a tile's request-to-drawable time could go under 1 ms. Per request, no:
the webview's IPC alone is several milliseconds, and a resting CPU makes the mesh slower still. But
the time a user waits for a tile is now 0 ms in every run, because the request happens before the
tile is visible.

## Zooming: the benchmark that was missing

Using the native renderer by hand showed what no benchmark had: zooming out left large gray areas,
and zooming in seemed to drop parts of the map as it redrew. Every benchmark so far had panned at a
fixed zoom, so nothing measured zooming. That was a gap in the tests, not just in the renderer.

`MAPS_ZOOM_BENCH=1` (`tools/zoombench.sh`) zooms out 15 -> 10 and in 10 -> 16 at the pier, 1.2 s
each, with the pixel check, and saves the worst frame as a PNG. First run, small cutout:

| | native | MapLibre |
|---|---|---|
| zoom out: frames with background / worst frame | 17 / 14% | 17 / 28% |
| zoom in: frames with background / worst frame | 13 / 13% | 5 / 11% |
| background at rest at zoom 10 | 14.7% | 0% |

The saved worst frames showed the cause: magenta bands at the sides, where the cutout's tiles end.
The cutout is about 0.2 by 0.15 degrees, so two scroll ticks out the window is mostly outside it.
Past zoom 10 the archive has no tiles there, and the native renderer drew nothing for an empty tile.

Fixes, each measured:
1. **An empty tile falls back to a coarser ancestor** (the archive keeps whole low-zoom tiles, so
   one almost always has data), and the whole ancestor chain of an empty tile is requested at once
   rather than one round trip per level.
2. **`areTilesLoaded()` was wrong for empty tiles.** It counted an empty tile as done by itself, so
   the benchmark measured "at rest" before the coarse stand-in had arrived. An empty tile now counts
   as done only once an ancestor with data is loaded (or none exists).
3. **Prefetch along the zoom:** while the zoom moves, the view's tiles one tile-zoom ahead in that
   direction are requested at low priority.
4. **Child tiles as stand-ins when zooming out**, if all four are loaded. It turned out not to
   matter: the tiles missing when zooming out are at the edges, outside anything loaded before.
5. **A bigger area to use the app in:** `data/westla.pmtiles`, Malibu to downtown and the mountains
   to LAX (25 MB, 772 tiles), extracted over the LAN. The app opens it by default; the benchmark
   scripts pin the small cutout so results stay comparable (`MAPS_ARCHIVE`).

After the fixes, small cutout (native): zoom out, 1 frame with 0.7% background, 0 frames drawing a
stand-in for a loading tile; zoom in, none. With prefetch off: 10 stand-in frames zooming out, 21
tiles never requested before they were visible. On the West LA archive: no background and no
stand-ins in any frame either way. MapLibre on the same zoom: up to 30% of a frame background.

Problems on the way:
- **My stand-in count measured the wrong thing twice.** After the empty-tile fix, an empty tile
  drawn by its ancestor (its final look; nothing better will load) was still labelled "ancestor",
  the same as a tile still loading, so the count measured the cutout's edges. It showed 14-17
  stand-in frames with prefetch on or off. Recording why each tile was not ready showed none were,
  which was the contradiction that exposed it. Earlier, on that bad count, I concluded the zoom
  prefetch "did not help" and changed its coverage on a theory; that change was harmless but
  unmotivated.
- **A multi-edit script half-applied.** The asserting edit helper refused an edit whose target text
  had changed, as intended, but the edits before it in the same script were already written, so
  one build had a subset of the changes. Measurements from that build are not used.

## Labels, and a zoom-out limit

**Zoom-out limit.** The lowest zoom is now the one at which the world exactly fills the window
(`worldFitZoom`), and the center is clamped so the view never passes the world's edge
(`clampCamera`, tested). Fully zoomed out the snapshot shows the whole world edge to edge, the
coarse world tiles filling in through the empty-tile fallback. Found on the way: a snapshot at
`MAPS_SNAPSHOT_ZOOM=0` silently used the default view, because 0 is falsy.

**Labels.** MapLibre draws text with signed-distance-field glyphs in WebGL: a glyph atlas, shaping,
placement along curves, days of work. v1 splits it differently:
- Rust chooses the labels while meshing (mesh format v3 adds a label block): place names at their
  points (kind, the data's `min_zoom`, its `sort_key` as priority), and for each road name the
  longest straight segment in the tile, its midpoint and angle (turned to read left to right).
  Only anchors inside the tile's own extent, so neighbouring tiles do not send the same label.
  About 78 labels and 2.4 KB per zoom-14 tile.
- The frontend (`labels.js`) decides what shows: by zoom, a street name only where its straight run
  is longer than the text, only whole labels on screen, and greedy collision in priority order
  (kind first, then labels placed last frame so they do not flicker, then the data's priority) over
  a grid. It draws on a 2D canvas over the map with the browser's text rendering, which gives full
  Unicode shaping. Colors and halos come from the Protomaps flavor through the palette generator.
  Only tiles drawn as themselves contribute labels, so a label never jumps when a stand-in is
  replaced.

Tests: the label block on the wire in Rust (UTF-8 cut on a character boundary) and its round trip
in JavaScript; collision (rank order, touching edges, overlaps across grid cells and in lower grid
rows, rotated boxes); 5 planted bugs in the collision code caught, one only after its test was
fixed (my first "lower row" test overlapped in the candidate's first row, so it could not tell).

Snapshots at zoom 15 and 12 show street names along their roads, district names in capitals, and
city names, with no overlaps. Cost on the pan benchmark: render CPU 1.6 ms a frame (p95 3) against
0.5 without labels, of which labels 0.9 ms for about 19 labels; worst 10 ms on the first pass,
before the text-width cache fills; about 20 MB more memory (the label canvas). MapLibre with its
labels: 4.7 ms. Zoom benchmark: 1 frame with 0.7% background zooming out, on both archives (it had
been 0 on West LA before labels; at noise level, but not identical).

Not in v1: road labels along curves, highway shields, POI icons and names, water labels, and the
search box in native mode.

## Polish: search, the pier, points of interest, shields, water names

**A search box for the native renderer** (`search.js`). MapLibre's geocoder widget depends on
MapLibre, so the native app has its own: debounced, 8 results, keyboard navigation. Coordinates are
answered in the frontend by the WASM build of `crates/coords`; names go to the Rust `search`
command. A chosen result flies the map there and drops a marker. `MAPS_SEARCH=<query>` runs it end
to end and snapshots the result, with the box and marker painted in from their DOM state (readPixels
cannot see HTML). A typed coordinate lands exactly under the marker. "santa monica pier" lands about
a kilometre inland: GeoNames itself records the pier at exactly 34.0, -118.5. That is the data, and
the MapLibre app uses the same index.

**The pier.** It was drawn as an outline because the mesher drew all landuse before water, and the
Protomaps style draws the pier layer (and pedestrian areas and dams) after water: the ocean polygon
painted over it. The palette generator now records which landuse kinds the style puts above water,
and the mesher draws those after the water polygons.

**Points of interest, shields and water names** (mesh format v4: each label also carries an icon
index and a color index). All rules come from the style through the generator: 36 POI kinds (icon
named after the kind, `station` uses `train_station`, `townhall` has no sprite in this sheet so it
is text only, as in MapLibre), text colors by kind group; shields from a road's network and the
length of its route number (US:I, NL:S-road, else the generic shield); water names in blue italic
along streams and rivers from zoom 13, at the point or polygon center for seas and lakes. The
frontend loads the same sprite sheet MapLibre uses and places by MapLibre's order: places, POIs,
major roads and shields, water, minor streets. A POI's name sits right of its icon, or left if right
collides (`placeLabels` now takes alternative positions; tested).

The snapshot at the pier at zoom 15 now carries what MapLibre's does, apart from styling details:
POIs with icons, route shields for 1, 10 and 66, Kenter Creek in blue italic, street and district
names, nothing overlapping.

Problem: the palette generator built the style without a language, and Protomaps adds label layers
only when given one, so the POI, water and shield label layers were missing from its lookup. It
crashed rather than producing an empty table, which is the better way to fail.

## Polish: the label hitch, casings, dashed rail

**A 16 ms label frame, found and fixed.** I assumed the first sprite draw or font loading. Recording
each label frame's time, its text-width cache misses and whether it was the first sprite frame
showed otherwise: the slow frames (7-11 ms) were the ones measuring new text (4-27 misses), never
the first sprite frame, and the cost did not scale with the number of misses, which points at
setting `ctx.font` (the browser parses and resolves it) rather than measuring. Text is now measured
when a tile arrives (`LabelLayer.prepare`), off the frame, and the font is set only when it changes.
Worst label frame 8-11 ms to 3-4 ms, with no measurement in any frame.

**Road casings.** The generator takes the style's five "late" casing layers (`#e0e0e0`; total width
= the road's gap width + twice the casing's own width, sampled every quarter zoom with MapLibre's
interpolation) and the zoom where each border first becomes visible, so low-zoom tiles carry no
invisible geometry. Five casing classes come before every road fill: 16 line classes, exactly what
the shader's uniform arrays hold. Cost: zoom-14 tiles 235 to 274 KB (+17%).

**Dashed railways**, cut into dashes in Rust at the tile's zoom (+0.5), with the style's pattern
(0.3 on, 0.75 off, in line widths). Trade-off: on an overzoomed tile the dashes stretch with the
zoom; a dash distance attribute on every line vertex would fix that at about 4 bytes a vertex.

Tests: dash pattern (straight, across corners, degenerate input), casings ordered before fills. Of
four planted bugs, one ("the pattern never repeats") made the dash loop run forever: my mutation
script hung with the bug still in the source file, and `pkill -f` on the test binary's name then
killed the shell running it, because the pattern appeared in its own command line. The file was
restored from the backup; mutation runs now carry a time limit, and a timeout counts as a catch.

## Curved street names, and two bugs they exposed

**Labels along their roads.** Mesh format v5 sends a street or stream name's path: the longest piece
of the line inside the tile, simplified (Douglas-Peucker, at most 32 points), oriented to read left to
right. The frontend lays the text out character by character along it (`layoutOnPath`, pure and
tested): each character at its own distance along the path, turned to the direction there; refused
where neighbouring characters would turn more than 45 degrees (MapLibre's rule) or stand upside
down; tried at the middle of the path and then at 35, 65, 20 and 80% of its length, since the middle
of a road may be a hairpin. A curved name claims one small box per character in collision. A nearly
straight one is still drawn as one string, keeping its kerning. Snapshots in Pacific Palisades show
West Sunset Boulevard, Las Lomas Avenue and Monument Street following their bends. Still missing:
Temescal Canyon Road, too tight for five anchors where MapLibre searches more; and per-character
drawing loses kerning on curves ("Whitfield" looks crowded).

Also fixed on the way: landuse drawn in the style's layer order rather than the tile's feature order
(a school under an overlapping park came out green).

**Bug 1, an infinite loop in Rust.** A path with more than 32 points was re-simplified with a
tolerance derived from its point count; a wiggly canyon road could leave the count unchanged, so the
same tolerance came round again forever, in a tile-serving thread. The native app hung only where
such a road was in view. Meshing all 772 West LA tiles offline reproduced it; a 40-point zigzag test
hung first (exit 124 under a time limit) and passes after the fix: double the tolerance each pass,
starting from the original line. The whole-archive mesh check should run on every mesher change; the
small cutout had no canyon roads, which is why the earlier check passed.

**Bug 2, a startup race I had met before.** Native runs hung at startup about one time in five, after
exactly 21 tile requests, with an empty log. Steps that narrowed it: uncaught errors are now reported
to stdout (the webview console is invisible to a terminal run) - none appeared; process CPU averages
said the page was waiting, not spinning; a watchdog for withheld animation frames stayed silent; a
heartbeat of the renderer's state showed the map idle at frame 5, `idle` fired, everything loaded.
The startup handler had not been listening: the search box's `await initCoords()` sat between creating
the map and registering for its first `idle`, and when tiles came back fast, `idle` fired during the
await. This is the trap recorded from the homelab map viewer ("an inserted await re-orders what
follows it; MapLibre never replays load"), reintroduced here. Fixed twice over: all startup awaits
happen before the map exists, and `RustMap.whenIdle()` resolves at once when the map is already idle,
so a late listener cannot miss it. 0 failures in 25 runs after the fix (about 1 in 5 before).

## The whole world: reading the planet over the LAN

Street detail existed only inside the extracts. The homelab's map service has the full planet
(PMTiles v3, gzip, zooms 0-15, 1.43 billion addressed tiles, 128 GB) with Range support, so the
Rust reader now reads it in place: `Archive::open_url` (vtile feature `http`, with `ureq` and rustls:
HTTPS is not a primitive worth hand-rolling) issues one Range request per directory or tile over a
reused connection, refuses anything but a 206 of the exact length, and caches leaf directories (a
planet's directory is split into many; each costs a round trip).

Checked before building: the format and zoom range (`go-pmtiles show` on the URL), the certificate
(Let's Encrypt, trusted by the standard roots), and latency: 34 ms median per request with a new TLS
connection each time, 12 ms over a reused one.

Check after: every one of the 772 tiles of the West LA extract, read again from the planet, is
byte-identical (the extract was cut from it). A remote read takes 9.7 ms median, 12.8 p90, 40 worst;
779 round trips for 772 tiles, so the leaf cache held.

The app reads the planet when `MAPS_ARCHIVE_URL` or the gitignored `data/remote.url` names it (the
LAN hostname stays out of the repository), falls back to the local extract if it cannot be reached,
and reports which it uses. A world-sized archive starts the camera at Santa Monica. The benchmark
scripts set `MAPS_LOCAL_ONLY=1`, so their numbers stay comparable. Snapshots of Paris at zoom 15 and
Tokyo at zoom 14 show full street detail, curved names, POIs, and Japanese text shaped by the
browser. The MapLibre app still reads the local file.

The search index is worldwide (13.46M places); with the planet, the map now is too.

## Optimization pass: one gate, less code, and rotation

**A single gate.** `tools/check.sh` (`npm run check`) runs every check the project has, in one
place: rustfmt, clippy with warnings as errors, ESLint (recommended rules plus unused variables,
strict equality, prefer-const), ruff check and format, shellcheck, a check that the generated palette
matches its generator, the Rust and JS unit tests, the decoder diff against @mapbox/vector-tile, a
time-limited mesh of every West LA tile (the canyon-road loop would now fail it), and search parity
with the production server. Each check was shown to fail on a planted violation before its pass was
trusted. Turning it on found 142 formatting diffs, 16 clippy findings and 7 ruff findings, all fixed.

**Dead code removed:** unused generated colours; the MapLibre prefetch, fade and warm-up switches
(experiments that changed nothing); a first-sprite diagnostic; exports used only inside their module;
the MapLibre app's startup search self-check (search parity covers it). The two apps had each wired
the dev tools (benchmark, memory probe, snapshot, coverage, overlay) their own way; that now lives
once in `src/devtools/harness.js`, and every mode was re-run in both apps.

**Rotation.** The native renderer had no bearing; MapLibre has one, so the stress benchmark could not
compare them. The camera now carries a bearing, and everything that asked "which tiles are on
screen" takes the rotated view as a quadrilateral (a separating-axis test against each tile's
square). The shaders rotate about the screen centre. Per-tile clipping was a scissor rectangle, which
cannot be rotated, so it is now the stencil buffer: each tile writes its square, then draws only
where the stencil matches. Right-drag rotates; `easeTo` takes the shortest way round.

Labels: positions go through the same projection as the geometry; a curved name whose path now runs
right to left is read the other way; a straight street name turns with the map and is then flipped to
read left to right. The first 90 degree snapshot showed place and area names standing on end. MapLibre
keeps point labels level with the screen, so now only labels with a run turn; the rule is a pure
function with a test, which fails when the rule is removed.

How I checked it did not change the unrotated map: the pre-rotation commit built in a second
worktree, the same snapshot from both. Labels off, 0 of 960,000 pixels differ, so stencil clipping
covers exactly what the scissor did. Labels on, 1.88% differed, which looked like a label bug. A diff
image coloured by version showed every POI name centred in the old build and beside its icon in the
new one: the old build had no icons at all. `public/assets` (the sprites) is gitignored, so a fresh
worktree lacks it. With it linked in, 0 pixels differ. The lesson for comparisons across
checkouts: list the ignored inputs (`git status --ignored`) before trusting a difference.

At 30 degrees the native map matches MapLibre's geometry. Pan benchmark, render time per frame
2.4 ms cold and 2.9 warm against 3.2 and 3.0 before: the stencil costs nothing measurable. The zoom
benchmark shows one or two frames with up to 3% background zooming out from 15 to 10; the
pre-rotation build does the same, so it is older than this work, and left for the stress benchmark.

## Test coverage, and seven bugs it found

Coverage had never been measured. Rust has no coverage tool installed here, but the nightly toolchain
ships `llvm-cov` and `llvm-profdata`, so `tools/coverage.sh` builds the tests with
`-C instrument-coverage` into a separate target directory and reports per file, with the uncovered
lines of any file named. Rust unit tests covered 49% of lines (the gate's decoder diff, whole-archive
mesh and search parity cover more, outside that figure): the PMTiles reader 40%, the MVT decoder 46%,
native search 0%.

Two test-writer agents took the reader and decoder, and search and the coordinate parser, editing
only test code, each test group proven by planting a defect and watching it fail. The HTTP source is
tested end to end against a local listener with canned responses: a 200, a 206 one byte short or
long, a body cut off by a closed connection. Search runs against a 12-row in-memory database with the
production schema, with FTS5 operator characters chosen so a swallowed SQL error cannot pass as an
empty result. 172 Rust tests now (was 37); those four files 99-100%; 72 planted defects, 69 caught,
and the 3 survivors are equivalent (one of three read-only guards removed, the other two still hold)
or unreachable code.

Seven tests failed on the code as it was, and I fixed each:

- A corrupt PMTiles directory whose tile-id deltas or chained offsets sum past 2^64 panicked in a
  debug build and wrapped silently in release. Now an error.
- An MVT bytes field declaring a huge length overflowed the bounds arithmetic instead of reporting
  truncation.
- Geometry deltas past the i32 range overflowed; they now wrap (a wrong point from corrupt input,
  never a panic).
- `search::open` put the database path into a `file:` URI unescaped, so a directory named with `#`,
  `?` or `%` could not be opened. No real path has one today.

## The stress benchmark

`tools/stress.sh` (src/devtools/stress.js) plays a seeded script of 150 camera operations in about
52 seconds: pans, zooms, turns and all three at once, most interrupted by the next before they end;
bursts of wheel notches, drags and right-drags sent as real DOM events so the input handlers run;
and teleports to views nothing has loaded. The script is a pure function of the seed (unit-tested:
deterministic, every kind present, every value inside the region), so both renderers get exactly the
same input. Each renderer runs twice: for timing, then with the pixel check, which reads every frame
back and would distort the timing. The region is the archive inset by a quarter, with a lowest zoom
at which the turned viewport stays inside the data, so the empty space beyond a cutout's edge is
never counted as a missing tile. The first version rounded that zoom to the nearest hundredth, which
can round down; a test caught it.

The harness was wrong twice before its numbers meant anything:

- MapLibre threw on every frame after the third operation. I made each operation catch its own
  error, then recorded the first operation after which the camera stopped being finite: a pan that
  passed `bearing: undefined`. MapLibre tests `'bearing' in options`, so an explicit undefined
  becomes a NaN bearing. Now only the keys an operation sets are passed. (WebKit's error `stack`
  carries no message, so the stdout reporter now prints both.)
- MapLibre ignored most synthetic drags, so it was getting an easier test. Mouse moves sent to
  `window` never reach a listener on `document`, which is where MapLibre listens; sent to the
  document they bubble to both. The run now counts input events that did not move the map; it is
  zero for the native renderer and one drag of 15 for MapLibre.

A control: with stand-ins switched off, the pixel check sees background in 15 frames of a short
run, worst 100% of the frame. So a zero is a measurement, not a blind spot.

West LA extract, seed 1, the same 150 operations:

| | native | MapLibre |
|---|---|---|
| frames with background showing | 0 of 2380 | 325 of 3066 (10.6%) |
| worst frame | 0% | 100% (after a teleport) |
| render p50 / p99 / worst | 2 / 7 / 14 ms | 5 / 13 / 23 ms |
| missed refreshes | 0.8% | 0.7% |
| memory start / peak / 10 s after | 435 / 619 / 558 MB | 494 / 1501 / 1106 MB |
| errors, settled at the end | 0, yes, 0% background | 0, yes, 0% background |

Native over the LAN, reading the planet: background in 44 frames (1.8%), render p99 8 ms, peak
700 MB. The worst frame is all background, after a teleport to a view with no coarser tile in
memory to stand in. The obvious fix is to keep the low zooms around the current region resident;
not done in this pass.

## Native memory at rest: where 296 MB became 375

Idle memory had drifted from 296 MB (the first native renderer) to 375. I built eight commits in a
scratch worktree and probed each twice on the reference cutout. The first native commit measures
322 MB today, so 26 MB of the drift is the platform (the blank-window floor moved), not this code.
The other 53 MB came in three steps: zoom prefetch +8 MB (app process), the native search box +22 MB
(web process), curved street names +18 MB (both processes, about half each). Labels themselves cost
nothing measurable at rest (374 MB with them off).

Of the search box's 22 MB, 17 MB is the coordinate parser's WASM module, compiled at startup
(skipping the load is the experiment that showed it). It now loads on the first search keystroke.
Startup awaits nothing before the map exists, so the earlier idle race cannot come back. At rest:
355 MB. Coordinate and place searches checked end to end afterwards. The curved-name 18 MB is not
yet broken down.

## Lessons from dugout: a policy, a retry, errors before results

Dugout (the baseball site) is local-first in a browser, and three of its bugs apply here.

Its bundled database engine fetched a component from the internet on first use, and the test meant
to catch that passed, because it checked content the page already had before the action ran. This
app had `"csp": null`, so nothing stopped the page reaching out. It now has a content security
policy: the page may load its own assets and talk to the Rust backend and `tiles://`, nothing else
(the remote planet is read by Rust, outside the page's reach). Every refusal is printed.
`tools/offline.sh` runs a stress run on each renderer and both kinds of search on the release
build and fails on any refusal.

The control run makes one request the policy must refuse. Its first run failed, and that was the
check working: the refusal lines start with `[frontend] `, so my pattern counted nothing, and the
four green runs before it were vacuous. With the pattern fixed, the policy turned up a real fault:
the search box's input and list were built from markup with `style=""` attributes, which the policy
refuses, so they rendered unstyled. They are now built with script-set styles. The search snapshot
could not have shown this, because it paints its own copy of the box, so the search run now reports
the input's computed style and the gate checks it.

Dugout's lazy database load guarded on the finished connection, so a call during the load started a
second one. The coordinate parser's lazy load already shared one promise, but kept a failed load
forever. `once()` (src/rust-renderer/once.js) drops a failed load so the next search retries;
tested, with both planted defects caught.

Errors before results: `offline.sh` fails on any reported uncaught error, unhandled rejection,
failed search or map error before it looks at a result, and `tools/stress.sh` now exits non-zero
on camera errors, listener or uncaught errors, policy refusals, a non-finite view or a failure to
settle. Two planted defects (the old markup box; a search that throws) each failed the gate.

## A disk cache for the planet: seen once, drawn offline

Dugout keeps what it has fetched in the browser (a service worker cache, then a database in the
browser's storage) and had to fight the browser for both. Here the same idea is Rust and a file.
The remote planet's byte ranges go into a SQLite file (`src-tauri/src/diskcache.rs`; vtile's
`RangeCache` trait keeps the crate free of SQLite). The header is always read from the network;
its ETag names the version, a new version drops the old one's ranges, and every later request
carries If-Range so a planet replaced mid-session is refused rather than mixed. Past a size cap
(2 GB by default) the least recently used ranges go, never the header and root directory. With the
server unreachable the app opens the planet from the cache alone and draws what it has seen.

`tools/cache.sh` proves it against a local Range server (`tools/rangeserve.py`) the test can stop
and restart: 239 requests cold, 1 warm (the header), 0% background offline over the same seeded
60-operation run, the old version dropped under a new ETag, and a cache file of noise costing only
the cache. A control with the cache off fails all of it. One thing the control taught: offline
with no cache the app falls back to the local West LA extract, which also draws 0% background, so
the pixel number alone proves nothing; the log line naming the cached copy does.

A reviewer found five things I fixed before committing: a read-only or locked cache dropped hits
it had already read (and so could not open offline at all); a cache file that would not open
turned off the remote planet entirely; a server that ignores If-Range could have put bytes of a
new build into the cache under the old version (now refused by comparing the response's ETag);
a weak ETag in If-Range would make a compliant server answer every request with the whole file;
and eviction's byte count could drift if a batch failed. Each has a test that a planted defect fails.

Over the LAN, 150 operations against the real planet: background in 7.8% of pixel-run frames with
the cache off, 0.2% with a cache filled by the timing run just before, 0.4% fully warm (52 MB).
The warm worst frame is still all background after a teleport: reading and meshing the target
tiles takes longer than a frame when no coarser tile is on the GPU. The cache removes the network
from that, not the gap; keeping low zooms resident is still the fix.

## Preparing the repository to publish

A scan of every file in every commit found two things that could not go public: the home map
service's hostname, as the default in 33 versions of `tools/fetch-data.sh`, and absolute paths
under the home directory in two benchmark results. The script now takes the service from
`MAPS_SRC` and refuses to run without it; the paths are relative. Comments that pointed into the
private repository the Python service lives in now say what it is instead. The history itself is
rewritten in a separate clone for publishing, so this working repository keeps its own.

The scan also found a licence gap: the native renderer showed no credit for the map data, which
OpenStreetMap's licence requires. It now draws "© OpenStreetMap contributors, Protomaps" in the
corner as plain text (a link would navigate the app's only window). The end-to-end search reports
the credit's text and whether it lies inside the window, and `tools/offline.sh` requires both; with
the credit planted out of the page, the gate failed.

The code is licensed MIT or Apache-2.0, and the README now describes the whole app, how it was
built, and how to get the data from public sources.

## Weather: NOAA radar and hazard areas

Before writing code I fetched one of everything and read it. The MRMS base reflectivity mosaic is a
0.7 MB gzipped GRIB2 file every two minutes: one message, a regular latitude/longitude grid of
7000 x 3500 cells at 0.01° (template 3.0), values packed as a 16-bit PNG (template 5.41), dBZ =
(raw - 9990) / 10. The G-AIRMET feed is GeoJSON, but asking for it without a forecast hour returned
only the snapshot six hours ahead; the API's schema shows a `fore` parameter (0, 3, 6, 9, 12), and
each hour is a separate snapshot valid at one instant. The first request for hour 0 failed with a 504.
So the design took three facts from the data rather than from assumption: fetch all five hours and
show the nearest, keep the last good copy when a request fails, and read only the two GRIB2 templates
MRMS uses, refusing anything else by name.

`crates/wx` decodes and reduces a frame in about 0.1 s (decode 63 ms, thin 38 ms). Thinning keeps
the strongest echo of each 2 x 2 block: averaging would wash out a storm's core. Its tests build
synthetic GRIB2 messages around real PNG encodings; five planted defects (a two's complement
latitude where GRIB2 uses sign and magnitude, a flipped binary scale, thinning that kept the weakest
echo, longitude left in 0-360, the wrong snapshot on a tie) each failed them. A real frame and real
hazard responses pass through the same parser under an environment variable.

The backend's fetching sits behind a small trait so its tests run a fake NOAA that fails, recovers
and moves on. The first version merged every forecast hour's features into the "nearest" snapshot,
which the fake caught by drawing one area five times. The same bug was reachable for real: a new
package issued between two of a round's requests leaves two hours valid at the same time. The fix
shows exactly one forecast hour.

The radar is drawn in one pass over the screen. Every pixel goes back through the camera's rotation
and scale to Web Mercator, then to latitude and longitude, then to a cell index; nothing is
reprojected on the CPU. The first country-wide picture looked right, but looking right at zoom 3.6
cannot show an error of a few cells. The check that can: draw a synthetic frame whose only lit cell
holds Santa Monica Pier, at zoom 15, twice (lit and unlit), and find the pixels that changed. Their
centre must be where the map's own projection puts the pier. The first run of this probe found
nothing at all, which looked like a broken shader but was the probe: its texture had no filter set,
WebGL's default wants mipmaps, and an incomplete texture reads as zero. With that fixed: offset 0, 0
px; 46 x 56 px for a cell that should be 47 x 56.7 (1/cos 34°). An inverted latitude and a
one-cell longitude shift, planted in the shader, both failed `tools/weather.sh`.

Nothing is fetched until a box is ticked, so the benchmarks and the offline gate stay offline; the
fetching is in Rust, so the content security policy is unchanged.

### Loading: 15 seconds of nothing, then everything

Ticking the boxes showed nothing for a long time and gave no sign that anything was happening. The
end-to-end check had hidden this: it polled the backend itself every two seconds, so it never saw what
a user saw. Changed to wait on the layer's own updates and timestamp them, it measured first frame
15.0 s, hazards 15.0 s, all frames 15.2 s: exactly the frontend's 15-second poll. NOAA was not the
cause (0.3-0.8 s a request); the data sat ready in Rust until the next poll.

Now Rust emits an event after every frame and forecast hour that arrives (the poll is a 60-second
safety net, and a burst of events is folded into one refresh), and a round fetches the most useful
thing first: the newest frame, then the hazards, then the older frames three at a time. The state
carries progress, and the panel shows "starting…" on the click, then "radar: loading 4 of 10…".
After: first frame 1.3 s, hazards 2.6 s, all ten frames 7.1 s. A backend test records the state at
every signal and requires the newest frame alone first and the hazards before any older frame;
fetching oldest first, planted, failed it.

## One frame loop: tick every frame, redraw on change

The weather bug (data ready in Rust, shown 15 seconds later) was a symptom of a renderer with no
single place where things happen: tiles were uploaded to the GPU in their network callbacks, radar
frames on events, the radar animation on a 100 ms timer, and a repaint was requested from wherever.
The model from my own game engine fixes that: one loop, every display frame, takes in what arrived,
updates (animations as a function of time), and draws.

Whether it should draw every frame was measured, not assumed. Idle CPU at rest: on demand 4.0% of a
core, ticking every frame without drawing 4.0%, drawing every frame 65.7%. So it ticks every frame and
redraws only when something changed.

The measurement first said 36% at rest for every mode, which sent me after the wrong cause: I blamed
the dev tools' own animation-frame loop, gated it, and saw no change. The gate had never engaged. The
F4 overlay remembers being open between sessions, so it was open in every measurement run, drawing
every frame and polling process memory every second. With it closed: 4.0%. `MAPS_F4=0|1` now forces
it, and every benchmark and gate script sets it closed. The idle memory figure moves too: 337 MB with
the overlay closed, against the 355 recorded earlier with it open.

`src/rust-renderer/loop.js` is the loop: an inbox of arrivals uploaded inside the frame, most urgent
first, within a 4 ms budget (at least one per frame, so a large arrival cannot starve the queue);
updaters that report whether the picture changed; a redraw only when it did; and a frame clock (ticks,
draws, frames later than 1.5x the median interval). It is pure, with a clock passed in, and its nine
tests run in Node; four planted defects (redraw every frame, no budget, least urgent first, arrivals
uploaded in the frame they arrive) each failed them. Tile responses and radar frames now arrive as
jobs in that inbox, and a tile counts as loaded when it is drawable, not when its bytes arrive.

Before and after, the same stress script (150 operations, seed 1), the overlay closed, the old build
from a worktree of the previous commit:

| | before | after (two runs) |
|---|---|---|
| missed refreshes | 28 (0.9%) | 19, 23 (0.6%, 0.7%) |
| frames over 50 ms | 2 | 0, 0 |
| frame interval p99 / worst | 25 / 55 ms | 23 / 41, 24 / 40 ms |
| render p99 / worst | 8 / 18 ms | 7 / 14, 7 / 11 ms |
| background frames | 0 | 0, 0 |
| idle CPU | 3.4% | 4.0%, 3.8% |
| idle memory (three runs each) | 335-337 MB | 336-337 MB |

The worst frames improved, as the upload budget would predict: a burst of tiles is spread over frames
instead of landing in one. Peak memory in the stress runs read 17-57 MB higher, but the two runs of
the new build differed by 38 MB between themselves, and the idle probe, three interleaved runs each,
shows no difference; I read it as run-to-run noise. The zoom benchmark's one zoom-out frame with 1-2%
background is there in both builds: the known gap, not a new one.

## A time bar: radar, satellite and hazards at any time in 18 hours

The model came from the Aviation Weather Center's GFA, read from its help page rather than guessed: one
time slider (18 hours back to 18 ahead), every layer synced to it, observations in the past and
forecasts in the future, the radar loop being the five images ending at the slider's time, the valid
time stamped on the map, and an altitude control that shows an advisory only within 3,000 ft of the
chosen level.

Satellite needed a source, and the first two candidates each failed on a fact checked before any code.
NASA GIBS serves GOES infrared as Web Mercator tiles with a time parameter, but its newest image was 5
hours 18 minutes old. AWC's own mosaic is not public. NOAA's nowCOAST WMS has GOES-East and West
longwave every 5 minutes, five minutes behind real time, but only about 6.5 hours of history, and it
renders greyscale where GIBS is colour-enhanced: stitching the two would change the clouds' colours
partway along the bar. Satellite uses nowCOAST alone for now, and the bar shows where its history
ends. Its PNG was histogrammed before the shader was written: 25% of pixels transparent (no
satellite), clear ground at grey 48-128, cloud tops 130-224; the shader fades cloud in from 130.

`wx::timeline` decides what to show at a time: a radar slot takes the frame nearest it within 4
minutes and never one from after the chosen time; satellite is the newest image at or before it and
no older than 30 minutes (a stopped feed is not passed off as current); hazards are the snapshot
valid nearest. A planted defect survived its tests: removing the deduplication of loop frames
changed nothing, because two slots 10 minutes apart can never both take a frame that must be within
4 minutes of each. The dedup was dead code; it is now a compile-time assertion of that reason.

The backend went from "keep the newest ten frames" to "serve any time": what exists on the servers is
refreshed every round, frames and images are fetched when first needed with one fetch per key
however many callers ask, kept compressed and evicted least recently used, but never the present's
loop. The first run of its tests hung: a line held the weather lock while calling a function that
takes it again, and Rust's Mutex is not re-entrant. Killing the hung test with `pkill -f` killed my
own shell, twice, because the pattern appeared in the command line doing the killing; `pkill -x` on
the process name does not have that problem.

The window, captured with the dev status line moved out of the weather panel's corner: the stamp
reads "Radar 22:00Z obs · Satellite 21:58Z obs · Hazards 21:00Z issued", and the bar shows radar
history back to its start, satellite from 15Z, hazard snapshots at 21Z to 09Z and the red now line.
`tools/weather.sh`: the present ready in 5.6 s, three hours back in 4.3 s, the flight-level filter
taking 21 hazard areas to 11-18 depending on the level, registration still exact.

## A review of the time bar, and METARs

An independent review of the time bar change, given the code and the spec but not my reasoning, found
five defects and two judgement calls; each was reproduced before it was fixed.

- Changing the time while frames were loading did nothing until every fetch for the old time had
  finished, and meanwhile the map showed the old picture under the new label. The guard meant to stop
  this compared a state object that could not change during the loop. Now every change of time bumps
  a generation; the fetch loop stops after the current request and a frame for an abandoned time is
  dropped before it reaches the GPU. The end-to-end check picks 6 hours back, then 4 hours back 30 ms
  later: it settles on 4 hours back in under 3 s with nothing uploaded for 6 hours back. With the
  guard planted out, 6 textures for the abandoned time reached the GPU and the gate failed.
- The step button did nothing from live: the first step landed less than a step from now, which reads
  as live. A pure function now steps to the nearest mark a whole step away; the old arithmetic,
  planted back, fails its test.
- A panic inside a fetch left its key marked in flight, and every later request for that time waited
  forever, the webview's refresh with it. A guard now releases the key however the fetch ends. The
  first version of the test hung rather than failed (a scoped thread waits for the stuck one); it now
  fails at a 5-second timeout.
- A change of radar grid cleared memory but not the disk, and rows dropped past the cap at load were
  never deleted, so both came back at every start.
- The two fetch commands ran blocking network calls on the async runtime's workers. My comment said a
  sync command marked async runs on a thread pool; the macro's source says it is spawned onto the
  runtime. They are async now and run their work on the blocking pool.
- Hazards had no distance limit: 17 hours back showed today's snapshot labelled as issued. A snapshot
  is now shown only within 90 minutes of the chosen time (snapshots are 3 hours apart, so one is
  always in reach where forecasts exist), and the stamp says when none is near. That broke my own
  end-to-end check, which waited for hazards at a time that now correctly has none: the definition of
  ready was wrong, not the code.

METARs: airport observations coloured by flight category. The API caps a request at 400 observations;
one past hour over the US took 30 requests (box split until no box was full) and 80 seconds,
including a 504 retried. Doing that across an 18-hour bar is not reasonable to ask of AWC's service.
So the present comes from AWC's bulk cache file (2,903 stations, 0.25 MB, 0.3 s), and history is
what this app collected from it every five minutes while it ran, kept on disk; before collection
began, the stamp says so. A click on a station opens its report. On the national picture the cluster
of MVFR stations in Texas sits on the forecast IFR area.

### Time-lapse

Play steps the bar 10 minutes at a time and moves on only when the step's newest radar frame and
satellite image are on screen (and at least 600 ms has passed), with the next step fetched into Rust's
memory while the current one shows. Two hours played in 10-14 s. My first check of "never leaves a
step before it is on screen" was a line that computed nothing. My second sampled the layer's readiness,
which also reads false while any refresh runs, so it reported complete steps as incomplete. The third
asks the GPU: were the step's frame and image there when the player left it (a frame for a time
already left is dropped before upload, so present means it arrived in time). A player planted to
ignore readiness raced through twelve steps in 0.6 s and the gate listed all twelve.

## Winds aloft as particles

The model screenshot the user found shows streaks over the map: winds at the selected flight level,
drawn as particles. One request to NOAA's NOMADS GRIB filter (U and V at 250 hPa over the box) came
back as two GRIB2 messages with simple packing (template 5.0) and rows south to north, on a 0.25°
grid of 321 x 181: 150 KB in 0.6 s. The decoder took three extensions, each tested at awkward cases
(10-bit values across byte boundaries, a zero-bit constant field, two messages in one file, short data
refused rather than read past, complex packing refused by name); the real file decodes to U from -13
to 69 m/s and a jet of 155 knots. Four planted defects failed the tests, once a planting helper of mine
stopped reporting plants it had failed to apply as survivors: cargo fmt had moved the lines, and the
helper ran the tests on the unchanged code.

GFS runs every six hours and is on NOMADS about four and a half hours later, with hourly forecasts.
A chosen time takes the newest run that had started by then and is published, at the nearest hour,
falling back to the run before when the newest is not there yet. The test for that was wrong before
the code was: my constant for 12:00Z was 16:00Z.

Particles move in longitude and latitude, a fixed number of screen pixels per m/s at any zoom, with
the Mercator stretch on latitude (planted out, it failed its test). The GPU keeps the trails in two
textures used in turn: each frame the old one is drawn into the new one a little fainter, the moved
segments on top, and the result over the map. Trails are cleared when the camera moves.

Adding winds broke the end-to-end check in three places, none of them the winds. The registration
probe compared two frames while particles moved between them. The check for "the selection is ready"
never confirmed the state was for the time asked for; the old 50 ms wait had hidden that race until
the wind fetch made refreshes slower. And that fetch was a real slowdown: it sat ahead of the radar and
satellite uploads, so a slow NOMADS answer delayed the radar for a new time. It runs alongside them now.

With winds alone on: 60 frames a second, no late frames, redraw p50 7 ms and p99 13 ms for the whole
frame. The national picture shows the jet around a trough in the West and a second jet off the
Southeast coast.

The gate then failed once more on unchanged code with no hazards at the present, the second time
that had happened (the first, during a planted run, I had not explained). The state showed no fetch
errors and no snapshot selected: the hazards had simply not arrived yet. When hazards were limited to
90 minutes, "no snapshot near this time" became a valid answer for readiness, but before the first
fetch completes it means "not loaded" too, and the check could not tell them apart. It now requires a
completed hazard fetch (or hazards from disk) before none counts. Three runs after the fix passed.

### Trails that stay through a pan

By hand, the user found two things: panning reset the trails every frame, and zooming did too, with
what looked like doubled wind. A dev mode (MAPS_WIND_MOTION=1) now pans and zooms (scripted, then with
real wheel events) and saves frames during and after each. The first capture confirmed the reset: I
cleared the trail texture on every camera change, so mid-move only one-frame dashes were left.

The first fix kept the texture and carried it with the map: world to screen is a similarity
transform, so last frame's pixels map to this frame's by one exact 2x2 matrix and offset (computed in
double precision, tested against the camera for pans, zooms and turns). The captures showed what that
costs. Resampling a texture every frame with linear filtering blurs it a little each time, so a pan
left a smeared wash behind the streaks; and during a zoom the old trails were magnified while new sharp
ones were drawn beside them: a doubled look, probably what the user had seen.

So the texture went. Each particle keeps its last 16 positions in world coordinates, and every frame
those paths are projected through the current camera and drawn as lines fading from head to tail.
Nothing lives in screen space, so nothing is resampled or left behind. The gate counts trail segments
drawn during the moves: before 64,000, mid-pan 67,000, mid-zoom 51,000-54,000 (particles leaving a
shrinking view start again). With histories planted to be wiped on every camera move, mid-pan fell to
4,961 and the gate failed. Cost with winds alone: still 60 frames a second, redraw p50 9 ms, p99 11 ms.

Two mistakes of mine on the way: a test-file splice that left a duplicate helper, and a shell command
that inserted gate text containing its own heredoc terminator, so the shell ran the remainder,
launching the app. The text now goes through a file.

### Fast moves: wind bunched where the old view was

"Really try zooming and panning fast." The slow scripted moves had passed; fast ones had not been
tried. Two numbers per frame now say what a picture shows: evenness (the coefficient of variation of
particle heads over a 4 x 4 grid of the screen; about 0.15 at rest) and the count of trail segments
longer than any one-frame move. Through a fast zoom out, evenness went to 1.6-1.8 and stayed there:
every particle was still inside the old view's rectangle, a dense patch in the middle of an empty map,
because particles only restart when they age out or leave the view, and a zoom out leaves none. That is
the doubling where the previous tiles were. A fast pan reached 0.51, the incoming side empty.

Respawning now knows the view moved. Particles that leave the view restart in the part just revealed;
on a zoom out, a share of the rest follows them, sized so the revealed part gets its fair proportion.
Trails are cut at any segment longer than 8 px (a 100 m/s wind moves 4 px a frame), so a zoom in no
longer stretches old history into long streaks. The unit tests reproduced the app's own numbers before
the fix (1.73 after a zoom out, 0.41 after a pan), and one of them first passed against a stub because
it compared to the very constant it was checking; it uses a fixed bound now.

After: evenness 0.05-0.09 through every move, no stretched segments. A fast zoom costs a quarter of a
second of shorter trails while the moved particles build history. The gate now fails on bunching or
stretching at any captured moment; with the fix planted out it reported 1.5-1.8.

### The doubling, from the user's screen recording

The user recorded it. I cannot play video, so I took two frames a second out of it with ffmpeg and
read them. They showed what no scripted move of mine had tried: panning off North America over the
Atlantic. The wind field was my box (60N to 15N, 135W to 55W) and stopped at its edges, and when only
a sliver of the view had wind, all 5,000 particles crowded into that sliver: dense, overlapping
streaks against an empty map. That was the doubling.

Two changes. The particles in use now scale with the part of the view the field covers, and restart
only there, so a strip of wind is as dense as any other. And the field is the world: NOMADS's 0.5°
GFS, 720 x 361, 0.7 MB. Asked for whole, NOMADS sends that grid in complex packing (template 5.3), which
the decoder refuses by name; asked for as a subregion, even one that is the whole globe, it repacks it
simply. The 0.5° files are every 3 hours (f007 is a 404), so the field is the nearest 3-hourly step and
the stamp shows its valid time. Sampling wraps at the 0/360 seam. The real file decodes; the fastest
wind at 250 hPa that evening was 200 knots. The gate now also jumps to the mid-Atlantic and pans
there fast: trails present, evenness 0.09.

## The world: SIGMETs, global clouds, METARs everywhere

Radar stays the US: MRMS is a mosaic of US and Canadian radars, there is no worldwide ground-radar
product, and the stamp now says "(US)" so its absence elsewhere is not read as clear sky. The rest
went worldwide.

METARs: the bulk file always held the world (5,147 stations); I had filtered it to the box.

SIGMETs: international SIGMETs from every FIR plus US convective SIGMETs, valid over periods. The feed
is loose: the same field is a number, a numeric string, null or the string "None" depending on the
request; some areas come as several polygons; areas crossing 180 come as longitudes past 180, kept
continuous. One record was skipped on the first real parse; it was a SIGMET reported at a point ("SEV
ICE OBS AT"), so points are now kept and drawn as markers (140 of 140 read). The API's date parameter
returns those in force at a time, so a past time's are fetched once per half hour of history.

Clouds: GOES (5-minute) covers the Americas; outside it, nowCOAST's hourly global infrared mosaic.
The two use different grey scales: over the same region at the same hour, the global image's 10th
percentile was 59 against GOES's 88 and its 99th 229 against 201. A pixel-by-pixel regression over
169,000 matched pixels gives GOES = 0.650 x global + 51.4 (r = 0.976, median error 1.6 grey levels,
5.3 within cloud), applied on decode so one cloud shading serves both. The global layer is drawn
first and discarded wherever the GOES texture has data, so hour-old cloud does not show through
GOES's clear sky.

The end-to-end check failed twice before the code did. Its "ready" for the present did not wait for
the background round to finish, and SIGMETs and the global mosaic come late in that round (the
mosaic is 5.7 MB); the world view was looked at before they arrived. Then its SIGMET click landed
within 10 px of a METAR station inside the SIGMET, and the station's report opened, which is the
right behaviour; the check now clicks with METARs off. The world picture: severe turbulence over
Shanwick and Santa Maria, severe icing from Scotland to Paris, thunderstorms over South America and
Africa, volcanic ash from Fuego and Reventador. Where SIGMETs cluster the labels overlap; they need the
collision handling the map's own labels have.

## The time-lapse buffers; the flash; hazard labels that do not collide

Measured before changing anything: the time-lapse's 12 steps took 11.9 s, the longest gap between
steps 1.8 s (the first run downloads each step when it reaches it), and in 342 of 714 frames a layer
that had something for the chosen time drew nothing. That is the flash: on each step the selection
moved to textures not yet on the GPU, and the draw showed nothing until they arrived.

Three changes. Play now buffers first: every step's radar frame, satellite and global images,
SIGMETs and winds are fetched into Rust's caches (`wx_prefetch`, three at a time; nothing goes to
the webview yet) while the button counts "Buffering k/n". While a step is on screen the next step's
textures are uploaded from Rust's memory and kept from eviction, and the player moves on only when
they are on the GPU. And the draw holds the last picture until the chosen time's texture arrives,
instead of drawing nothing. A held loop now fetches only the radar frame it shows, not all five.
After: buffering 6 to 8 s, then 12 steps in 7.5 s, longest gap 0.65 s (each step holds 0.6 s by
design), blank frames 0 of 448. The gate now fails on any blank frame or a gap over 1.2 s.

Changing what radar fetches broke the gate's readiness, which still waited for all five loop frames:
the 3-hours-back wait ran to 286 s and the run hit its timeout. Readiness and the status line now count
what will be shown.

Labels: G-AIRMET and SIGMET labels are gathered and placed together, the most important first
(volcanic ash and cyclones, then thunderstorms, then severe turbulence or icing, then the rest, then
G-AIRMETs), each at its area's centre if free, else a little above or below or to a side, else left
out; the outline is still drawn and a click still opens it. Over the Atlantic at zoom 2.4, 13 of
157 labels fit, none overlapping.

The world view failed once more, on a real gap: at 00:36Z nowCOAST's newest global mosaic was 23:00Z,
96 minutes old, past the 90 minutes I had allowed, so the world's clouds disappeared. Allowed now 150;
the stamp shows the image's own time.
