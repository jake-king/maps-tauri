# maps-tauri

An experiment: the Datastrike offline map viewer (MapLibre GL over a PMTiles basemap, with a
GeoNames search box) rebuilt as a Tauri 2 desktop app, with its search-box parsing ported from
Python to Rust and compiled to WebAssembly for the frontend. Built to learn Tauri.

## Layout
- `crates/coords`: the coordinate parser, name folding and FTS5 query builder, ported from
  `Infrastructure/containers/epoch/maps/gazetteer/serve.py`. Native for the backend, WASM (`--features wasm`)
  for the frontend.
- `src-tauri`: the Tauri backend.
- `src`, `index.html`: the frontend (Vite).
- `tools/diff.py`: the differential test, Rust port against the Python original.

## Run
Needs `wasm-pack` on PATH (`~/.cargo/bin`).

    npm install
    npx tauri dev                 # builds the WASM module, then opens the app
    cargo test -p coords          # the parser's test tables, ported from the Python selftest
    npm run diff                  # Rust vs Python over ~190k inputs; exit 0 = no unexplained disagreement

## Known differences from the Python
`tools/diff.py` lists them on every run:
- The port reads ASCII digits only. Python's `\d` and `float()` also accept `١٢` or `１２`, so
  the port refuses some inputs Python reads as coordinates. Refusing sends the text to the
  gazetteer, the safe direction by `serve.py`'s own rule.
- `fts_query` keeps combining marks (Indic vowel signs, Arabic harakat) inside words where Python
  splits at them. FTS5's `unicode61` tokenizer re-splits the phrase, so both find the same rows.
