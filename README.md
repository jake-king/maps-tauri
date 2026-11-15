# maps-tauri

An experiment: the Datastrike offline map viewer (MapLibre GL over a PMTiles basemap, with a
GeoNames search box) rebuilt as a Tauri 2 desktop app, with its search-box parsing ported from
Python to Rust and compiled to WebAssembly for the frontend. Built to learn Tauri.

## Layout
- `crates/coords`: the coordinate parser, name folding and FTS5 query builder, ported from
  `Infrastructure/containers/epoch/maps/gazetteer/serve.py`. Native for the backend, WASM (`--features wasm`)
  for the frontend.
- `src-tauri`: the Tauri backend. `tiles.rs` serves the archive over a `tiles://` URI scheme with
  HTTP Range semantics, off the UI thread; only listed file names are reachable.
  `search.rs` is `serve.py`'s `search()` over the same GeoNames FTS5 index with rusqlite, exposed as
  the `search` command. `build.rs` declares the app's commands so each needs a capability grant
  (`capabilities/default.json` grants exactly `report` and `search`).
- `src`, `index.html`: the frontend (Vite). The search box answers coordinates in WASM with no
  round trip, and sends anything else to `search`.
- `tools/diff.py`: the differential test, Rust port against the Python original.

## Data
`tools/fetch-data.sh` pulls everything from the Datastrike maps service on the LAN, never the
internet: an LA cutout of the planet archive (6.2 MB, by HTTP Range with `go-pmtiles extract`),
the Protomaps glyphs and sprites, and with `--gazetteer` the production GeoNames index (1.6 GB).

## Run
Needs `wasm-pack` in `~/.cargo/bin` and `go-pmtiles` in `~/go/bin`.

    npm install
    tools/fetch-data.sh --gazetteer
    npx tauri dev                 # builds the WASM module, then opens the app
    cargo test -p coords          # the parser's test tables, ported from the Python selftest
    npm run diff                  # Rust vs Python over ~190k inputs; exit 0 = no unexplained disagreement
    npm test                      # the overlay's pure layout and statistics
    MAPS_BENCH=1 target/debug/maps-tauri   # the pan benchmark against a running `npm run dev`
    tools/search_diff.py          # the whole search path vs serve.py on the real index, feature by feature

## Dev tools
- **F4**: the profiler overlay. A row per measurement (average, worst over the last 120 frames,
  a bar against one frame): frame interval, MapLibre's render CPU time, tile fetch round trip
  through `tiles://`, the Rust handler's own time, and tile request-to-decoded time. Then frames
  drawn with tiles still missing (the gray boxes), memory per process from `/proc` (WebKitGTK
  renders in separate processes), and a 120-frame graph with the budget line. Drag it anywhere.
- **F6**: the pan benchmark, a fixed fast pan at zoom 15 run cold then warm. `MAPS_BENCH=1` runs
  it at startup, prints one `[bench]` JSON line and exits. Results to compare against live in
  `bench/`.

Benchmark switches (environment variables, no rebuild needed with `SKIP_BUILD=1`):
`MAPS_CACHE_ZOOMS`, `MAPS_WORKERS`, `MAPS_PREFETCH`, `MAPS_FADE_MS`, `MAPS_WARM_ZOOM`,
`MAPS_NO_LABELS=1`, `MAPS_CACHED_PARENTS=0`, `MAPS_TILE_DELAY_MS` (a control: slows every tile),
and `MAPS_PIXEL_CHECK=1` (magenta background, counts it in every frame: coverage truth, timings
invalid). The investigation and every result are written up in `docs/JOURNAL.md`.

## MapLibre patch
`src/maplibre-patch.js` makes MapLibre draw a loaded parent from its cache while a newly visible
tile decodes, instead of the background (gray boxes in 30% of first-visit frames before, 0%
after). It overrides a private method, so it applies only to MapLibre 6.7.0. See the journal.

## Known differences from the Python
The full search path matches `serve.py` exactly: 156 queries, 896 features, identical.

`tools/diff.py` lists them on every run:
- The port reads ASCII digits only. Python's `\d` and `float()` also accept `١٢` or `１２`, so
  the port refuses some inputs Python reads as coordinates. Refusing sends the text to the
  gazetteer, the safe direction by `serve.py`'s own rule.
- `fts_query` keeps combining marks (Indic vowel signs, Arabic harakat) inside words where Python
  splits at them. FTS5's `unicode61` tokenizer re-splits the phrase, so both find the same rows.
