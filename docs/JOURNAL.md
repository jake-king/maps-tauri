# Engineering journal

A running record of building maps-tauri: what was done, what broke, why, and what the
measurements said. Newest entries at the bottom. Commits are named where they exist.

The project started on 2026-11-15 as a way to learn Tauri. It rebuilds the offline map viewer
from my homelab as a Tauri 2 desktop app. The homelab viewer is MapLibre GL over a PMTiles
basemap, with a GeoNames search box served by a small Python service. The search-box parsing
is ported to Rust and compiled to WebAssembly.

## Setup (2026-11-15)

Rust 1.99 with the `wasm32-unknown-unknown` target, WebKitGTK 4.1, Node 22 and SQLite 3.54 were
already on the machine. Missing were the Tauri CLI, `wasm-pack`, and `go-pmtiles`. The Tauri CLI
is installed as an npm dev dependency, pinned to 2.12.4. Tauri 3 is still alpha.

The JavaScript dependencies are pinned to the versions the homelab viewer already ships:
MapLibre GL 6.7.0, pmtiles 4.5.0, @protomaps/basemaps 5.7.2, maplibre-gl-geocoder 1.9.4.
I first typed `maplibre-gl@5.7.2`, mixing it up with the basemaps version, and `npm audit`
flagged a critical XSS advisory against MapLibre <= 6.4.0. Pinning the intended 6.7.0 cleared it.

All map data comes from the homelab's map service over the LAN, never the internet
(`tools/fetch-data.sh`):
- a 6.2 MB Los Angeles cutout of the planet archive, extracted with `go-pmtiles extract` in 33
  HTTP Range requests;
- the Protomaps glyphs and sprites;
- the production GeoNames index (1.6 GB, 13,464,063 places).

## M0: a window that renders (289776e)

The first goal was to settle the riskiest unknown before building anything on it: does
MapLibre's WebGL work inside WebKitGTK at all? A minimal map, with a background layer and one
point, reached `idle` over WebGL 2.0. WebKit reports the GPU as "Apple GPU" whatever the
hardware. That looks like its fingerprinting protection rather than a real reading.

To check runs without watching the screen, the frontend sends its status lines to a `report`
command that prints them on the backend's stdout. Every later milestone was verified this way.

Problems:
- MapLibre 6 has no default export. `import maplibregl from "maplibre-gl"` failed with
  "Importing binding name 'default' cannot be resolved". Fixed with `import * as maplibregl`.
- Vite's dependency pre-bundler breaks MapLibre 6. MapLibre loads its worker from a sibling file
  next to its entry module. The pre-bundler moves the entry into `.vite/deps` without that file.
  Fixed by excluding `maplibre-gl` from `optimizeDeps`.

## M2: the search-box parsing in Rust and WebAssembly (96975c8)

`crates/coords` ports three functions from the Python service:
- `parse_coords` parses decimal, DMS and DDM coordinates, with deliberate refusals for postcodes
  and half-typed place names.
- `fold` is the index's accent-stripping.
- `fts_query` turns user text into a safe FTS5 query.

Tests came over from the Python suite's tables: 19 accepted notations, 47 refusals, 6 hostile
inputs. All passed on the first run, so I planted bugs to check they could fail. Removing the
"must look like a coordinate" check and counting the length cap in bytes were both caught.
Allowing exactly 60 minutes was not: neither the Python suite nor the port tested the boundary.
I added minute and second boundary cases.

A differential test (`tools/diff.py`) runs the Rust and the Python over the same ~188,000 inputs.
The inputs are the case tables plus seeded random strings. The test fails on any disagreement it
cannot explain. It caught a planted bug that every unit test missed: a hemisphere letter on the
second number read backwards. That bug then became a unit test too. The diff also found two real
differences:
- Python accepts non-ASCII digits (`١٢`, `１２`), because its `\d` and `float()` are
  Unicode-aware. The port reads ASCII digits only. By the original's own rule, refusing is the
  safe direction: an unparsed coordinate falls through to the place search. So this is kept and
  documented.
- Python splits words at combining marks, such as Hindi vowel signs, Arabic harakat and Hebrew
  niqqud. Rather than guess which side was right, I checked SQLite. FTS5's `unicode61` tokenizer
  splits the same way, and it re-tokenizes the query phrase, so both versions find the same rows.
  The diff now compares queries by the tokens FTS5 sees.

The Python version had a ReDoS: 71 s of CPU for a 303-byte query, fixed there by rewriting the
pattern. Rust's `regex` crate runs in linear time, so that cannot occur in the port at all.

Problems:
- Python's `splitlines()` broke the diff's input. It splits on U+2028 and U+0085 as well as
  `\n`, and serde_json leaves those characters unescaped, so JSON rows were cut in half. The fix
  splits on `\n` only, the way the producer wrote them.
- The WASM module is 462 KB, mostly Unicode tables. Dropping the regex crate's `unicode-perl`
  feature saved 36 KB. It was no longer needed once the pattern used `[0-9]`.

## M1: the basemap through a Rust URI scheme (ac086cb)

The webview never touches the filesystem. A `tiles://` URI scheme in Rust serves the PMTiles
archive with HTTP Range semantics: 206 with `Content-Range` for a single range, 416 for anything
else, and only listed file names are reachable. It uses the asynchronous protocol API and reads
on the blocking thread pool, so file I/O never runs on the UI thread. The first run read the
16 KB header and six tile ranges, and MapLibre decoded 466 road and 189 place features.

Problem:
- The auto-mode permission classifier blocked internet downloads: a `go install` of go-pmtiles,
  and a fetch from build.protomaps.com. Rather than work around it, the data now comes from the
  homelab's own map service, which already serves every file with Range support.

## M3: place search as a Tauri command (eb2e8ab)

`search.rs` ports the Python service's `search()` to rusqlite with bundled SQLite, which includes
FTS5. It opens the index read-only and immutable. A parity test (`tools/search_diff.py`) runs both
over the real 13.46M-row index: 156 queries, 896 results, identical. Two planted bugs both turned
it red: the exact-match ranking term removed, and numeric region codes kept in labels.

The search box answers coordinates in the frontend through the WASM build, in under a
millisecond. It sends place names to the `search` command, which takes about 20 ms on a debug
build. Both sides build coordinate results with the same Rust function, so there is one
definition.

Tauri 2 lets the frontend call any app command unless the commands are declared. `build.rs` now
declares them, so each needs a grant in `capabilities/default.json`. Checked both ways: with
`allow-search` removed, the call is refused ("search not allowed") and the map and coordinate
search keep working.

A finding in the production data, left unchanged in the port: "santa mon" ranks Santa Monica,
Philippines above Santa Monica, California, because the Philippine entry has the higher
precomputed notability score.

Problems:
- `wasm-pack` was not on PATH for npm scripts. It lives in `~/.cargo/bin`, so the script sets
  PATH itself.
- A global commit hook refuses commits on `main`. Work happens on `feat/tauri-port`.

## Dev tools: the F4 overlay and the pan benchmark (ad9ef67)

Panning fast at high zoom showed gray boxes where tiles were still loading. Before changing
anything I built tools to measure it, modelled on the F4 profiler in my guile-2d engine.

F4 toggles a canvas overlay. Each measurement gets a row with its average, its worst value, and a
bar against one frame. The rows are:
- frame interval;
- MapLibre's render CPU time;
- tile fetch round trip through `tiles://`;
- the Rust handler's own time;
- tile request-to-decoded time.

Below the rows are memory for every process in the app's tree and a 120-frame graph with the
budget line. The backend reads the memory from `/proc`. WebKitGTK renders in separate web and
network processes, so the app's own RSS is a small part of the total. The layout and statistics
are pure functions with tests. All 7 planted bugs were caught after adding one missing case.

F6, or `MAPS_BENCH=1` at startup, runs a fixed benchmark: a lawnmower path over the cutout at
zoom 15 and 4000 px/s, run cold and then warm, reported as one JSON line. `tools/bench.sh` wraps
it and saves results to `bench/`.

First baseline (debug build): tiles missing in 34% of frames cold and still 30% warm. Rust's
handler took 0.2 ms per request and the `tiles://` round trip about 3 ms, so neither file I/O nor
IPC was the problem. Almost all of the ~35 ms from request to drawable tile came after the bytes
arrived.

Problems:
- Benchmark runs left app windows open. `timeout` signalled the `systemd-run` wrapper and the app
  outlived it. Under `MAPS_BENCH=1` the app now exits itself when the benchmark is done, and
  `timeout` runs inside the scope as the app's parent.
- The benchmark ran twice in one session. The probable cause, not confirmed, is Vite reloading
  the page because it was watching the log file being written. Vite now ignores `logs/`, `data/`,
  `target/` and `crates/`.
- Starting the benchmark failed when I had closed my own dev server. Nothing served the frontend,
  so the app showed a blank page. `bench.sh` now starts its own Vite when none is running and
  stops its whole process group afterwards.

## The tile cache (f157654)

The warm pass re-fetched and re-decoded all 161 tiles. MapLibre sizes its out-of-view cache as
(tiles across + 1) x (tiles down + 1) x `maxTileCacheZoomLevels`. That gives 60 tiles for a
1200x800 window at the default of 5, against 161 on the path. The obvious option,
`maxTileCacheSize`, cannot raise it, because MapLibre takes the minimum of the two figures.

| maxTileCacheZoomLevels | cache | warm: frames with tiles missing | warm: tiles reloaded |
|---|---|---|---|
| 5 (default) | 60 | 30% | 161 |
| 10 | 120 | 30% | 161 |
| 20 | 240 | 0% | 3 |
| 40 | 480 | 0% | 3 |

20 is now the default. Memory stayed between 860 and 970 MB in every run, within run-to-run
noise. `MAPS_CACHE_ZOOMS` overrides it for comparisons.

## The release build, and a measurement correction (435e9a2)

The release build drew no map. MapLibre 6 works out its worker script's URL from
`import.meta.url`, but only when the page is served over http(s). A release build serves the
page from `tauri://localhost`, so MapLibre got an empty URL and started no workers. The dev
build worked only because Vite serves over http. Fixed by bundling the worker through Vite's
`?worker&url` import and passing it to `maplibregl.setWorkerUrl()`. I had expected the dev
build to load MapLibre's development bundle. It does not, because the package exports one build,
so that hypothesis was wrong.

My "frames over budget" count was mostly timer rounding. WebKit rounds `performance.now()` to
whole milliseconds, so a perfect 16.7 ms frame can measure 18 ms. Counting frames over 17.7 ms
gave 69-95 of ~487 per pass, and I reported that as a separate source of jank. Counting frames
over 1.5 budgets, where a refresh was missed, gives 1-4. The frame rate was smooth all along. The
remaining problem is tiles on first visit. The benchmark now reports real drops and rounding
separately, and F4's graph marks only real drops.

| build | frames that missed a refresh (cold / warm) | frames with tiles missing (cold / warm) |
|---|---|---|
| release | 2 / 1 of ~487 | 34.6% / 0.2% |
| debug | 4 / 1 of ~487 | 33.1% / 0.2% |

## First-visit gray boxes: finding the real cause

After the cache change, the remaining problem was tiles on a first visit. Finding the cause took
longer than the fix itself. The main reason is that the first two measures were wrong in ways
that took controls to expose.

Workers and prefetch did nothing. MapLibre runs one worker by default, except on Safari, where it
uses half the cores up to three. WebKitGTK's user agent says Safari, so the app already had 3
workers. The webview reports 8 cores on a 16-thread machine. 1, 2, 3 and 6 workers gave
12.9-16.6% of frames with gray tiles by the tile measure, with 6 the worst. `prefetchZoomDelta`
at 0, 2, 4 and 6 changed nothing. Every run loaded exactly the same 161 tiles, so prefetch was not
fetching anything during a pan.

Labels cost about a quarter of decode time. With every label layer removed, request to drawable
fell from about 38 ms to 28 ms. The saving is real, but labels are not the main cost.

The measure itself had to be replaced twice, first with a better one and then with a correct one.
"Frames with tiles loading" overstated the problem, because a loading tile is often drawn over by
a parent placeholder. I wrote a tile-coverage measure, with tests. It counts needed tiles that no
drawable tile overlaps, using the public `coveringTiles()` and the tile manager's renderable set.
A delay control validated it in one direction: slowing every tile by 50 and 150 ms raised it from
16% to 31% and 61%. But it disagreed with MapLibre's code, which suggested parents should be
filling those gaps. I suspected it was sampled at the wrong moment. It ran from my
animation-frame callback, before MapLibre's tile manager caught up with the camera. I moved it
into MapLibre's `render` event. The numbers did not change, so that suspicion was wrong.

The deciding measure was the screen itself. With `MAPS_PIXEL_CHECK=1` the style's background is
pure magenta. Every rendered frame is read back with `readPixels` and the magenta pixels counted,
which gives the gray-box area exactly. It stalls the GPU, so those runs are for coverage only,
never timing. The pixel check showed gray in 30.5% of frames on a first visit, 2.6% of the screen
on average and up to 34%. The tile measure had undercounted. Every frame it flagged was gray, but
it missed 81 more. Its "a child covers its parent" rule counts partial coverage as full.

Problems on the way:
- The pixel check was blind at first, reading 0% everywhere. Its first self-test was flawed. I
  looked just outside the archive's bounds expecting background. The cutout keeps whole low-zoom
  tiles that extend past the bounds, so a blurry parent was drawn there too. The second self-test
  hides every layer but the background, so the frame must read 100%. It read 0%. A diagnostic
  showed that `readPixels` worked (204,204,204) but the background was still `#cccccc`. My edit
  that set it to magenta had never been applied. I edit files with small Python scripts. That one
  used a plain string replace whose target text had changed after an earlier edit, so it did
  nothing and said nothing. Edits now go through a helper that asserts its target appears exactly
  once.
- A backdrop layer was my first idea for a fix using only public API. It would be a second source
  capped at zoom 10, drawing land and water under everything. I dropped it on reasoning, without
  measuring. MapLibre lays out an overzoomed copy of a low-zoom tile separately for each 512 px
  patch of the view, so the backdrop would have the same first-visit delay.

## The cause, and the fix: parents from the cache

The cause is in MapLibre's `TileManager._updateRetainedTiles`. A needed tile without data walks up
its parents to find something to draw in its place. It pulls a parent out of the out-of-view
cache only if the needed tile `wasRequested()`, which is false while the tile's first load is in
flight. A tile that has just entered the view, which is every new tile during a pan, only finds
parents already in view. At zoom 15 the low-zoom parents never are. That is why a bigger cache,
more prefetch and a warm-up pass at zoom 10 or 12 all failed. The warm-up passes left 27.8% and
30.7% gray by the pixel check. The parents were in the cache, and MapLibre did not look there.

`src/maplibre-patch.js` adds a second pass after MapLibre's own. For each needed tile still
without data, it retains the first parent with data, in view or in the cache. Taking it from the
cache is MapLibre's own `_addTile`, so no new request is made. The patch overrides a private
method, so it applies only to MapLibre 6.7.0. On any other version it does nothing and reports
why. `MAPS_CACHED_PARENTS=0` turns it off for comparison.

| first visit, pixel check | frames with background showing | worst frame |
|---|---|---|
| without the patch | 29.5-30.5% | 30-34% of the screen |
| with the patch | 0% | 0% |
| with the patch, tiles slowed by 150 ms | 0% | 0% |

Timing runs show no cost: 5 dropped frames of ~490 and about 800 MB, as before. While a detailed
tile decodes, the view now shows its parent scaled up, blurry for a frame or two, instead of gray.

## Memory, and what transfers from guile-2d

The app felt heavy next to my guile-2d engine, about 800 MB after a pan. guile-2d works from a
fixed budget:
- one block of memory carved up at startup;
- frame scratch wiped every frame;
- nothing allocated per frame;
- a profiler counting bytes per block.

Its test arenas are kilobytes.

The first step was finding where the 800 MB lives. `MAPS_MEMPROBE=blank|idle` (`tools/memprobe.sh`)
records memory (PSS) for every process in the app's tree at a stage, then exits:

| stage | total | app process | WebKit network process | WebKit web process |
|---|---|---|---|---|
| blank window, MapLibre's code loaded, no map | 293 MB | 96 | 32 | 164 |
| map at rest after its first load | 599 MB | 101 | 32 | 464 |
| after the pan benchmark | ~830 MB | | | |

About 290 MB is the floor any Tauri window on WebKitGTK pays before drawing anything. No
technique from guile-2d reaches it: SDL and OpenGL are a thin layer, a browser engine is not. The
rest is the map, all inside the web process.

At rest, each MapLibre worker costs about 45-50 MB, because each is a JavaScript engine of its
own. With 1, 3 and 6 workers the totals were 494, 599 and 694 MB. Labels cost about 80 MB. On the
pan benchmark:

| workers | tile cache | memory after the pan | gray, first visit / revisit | dropped frames |
|---|---|---|---|---|
| 3 (the default here) | 240 tiles | ~830 MB | 0% / 0% | 2-4 |
| 1 | 240 tiles | ~700 MB | 0% / 0% (pixel check) | 1-2 |
| 1 | 120 tiles | ~705 MB | 0% / 2.5% | 2-4 |
| 1 | 60 tiles | ~695 MB | 7.6% / 13.8% (pixel check) | 2-3 |

One worker decodes tiles no slower than three on this workload, so the app now runs one. That
saves about 130 MB for nothing measurable. Shrinking the cache saves almost nothing and brings
gray boxes back, because the parent patch needs parents still in the cache. The cache stays at
240 tiles.

The dev tools themselves broke guile-2d's per-frame rule:
- the sampler ran every frame with F4 closed;
- each ring buffer allocated through `Array.shift()`;
- tile coverage was measured on every render while tiles loaded.

The rings are now one preallocated `Float64Array` each, written in a circle. 3 more planted bugs
were all caught. Nothing runs per frame unless the overlay is open or the benchmark is running.

Result: 500 MB at rest instead of 599, about 700 MB after a pan instead of ~830.

Problem:
- My first sweep was invalid. I looped over `"1 20"`-style pairs with `set -- $cfg`. My shell is
  zsh, which does not split an unquoted variable into words. So `MAPS_WORKERS` got `"1 20"`,
  which is unparseable and falls back to the default, and every run measured the defaults. The
  giveaway was `cache max 240` in runs meant to have a smaller cache. Rerun with explicit values.

## Bytes per block

guile-2d's profiler counts bytes per block, so F4 now does too, at three levels. They refresh once
a second while it is open and never per frame:

- The GPU ledger (`src/devtools/gpumem.js`) wraps WebGL's allocation calls (`bufferData`,
  `texImage2D`, `texStorage2D`, `renderbufferStorage`, their deletes, and `linkProgram`). It
  keeps live bytes per buffer, texture and renderbuffer. Counting at the call is the only exact
  place: MapLibre frees each buffer's CPU copy after upload, and an index buffer does not keep its
  size.
- Tile CPU arrays (`src/devtools/tilemem.js`) walks each loaded tile, in view and cached. It adds
  up the ArrayBuffers it reaches by the tile field that holds them, each buffer counted once.
- Process memory by mapping kind (backend, `/proc/<pid>/smaps`) splits memory into anonymous
  (JavaScript heaps, malloc, JIT), GPU-mapped, shared memory, and files.

12 tests cover both meters' bookkeeping, and all 8 planted bugs were caught. The live control is
that GPU and tile bytes must follow the cache size, and they do. After the pan with a 240-tile
cache, there were 26 MB of GPU buffers and 13 MB of tile arrays (167 tiles). With a 60-tile
cache, there were 9 MB and 4.8 MB (68 tiles).

What the blocks show:

| | web process, anonymous | map data the meters see |
|---|---|---|
| blank window | 58 MB | none |
| map with an empty style (map, WebGL context, one worker) | 104 MB | none |
| full map at rest | 226-231 MB | ~3 MB GPU, ~1 MB tile arrays, 12 shader programs |
| right after the pan | 437 MB | ~30 MB GPU, ~13 MB tile arrays |

So at rest about 120 MB comes from the style itself: 71 layers, fonts, sprites, label layout. It
does not come from tile data, and not from shaders (12 programs). During a pan the web process
grows by about 210 MB, of which the map holds about 45 MB.

The rest is a high-water mark, not a leak. Memory did not come down 20 s after the pan
(710 -> 702 MB). Earlier sweeps showed the total barely depends on cache size, so it is not data
being held either. A soak run settled it. It ran ten passes in a row with a cache too small to
keep the path, so every pass decoded all ~160 tiles again, about 1,600 decodes in all. Memory went
from 507 MB at rest to 685 MB after the first pass. It then stayed between 670 and 695 MB for the
other nine, with no upward trend. Decoding a burst of tiles takes about 180 MB of working memory
that the JavaScript engine and malloc keep for reuse. For a tool left open all day, that it levels
off matters more than its size.

Two things would lower it. One is less decoding work per tile, such as fewer style layers; labels
alone are about 80 MB at rest. The other is a renderer that does not decode in JavaScript at all.
Neither is a setting.

## A renderer that does not decode in JavaScript: R0, reading tiles in Rust

The memory work showed that most of what a pan costs is MapLibre decoding tiles in JavaScript. The
next step is a second renderer, side by side with MapLibre. Rust reads and decodes the tiles and
turns them into ready-to-draw triangle buffers. A small WebGL2 renderer in the webview only
uploads and draws. JavaScript does no decoding. The webview stays, so the frontend remains web
code, which is DX's argument for the iPad. Decoding moves to Rust, where allocation is ours to
control. The first version draws fills and lines only. Labels (shaping, placement, collision) are
a large share of what MapLibre does and come later, if at all.

`crates/vtile` holds a PMTiles v3 reader and a Mapbox Vector Tile decoder, both hand-rolled,
because the formats are a few pages of spec each. `flate2` does the gzip. Spec examples are unit
tests: tile ids, varints, directory offsets, the MVT geometry examples, value types.

The main check is a differential test (`tools/mvt_diff.mjs`). Every tile in the archive is read
and decoded by two independent pipelines. One is Rust. The other is pmtiles.js and
`@mapbox/vector-tile`, the decoder MapLibre 6.7.0 itself uses. They are compared layer by layer:
features, geometry types, rings, vertices, a coordinate checksum, extent and `kind` values. All
142 tiles and 926 layers are identical. A planted bug, coordinate deltas read without their sign,
turns 833 of them red.

Problems:
- I assumed `@mapbox/vector-tile` leaves polygon rings open. The first diff agreed on everything
  except vertex counts, always a few more on the JavaScript side. Its source closes a ring by
  repeating the first point on ClosePath. The Rust decoder now does the same. Outlines need the
  closing segment anyway.
- `pbf` 5 has no default export (`import { PbfReader }`). I had also first pinned 4.0.1, a
  different major from the 5.1.2 `@mapbox/vector-tile` itself uses.

## R1: meshes in Rust

`crates/vtile/src/mesh.rs` turns a decoded tile into two GPU-ready meshes:
- Fills. Rings are grouped into polygons with holes by winding (MapLibre's classifyRings), then
  triangulated with `earcutr`, a port of the earcut MapLibre uses.
- Lines. Each point becomes two vertices, pushed apart along the miter. The miter is capped at 2
  half widths so sharp corners do not spike. The width is applied in the shader, so a mesh never
  needs rebuilding when the zoom changes.

Colors and line classes live in the vertices, indexed, so a tile draws in two calls, one for fills
and one for lines. Scratch buffers are reused from tile to tile.

The palette is generated (`tools/gen_palette.mjs`) from the same Protomaps light style MapLibre
draws. It has 18 fill colors, which 32 landuse kinds share, and 11 line classes with their real
zoom width stops. Two details of that style matter. Landcover fades to transparent by zoom 7, so
it is skipped. Buildings are 50% transparent. v1 simplifies roads: no casings, no tunnel or bridge
styling.

The unit tests cover:
- ring classification;
- a square with a hole triangulating to its exact area;
- miters on a straight line, a right angle and a near-reversal;
- the wire format byte by byte;
- index widening past 65,535 vertices.

7 planted bugs were all caught. On the whole archive (`examples/meshcheck.rs`), 25,300 of 25,302
polygon features triangulate to exactly their area. Two are off by up to 0.5%, which earcut does
not promise for self-touching polygons. Decode + mesh takes 1.7 ms a tile.

The first wire format was too big: 415 KB per zoom-14 tile, against about 160 KB of MapLibre GPU
buffers per tile measured by the GPU ledger. 32-bit indices, full colors in every vertex and 16-bit
extrusion vectors were the waste. Version 2 uses 16-bit indices, with 32-bit only past 65,535
vertices, which no tile here reaches. It uses palette indices instead of colors, and `i8`
extrusion. The result is 6-byte fill and 8-byte line vertices, and 235 KB per zoom-14 tile.

## R2: the WebGL renderer, and R3: measured against MapLibre

The backend serves `tiles://localhost/mesh/{z}/{x}/{y}`: the tile decoded and meshed in Rust on
request, in about 1.7 ms. Each blocking-pool thread reuses one mesher and one output buffer.
`src/main.js` now picks a renderer, and only the chosen one is imported, so a `MAPS_RENDERER=rust`
run never loads MapLibre's code. `archive_info` reads the archive's bounds in Rust, so the native
app loads no JavaScript tile library either.

`src/rust-renderer/renderer.js` is about 450 lines. It fetches meshes, uploads them as they
arrive, and draws two calls a tile. The only parsing is a 28-byte header. Each needed tile is
drawn clipped to its own rectangle (WebGL scissor). A tile still loading is replaced by its
nearest loaded ancestor, clipped the same way. Each needed tile's ancestor three zooms up is
requested as a backstop. That is the behaviour MapLibre needed a patch for, built in, with no
draw-order problems between tiles. The renderer also implements the part of MapLibre's API the dev
tools use, so F4, F6, the memory probes and the pixel check measure both renderers the same way.
The pure parts have 16 tests: Mercator math, tile coverage, MapLibre's exponential interpolation,
and the mesh header. Among them is a test that the camera's center tile is the archive's own tile
under Santa Monica.

To check the output without a screen, `MAPS_SNAPSHOT=<name>` settles at a fixed view, reads the
frame and saves a PNG through a `save_snapshot` command. At the Santa Monica Pier at zoom 15 the
native renderer draws the same coastline, beach, water, parks, buildings and roads as MapLibre,
almost line for line. Missing, as planned: labels and POI icons, road casings, dashed rail. The pier
comes out as an outline rather than a filled shape, a classification gap still to fix.

A bug in my own tools invalidated the first comparison. The first native runs showed 0% of frames
with tiles loading, which cannot be right with 150 ms tile delays. Only 201 of 504 frames
rendered. I recorded each tile's state the first time a render saw it. 44 were already loaded,
though they had been requested only as needed. The frames where they were missing never reached
the render listeners. The dev tools' render listener calls `measureCoverage(map)`, which called
`map.getSource()`, a MapLibre method the native renderer does not have. It threw, the exception
escaped `_render`, and the next frame was never scheduled, exactly while tiles were loading.
Those frames vanished from every measure, and the pan stalled until a tile arrived. The fixes:
- `measureCoverage` returns null for a map without MapLibre's internals.
- The renderer catches and counts a listener's exception and keeps rendering.
- The benchmark now reports the share of frames rendered and listener errors, and warns when
  frames were not rendered.

Controls after the fix:
- With the ancestor fallback turned off (`MAPS_NO_FALLBACK=1`), background shows in 8.4% of
  first-visit frames by the pixel check. The renderer's own count agrees: 41 frames both, 1
  flagged by the count only, none by pixels only.
- With the fallback, 0%. It is still 0% with every tile slowed by 150 ms, when 62.7% of frames
  draw an ancestor.
- Pixel self-test 100%.

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
separate overzoomed tiles. The native renderer scales the zoom-14 mesh. At rest the web process is
at the blank-window floor. Everything the map costs now lives in the app process (Rust meshing
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

It was not queueing. It was not concurrency either: 1.4 requests at once on average, 3 at most.
It was not the extra copy. Every stage was uniformly 5-8 times slower. `examples/stages.rs`
reproduces it offline:

| offline run (z14 tiles) | per tile |
|---|---|
| back to back | 1.74 ms |
| 16 ms sleep between tiles (the app's rhythm) | 7.75 ms |
| the same, pinned to a performance core | 7.53 ms |
| pinned to an efficiency core, back to back | 2.55 ms |
| a fresh thread per tile, back to back | 5.67 ms |
| 16 ms gap spent busy-waiting instead of sleeping | 1.76 ms |

The busy-wait run separates the two explanations. Spinning keeps the core awake and clocked
without touching the caches, and the tile is fast again. So the cause is the CPU going idle
between requests, not cold caches. The i5-12600KF under the `powersave` governor swung between
1.2 and 4.2 GHz while gapping. The hybrid chip's efficiency cores and thread hopping are smaller
factors.

This is how a real machine behaves, a laptop with power saving more so. A benchmark that runs
tiles back to back would never show it. Keeping the CPU awake would waste power. The fix is to
take meshing off the critical path with a mesh cache and background prefetch. Then a tile is ready
before it is visible, however slowly a resting CPU builds it.

Problem:
- zsh again: `B="cmd args"; $B` does not split into words in zsh. A shell function does.

## Prefetch, and a cache that turned out not to be needed

Profiling showed the per-request time is mostly a resting CPU building the mesh. There are two
ways to take it off the critical path. Each was measured by a new figure: for every tile, how long
it was on screen before it could be drawn (`visible_wait`). The figure is 0 when the tile was
ready in time.

The first is a prefetch ring. The renderer also requests the ring of tiles one beyond the view, at
a lower priority than anything visible (`ringTiles` in geo.js, tested; `MAPS_PREFETCH_RING`). At
zoom 15 a zoom-14 tile is 1,024 px wide. At 4,000 px/s the ring gives about 256 ms of lead,
against a fetch of about 16 ms.

| pan benchmark, first visit | no ring | ring of 1 |
|---|---|---|
| tiles not ready when they came into view | 40 of 40 | **0 of 40** |
| wait while visible, avg / p95 | 15.3 / 27 ms | **0 / 0 ms** |
| frames drawing a fallback ancestor | 9.3% | **0%** |
| tiles fetched | 43 | 77 |
| memory | ~354 MB | ~364 MB (Rust cache off) |

The second is a Rust mesh cache (`MeshCache`), an LRU by bytes, set by `MAPS_MESH_CACHE_MB` and
tested. In the benchmark it had zero hits. The renderer's GPU-side cache holds every tile, so the
Rust cache only duplicated it, at about 20 MB. Shrinking the GPU cache to 12 tiles forces
re-fetches. There the Rust cache halved request time (6.7 against 15.9 ms) but changed nothing
visible, because the ring kept every tile ahead of the view. A stress run at 16,000 px/s
(`MAPS_BENCH_SPEED`) gave the same result: every tile ready, cache or not. In that run the pan
took 2.1 s instead of 8, and every frame rendered. So the cache is off by default and kept for a
case that needs it.

The question was whether a tile's request-to-drawable time could go under 1 ms. Per request, no.
The webview's IPC alone is several milliseconds, and a resting CPU makes the mesh slower still.
But the time a user waits for a tile is now 0 ms in every run, because the request happens before
the tile is visible.

## Zooming: the benchmark that was missing

Using the native renderer by hand showed what no benchmark had. Zooming out left large gray areas,
and zooming in seemed to drop parts of the map as it redrew. Every benchmark so far had panned at
a fixed zoom, so nothing measured zooming. That was a gap in the tests, not just in the renderer.

`MAPS_ZOOM_BENCH=1` (`tools/zoombench.sh`) zooms out 15 -> 10 and in 10 -> 16 at the pier, 1.2 s
each, with the pixel check, and saves the worst frame as a PNG. First run, small cutout:

| | native | MapLibre |
|---|---|---|
| zoom out: frames with background / worst frame | 17 / 14% | 17 / 28% |
| zoom in: frames with background / worst frame | 13 / 13% | 5 / 11% |
| background at rest at zoom 10 | 14.7% | 0% |

The saved worst frames showed magenta bands at the sides, where the cutout's tiles end. The
cutout is about 0.2 by 0.15 degrees, so two scroll ticks out the window is mostly outside it.
Past zoom 10 the archive has no tiles there, and the native renderer drew nothing for an empty
tile.

Fixes, each measured:
1. An empty tile falls back to a coarser ancestor. The archive keeps whole low-zoom tiles, so one
   almost always has data. The whole ancestor chain of an empty tile is requested at once, rather
   than one round trip per level.
2. `areTilesLoaded()` was wrong for empty tiles. It counted an empty tile as done by itself, so
   the benchmark measured "at rest" before the coarse stand-in had arrived. An empty tile now
   counts as done only once an ancestor with data is loaded, or when none exists.
3. Prefetch along the zoom. While the zoom moves, the view's tiles one tile-zoom ahead in that
   direction are requested at low priority.
4. Child tiles as stand-ins when zooming out, if all four are loaded. This did not matter. The
   tiles missing when zooming out are at the edges, outside anything loaded before.
5. A bigger area to use the app in: `data/westla.pmtiles`, Malibu to downtown and the mountains
   to LAX (25 MB, 772 tiles), extracted over the LAN. The app opens it by default. The benchmark
   scripts pin the small cutout so results stay comparable (`MAPS_ARCHIVE`).

After the fixes, on the small cutout (native), zooming out gave 1 frame with 0.7% background and 0
frames drawing a stand-in for a loading tile. Zooming in gave none. With prefetch off there were
10 stand-in frames zooming out, and 21 tiles were never requested before they were visible. On
the West LA archive there was no background and no stand-in in any frame, either way. MapLibre on
the same zoom showed up to 30% of a frame as background.

Problems on the way:
- My stand-in count measured the wrong thing twice. After the empty-tile fix, an empty tile drawn
  by its ancestor was still labelled "ancestor", the same as a tile still loading. For an empty
  tile that is its final look; nothing better will load. So the count measured the cutout's edges.
  It showed 14-17 stand-in frames with prefetch on or off. Recording why each tile was not ready
  showed that none were, and that contradiction exposed the problem. Earlier, on that bad count, I
  concluded the zoom prefetch "did not help" and changed its coverage on a theory. That change was
  harmless but unmotivated.
- A multi-edit script half-applied. The asserting edit helper refused an edit whose target text
  had changed, as intended. But the edits before it in the same script were already written, so
  one build had a subset of the changes. Measurements from that build are not used.
## Labels, and a zoom-out limit

The zoom-out limit is now the zoom at which the world exactly fills the window (`worldFitZoom`). The
center is clamped so the view never passes the world's edge (`clampCamera`, tested). Fully zoomed
out, the snapshot shows the whole world edge to edge, with the coarse world tiles filling in through
the empty-tile fallback. One bug came up along the way: a snapshot at `MAPS_SNAPSHOT_ZOOM=0`
silently used the default view, because 0 is falsy.

MapLibre draws label text with signed-distance-field glyphs in WebGL. That means a glyph atlas,
shaping, and placement along curves, which is days of work. v1 splits the work differently:
- Rust chooses the labels while meshing, and mesh format v3 adds a label block. Place names go at
  their points, with their kind, the data's `min_zoom`, and its `sort_key` as priority. For each
  road name, Rust takes the longest straight segment in the tile, with its midpoint and its angle,
  turned to read left to right. Only anchors inside the tile's own extent are kept, so neighbouring
  tiles do not send the same label. That comes to about 78 labels and 2.4 KB per zoom-14 tile.
- The frontend (`labels.js`) decides what shows. It filters by zoom, shows a street name only where
  its straight run is longer than the text, and shows only whole labels on screen. It then runs
  greedy collision over a grid in priority order: kind first, then labels placed last frame so they
  do not flicker, then the data's priority. It draws on a 2D canvas over the map with the browser's
  text rendering, which gives full Unicode shaping. Colors and halos come from the Protomaps flavor
  through the palette generator. Only tiles drawn as themselves contribute labels, so a label never
  jumps when a stand-in is replaced.

Tests:
- the label block on the wire in Rust, including a UTF-8 cut on a character boundary, and its round
  trip in JavaScript
- collision: rank order, touching edges, overlaps across grid cells and in lower grid rows, rotated
  boxes

The tests caught 5 planted bugs in the collision code. One was caught only after its test was fixed:
my first "lower row" test overlapped in the candidate's first row, so it could not tell.

Snapshots at zoom 15 and 12 show street names along their roads, district names in capitals, and
city names, with no overlaps. On the pan benchmark, render CPU is 1.6 ms a frame (p95 3) against 0.5
without labels. Labels account for 0.9 ms of that, for about 19 labels. The worst frame is 10 ms, on
the first pass, before the text-width cache fills. Memory rises by about 20 MB, for the label
canvas. MapLibre with its labels takes 4.7 ms. On the zoom benchmark, 1 frame shows 0.7% background
when zooming out, on both archives. It had been 0 on West LA before labels: at noise level, but not
identical.

Not in v1: road labels along curves, highway shields, POI icons and names, water labels, and the
search box in native mode.

## Polish: search, the pier, points of interest, shields, water names

The native renderer now has its own search box (`search.js`), because MapLibre's geocoder widget
depends on MapLibre. It is debounced, shows 8 results, and supports keyboard navigation. Coordinates
are answered in the frontend by the WASM build of `crates/coords`; names go to the Rust `search`
command. A chosen result flies the map there and drops a marker. `MAPS_SEARCH=<query>` runs it end
to end and snapshots the result. The box and marker are painted in from their DOM state, because
readPixels cannot see HTML. A typed coordinate lands exactly under the marker. "santa monica pier"
lands about a kilometre inland, because GeoNames itself records the pier at exactly 34.0, -118.5.
That is the data, and the MapLibre app uses the same index.

The pier was drawn as an outline. The mesher drew all landuse before water, while the Protomaps
style draws the pier layer after water, along with pedestrian areas and dams. The ocean polygon
painted over it. The palette generator now records which landuse kinds the style puts above water,
and the mesher draws those after the water polygons.

Mesh format v4 adds points of interest, shields and water names: each label also carries an icon
index and a color index. All rules come from the style through the generator:
- POIs: 36 kinds, with each icon named after the kind. `station` uses `train_station`. `townhall`
  has no sprite in this sheet, so it is text only, as in MapLibre. Text colors go by kind group.
- Shields come from a road's network and the length of its route number (US:I, NL:S-road, else the
  generic shield).
- Water names are in blue italic, along streams and rivers from zoom 13, and at the point or polygon
  center for seas and lakes.

The frontend loads the same sprite sheet MapLibre uses and places labels in MapLibre's order:
places, POIs, major roads and shields, water, minor streets. A POI's name sits right of its icon, or
left if right collides. `placeLabels` now takes alternative positions (tested).

The snapshot at the pier at zoom 15 now carries what MapLibre's does, apart from styling details:
POIs with icons, route shields for 1, 10 and 66, Kenter Creek in blue italic, street and district
names, nothing overlapping.

One problem came up. The palette generator built the style without a language, and Protomaps adds
label layers only when given one. The POI, water and shield label layers were therefore missing from
its lookup. The generator crashed rather than producing an empty table, which is the better way to
fail.

## Polish: the label hitch, casings, dashed rail

I found and fixed a 16 ms label frame. I assumed the cause was the first sprite draw or font
loading. I recorded each label frame's time, its text-width cache misses, and whether it was the
first sprite frame. The slow frames (7-11 ms) were the ones measuring new text (4-27 misses), never
the first sprite frame. Their cost did not scale with the number of misses. That points at setting
`ctx.font` rather than measuring, since the browser parses and resolves it. Text is now measured
when a tile arrives (`LabelLayer.prepare`), off the frame, and the font is set only when it changes.
The worst label frame went from 8-11 ms to 3-4 ms, with no measurement in any frame.

For road casings, the generator takes the style's five "late" casing layers (`#e0e0e0`). A casing's
total width = the road's gap width + twice the casing's own width, sampled every quarter zoom with
MapLibre's interpolation. The generator also takes the zoom where each border first becomes visible,
so low-zoom tiles carry no invisible geometry. Five casing classes come before every road fill,
giving 16 line classes, exactly what the shader's uniform arrays hold. Cost: zoom-14 tiles grew from
235 to 274 KB (+17%).

Railways are now dashed. Rust cuts them into dashes at the tile's zoom (+0.5), with the style's
pattern (0.3 on, 0.75 off, in line widths). The trade-off is that on an overzoomed tile the dashes
stretch with the zoom. A dash distance attribute on every line vertex would fix that, at about 4
bytes a vertex.

Tests cover the dash pattern (straight, across corners, degenerate input) and casings ordered before
fills. Of four planted bugs, one ("the pattern never repeats") made the dash loop run forever. My
mutation script hung with the bug still in the source file. `pkill -f` on the test binary's name
then killed the shell running it, because the pattern appeared in its own command line. The file was
restored from the backup. Mutation runs now carry a time limit, and a timeout counts as a catch.

## Curved street names, and two bugs they exposed

Street and stream names now follow their roads. Mesh format v5 sends the name's path: the longest
piece of the line inside the tile, simplified (Douglas-Peucker, at most 32 points) and oriented to
read left to right. The frontend lays the text out character by character along it (`layoutOnPath`,
pure and tested):
- Each character sits at its own distance along the path, turned to the direction there.
- A placement is refused where neighbouring characters would turn more than 45 degrees (MapLibre's
  rule) or stand upside down.
- A placement is tried at the middle of the path and then at 35, 65, 20 and 80% of its length, since
  the middle of a road may be a hairpin.

A curved name claims one small box per character in collision. A nearly straight one is still drawn
as one string, keeping its kerning. Snapshots in Pacific Palisades show West Sunset Boulevard, Las
Lomas Avenue and Monument Street following their bends. Two gaps remain:
- Temescal Canyon Road is too tight for five anchors, where MapLibre searches more.
- Per-character drawing loses kerning on curves ("Whitfield" looks crowded).

Landuse is now drawn in the style's layer order rather than the tile's feature order. Before, a
school under an overlapping park came out green.

Bug 1 was an infinite loop in Rust. A path with more than 32 points was re-simplified with a
tolerance derived from its point count. A wiggly canyon road could leave the count unchanged, so the
same tolerance came round again forever, in a tile-serving thread. The native app hung only where
such a road was in view. Meshing all 772 West LA tiles offline reproduced it. A 40-point zigzag test
hung before the fix (exit 124 under a time limit) and passes after it. The fix doubles the tolerance
each pass, starting from the original line. The whole-archive mesh check should run on every mesher
change. The small cutout had no canyon roads, which is why the earlier check passed.

Bug 2 was a startup race I had met before. Native runs hung at startup about one time in five, after
exactly 21 tile requests, with an empty log. These steps narrowed it:
- Uncaught errors are now reported to stdout, since the webview console is invisible to a terminal
  run. None appeared.
- Process CPU averages said the page was waiting, not spinning.
- A watchdog for withheld animation frames stayed silent.
- A heartbeat of the renderer's state showed the map idle at frame 5: `idle` fired, and everything
  loaded.

The startup handler had not been listening. The search box's `await initCoords()` sat between
creating the map and registering for its first `idle`. When tiles came back fast, `idle` fired
during the await. This is the trap recorded from the homelab map viewer ("an inserted await
re-orders what follows it; MapLibre never replays load"), reintroduced here. It is fixed twice over:
- All startup awaits happen before the map exists.
- `RustMap.whenIdle()` resolves at once when the map is already idle, so a late listener cannot miss
  it.

After the fix there were 0 failures in 25 runs (about 1 in 5 before).

## The whole world: reading the planet over the LAN

Street detail existed only inside the extracts. The homelab's map service has the full planet
(PMTiles v3, gzip, zooms 0-15, 1.43 billion addressed tiles, 128 GB) with Range support. The Rust
reader now reads it in place with `Archive::open_url`. This uses vtile feature `http`, with `ureq`
and rustls, since HTTPS is not a primitive worth hand-rolling. The reader issues one Range request
per directory or tile over a reused connection, and refuses anything but a 206 of the exact length.
It caches leaf directories, because a planet's directory is split into many and each costs a round
trip.

Three things were checked before building:
- the format and zoom range (`go-pmtiles show` on the URL)
- the certificate (Let's Encrypt, trusted by the standard roots)
- latency: 34 ms median per request with a new TLS connection each time, 12 ms over a reused one

After building, every one of the 772 tiles of the West LA extract, read again from the planet, is
byte-identical (the extract was cut from it). A remote read takes 9.7 ms median, 12.8 p90, 40 worst.
The 772 tiles took 779 round trips, so the leaf cache held.

The app reads the planet when `MAPS_ARCHIVE_URL` or the gitignored `data/remote.url` names it, which
keeps the LAN hostname out of the repository. If the planet cannot be reached, the app falls back to
the local extract, and it reports which it uses. A world-sized archive starts the camera at Santa
Monica. The benchmark scripts set `MAPS_LOCAL_ONLY=1`, so their numbers stay comparable. Snapshots
of Paris at zoom 15 and Tokyo at zoom 14 show full street detail, curved names, POIs, and Japanese
text shaped by the browser. The MapLibre app still reads the local file.

The search index is worldwide (13.46M places); with the planet, the map now is too.

## Optimization pass: one gate, less code, and rotation

`tools/check.sh` (`npm run check`) is now a single gate that runs every check the project has, in
one place:
- rustfmt
- clippy with warnings as errors
- ESLint (recommended rules plus unused variables, strict equality, prefer-const)
- ruff check and format
- shellcheck
- a check that the generated palette matches its generator
- the Rust and JS unit tests
- the decoder diff against @mapbox/vector-tile
- a time-limited mesh of every West LA tile (the canyon-road loop would now fail it)
- search parity with the production server

Each check was shown to fail on a planted violation before its pass was trusted. Turning it on found
142 formatting diffs, 16 clippy findings and 7 ruff findings, all fixed.

Dead code removed:
- unused generated colours
- the MapLibre prefetch, fade and warm-up switches (experiments that changed nothing)
- a first-sprite diagnostic
- exports used only inside their module
- the MapLibre app's startup search self-check (search parity covers it)

The two apps had each wired the dev tools (benchmark, memory probe, snapshot, coverage, overlay)
their own way. That now lives once in `src/devtools/harness.js`, and every mode was re-run in both
apps.

The native renderer had no rotation. MapLibre has a bearing, so the stress benchmark could not
compare them. The camera now carries a bearing. Everything that asked "which tiles are on screen"
takes the rotated view as a quadrilateral, using a separating-axis test against each tile's square.
The shaders rotate about the screen centre. Per-tile clipping was a scissor rectangle, which cannot
be rotated, so it is now the stencil buffer: each tile writes its square, then draws only where the
stencil matches. Right-drag rotates; `easeTo` takes the shortest way round.

Label positions go through the same projection as the geometry. A curved name whose path now runs
right to left is read the other way. A straight street name turns with the map and is then flipped
to read left to right. The first 90 degree snapshot showed place and area names standing on end.
MapLibre keeps point labels level with the screen, so now only labels with a run turn. The rule is a
pure function with a test, which fails when the rule is removed.

To check that rotation did not change the unrotated map, I built the pre-rotation commit in a second
worktree and took the same snapshot from both. With labels off, 0 of 960,000 pixels differ, so
stencil clipping covers exactly what the scissor did. With labels on, 1.88% differed, which looked
like a label bug. A diff image coloured by version showed every POI name centred in the old build
and beside its icon in the new one. The old build had no icons at all: `public/assets` (the sprites)
is gitignored, so a fresh worktree lacks it. With it linked in, 0 pixels differ. The lesson for
comparisons across checkouts is to list the ignored inputs (`git status --ignored`) before trusting
a difference.

At 30 degrees the native map matches MapLibre's geometry. On the pan benchmark, render time per
frame is 2.4 ms cold and 2.9 warm, against 3.2 and 3.0 before, so the stencil costs nothing
measurable. The zoom benchmark shows one or two frames with up to 3% background when zooming out
from 15 to 10. The pre-rotation build does the same, so the problem is older than this work. It is
left for the stress benchmark.

## Test coverage, and seven bugs it found

Coverage had never been measured. Rust has no coverage tool installed here, but the nightly
toolchain ships `llvm-cov` and `llvm-profdata`. `tools/coverage.sh` builds the tests with
`-C instrument-coverage` into a separate target directory. It reports per file, and lists the
uncovered lines of any file named. Rust unit tests covered 49% of lines. The gate's decoder diff,
whole-archive mesh and search parity cover more, outside that figure. By file, the PMTiles reader
was at 40%, the MVT decoder at 46%, and native search at 0%.

Two test-writer agents took the reader and decoder, and search and the coordinate parser. They
edited only test code, and each test group was proven by planting a defect and watching it fail. The
HTTP source is tested end to end against a local listener with canned responses: a 200, a 206 one
byte short or long, and a body cut off by a closed connection. Search runs against a 12-row
in-memory database with the production schema. Its FTS5 operator characters are chosen so a
swallowed SQL error cannot pass as an empty result. There are now 172 Rust tests (was 37), and those
four files are at 99-100%. Of 72 planted defects, 69 were caught. The 3 survivors are equivalent
(one of three read-only guards removed, the other two still hold) or unreachable code.

Seven tests failed on the code as it was, and I fixed each:

- A corrupt PMTiles directory whose tile-id deltas or chained offsets sum past 2^64 panicked in a
  debug build and wrapped silently in release. Now an error.
- An MVT bytes field declaring a huge length overflowed the bounds arithmetic instead of reporting
  truncation.
- Geometry deltas past the i32 range overflowed. They now wrap: a wrong point from corrupt input,
  never a panic.
- `search::open` put the database path into a `file:` URI unescaped, so a directory named with `#`,
  `?` or `%` could not be opened. No real path has one today.

## The stress benchmark

`tools/stress.sh` (src/devtools/stress.js) plays a seeded script of 150 camera operations in about
52 seconds:
- pans, zooms, turns and all three at once, most interrupted by the next before they end
- bursts of wheel notches, drags and right-drags, sent as real DOM events so the input handlers run
- teleports to views nothing has loaded

The script is a pure function of the seed, so both renderers get exactly the same input. It is
unit-tested: deterministic, every kind present, every value inside the region. Each renderer runs
twice, first for timing, then with the pixel check, which reads every frame back and would distort
the timing. The region is the archive inset by a quarter. It has a lowest zoom at which the turned
viewport stays inside the data, so the empty space beyond a cutout's edge is never counted as a
missing tile. The first version rounded that zoom to the nearest hundredth, which can round down; a
test caught it.

The harness was wrong twice before its numbers meant anything:

- MapLibre threw on every frame after the third operation. I made each operation catch its own
  error, then recorded the first operation after which the camera stopped being finite. It was a pan
  that passed `bearing: undefined`. MapLibre tests `'bearing' in options`, so an explicit undefined
  becomes a NaN bearing. Now only the keys an operation sets are passed. WebKit's error `stack`
  carries no message, so the stdout reporter now prints both.
- MapLibre ignored most synthetic drags, so it was getting an easier test. Mouse moves sent to
  `window` never reach a listener on `document`, which is where MapLibre listens. Sent to the
  document, they bubble to both. The run now counts input events that did not move the map. The
  count is zero for the native renderer and one drag of 15 for MapLibre.

As a control, with stand-ins switched off, the pixel check sees background in 15 frames of a short
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

Native over the LAN, reading the planet, shows background in 44 frames (1.8%), render p99 8 ms and a
peak of 700 MB. The worst frame is all background, after a teleport to a view with no coarser tile
in memory to stand in. The obvious fix is to keep the low zooms around the current region resident.
That is not done in this pass.

## Native memory at rest: where 296 MB became 375

Idle memory had drifted from 296 MB (the first native renderer) to 375. I built eight commits in a
scratch worktree and probed each twice on the reference cutout. The first native commit measures 322
MB today, so 26 MB of the drift is the platform, not this code: the blank-window floor moved. The
other 53 MB came in three steps:
- zoom prefetch: +8 MB (app process)
- the native search box: +22 MB (web process)
- curved street names: +18 MB (both processes, about half each)

Labels themselves cost nothing measurable at rest (374 MB with them off).

Of the search box's 22 MB, 17 MB is the coordinate parser's WASM module, compiled at startup.
Skipping the load is the experiment that showed it. The module now loads on the first search
keystroke. Startup awaits nothing before the map exists, so the earlier idle race cannot come back.
At rest: 355 MB. Coordinate and place searches were checked end to end afterwards. The curved-name
18 MB is not yet broken down.

## Lessons from dugout: a policy, a retry, errors before results

Dugout (the baseball site) is local-first in a browser, and three of its bugs apply here.

Its bundled database engine fetched a component from the internet on first use. The test meant to
catch that passed, because it checked content the page already had before the action ran. This app
had `"csp": null`, so nothing stopped the page reaching out. It now has a content security policy:
the page may load its own assets and talk to the Rust backend and `tiles://`, nothing else. The
remote planet is read by Rust, outside the page's reach. Every refusal is printed.
`tools/offline.sh` runs a stress run on each renderer and both kinds of search on the release build,
and fails on any refusal.

The control run makes one request the policy must refuse. Its first run failed, and that was the
check working. The refusal lines start with `[frontend] `, so my pattern counted nothing, and the
four green runs before it were vacuous. With the pattern fixed, the policy found a fault. The search
box's input and list were built from markup with `style=""` attributes, which the policy refuses, so
they rendered unstyled. They are now built with script-set styles. The search snapshot could not
have shown this, because it paints its own copy of the box. The search run now reports the input's
computed style, and the gate checks it.

Dugout's lazy database load guarded on the finished connection, so a call during the load started a
second one. The coordinate parser's lazy load already shared one promise, but kept a failed load
forever. `once()` (src/rust-renderer/once.js) now drops a failed load so the next search retries. It
is tested, with both planted defects caught.

Errors come before results. `offline.sh` fails on any reported uncaught error, unhandled rejection,
failed search or map error before it looks at a result. `tools/stress.sh` now exits non-zero on
camera errors, listener or uncaught errors, policy refusals, a non-finite view or a failure to
settle. Two planted defects (the old markup box; a search that throws) each failed the gate.

## A disk cache for the planet: seen once, drawn offline

Dugout keeps what it has fetched in the browser, in a service worker cache and then a database in
the browser's storage, and had to fight the browser for both. Here the same idea is Rust and a file.
The remote planet's byte ranges go into a SQLite file (`src-tauri/src/diskcache.rs`). vtile's
`RangeCache` trait keeps the crate free of SQLite. The header is always read from the network. Its
ETag names the version, and a new version drops the old one's ranges. Every later request carries
If-Range, so a planet replaced mid-session is refused rather than mixed. Past a size cap (2 GB by
default) the least recently used ranges go, never the header and root directory. With the server
unreachable, the app opens the planet from the cache alone and draws what it has seen.

`tools/cache.sh` proves it against a local Range server (`tools/rangeserve.py`) that the test can
stop and restart:
- 239 requests cold, 1 warm (the header)
- 0% background offline over the same seeded 60-operation run
- the old version dropped under a new ETag
- a cache file of noise costing only the cache

A control with the cache off fails all of it. The control also showed that offline with no cache the
app falls back to the local West LA extract, which also draws 0% background. The pixel number alone
therefore proves nothing; the log line naming the cached copy does.

A reviewer found five things, which I fixed before committing:
- A read-only or locked cache dropped hits it had already read, and so could not open offline at
  all.
- A cache file that would not open turned off the remote planet entirely.
- A server that ignores If-Range could have put bytes of a new build into the cache under the old
  version. This is now refused by comparing the response's ETag.
- A weak ETag in If-Range would make a compliant server answer every request with the whole file.
- Eviction's byte count could drift if a batch failed.

Each has a test that a planted defect fails.

Over the LAN, 150 operations against the real planet show background in 7.8% of pixel-run frames
with the cache off. With a cache filled by the timing run just before, the figure is 0.2%, and fully
warm (52 MB) it is 0.4%. The warm worst frame is still all background after a teleport. Reading and
meshing the target tiles takes longer than a frame when no coarser tile is on the GPU. The cache
removes the network from that, not the gap. Keeping low zooms resident is still the fix.

## Preparing the repository to publish

A scan of every file in every commit found two things that could not go public:
- the home map service's hostname, as the default in 33 versions of `tools/fetch-data.sh`
- absolute paths under the home directory in two benchmark results

The script now takes the service from `MAPS_SRC` and refuses to run without it; the paths are
relative. Comments that pointed into the private repository the Python service lives in now say what
it is instead. The history itself is rewritten in a separate clone for publishing, so this working
repository keeps its own.

The scan also found a licence gap. The native renderer showed no credit for the map data, which
OpenStreetMap's licence requires. It now draws "© OpenStreetMap contributors, Protomaps" in the
corner as plain text, since a link would navigate the app's only window. The end-to-end search
reports the credit's text and whether it lies inside the window, and `tools/offline.sh` requires
both. With the credit planted out of the page, the gate failed.

The code is licensed MIT or Apache-2.0, and the README now describes the whole app, how it was
built, and how to get the data from public sources.

## Weather: NOAA radar and hazard areas

Before writing code I fetched one of everything and read it. The MRMS base reflectivity mosaic is a
0.7 MB gzipped GRIB2 file every two minutes. It holds one message: a regular latitude/longitude grid
of 7000 x 3500 cells at 0.01° (template 3.0), with values packed as a 16-bit PNG (template 5.41) and
dBZ = (raw - 9990) / 10. The G-AIRMET feed is GeoJSON, but asking for it without a forecast hour
returned only the snapshot six hours ahead. The API's schema shows a `fore` parameter (0, 3, 6, 9,
12), and each hour is a separate snapshot valid at one instant. The first request for hour 0 failed
with a 504. So the design took three facts from the data rather than from assumption:
- fetch all five hours and show the nearest
- keep the last good copy when a request fails
- read only the two GRIB2 templates MRMS uses, refusing anything else by name

`crates/wx` decodes and reduces a frame in about 0.1 s (decode 63 ms, thin 38 ms). Thinning keeps
the strongest echo of each 2 x 2 block, because averaging would wash out a storm's core. Its tests
build synthetic GRIB2 messages around real PNG encodings. Five planted defects each failed them:
- a two's complement latitude where GRIB2 uses sign and magnitude
- a flipped binary scale
- thinning that kept the weakest echo
- longitude left in 0-360
- the wrong snapshot on a tie

A real frame and real hazard responses pass through the same parser under an environment variable.

The backend's fetching sits behind a small trait, so its tests run a fake NOAA that fails, recovers
and moves on. The first version merged every forecast hour's features into the "nearest" snapshot.
The fake caught it by drawing one area five times. The same bug was reachable for real: a new
package issued between two of a round's requests leaves two hours valid at the same time. The fix
shows exactly one forecast hour.

The radar is drawn in one pass over the screen. Every pixel goes back through the camera's rotation
and scale to Web Mercator, then to latitude and longitude, then to a cell index; nothing is
reprojected on the CPU. The first country-wide picture looked right, but looking right at zoom 3.6
cannot show an error of a few cells. The check that can draws a synthetic frame whose only lit cell
holds Santa Monica Pier, at zoom 15, twice (lit and unlit), and finds the pixels that changed. Their
centre must be where the map's own projection puts the pier. The first run of this probe found
nothing at all. That looked like a broken shader, but the fault was in the probe. Its texture had no
filter set, WebGL's default wants mipmaps, and an incomplete texture reads as zero. With that fixed,
the offset is 0, 0 px, and a cell measures 46 x 56 px where it should be 47 x 56.7 (1/cos 34°). An
inverted latitude and a one-cell longitude shift, planted in the shader, both failed
`tools/weather.sh`.

Nothing is fetched until a box is ticked, so the benchmarks and the offline gate stay offline. The
fetching is in Rust, so the content security policy is unchanged.

### Loading: 15 seconds of nothing, then everything

Ticking the boxes showed nothing for a long time and gave no sign that anything was happening. The
end-to-end check had hidden this. It polled the backend itself every two seconds, so it never saw
what a user saw. Changed to wait on the layer's own updates and timestamp them, it measured first
frame at 15.0 s, hazards at 15.0 s, and all frames at 15.2 s. That is exactly the frontend's
15-second poll. NOAA was not the cause (0.3-0.8 s a request); the data sat ready in Rust until the
next poll.

Now Rust emits an event after every frame and forecast hour that arrives. The poll is a 60-second
safety net, and a burst of events is folded into one refresh. A round fetches the most useful thing
first: the newest frame, then the hazards, then the older frames three at a time. The state carries
progress, and the panel shows "starting…" on the click, then "radar: loading 4 of 10…". After the
change, the first frame takes 1.3 s, hazards 2.6 s, and all ten frames 7.1 s. A backend test records
the state at every signal. It requires the newest frame alone first, and the hazards before any
older frame. Fetching oldest first, planted, failed it.

## One frame loop: tick every frame, redraw on change

The weather bug (data ready in Rust, shown 15 seconds later) was a symptom of a renderer with no
single place where things happen. Tiles were uploaded to the GPU in their network callbacks, radar
frames on events, and the radar animation on a 100 ms timer. A repaint was requested from wherever.
The model from my own game engine fixes that: one loop, every display frame, takes in what arrived,
updates (animations as a function of time), and draws.

Whether it should draw every frame was measured, not assumed. Idle CPU at rest:
- on demand: 4.0% of a core
- ticking every frame without drawing: 4.0%
- drawing every frame: 65.7%

So it ticks every frame and redraws only when something changed.

The measurement first said 36% at rest for every mode, which sent me after the wrong cause. I blamed
the dev tools' own animation-frame loop, gated it, and saw no change. The gate had never engaged.
The F4 overlay remembers being open between sessions, so it was open in every measurement run,
drawing every frame and polling process memory every second. With it closed: 4.0%. `MAPS_F4=0|1` now
forces it, and every benchmark and gate script sets it closed. The idle memory figure moves too: 337
MB with the overlay closed, against the 355 recorded earlier with it open.

`src/rust-renderer/loop.js` is the loop. It has:
- an inbox of arrivals uploaded inside the frame, most urgent first, within a 4 ms budget (at least
  one per frame, so a large arrival cannot starve the queue)
- updaters that report whether the picture changed
- a redraw only when it did
- a frame clock (ticks, draws, frames later than 1.5x the median interval)

It is pure, with a clock passed in, and its nine tests run in Node. Four planted defects each failed
them: redraw every frame, no budget, least urgent first, and arrivals uploaded in the frame they
arrive. Tile responses and radar frames now arrive as jobs in that inbox. A tile counts as loaded
when it is drawable, not when its bytes arrive.

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

The worst frames improved, as the upload budget would predict: a burst of tiles is spread over
frames instead of landing in one. Peak memory in the stress runs read 17-57 MB higher. But the two
runs of the new build differed by 38 MB between themselves, and the idle probe, three interleaved
runs each, shows no difference. I read it as run-to-run noise. The zoom benchmark's one zoom-out
frame with 1-2% background is there in both builds: the known gap, not a new one.

## A time bar: radar, satellite and hazards at any time in 18 hours

The model came from the Aviation Weather Center's GFA, read from its help page rather than guessed:
- One time slider, 18 hours back to 18 ahead, with every layer synced to it.
- Observations in the past and forecasts in the future.
- The radar loop is the five images ending at the slider's time.
- The valid time is stamped on the map.
- An altitude control shows an advisory only within 3,000 ft of the chosen level.

Satellite needed a source, and the first two candidates each failed on a fact checked before any code.
NASA GIBS serves GOES infrared as Web Mercator tiles with a time parameter, but its newest image was 5
hours 18 minutes old. AWC's own mosaic is not public. NOAA's nowCOAST WMS has GOES-East and West
longwave every 5 minutes, five minutes behind real time, but only about 6.5 hours of history. It also
renders greyscale where GIBS is colour-enhanced, so stitching the two would change the clouds' colours
partway along the bar. Satellite uses nowCOAST alone for now, and the bar shows where its history
ends. Its PNG was histogrammed before the shader was written. 25% of pixels are transparent (no
satellite), clear ground is at grey 48-128, and cloud tops are at 130-224. The shader fades cloud in
from 130.

`wx::timeline` decides what to show at a time:
- A radar slot takes the frame nearest it within 4 minutes, and never one from after the chosen time.
- Satellite is the newest image at or before it and no older than 30 minutes, so a stopped feed is
  not passed off as current.
- Hazards are the snapshot valid nearest.

A planted defect survived its tests. Removing the deduplication of loop frames changed nothing,
because two slots 10 minutes apart can never both take a frame that must be within 4 minutes of each.
The dedup was dead code; it is now a compile-time assertion of that reason.

The backend went from "keep the newest ten frames" to "serve any time". What exists on the servers is
refreshed every round. Frames and images are fetched when first needed, with one fetch per key however
many callers ask. They are kept compressed and evicted least recently used, but the present's loop is
never evicted. The first run of its tests hung. A line held the weather lock while calling a function
that takes it again, and Rust's Mutex is not re-entrant. Killing the hung test with `pkill -f` killed
my own shell, twice, because the pattern appeared in the command line doing the killing. `pkill -x` on
the process name does not have that problem.

The window was captured with the dev status line moved out of the weather panel's corner. The stamp
reads "Radar 22:00Z obs · Satellite 21:58Z obs · Hazards 21:00Z issued". The bar shows radar history
back to its start, satellite from 15Z, hazard snapshots at 21Z to 09Z and the red now line. In
`tools/weather.sh`, the present was ready in 5.6 s and three hours back in 4.3 s. The flight-level
filter took 21 hazard areas to 11-18 depending on the level, and registration was still exact.

## A review of the time bar, and METARs

An independent review of the time bar change, given the code and the spec but not my reasoning, found
five defects and two judgement calls; each was reproduced before it was fixed.

- Changing the time while frames were loading did nothing until every fetch for the old time had
  finished, and meanwhile the map showed the old picture under the new label. The guard meant to stop
  this compared a state object that could not change during the loop. Now every change of time bumps
  a generation. The fetch loop stops after the current request, and a frame for an abandoned time is
  dropped before it reaches the GPU. The end-to-end check picks 6 hours back, then 4 hours back 30 ms
  later. It settles on 4 hours back in under 3 s with nothing uploaded for 6 hours back. With the
  guard planted out, 6 textures for the abandoned time reached the GPU and the gate failed.
- The step button did nothing from live. The first step landed less than a step from now, which reads
  as live. A pure function now steps to the nearest mark a whole step away; the old arithmetic,
  planted back, fails its test.
- A panic inside a fetch left its key marked in flight, and every later request for that time waited
  forever, the webview's refresh with it. A guard now releases the key however the fetch ends. The
  first version of the test hung rather than failed, because a scoped thread waits for the stuck one.
  It now fails at a 5-second timeout.
- A change of radar grid cleared memory but not the disk, and rows dropped past the cap at load were
  never deleted, so both came back at every start.
- The two fetch commands ran blocking network calls on the async runtime's workers. My comment said a
  sync command marked async runs on a thread pool; the macro's source says it is spawned onto the
  runtime. They are async now and run their work on the blocking pool.
- Hazards had no distance limit, so 17 hours back showed today's snapshot labelled as issued. A
  snapshot is now shown only within 90 minutes of the chosen time. Snapshots are 3 hours apart, so one
  is always in reach where forecasts exist. The stamp says when none is near. That broke my own
  end-to-end check, which waited for hazards at a time that now correctly has none. The definition of
  ready was wrong, not the code.

METARs are airport observations coloured by flight category. The API caps a request at 400
observations. One past hour over the US took 30 requests, with the box split until no box was full,
and 80 seconds, including a 504 that was retried. Doing that across an 18-hour bar is not reasonable
to ask of AWC's service. So the present comes from AWC's bulk cache file (2,903 stations, 0.25 MB,
0.3 s). History is what this app collected from it every five minutes while it ran, kept on disk. For
times before collection began, the stamp says so. A click on a station opens its report. On the
national picture the cluster of MVFR stations in Texas sits on the forecast IFR area.

### Time-lapse

Play steps the bar 10 minutes at a time. It moves on only when the step's newest radar frame and
satellite image are on screen and at least 600 ms has passed. The next step is fetched into Rust's
memory while the current one shows. Two hours played in 10-14 s.

My first check of "never leaves a step before it is on screen" was a line that computed nothing. My
second sampled the layer's readiness, which also reads false while any refresh runs, so it reported
complete steps as incomplete. The third asks the GPU whether the step's frame and image were there
when the player left it. A frame for a time already left is dropped before upload, so present means
it arrived in time. A player planted to ignore readiness raced through twelve steps in 0.6 s, and the
gate listed all twelve.

## Winds aloft as particles

The model screenshot the user found shows winds at the selected flight level drawn as particles,
streaks over the map. One request to NOAA's NOMADS GRIB filter, for U and V at 250 hPa over the box,
returned 150 KB in 0.6 s. It held two GRIB2 messages with simple packing (template 5.0) and rows south
to north, on a 0.25° grid of 321 x 181. The decoder took three extensions, each tested at awkward
cases:
- 10-bit values across byte boundaries;
- a zero-bit constant field;
- two messages in one file;
- short data refused rather than read past;
- complex packing refused by name.

The real file decodes to U from -13 to 69 m/s and a jet of 155 knots. Four planted defects failed the
tests, after a planting helper of mine stopped reporting plants it had failed to apply as survivors.
cargo fmt had moved the lines, and the helper ran the tests on the unchanged code.

GFS runs every six hours and is on NOMADS about four and a half hours later, with hourly forecasts.
A chosen time takes the newest run that had started by then and is published, at the nearest hour.
When the newest run is not there yet, it falls back to the run before. The test for that was wrong
before the code was: my constant for 12:00Z was 16:00Z.

Particles move in longitude and latitude at a fixed number of screen pixels per m/s at any zoom, with
the Mercator stretch on latitude. With the stretch planted out, the test failed. The GPU keeps the
trails in two textures used in turn. Each frame, the old one is drawn into the new one a little
fainter, the moved segments go on top, and the result is drawn over the map. Trails are cleared when
the camera moves.

Adding winds broke the end-to-end check in three places, none of them the winds:
- The registration probe compared two frames while particles moved between them.
- The check for "the selection is ready" never confirmed the state was for the time asked for. The
  old 50 ms wait had hidden that race until the wind fetch made refreshes slower.
- That fetch was a real slowdown. It sat ahead of the radar and satellite uploads, so a slow NOMADS
  answer delayed the radar for a new time. It runs alongside them now.

With winds alone on, the app ran at 60 frames a second with no late frames. Redraw was p50 7 ms and
p99 13 ms for the whole frame. The national picture shows the jet around a trough in the West and a
second jet off the Southeast coast.

The gate then failed once more on unchanged code, with no hazards at the present. That was the second
time; the first, during a planted run, I had not explained. The state showed no fetch errors and no
snapshot selected, so the hazards had not arrived yet. When hazards were limited to 90 minutes, "no
snapshot near this time" became a valid answer for readiness. Before the first fetch completes it
also means "not loaded", and the check could not tell them apart. It now requires a completed hazard
fetch, or hazards from disk, before none counts. Three runs after the fix passed.

### Trails that stay through a pan

Testing by hand, the user found that panning reset the trails every frame, and that zooming did too,
with what looked like doubled wind. A dev mode (MAPS_WIND_MOTION=1) now pans and zooms, scripted and
then with real wheel events, and saves frames during and after each. The first capture confirmed the
reset. I cleared the trail texture on every camera change, so mid-move only one-frame dashes were
left.

The first fix kept the texture and carried it with the map. World to screen is a similarity
transform, so last frame's pixels map to this frame's by one exact 2x2 matrix and offset. The matrix
is computed in double precision and tested against the camera for pans, zooms and turns. The captures
showed what that costs. Resampling a texture every frame with linear filtering blurs it a little each
time, so a pan left a smeared wash behind the streaks. During a zoom, the old trails were magnified
while new sharp ones were drawn beside them. That gave a doubled look, probably what the user had
seen.

So the texture was dropped. Each particle keeps its last 16 positions in world coordinates. Every
frame, those paths are projected through the current camera and drawn as lines fading from head to
tail. Nothing lives in screen space, so nothing is resampled or left behind. The gate counts trail
segments drawn during the moves: 64,000 before, 67,000 mid-pan, and 51,000-54,000 mid-zoom, where
particles leaving a shrinking view start again. With histories planted to be wiped on every camera
move, mid-pan fell to 4,961 and the gate failed. With winds alone, the cost is still 60 frames a
second, with redraw p50 9 ms and p99 11 ms.

I made two mistakes on the way:
- A test-file splice left a duplicate helper.
- A shell command inserted gate text containing its own heredoc terminator, so the shell ran the
  remainder and launched the app. The text now goes through a file.

### Fast moves: wind bunched where the old view was

"Really try zooming and panning fast." The slow scripted moves had passed; fast ones had not been
tried. Two numbers per frame now say what a picture shows:
- evenness, the coefficient of variation of particle heads over a 4 x 4 grid of the screen (about
  0.15 at rest);
- the count of trail segments longer than any one-frame move.

Through a fast zoom out, evenness went to 1.6-1.8 and stayed there. Every particle was still inside
the old view's rectangle, a dense patch in the middle of an empty map. Particles only restart when
they age out or leave the view, and a zoom out leaves none. That is the doubling where the previous
tiles were. A fast pan reached 0.51, with the incoming side empty.

Respawning now knows the view moved. Particles that leave the view restart in the part just revealed.
On a zoom out, a share of the rest follows them, sized so the revealed part gets its fair proportion.
Trails are cut at any segment longer than 8 px; a 100 m/s wind moves 4 px a frame. A zoom in
therefore no longer stretches old history into long streaks. The unit tests reproduced the app's own
numbers before the fix: 1.73 after a zoom out and 0.41 after a pan. One of them initially passed
against a stub, because it compared to the very constant it was checking. It uses a fixed bound now.

After the fix, evenness is 0.05-0.09 through every move, with no stretched segments. A fast zoom costs
a quarter of a second of shorter trails while the moved particles build history. The gate now fails
on bunching or stretching at any captured moment. With the fix planted out, it reported 1.5-1.8.

### The doubling, from the user's screen recording

The user recorded it. I cannot play video, so I took two frames a second out of it with ffmpeg and
read them. They showed what no scripted move of mine had tried: panning off North America over the
Atlantic. The wind field was my box (60N to 15N, 135W to 55W) and stopped at its edges. When only a
sliver of the view had wind, all 5,000 particles crowded into that sliver, as dense, overlapping
streaks against an empty map. That was the doubling.

Two changes:
- The particles in use now scale with the part of the view the field covers, and restart only there,
  so a strip of wind is as dense as any other.
- The field is the world: NOMADS's 0.5° GFS, 720 x 361, 0.7 MB.

Asked for whole, NOMADS sends that grid in complex packing (template 5.3), which the decoder refuses
by name. Asked for as a subregion, even one that is the whole globe, it repacks it simply. The 0.5°
files are every 3 hours (f007 is a 404), so the field is the nearest 3-hourly step, and the stamp
shows its valid time. Sampling wraps at the 0/360 seam. The real file decodes; the fastest wind at
250 hPa that evening was 200 knots. The gate now also jumps to the mid-Atlantic and pans there fast.
Trails are present there, and evenness is 0.09.

## The world: SIGMETs, global clouds, METARs everywhere

Radar stays the US. MRMS is a mosaic of US and Canadian radars, and there is no worldwide
ground-radar product. The stamp now says "(US)" so its absence elsewhere is not read as clear sky. The
rest went worldwide.

METARs: the bulk file always held the world (5,147 stations); I had filtered it to the box.

SIGMETs: international SIGMETs from every FIR plus US convective SIGMETs, valid over periods. The feed
is loose:
- the same field is a number, a numeric string, null or the string "None", depending on the request;
- some areas come as several polygons;
- areas crossing 180 come as longitudes past 180, kept continuous.

One record was skipped on the first real parse. It was a SIGMET reported at a point ("SEV ICE OBS
AT"), so points are now kept and drawn as markers (140 of 140 read). The API's date parameter returns
the SIGMETs in force at a time, so a past time's SIGMETs are fetched once per half hour of history.

Clouds: GOES (5-minute) covers the Americas, and outside it nowCOAST's hourly global infrared mosaic
is used. The two use different grey scales. Over the same region at the same hour, the global image's
10th percentile was 59 against GOES's 88, and its 99th was 229 against 201. A pixel-by-pixel
regression over 169,000 matched pixels gives GOES = 0.650 x global + 51.4 (r = 0.976, median error
1.6 grey levels, 5.3 within cloud). It is applied on decode, so one cloud shading serves both. The
global layer is drawn first and discarded wherever the GOES texture has data, so hour-old cloud does
not show through GOES's clear sky.

The end-to-end check failed twice before the code did:
- Its "ready" for the present did not wait for the background round to finish. SIGMETs and the
  global mosaic come late in that round (the mosaic is 5.7 MB), so the world view was looked at
  before they arrived.
- Its SIGMET click landed within 10 px of a METAR station inside the SIGMET, and the station's report
  opened, which is the right behaviour. The check now clicks with METARs off.

The world picture shows severe turbulence over Shanwick and Santa Maria, severe icing from Scotland
to Paris, thunderstorms over South America and Africa, and volcanic ash from Fuego and Reventador.
Where SIGMETs cluster, the labels overlap. They need the collision handling the map's own labels
have.

## The time-lapse buffers; the flash; hazard labels that do not collide

Measurements before any change: the time-lapse's 12 steps took 11.9 s. The longest gap between steps
was 1.8 s, because the first run downloads each step when it reaches it. In 342 of 714 frames, a
layer that had something for the chosen time drew nothing. That is the flash. On each step the
selection moved to textures not yet on the GPU, and the draw showed nothing until they arrived.

Three changes:
- Play now buffers first. Every step's radar frame, satellite and global images, SIGMETs and winds
  are fetched into Rust's caches (`wx_prefetch`, three at a time; nothing goes to the webview yet)
  while the button counts "Buffering k/n".
- While a step is on screen, the next step's textures are uploaded from Rust's memory and kept from
  eviction. The player moves on only when they are on the GPU.
- The draw holds the last picture until the chosen time's texture arrives, instead of drawing
  nothing.

A held loop now fetches only the radar frame it shows, not all five. After the changes, buffering
takes 6 to 8 s, then 12 steps play in 7.5 s. The longest gap is 0.65 s (each step holds 0.6 s by
design), and blank frames are 0 of 448. The gate now fails on any blank frame or a gap over 1.2 s.

Changing what radar fetches broke the gate's readiness, which still waited for all five loop frames.
The 3-hours-back wait ran to 286 s and the run hit its timeout. Readiness and the status line now
count what will be shown.

G-AIRMET and SIGMET labels are gathered and placed together, the most important first: volcanic ash
and cyclones, then thunderstorms, then severe turbulence or icing, then the rest, then G-AIRMETs.
Each label goes at its area's centre if free, else a little above or below or to a side, else it is
left out. The outline is still drawn and a click still opens it. Over the Atlantic at zoom 2.4, 13 of
157 labels fit, none overlapping.

The world view failed once more, on a real gap. At 00:36Z nowCOAST's newest global mosaic was 23:00Z,
96 minutes old, past the 90 minutes I had allowed, so the world's clouds disappeared. The allowance is
now 150, and the stamp shows the image's own time.

## The satellite seam; the time-lapse fetched ahead

The seam over Canada was not where I had put GOES's box (60°N). nowCOAST's GOES layer stops near
49.7°N there, and north of that the global mosaic showed through, about 23 grey levels darker. The
fixed fit from the central US (GOES = 0.650 g + 51.4) does not hold at the border. Over the same
pixels at the same hour the relation is 0.420 g + 99.3. One line does not fit everywhere, so each
global image is now fitted when it is decoded, against the GOES image nearest its hour. The fit uses
the GOES pixels within about a degree of no data or of the image's edge (`wx::satellite::fit_edge`).
It falls back to the fixed fit when there are too few pixels or r < 0.8. The fits measured live ran
from 0.877 g + 33.3 to 1.060 g + 15.7, each on 201,295 edge pixels.

The two images also now meet in one pass. GOES is blended in from none at the edge of its coverage to
all of it 1.5° inside, weighted by how much of a disc around each point it covers. A thin light line
remained exactly on the edge, because filtering mixed GOES's edge pixels with no data. Filtering
there now counts only pixels with data. What still shows is the global image's coarser grain.

Play started with 6 to 14 s of buffering. Now, after each live round, the backend fetches the
default time-lapse's steps into its caches. That is three hours of 10-minute steps, each with its
newest radar frame, satellite and global images, and SIGMETs. After the first round, only the newest
step costs anything. Play still asks for every step, but from memory. It starts once its first three
are in and keeps buffering ahead. From an empty cache, Play pressed about a minute after launch
buffered 94 ms. The gate fails over 2 s.

## The same checks on every platform

Tauri uses each platform's web view (WebKitGTK, WKWebView, WebView2), and the renderer leans on the
WebGL2 features engines differ in. The app already drives itself and reports one `[bench]` line, so
the plan is to run that same run on each platform and judge it the same way.

A run against live NOAA is not repeatable, so the HTTP layer gained a tape (`wx::tape`). With
`MAPS_TAPE_RECORD`, every response is saved under a hash of its URL. With `MAPS_TAPE_REPLAY`,
requests are answered from the tape alone, and the app's clock starts at the recorded time and runs
on. Everything NOAA goes through one `get`, so one change covers radar, satellite, hazards, SIGMETs,
winds and METARs. A recorded run is 83 responses, 69 MB. Replayed inside a network namespace with no
network at all, the whole weather and wind-motion check passed, with no request missing from the
tape. With the satellite capabilities removed from a copy of the tape, it failed and named the URL.

The checks moved from heredocs in `weather.sh` into `tools/e2e_check.py`, plain Python so they run
on Windows. Planted defects in a real report (blank frames, a slow buffer, a wrong popup) each failed
it, and `--no-timing` drops only the speed limits. `tools/e2e.py` runs the app and the checks on any
platform.

Snapshots: two Linux runs of the same tape differ by 0 in the views without wind particles. In the
views with them they differ by up to 18% of 8 px blocks, because the particles are random. So only
the weather views are compared across platforms. Their limits are guesses until the first macOS and
Windows runs.

Found on the way: the `wasm` npm script set PATH with POSIX shell syntax, which npm on Windows runs
through cmd.exe. It is a small Node launcher now.

Not proven here: macOS, Windows and iOS. Their first runs are on GitHub, after a push.

The first run of the Linux leg in a clean Ubuntu 24.04 container, under Xvfb with software GL,
passed everything but one check: one texture was uploaded for a time abandoned mid-load. This was a
real race, which the dev box's speed hid. `refreshOnce` read the generation, awaited the state for
the old time, then stored that state anyway. `sync()` read the generation afresh, so the old time's
images went through the guard. On the dev box `wx_state` answers within the 30 ms between the two
picks; on software GL it does not. It now drops a state whose generation is stale and passes the
generation it started with into `sync`. A unit test with a fake backend whose answer for the first
time is slow reproduces it. The test is red on the old code (the abandoned time's satellite image
uploaded) and green on the fix.

The second container run never finished; it hit its hour. The container was still up afterwards,
because the timeout killed `ci-local.sh` and not the container, so I could look inside it.
`xvfb-run` sat in its `wait` for Xvfb's ready signal (SIGUSR1). Xvfb had been listening on its
socket the whole time, and the app had never started. `tools/e2e.py --xvfb` now starts Xvfb itself
and waits on its `-displayfd` pipe, which is written once the server is ready. The workflow uses it
on Linux. `ci-local.sh` names its container and removes it when the script exits, killed or not.

The third run passed the weather checks and failed one wind-motion check by 0.1%. The sample taken
mid-wheel had 38,198 trail segments, against a floor of 60% of 63,712. That sample counts the trails
grown in the frames the zoom has run so far, so it follows the frame rate:
- 81% of the starting count on the dev box's GPU;
- 74% in software GL on the dev box (a frame every 17 ms);
- 60% in the container (a frame every 37 ms).

The samples after each move were all over 100%. Like the speed limits, the mid-move samples are now
judged only with timing on. On a planted report, the container's mid-move value fails with timing
and passes without, and trails lost after a slow move fail both ways.

Found on the way: pointed at a directory with no tape, the app panicked in a fetch thread and then
waited out the 400 s timeout. `e2e.py` refuses such a directory before starting the app.

## CI on the self-hosted forge

The repository now lives on a self-hosted Forgejo forge first, and its CI runs there
(`.forgejo/workflows/ci.yml`). The gate runs first, then the Linux end-to-end leg, each in a bare
Ubuntu 24.04 container on the forge's one shared runner. The job logic is `tools/forge-job.sh`, so a
job runs the same way in a local container. GitHub's workflows are kept as they are, for macOS and
Windows later.

The runner keeps one directory between jobs, so rustup, cargo, npm's cache and the build tree live
there. The tree is linked as `./target`, since the scripts name that path. It is emptied past 20 GB,
because the disk is shared with other projects' CI and cargo never shrinks a tree. With the cache
warm, the gate takes about two minutes and the end-to-end leg about three, including the release
build.

Two things only the real runner showed:
- The container has no Node until setup installs it, and Forgejo runs JavaScript actions inside the
  job container. So the checkout is a plain `git fetch` with the job's token in git's environment.
- The end-to-end fixtures could not be a release asset. The job's token is accepted by git and the
  API but not by the forge's web download route. A throwaway workflow measured it: 404 with the token
  as a header, as a bearer token and as basic auth, and a redirect to sign-in with none. The archive
  now sits on an `e2e-fixtures` branch with no code on it. The job fetches it over git, checks it
  against `tools/fixtures.lock`, and keeps it in the cache.

Also caught before the first push: the log-tail step, meant to print a failed check's log, ran `grep`
over a passing run. It found nothing and exited 1, and under `pipefail` that failed a passing gate.
With a planted formatting error, the job now goes red and prints each failed step's log. On a clean
tree it is green.

## Releases on the forge

A version tag on the forge now runs `.forgejo/workflows/release.yml`. It runs the gate, then
`tauri build --bundles deb`, then creates a forge release holding the package and its SHA256SUMS
(`tools/forge-job.sh release`). The tag must name the version in `tauri.conf.json`; `v0.1.0-rc.1`
style tags publish a prerelease. The check is done with `sed` before any setup, so a wrong tag fails
in seconds rather than after a build.

One premise was measured first. The job's token, refused by the forge's web downloads, is accepted
for release writes through the API. A throwaway job created a draft release, uploaded a file, and
deleted both (201, 201, 204). The release is created as a draft, filled, then published, so a failed
upload never leaves a public release with half its files.

`ci.yml` now runs on branch pushes only. A tag push used to start it as well, which would have run the
gate twice and put two builds on the shared cache directory at once.

Both directions were proven. A `v9.9.9-rc.1` tag failed at the version check and started no CI run.
`v0.1.0-rc.1` passed the gate and published a prerelease whose `.deb` (12 MiB, `maps-tauri 0.1.0`,
depends on libwebkit2gtk-4.1 and GTK 3) downloads and matches its SHA256SUMS.

## Documentation is part of the gate

Coverage was measured first, with nightly `rustdoc --show-coverage` over public items: `coords` 6/6,
`vtile` 56/165 (34%), `wx` 79/153 (52%), the app crate 3/7. No lint asked for docs, and the gate
never built them.

The workspace `Cargo.toml` now sets the lints every crate inherits:
- rustc's `missing_docs`;
- clippy's `missing_errors_doc` and `missing_panics_doc`;
- clippy's `doc_markdown`: names in prose are code-formatted, and proper nouns such as PMTiles and
  GeoJSON are listed in `clippy.toml`;
- clippy's `too_long_first_doc_paragraph`: the first paragraph is the one-line summary rustdoc's
  index shows;
- rustdoc's broken-link, bare-URL and code-block lints.

`tools/check.sh` gains a `rustdoc` step, `cargo doc` with every warning fatal. clippy already runs
with `-D warnings`. On the tree as it was, the two steps failed with 305 errors before any doc was
written. A planted broken intra-doc link fails the `rustdoc` step; removed, it passes.

About 300 items were documented. Fields got their units and coordinate spaces. Each function
returning a `Result` got an `# Errors` section naming its real failure conditions. Each function that can
panic got a `# Panics` section. `vtile/src/palette.rs` is generated, so its docs come from
`tools/gen_palette.mjs`. Reading the code to document it turned up five existing docs that were
wrong:
- The satellite bounding box said Web Mercator metres and is in degrees.
- `Tape::replay` said it returns `None` for an unrecorded URL, and it returns an error naming it.
- `fetch::metars` said "those in the box" and returns every station.
- A GetMap parameter was misnamed.
- One item carried two conflicting paragraphs.

Coverage is now 100% in all four crates, and the gate keeps it there.

## Doctests, a book, and documentation in every release

Ten doctests now sit on the APIs a reader meets first:
- `parse_coords`, `fold` and `fts_query`;
- the PMTiles tile-id mapping and varint;
- MVT geometry decoding, on the polygon from the format spec's own example;
- radar codes, METAR categories, the time bar's `snap`, and the wind level lookup.

They run in the gate's `cargo test`. All ten passed first time, which proves nothing, so a real bug
was planted: `snap` with truncating division instead of `div_euclid`. The doctest for a time before
1970 failed (0, want -300).

`book/` is an mdBook of eight short chapters, drawn from this journal and the README with the code as
the arbiter. `tools/docs.sh` builds it and the API reference into one site (`target/site`, the book
at the root, rustdoc under `api/`), and the gate runs it as the `docs-site` step.

mdBook 0.5.4 checks almost nothing on its own, which a probe showed before anything relied on it. A
chapter named in `SUMMARY.md` but never written is created empty, and a broken link or image builds
with exit 0 and no warning. `book.toml` now sets `create-missing = false`. `tools/linkcheck.py`
resolves every relative link and `#anchor` in the book's built pages against the assembled site (334
links over 11 pages). Its own controls are a missing file, a missing anchor and a path out of the
site, and each is reported. Against the real book, a deleted chapter and a mistyped API link each
fail the step. mdBook's 404 page is dropped from the site, because its `<base href="/">` is only
right on a server with the book at its root.

A release now also carries `maps-tauri-docs-<tag>.tar.gz`, the same site, listed in SHA256SUMS.
mdBook is pinned (0.5.4) and installed by `tools/ci-setup.sh`, like the other tools CI needs.

## Coverage in CI, and architecture diagrams

`tools/coverage.sh` existed but ran only by hand. It now uses the gate's own toolchain (stable 1.99.1
with its `llvm-tools` component, not nightly). CI runs it on every push as a job beside the
end-to-end leg, with a floor in `tools/coverage-floor.txt` (Rust 80%, JavaScript 73% of lines) that
only moves up. The HTML report is part of the documentation site and so of every release.

Three things were wrong with the numbers before they could be trusted:

- The script found test binaries by listing the build directory, which also held binaries from older
  builds. `llvm-cov` reported "17 functions have mismatched data" and dropped them. Four of the most
  tested files read 31-60% instead of 63-100% (`mvt.rs` 51% -> 100%, `pmtiles.rs` 59% -> 99%,
  `search.rs` 51% -> 100%, `mesh.rs` 31% -> 63%). The binaries now come from cargo's own report of
  the build. Three mismatches remain and are expected. `coords` and the app crate are compiled twice,
  differently under `cfg(test)`: as their own unit-test binary, and as the library their integration
  tests link.
- The report embedded absolute source paths, which would have published the dev box's home directory
  in every release. Paths are now remapped to the repository root at build time.
- On the runner, the first CI run failed its floor at 6.14%. The exclusion pattern `\.cargo` assumed
  cargo's registry lives in `~/.cargo`. On the runner it is under the job cache, so every dependency
  was counted. With the paths remapped, the workspace's files are the only relative ones, so the
  report now ignores every absolute path and needs no list of where other code lives. The floor
  caught this on its first run, which is what it is for.

The floor was proven both ways. Set above the real totals, it fails naming both; at the real floor,
it passes.

The book has five Mermaid diagrams: the process and its data, the crate graph, the native render
pipeline, the weather flow, and CI and releases. The crate graph is generated from `cargo metadata`
(`tools/crate_graph.py`), and the gate fails if it is stale. Mermaid draws in the browser, so a
syntax error would build cleanly. `tools/mermaid_check.mjs` parses every diagram with the same
Mermaid the pages load. That was 11.17.2 then and is 12.3.0 now; it comes from `package.json` and is
served in place of the older copy `mdbook-mermaid` bundles. The check runs under jsdom, because
Mermaid sanitises HTML labels with DOMPurify. A planted syntax error in a chapter and in the
generated include each fail it. The pages were rendered in headless Chromium and looked at. The
first layout of the system diagram read as a chain of data stores, and was regrouped.

## Mutation testing

`cargo-mutants` 27.1.0, pinned in `tools/forge-job.sh`, now runs in CI in two modes. Every push
mutates the lines it changed, in place on the runner's warm build tree, and fails on any surviving
mutant. The diff is `--in-diff` against the commit the branch was at before the push. A weekly
workflow, also runnable by hand, mutates the whole tree and compares the number missed with
`tools/mutants-baseline.txt`.

What it took:

- The first full run on the dev box died silently after 11 seconds. cargo-mutants 27 copies the tree
  for each job and does not honour `.gitignore` by default. Every job therefore copied the map data,
  `node_modules` and the logs into `/tmp`, which is memory here. Setting `gitignore = true` in
  `.cargo/mutants.toml` and putting scratch under `logs/` fixed it.
- `--in-place` and `--jobs` cannot be combined; cargo-mutants refuses the pair at argument parsing.
  The per-push mode would have failed on its first run. A local trial with the CI job's own
  arguments found it, and `tools/mutants.sh` now passes `-j` only to copied-tree runs.
- A run that finds nothing to mutate exits 0 without writing a report, so the job could have read
  the previous run's `missed.txt`. It now deletes the old report first.
- The whole tree is 2,583 mutants. A full run on the dev box was stopped for memory after 549 of
  them: 366 caught, 115 missed, 7 timeouts, 61 unviable. The misses were almost all in the app
  crate's glue (`weather.rs`, `devstats.rs`, `tiles.rs`). The baseline is recorded from the first
  full run on the CI runner instead.
- That run is longer than the runner allows. The ~600 app-crate mutants each relink Tauri and run
  its tests, about a minute apiece; the ~2,000 in the libraries take seconds. Serially that is over
  10 hours. forgejo-runner stops a job at its `runner.timeout`, whatever the workflow's
  `timeout-minutes` says. That timeout is 3 hours by default. It is not set in our runner's config,
  and the figure is the documented default, not measured here. The weekly workflow is therefore four
  `--shard` jobs run one after another, whose missed lists meet in the runner's cache. A last job
  adds them up against the baseline. Copied trees build without debuginfo, at 1.7 GB each on a 60 GB
  runner disk that six repositories share.

Both outcomes were tested. A diff touching two lines of `timeline.rs` gave 4 missed mutants and exit
2; the job fails and lists them. A diff with no Rust gave "no Rust source files" and exit 0. The
four were real gaps. No test had a closer later radar frame, a tie between frames, or two hazard
snapshots in reach at different distances. Two new tests catch them, and the file's 35 viable
mutants are all caught.

The first full run on the runner (run 11195) took 69 minutes. The first shard took 41 of them,
because it built four copied trees from nothing; the others took 4-11 minutes per shard. 516 of the
2,461 viable mutants were missed, and 122 more do not compile. 516 is the baseline. Most misses are
in two places:

- `vtile/src/mesh.rs`, 223 of its 538. The gate's whole-archive step meshes every tile under a time
  limit. That catches a hang or a panic but checks no output. It is also not a cargo test, so
  cargo-mutants cannot see it either way. The unit tests check the shapes produced, not the
  arithmetic inside. This was the clearest gap the run found, and the first to close.
- The app crate's glue, 151: `weather.rs` (71, mostly the NOAA source's methods, whose tests would
  be the network), `devstats.rs` (33, a debug overlay), `tiles.rs` (24).

The shards were not even: 235 missed in one, 67 in another. cargo-mutants' default sharding takes
consecutive slices of its list, not round-robin as the workflow's comment originally said, so all of
`mesh.rs` fell in one shard.

## Dependencies and advisories

Nothing checked the third-party code: 458 crates in Cargo.lock and the npm tree. Now `deny.toml`
holds the policy, for cargo-deny 0.21.0 pinned in `tools/ci-setup.sh`. It is checked two ways. The
gate's `deps` step runs licenses, bans and sources offline, so its verdict depends only on the
commit. CI's `audit` job runs on every push and daily (`.forgejo/workflows/audit.yml`). It fetches
the RustSec database for `cargo deny check advisories` and runs `tools/npm_audit.py`.

What the first runs found:

- Licenses: with no allow-list, cargo-deny rejects all 435 crates. The list it needed is permissive
  licenses plus MPL-2.0 and CDLA-Permissive-2.0. MPL-2.0 covers cssparser and selectors through
  Tauri, and option-ext through dirs. It is copyleft per file, so linking it unchanged is fine.
  CDLA-Permissive-2.0 covers Mozilla's CA list, webpki-roots. LGPL appears only as one choice among
  MIT and Apache (r-efi), so it is not allowed.
- None of the four crates said `publish = false`, so cargo-deny read the app's path dependencies as
  wildcards. They now do.
- One Rust advisory and no vulnerabilities. proc-macro-error is unmaintained (RUSTSEC-2024-0370). It
  is a build-time macro crate reached through glib-macros in the GTK 0.18 bindings Tauri uses on
  Linux. It is accepted in `deny.toml` with that reason.
- npm: picomatch's ReDoS (high), reached only through Vite's build with our own globs. No fixed
  release exists, and npm's suggested fix is a downgrade of Vite 8 to 6.
- npm: a KaTeX prototype pollution note (low) inside Mermaid, which the docs site serves. Mermaid
  was upgraded from 11.17.2 to 12.3.0 (KaTeX 0.18.10). All five diagrams still parse, and the pages
  were rendered in headless Chromium to see them drawn.
- npm has no way to accept an advisory, so `tools/npm_audit.py` keeps a list. Each entry has a
  reason and an expiry date. An expired entry, or one that no longer matches anything, fails.

Each check was proven red with a planted defect:

- each cargo-deny section: MPL-2.0 removed from the list, the advisory ignore removed, `ring`
  banned, crates.io not an allowed registry
- the `deps` gate step, with MPL-2.0 removed
- the npm check four ways: an advisory not on the list, an expired entry, a stale entry, and an
  unusable report, which exits 2 rather than passing

## Fuzzing

Seven cargo-fuzz targets in `fuzz/` cover every parser of outside bytes. They use libFuzzer on
nightly-2026-07-11, the 1.99 line, since its sanitizer flags are nightly-only. The targets are:

- the PMTiles archive read path, through an in-memory `RangeCache`
- PMTiles directories
- MVT decode plus meshing
- GRIB2 (MRMS and GFS winds)
- radar decompression
- the weather services' text formats
- the search box, whose coordinates must come out in range

`tools/fuzz.sh seeds` takes starting inputs from the pinned e2e fixtures. From the map it takes the
header and root directory and two tiles per zoom, through a `vtile` example so the real reader picks
them. From the tape it takes one answer of each kind, cut down. CI fuzzes 30 seconds per target per
push, and 10 minutes weekly with corpus minimisation. The corpus lives in the runner's cache. Builds
use `-a`, because with overflow checks off the bugs below would not crash but silently wrap.

The first hour of fuzzing found these bugs, all integer overflows on numbers from the input:

- `archive`: the header's tile-data offset plus a directory entry's offset (`pmtiles.rs`, tile
  lookup). A debug build panicked; a release build would have read from a wrapped offset. The fix
  treated it as a class. The same unchecked sums were in the leaf-directory offset (twice), the run
  end in `find` and the tile lookup, `all_tiles`, and the HTTP Range header's last byte. Sums of two
  file-supplied offsets now go through `offset_in`. The run checks now compare a distance
  (`id - tile_id < run_length`, where `id >= tile_id` is already known), which cannot overflow.
- `pmtiles_dir`: `tile_id_to_zxy` on an id past zoom 31 (the deepest a u64 id reaches) shifted past
  64 bits. It now returns `None` for those.
- `mvt_mesh`: route-shield placement added and subtracted two tile coordinates as `i32`. The decoder
  deliberately wraps corrupt deltas, so coordinates near `i32::MAX` are reachable. The arithmetic is
  now in i64 and f64. A bound on coordinates at decode would have fixed every mesher site at once.
  But the decoder is diffed against @mapbox/vector-tile in the gate, and that decoder has no bound.

Each has a unit test that failed with the fuzzer's panic before the fix. The archive and directory
crash inputs are in `fuzz/regressions/`. The mesh crash input was a 140 KB modified OpenStreetMap
tile, and its unit test is the regression.

As a control for the fuzzer itself, the tile-id overflow in `parse_directory`, fixed earlier by
hand, was planted back. The archive target missed it for 10 minutes and 5.8 million runs.
Directories are gzipped in the file, and a mutation of gzipped bytes almost always fails the
checksum, so the parser behind it was out of reach. The `pmtiles_dir` target exists for that case,
and it found the plant in 8 seconds. An archive target started with no seed ran 26 million inputs
without getting past the magic number. Seeds matter.

Throughput varies by four orders of magnitude. The search box runs ~40,000 inputs a second. GRIB2
runs about 10, because its seeds are whole real messages of 350-700 KB. Smaller synthetic GRIB2
seeds would help that target most.

The per-push mutation check, run locally on these fixes before committing, found two more things.

cargo-mutants built without `--all-features`, so code behind vtile's and wx's `http` feature was
never compiled. Its 11 mutants read as missed though no test ran. The gate, clippy and coverage all
build with every feature. `.cargo/mutants.toml` now sets `all_features = true`. The 516 baseline was
measured without it, so it is a ceiling until the next full run gives the real number.

The check also showed that nothing but the end-to-end run checked the mesher's labels at all:
`Mesher::labels` replaced by nothing survived. A test now places two route shields where only the
right length arithmetic puts them. `find` lost a redundant condition that made one mutant
equivalent. All 60 viable mutants on the changed lines are caught.

## A hang the weekly fuzz run found

The first weekly fuzz run (10 minutes a target) failed on `mvt_mesh` with a libFuzzer timeout: one
48 KB fuzzed tile took longer than 10 seconds. Replayed locally, it ran without end, CPU-bound, at
56 MB. Stack samples under gdb put it in `earcutr::earcut_linked_hashed`, inside the ear-clipping
passes that `fill_polygon` calls. Timing every polygon of the tile separately, at 20 seconds a
polygon and release speed, named one landuse polygon: 162 points, 39 holes, ordinary coordinates.
Greedy deletion of holes and points, keeping the hang, cut it to a 28-point ring, a triangular hole
and 27 one-point holes. That is the unit test. It runs on its own thread, so a regression fails in
10 seconds instead of hanging the suite. The test failed against `earcutr` before the change.

`earcutr` 0.5.0 is the newest release, so the fix is a different crate. Three candidates were run on
the same tile:

| Triangulator | As decoded | Clamped to i16 |
|---|---|---|
| `earcutr` 0.5.0 | hangs (the 162-point polygon) | hangs |
| `earcut` 0.4.12, f64 | completes | completes |
| `earcut` 0.4.12, `EarcutI32` | hangs (a polygon with x near -2e9) | completes |

`earcut` (georust) tracks earcut.js 3.2.4, which is the version in MapLibre's lock file, so the two
renderers now triangulate with the same algorithm. The integer variant hangs because exact i64
predicates on coordinate differences near 4e9 overflow. The f64 one survived that polygon, but at
that size its orientation tests are not exact either. Ring points are now clamped to i16 before
triangulating. The mesh stored them as i16 already (`to_i16`), so earcut was computing triangles for
points other than the ones drawn. Clamped, every product it forms is under 2^53 and exact.

On the whole West LA archive (`examples/meshcheck.rs`, 772 tiles, 85,711 polygon features), against
HEAD:

| | `earcutr` | `earcut` |
|---|---|---|
| features off their exact area | 2, worst 0.54% | 0 |
| decode + mesh | 1.46 ms/tile | 1.13 ms/tile |

A planned control for the clamp, a polygon with one vertex at -2e9 checked by area, passed against
the old code too. The stored vertices were already clamped and the indices came out the same. It
showed nothing, so it is not in the suite. The clamp's evidence is the exactness argument and the
integer variant's hang, not a test. Five more minutes of `mvt_mesh` from the CI corpus plus the
timeout input found nothing.

## Release provenance: SBOMs and a signature

A release now says what it is built from and who built it. `tools/sbom.sh` writes two CycloneDX 1.5
SBOMs:

- the app crate with every crate it builds with on Linux x86-64 (`cargo-cyclonedx`, 305 components)
- the frontend's runtime npm packages (`npm sbom --omit dev`, 28)

`cargo-cyclonedx` names the workspace's own crates by absolute path (`path+file:///home/...`), which
would publish the builder's home directory. The script rewrites those to repository-relative paths,
and fails if the checkout's path or `$HOME` is still in either file. `cargo-cyclonedx` also writes a
raw report beside every workspace member, not only the one asked for; a run left six stray files.
All of them are removed on exit, pass or fail.

`SHA256SUMS` covers the packages and both SBOMs. `tools/sign-release.sh` signs it with a minisign
key, made with `tauri signer generate` so the same key can later sign Tauri updater bundles. It then
verifies the signature against the committed `release-signing.pub` before anything is uploaded. A
release with no key fails instead of publishing unsigned. The private key lives in the forge secret
`RELEASE_SIGNING_KEY` and a password manager, never in the repository.

This was checked in the release container with a throwaway key. A good key signs and verifies. No
key, the wrong public key, and a file changed after signing each fail. With the real key (ID
8BC05ECE547F9F2D) it signs and verifies, and the signature does not verify a different file.

## A protected main, and releases only from it

Until now every commit went straight to one long branch, and `main` on the forge was still the M0
scaffold. That branch is now merged and retired. The merge was pull request #1, a fast-forward, so
`main` is the exact commit CI tested, `fdac556`. `main` is protected: no direct pushes, the
maintainer's included.

A change is a pull request from a short-lived branch. It is mergeable when:

- the `ci / verdict (push)` status is green on its head (`verdict` needs every other CI job)
- the branch is current with `main`
- one approval is given

Changes are mostly prepared by an automation account (`datastrike-agent`), and the forge does not
let an author approve their own pull request. That approval is therefore the maintainer reading the
diff. Only fast-forward merges are allowed. An outdated branch is updated by rebase, which reruns CI
on the new head. Read back from the forge's API as the agent account, the settings are
`protected: true`, `user_can_push: false`, `required_approvals: 1`,
`status_check_contexts: ["ci / verdict (push)"]`, and merge styles fast-forward only.

Releases follow the same line. `forge-job.sh release` fails before any setup unless the tagged
commit is on `main`; the release checkout fetches `main`'s history without blobs. It was checked red
with the tag on the then-unmerged branch and with no `main` to compare against, and green with a
commit on `main`. The scheduled workflows (daily advisories, weekly fuzzing and the full mutation
run) run from the default branch, so they start running only now. Until this merge they had run only
when dispatched by hand.

## The required check that could not fail

Wiring a new CI job into `verdict` showed that `verdict` itself never failed. It worked out
`success` or `failure` from the other jobs, published that as an event, and exited 0 either way, a
cancelled run included. It had been written as a notifier, before anything depended on its status.
Since this morning, `main`'s branch protection requires `ci / verdict (push)` and nothing else. A
pull request with a red gate would therefore have shown a green required check. Nothing red was
merged, since every pull request so far had all jobs green, but the protection was not protecting.

The step now ends with `[ "$v" = success ]`. The event stays best effort; the status no longer is.
This was checked by running the step's script, taken from the workflow file, before and after, for
four leg outcomes: all green, a red gate with the rest skipped, one red leg, and one cancelled.
Before the change, all four exited 0. After it, all green exits 0 and the other three exit 1.

## A performance gate

Every check so far asks whether the code is right; none asked whether it got slower. The gate
benchmarks the per-tile and per-update work:

- vector-tile decode and mesh at z5, z9 and z14, on the largest tile of the fixture archive at each
- the root directory, parsed and searched for every tile id to z9
- a radar frame gunzipped, decoded and thinned as `weather.rs` does it
- a GFS wind field

It counts instructions under Callgrind through gungraun 0.20. `tools/perf.sh` compares the counts
against the merge base with `main`, with both sides measured in the same job. It fails above +3%
unless a commit carries `Perf-accepted: <why>`.

It counts instructions, not time, because the runner is shared by four concurrent jobs. Measured in
the CI image over three runs of the same build, decode and directory were identical to the
instruction. The mesher was within 0.022%, because its label placement uses a hash map. Radar and
wind were identical. 3% is a hundred times that. The suite takes 13 seconds a side. In a container
gungraun cannot turn address randomisation off: `setarch -R` needs `personality()`, which the
seccomp policy refuses. It therefore runs with `GUNGRAUN_ALLOW_ASLR`, and the repeat runs above were
made that way.

The gate was proven against planted regressions in the CI image:

| Change | Result |
|---|---|
| nothing (base = head's code) | pass, every count within 0.004% |
| triangulator scratch rebuilt per polygon | mesh z14 +7.4%, z5 +2.9%, z9 +1.5%: fail (exit 1) |
| the same, with a `Perf-accepted:` trailer | pass, reason printed |
| linear search in `pmtiles::find` | directory **-29%** |

The last was meant as a regression. The root directory is short and the benchmark searches it in id
order, where a linear scan beats a binary search. The plant measured a fact about the input, not a
defect of the gate. The lost-reuse plant replaced it.

Those runs found two bugs in the gate itself. gungraun clears the environment before it starts
Valgrind, so the fixture paths (`MAPS_BENCH_ARCHIVE`, `MAPS_BENCH_TAPE`) reached only the head. They
are passed by name with `--envs`.

The worse bug was that the first version compared the base with itself. Base and head built into one
target directory. Cargo names a workspace crate's artifacts the same whichever directory the source
is in. Its freshness check reads the dependency list the last build wrote, which named the base
tree's unchanged files. So the head was never rebuilt, and a planted regression read "No change".
The base now builds in a target directory of its own. Before measuring, the gate rebuilds a head
whose dependency lists name the base tree. This was proven by forcing the shared directory: the
guard fired and the plant failed the gate.

A third trap is handled before it could bite. Given `--baseline` with no saved baseline, gungraun
saves one and compares nothing, so the gate fails when the base run leaves no `*.out.base@base`
file.

The suite also showed that the weather path's 1.1 billion instructions for one radar frame dwarf
everything on the tile path. The heaviest mesh is 50 million.

## Secrets in the gate

A gate step, `secrets` (`tools/secrets.sh`), runs gitleaks 8.30.1 with its default rules. It scans
the checkout's history and the working tree as a commit would take it:
`git ls-files -co --exclude-standard`, copied to a scratch directory and scanned there. The history
alone would miss the case the gate exists for, because the gate runs before the commit. Findings are
printed with `--verbose` and `--redact`. Without `--verbose`, gitleaks says only "leaks found: 1",
which names nothing to fix. `.gitleaks.toml` extends the defaults and holds no exceptions yet. Any
later exception goes there with its reason, not as an inline `gitleaks:allow`.

CI installs the release tarball, checked against its published SHA-256 (`tools/ci-setup.sh`). The
dev box takes Arch's package, the same 8.30.1. Its binary answers `gitleaks version` with "version
is set by build process". In that case the step asks pacman which version owns the binary. The first
run on the dev box failed exactly that way, with exit 2, before the fallback existed. A gitleaks
that is missing or another version fails the step with exit 2: a tool that is absent is not a scan
that passed.

The 80 commits of history scan clean in under a second. The red-first cases ran in the CI image on a
clone, with a random `ghp_`-shaped token generated in the container:

- untracked file: exit 1
- staged file: exit 1
- committed and then deleted in a later commit: exit 1 (the history scan)
- under the ignored `logs/`: exit 0
- gitleaks absent: exit 2

CI's check job fetches 50 commits, so in CI the history scan covers the last 50. The whole history
was scanned once here, and the gate on the dev box scans all of it every time.

## An accepted advisory withdrawn

On 2026-11-18 GitHub withdrew GHSA-fc5q-39xc-vjpf, the picomatch ReDoS accepted in
`tools/npm-audit-allow.json`. picomatch is still 4.0.7, and the advisory record says it was
withdrawn at 15:38 UTC. npm stopped reporting it, and the `audit` job failed on main's next push
with "in the allowlist but no longer reported, remove it". That is the check that keeps the list
from outliving its reasons, working as intended. The entry is gone and the list is empty.

## The oldest Rust it builds with

The workspace now declares `rust-version = "1.90"`, and CI proves it. A job, `msrv`
(`tools/msrv.sh`), installs Rust 1.90 and runs `cargo check` on the whole workspace, with all
targets and features. It also checks the search crate for `wasm32-unknown-unknown`. Until now the
only Rust the project was known to build with was the 1.99.1 the gate pins.

The number was measured, not chosen. Among the 467 resolved packages the highest declared
`rust-version` is 1.90 (Tauri 2.12, its runtime, muda, tray-icon, bincode-next), so cargo refuses
anything older. 1.90.0 then built everything with no change to our code. A `cargo check` is enough.
The version decides whether the code compiles, and a test run would repeat the gate on an older std.

The red-first checks ran on the dev box:

- `std::iter::chain` (stable since 1.91), planted in the search crate, built on 1.99.1 and failed
  the job with exit 1 (E0658, the planted line).
- A member with its `rust-version.workspace = true` removed failed the job with exit 2.

Declaring the version also arms clippy's `incompatible_msrv` lint inside the gate, which failed on
the same plant. That lint covers standard-library items only, so the job stays the authority for
syntax.

## Unused dependencies

A gate step, `unused-deps`, runs cargo-machete 0.10.1 (pinned in `tools/ci-setup.sh`). The workspace
was already clean. Planted dependencies, one per kind of table, gave these results:

- An unused `[dependencies]` entry failed the step, in `coords` and in the separate fuzz workspace.
- One in `wx`'s `[dev-dependencies]` passed.
- One in the app's `[build-dependencies]` passed.

Its source says why: `find_unused` reads `manifest.dependencies` only, under a "TODO extend to dev
dependencies + build dependencies". So the step covers what ships, and the few dev- and
build-dependencies are left to review. A crate named only in a comment is still reported unused.

The step was also proven red through the gate itself, with `coords` planted as a path dependency of
`wx`. That dependency resolves, so nothing else breaks for the wrong reason. `unused-deps` failed.
So did `docs-site`, correctly, because the book's crate graph is generated from the dependency graph
and the plant changed it.


Running the whole gate in a fresh container for this change found an older fault. The `deps` step
(cargo-deny, `--offline`) failed there on `windows-sys` and `android_system_properties`. cargo-deny
reads the crates of every platform `deny.toml` lists, and nothing in the gate downloads the
non-Linux ones. The forge passed because its runner cache already held them; a new clone or GitHub's
runner would not have. The step now runs `cargo fetch --locked` first.

## A security policy

`SECURITY.md` says:

- how to report a vulnerability (email to a dedicated address, encrypted to the project's OpenPGP
  key if it holds anything sensitive)
- what a reporter can expect (acknowledgement in 7 days, assessment in 30, disclosure at 90 by
  default)
- that only the newest release is fixed
- what is in scope: the code that reads outside data, which is what the fuzz targets cover

The reporting key is `security-key.asc`, an Ed25519 certification key with a Curve25519 encryption
subkey, both expiring 2028-11-17. The maintainer made it on their own machine. Its private half and
revocation certificate are in the password manager, never in the repository. It was checked before
the policy named it: a message encrypted to the committed key file alone, in an empty keyring, was
decrypted by the maintainer's key. It is separate from the release-signing key. One signs what the
project publishes; the other receives what is sent to it.

Forgejo has no private reporting channel; confidential issues are an open request, forgejo#4944. The
forge is not reachable from outside anyway. Email is therefore the channel, as it is for Forgejo
itself.

## Updates, signed and checked

The app can update itself. It uses Tauri's updater plugin (2.14.1), asked from Rust when the window
opens, with a notice and an "Install and restart" button in the window. A release now also publishes
the `.deb`'s updater signature and `latest.json` (`tools/update-manifest.sh`), both listed in the
signed SHA256SUMS. The release key verifies updates too, since it was made with Tauri's signer for
this.

Two properties of the plugin, read from its source, set the design:

- On Linux it can update a `.deb` install (download, verify, `pkexec dpkg -i`), not only an
  AppImage, so releases stay `.deb` only.
  It detects the install type from a marker the bundler writes into the binary, and the test checks
  that the marker is present.
- Only the package is signed, not the manifest. The signature therefore carries the version
  (`tauri signer sign --app-version`), and the app checks it against the version the manifest
  announces.

The update URL is a build-time setting, unset for now. The forge is not reachable from outside, so a
release built today has no updater and makes no request. Setting the forge variables `UPDATE_URL`
and `UPDATE_BASE` when the releases have a public home turns it on.

`tools/update-e2e.sh` (weekly, `update.yml`) installs 0.1.0 in a fresh container, serves manifests
over local HTTPS with a throwaway key, and runs the installed app:

- a genuine 0.1.1 installs
- a tampered package, another key, a version replay and an unversioned replay are refused
- 0.1.0 is "nothing newer"

It uses HTTPS, not HTTP, so the run goes through the TLS stack a real update uses.

The planted defect, `requireSignedVersion` off, initially passed the test. The plugin compares a
signed version with the announced one whenever the signature includes a version; the setting only
governs signatures without one, and the test had no such case. With that case added, the plant fails
(the old package installs as 0.1.2), and with the setting on the update is refused.


## The native renderer by default

The native renderer is now what the app starts with; `MAPS_RENDERER=maplibre` selects MapLibre. The
weather layers exist only in the native renderer, so a plain launch or an installed package showed
the map without them. `tools/bench.sh` and `tools/memprobe.sh` still default to MapLibre, since the
earlier results in `bench/` were taken on it. Every other script already named its renderer. This
was checked by launching the release build both ways with a short stress run. The `[bench]` line
reports `"renderer":"rust"` with no flag and `"renderer":"maplibre"` with it, with no errors and no
policy violations in either.

## An installed app finds its data

The data directory was the source tree's `data/`, compiled in as an absolute path. An installed
package on any other machine therefore looked in a directory that does not exist there and drew an
empty map. The lookup order is now:

- `MAPS_DATA`
- the source tree's `data/` when it exists (a build run where it was built)
- `local.maps-tauri` under the XDG data directory or `~/.local/share`

A missing archive is reported by path on stdout. The choice is a pure function with a unit test,
which fails with the old rule planted back.

The README's install steps were then run as written in a clean Ubuntu 24.04 container, as an
ordinary user. The package installed. go-pmtiles 1.31.2, checked by sha256, cut West LA from
Protomaps' build of 2026-11-18 in 3 s (25 MB). A 10-operation stress run on the native renderer
showed no blank frames, no errors and no policy violations. Without the archive, the same install
printed the path it looked in.

Cutting the map was most of the install, so the extract is now published once, as the GitHub release
`map-westla-20261118`. It is 25 MB, sha256 `7e598c19…0445`, cut with the same command from the
2026-11-18 build. The install is now a package, one download and a launch. The downloaded asset
matched the local cut byte for byte, and the same clean-container run on it drew with no blank
frames. The anonymous download URL can only be checked once the repository is public.

Search needed the GeoNames index, whose builder is private. The release therefore also carries the
6,465 places inside the map's bounds, copied from the full index (13.5 million places) with the same
schema and the full-text index rebuilt. It is 1.1 MB. In the same clean install, "santa mon",
"venice beach" and "griffith" each found the place they name.

## A README for people, and a recorded tour

The README now opens with what the app does in plain words, a looping clip of the weather layers
coming on, and the install steps. The engineering detail moved under For developers, and the weather
section's specifics moved to the book. The clip comes from `MAPS_DEMO=1`, a paced tour: West LA, the
US with satellite, radar, hazards, airport reports and winds one at a time, the time-lapse, the
world, and LAX's report. `tools/demo-video.sh` records it in a virtual display. The first recordings
got three things wrong:

- The window opened on the desktop's Wayland session instead of the virtual display.
  `WAYLAND_DISPLAY` is now unset for it.
- The display's cursor sat in every frame.
- With WebGL in software, the capture dropped frames. The video ran shorter than the tour, and the
  clip's cut, taken from the tour's step times, landed on the wrong frames. The capture now stamps
  frames by the wall clock.

The full tour is an asset on the v0.1.0 release (7.5 MB); the clip is `docs/media/demo.gif` (4.7
MB).

The first plain-language version over-explained. It described a renderer as Rust preparing pieces
and the graphics card drawing them. The README's top half is now written the way a project README
usually is: short declarative lines in the field's own terms, a feature list, the measurements as a
table with their methodology linked, and the limitations.

## Windows and macOS

The first platforms run on GitHub's runners found two bugs that the Linux-only CI could not.

`vtile` read archive files with `FileExt::read_exact_at`, which exists only on Unix, so the Windows
build failed to compile. It now reads through a small helper: `pread` on Unix, and on Windows an
offset `ReadFile` (`seek_read`, looped until the buffer is full). A cross-check of `vtile` for
`x86_64-pc-windows-gnu` fails on the old code (no `std::os::unix`) and passes on the new.

`devstats::tests::this_process_is_found` asserted that the app finds its own process in `/proc`,
which macOS does not have. Process memory is Linux-only by design, so the test now runs on Linux
only.

With those two fixed, the Windows leg reached tauri-build and stopped there. A Windows build embeds
an `.ico` in its resource file, and the app had only `icon.png`. `icons/icon.ico` is now made from
that PNG (16 to 128 px) and listed after it, so Linux and macOS keep using the PNG. The Debian
package builds as before.

Then the unit tests ran on Windows, and three failed, all on Unix path rules: `/x` is not an
absolute path there, and `?` is not allowed in a file name. Those three tests now run on Unix only.
The cache and data directories themselves still follow `HOME` and the XDG variables. On Windows the
disk cache therefore stays off, and the data directory is `MAPS_DATA` or the source tree's. A
Windows layout (`%LOCALAPPDATA%`) is not done.

The next Windows run found that the Windows read helper itself overflowed. The archive tests ask for
a read at an offset near `u64::MAX`. `pread` refuses that on Unix, because its offset is signed. The
helper instead added the bytes read to the offset and panicked. It now refuses an offset past
`i64::MAX` up front and checks each step, so both systems answer the same way.

The forge's mutation job then reported the Windows helper's mutant as missed. The function is not
compiled on Linux, so no test there can run it. It now lives in its own file, `vtile/src/winio.rs`,
excluded from mutation testing with that reason, like the other files only the app or a web view
runs. `platforms.yml` runs the archive tests on Windows. The Unix helper stays in `pmtiles.rs`, and
all 7 mutants on the changed lines are caught.

With the unit tests passing, the Windows leg stopped at its fixtures download. `python` there ends
its line with `\r\n`, so the file name read from `tools/fixtures.lock` carried a `\r` and matched no
release asset. The workflow strips it.

The first platforms run with every desktop leg green failed only the comparison: all four weather
snapshots differed in every block. The data agreed: the same radar frames, hazards and METAR count.
The images did not have the same size. The window asks for 1200x800, and the macOS and Windows
runners' screens are smaller, so the system shrank it to 1024x653 and 1028x749. The comparison
scored a size mismatch as 100% different; it now fails naming the two sizes. `tools/e2e.py` also
sets `MAPS_WINDOW_SIZE=960x600`, which the app applies to its window at start-up, so every platform
takes its snapshots at one size. Users and the benchmarks keep 1200x800. Locally the end-to-end run
passed with all five weather snapshots at 960x600.

The next run measured the engines at one size. On Windows the differing fractions of blocks were
weather 0.0376, weather-past 0.0108, weather-seam 0.0001 and weather-world 0.1267. The world view is
mostly wind trails, which are random by design, and SIGMET labels that the engines' font metrics
place differently. Its limit is now 0.2; the other limits held. macOS came out 960x572, because
there the window size includes the 28 px title bar. The app now reads the page's size back and grows
the window by any shortfall. It only grows. The first version also shrank, and on Linux, where the
resize lands later, it read the old 1200x800 back and shrank the page to 720x400. The local
end-to-end run caught that.

The read-back did nothing on macOS: the page still came out 28 px short. The fit now happens in the
page, which knows its own size. In a test run (`MAPS_WINDOW_SIZE`) it compares `innerWidth` and
`innerHeight` with the size asked for, and has the app resize the window by the difference until
they match. The resize is a new command, refused outside a test run. A planted 28 px shortfall at
start-up (the macOS case, made on Linux) first showed the command missing from the app's permissions
(`not allowed by ACL`). The Linux end-to-end run could not show that, because there no fit is
needed. With the permission granted, the page fits itself back to 960x600.

With macOS and Windows passing, `platforms.yml` is back on its nightly schedule and on version tags.
The iOS simulator job is gone. The app has no mobile entry point, so `tauri ios build` can only fail
(the library lacks the required runtime symbols). As an allowed failure it put a red mark in every
otherwise green run. It returns with the entry point.
