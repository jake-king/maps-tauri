# Testing, CI and releases

## The gate

`tools/check.sh` is the one command that must pass before a commit, and CI runs the same script. Each
step runs under a time limit (and, on the dev box, a memory cap):

| step | what it holds |
|---|---|
| rustfmt, clippy | formatting; clippy with every warning an error |
| rustdoc | the API docs build with every rustdoc warning fatal |
| docs-site | this book and the API reference build, and every link in the book resolves |
| eslint, ruff, shellcheck | the JavaScript, Python tools and shell scripts |
| palette-fresh | the generated palette matches a fresh run of its generator |
| cargo-test, js-test | the Rust unit tests and doctests; the frontend's pure modules under Node |
| decoder-diff | every tile decoded by Rust and by `@mapbox/vector-tile`, compared ([The native renderer](renderer.md)) |
| mesh-westla | every tile of the larger extract meshed under a time limit, so a mesher that loops forever fails here |
| search-parity | the port against the production search service ([Search](search.md)) |

The whole-data steps skip, saying why, when their data is absent (it is not in the repository).

A check is trusted only after it has been seen to fail. Each gate step, and most test groups, were
proven by planting a defect and watching it go red; a planted defect that survives is treated as a
finding about the test. When coverage was first measured (`tools/coverage.sh`, nightly `llvm-cov`),
Rust unit tests covered 49% of lines. The tests written to close that gap took four files to 99-100%,
caught 69 of 72 planted defects (the other three are equivalent or unreachable), and found seven real
bugs, among them a corrupt PMTiles directory that panicked a debug build and wrapped silently in
release.

### Documentation is part of the gate

Every crate inherits the workspace's lints: rustc's `missing_docs`; clippy's `missing_errors_doc`,
`missing_panics_doc`, `doc_markdown` and `too_long_first_doc_paragraph`; rustdoc's broken-link,
bare-URL and code-block lints. So every public item is documented, every function returning a
`Result` says when it fails, and the first paragraph is a summary rustdoc's index can show. Before the
lints, 56 of 165 public items in `vtile` and 79 of 153 in `wx` were documented; now all are. Writing
the missing docs from the code found five existing docs that were wrong. Examples on key functions
(`parse_coords`, the PMTiles tile ids, the MVT geometry decoder, the radar codes) are doctests, so
they run with the unit tests and cannot drift from the code.

## End to end

The app drives itself: `MAPS_BENCH=1` runs a scripted session and reports one line of results, which
`tools/e2e_check.py` judges (plain Python, so it runs on Windows). With the weather tape replayed, the
run needs no network, and every platform draws the same data. `python3 tools/e2e.py --tape
fixtures/tape` runs it anywhere; on Linux `--xvfb` gives it a virtual display. Speed limits apply only
on the dev box; CI runners draw WebGL in software, so CI checks correctness. The tape and the map
archive used are pinned by sha256 (`tools/fixtures.lock`).

Running this in a clean Ubuntu container found a real race the dev box's speed had hidden ([Weather](weather.md)),
and a hang in `xvfb-run`'s ready signal, now avoided by starting Xvfb and waiting on its
`-displayfd` pipe.

## CI

Development happens on a self-hosted Forgejo forge, whose CI runs on every branch push: the gate,
then the Linux end-to-end leg, each in a bare Ubuntu 24.04 container. The job logic lives in
`tools/forge-job.sh`, so a job runs the same way in a local container (`tools/ci-local.sh`). The
runner keeps rustup, cargo, npm's cache and the build tree between jobs, so with the cache warm the
gate takes about two minutes and the end-to-end leg about three, including the release build.

Tauri draws with each platform's own web view, and WebGL2 is where engines differ most, so the GitHub
workflows in `.github/` run the same end-to-end check on Linux, macOS and Windows and compare each
platform's snapshots with Linux's in 8 px blocks; a job also builds for the iOS simulator.

## Releases

On the forge, a version tag runs the gate, builds the Debian package and publishes it as a release
with its SHA256SUMS, and the API reference and this book with it (`tools/forge-job.sh release`). The tag must name the version in `tauri.conf.json` (`v0.1.0`; `v0.1.0-rc.1` publishes a
prerelease), and that is checked before any setup, so a wrong tag fails in seconds. The release is
created as a draft, filled, then published, so a failed upload never leaves a public release with half
its files.
