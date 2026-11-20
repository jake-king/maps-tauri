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
finding about the test. When coverage was first measured (`tools/coverage.sh`, `llvm-cov`), Rust
unit tests covered 49% of lines. The tests written to close that gap took four files to 99-100%,
caught 69 of 72 planted defects (the other three are equivalent or unreachable), and found seven real
bugs, among them a corrupt PMTiles directory that panicked a debug build and wrapped silently in
release.

### Coverage

`tools/coverage.sh` measures line coverage of the Rust unit and integration tests (source-based,
`-C instrument-coverage`, read with `llvm-cov` from the gate's own toolchain) and of the frontend's
Node tests. CI runs it on every push as its own job, and fails if either total falls below the floor
in `tools/coverage-floor.txt`, which only ever moves up. When this was written: Rust 80.6% of lines,
JavaScript 73.5%. The figure understates what is exercised: the decoder diff, the whole-archive mesh
check, search parity and the end-to-end tape are not instrumented. The files at 0% are the app's
start-up, the network fetchers, the WebAssembly bindings and the generated palette.

The [coverage report](coverage/index.html), line by line, is published with each release beside this
book.

An earlier version of the script found its test binaries by listing the build directory, which also
held binaries from older builds. `llvm-cov` then reported "mismatched data" and dropped the affected
functions: four files read 31-60% instead of 63-100%. It now takes the binaries
cargo reports for the build it just ran.

### Mutation testing

Coverage says a line ran, not that a test would notice it being wrong. `cargo-mutants` checks the
second: it makes one small deliberate bug at a time (an operator swapped, a function body replaced by
a default value, a match arm deleted), runs the tests, and lists each mutant they do not catch.
`tools/mutants.sh` runs it with the settings in `.cargo/mutants.toml`, which skip the generated
palette, the WebAssembly bindings, the app's start-up and the network fetcher, whose tests would be
the network.

The whole tree is about 2,600 mutants, hours of work, so CI splits it. Every push mutates only the
lines that push changed, in place on the warm build tree, and fails if any of those mutants survives:
new or edited code arrives with tests that would catch it breaking. A weekly workflow
(`.forgejo/workflows/mutants.yml`, or run by hand) mutates everything, in four shards one after
another because the whole is longer than the runner allows one job, and fails if more mutants are
missed than `tools/mutants-baseline.txt` records; that number only goes down.

The first full run missed 516 of 2,461 viable mutants; with every feature compiled, the next missed
467 of 2,451, the current baseline. Most are in the mesh builder's arithmetic,
which the gate's whole-archive step runs (for hangs and panics) but no test checks, and in the app's
glue to NOAA and its debug overlay.

The first trial, on the timeline, found four surviving mutants: no test had a radar frame nearer
after its slot than before it, an exact tie between two frames, or two hazard snapshots in reach at
different distances. Two tests now pin those, and all 35 viable mutants there are caught.

### Fuzzing

Most of what the app parses comes from elsewhere: map tiles and archive directories over HTTP from a
server it does not run, GRIB2 and radar files, text and JSON from NOAA's services, and whatever is
typed in the search box. Each parser has a coverage-guided fuzz target (`fuzz/`, cargo-fuzz and
libFuzzer): the PMTiles archive read path, PMTiles directories, MVT decoding with meshing, GRIB2,
radar decompression, the weather services' text formats, and the search box. The fuzzer starts from
real inputs taken from the recorded tape and map (`tools/fuzz.sh seeds`), mutates them, and keeps
every input that reaches new code. The targets build with overflow checks on, so arithmetic that a
release build would silently wrap is a crash.

CI fuzzes each target for 30 seconds on every push, from a corpus kept between runs, and for 10
minutes each weekly, after which the corpus is minimised. A crash fails the job and prints the
input. Once fixed, the bug gets a unit test, and a small input also goes in `fuzz/regressions/`,
which every run replays.

The first hour found three real bugs, all integer overflows on values read from the input: the
archive's tile-data offset plus an entry's offset (a release build would have read from a wrapped
offset), a tile id past zoom 31 in `tile_id_to_zxy`, and coordinates near `i32::MAX` in the mesh
builder's road-shield placement. The same class was then fixed wherever else the reader added two
of the file's numbers.

The first weekly run found a hang: a fuzzed landuse polygon (a ring, a hole, and 27 one-point
holes) on which the triangulation crate, `earcutr`, never returned. A tile like that from a server
would have spun a render thread forever. The libFuzzer timeout (10 seconds an input) is what makes
a hang a finding at all. The mesher now uses the maintained `earcut` crate, which also triangulates
two polygons exactly that `earcutr` got 0.5% wrong, and is 20% faster.

The fuzzer was checked the way any check is: a bug planted in the directory parser. The archive
target missed it for ten minutes, because directories are gzipped inside the file and nearly every
mutation broke the checksum before the parser ran. A target that feeds the directory parser
directly found the same bug in eight seconds.

### Performance

A change that makes the renderer or a decoder slower passes every test, so speed has its own check.
Benchmarks (`crates/vtile/benches`, `crates/wx/benches`) cover the work done per tile and per
weather update: decoding a vector tile and meshing it at three zooms, parsing the archive's root
directory and looking up tiles in it, a radar frame through decompression, GRIB2 decoding and
thinning, and a GFS wind field. Their inputs are the end-to-end fixtures.

They count instructions under Valgrind's Callgrind (gungraun) instead of timing. CI runs on a shared
machine, where a wall-clock benchmark measures whatever else is running. A count repeats to the
instruction for the decoders and within 0.03% for the mesher, so `tools/perf.sh` can compare a
branch with its merge base on `main`, both measured in the same job, and fail on any benchmark that
rose by more than 3%. A count does not see cache misses, memory bandwidth or vector width, so it
catches algorithmic regressions (lost buffer reuse, an extra pass, a worse search), not every kind
of slowdown. A slowdown that is the price of a change is accepted in writing: a commit trailer
`Perf-accepted: <why>`, printed with the result.

The gate was checked against planted regressions. Rebuilding the triangulator's scratch buffers for
every polygon raised the street-level mesh by 7.4% and failed it; the trailer let the same change
through; unchanged code passed. The first plant, a linear search in place of the directory's binary
search, made that benchmark 29% faster: the root directory is small and is searched in order. And
the first version of the gate compared the base with itself: the two builds shared a target
directory, cargo gave both the same artifact names, and its freshness check, reading the paths the
base build recorded, never rebuilt the head. The base now builds apart, and the gate rebuilds a head
whose dependency lists name the base tree.

### Rust version

`rust-version = "1.90"` in the workspace `Cargo.toml` is a promise that the project builds with Rust
1.90, and a CI job, `msrv` (`tools/msrv.sh`), keeps it: it installs that Rust and checks the whole
workspace (every target and feature) and the WebAssembly crate with it. The gate itself runs the
newer Rust that `rust-toolchain.toml` pins, so without the job a newer API or syntax would pass every
check and break only for someone on an older compiler. 1.90 is the floor the dependencies set (Tauri
2.12 and its runtime declare it); the project's own code asks for nothing newer. The number moves on
purpose, with a reason in the commit.

Two layers catch a slip. Clippy's `incompatible_msrv` lint, in the gate, reads `rust-version` and
flags a standard-library item stabilized after it, before the commit. It knows nothing of language
features, and the CI job, which compiles with 1.90 itself, catches both. Each was checked with
`std::iter::chain` (stable since 1.91) planted in the search crate: the lint failed clippy, and the
job failed with exit 1 naming the line. A member that does not inherit `rust-version` fails the job
with exit 2.

### Dependencies

The app is a few thousand lines of its own on 458 crates and a few hundred npm packages, so most of
the code it ships was written elsewhere. `deny.toml` says what that code may be: licenses from a
list (permissive, plus MPL-2.0, which is copyleft per file and reached through Tauri's HTML parser),
crates from crates.io only, no OpenSSL beside rustls. The gate checks it offline with `cargo-deny`.
CI's `audit` job checks the lock files against the RustSec and npm advisory databases, fetched fresh,
on every push and every day, since a new advisory concerns code nobody changed.

A dependency nothing uses still costs build time, attack surface and audit noise. The gate's
`unused-deps` step runs cargo-machete over both workspaces (the app's and `fuzz/`): it reads the
sources for each `[dependencies]` entry, without building, and fails on one never named. It does not
look at `[dev-dependencies]` or `[build-dependencies]` (a TODO in its own source), and it counts a
crate named anywhere in the package, so a normal dependency used only by a test passes as used.
Checked red with unused dependencies planted in `coords`, `wx` and the fuzz workspace; a plant in each of the
two unchecked sections passed, which is how the gap was found.

An advisory with no fix can be accepted, with a reason in writing: in `deny.toml` for Rust (an
unmaintained macro crate deep in the GTK bindings), in `tools/npm-audit-allow.json` for npm (empty
now). npm has no such list of its own, so `tools/npm_audit.py` keeps one, and each entry has a date
after which it fails again, so an accepted risk gets looked at again rather than forgotten. An entry
that no longer matches an advisory fails too: when GitHub withdrew the picomatch ReDoS advisory
(GHSA-fc5q-39xc-vjpf) that npm's list had carried, the next audit failed until the entry was removed. The first run also found a KaTeX advisory
in the Mermaid that draws this book's diagrams; Mermaid 12 fixed it.

### Secrets

The repository now handles a signing key and CI tokens, so the gate looks for secrets before they
reach history, where removing one means rewriting it. `tools/secrets.sh` runs gitleaks, with its
default rules (`.gitleaks.toml`), over every commit of the checkout and over the working tree as a
commit would take it: tracked files plus untracked ones git does not ignore. The second scan matters
because the gate runs before the commit, and a history scan sees only what is already committed. A
finding names the file, line and rule, with the secret itself redacted. A missing gitleaks, or one
that is not the pinned version, fails the step rather than skipping it. CI installs the release
binary against its published SHA-256.

Checked in the CI image on a clone, with random GitHub-token-shaped strings: found in an untracked
file, in a staged file, and in a commit whose file a later commit deleted; not reported under the
ignored `logs/`; and the step exits 2 when gitleaks is absent.

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

```mermaid
flowchart LR
  push["branch push"] --> check["check<br/>tools/check.sh"]
  check --> e2e["linux-e2e<br/>release build, recorded tape, Xvfb"]
  check --> cov["coverage<br/>llvm-cov and Node, with a floor"]
  e2e --> verdict["verdict<br/>passes only if every job passed;<br/>main's required check"]
  check --> mut["mutants<br/>on the changed lines"]
  check --> audit["audit<br/>RustSec and npm advisories"]
  check --> fuzz["fuzz<br/>7 parser targets, 30 s each"]
  check --> perf["perf<br/>instruction counts vs main, +3% fails"]
  check --> msrv["msrv<br/>builds with rust-version (1.90)"]
  msrv --> verdict
  fuzz --> verdict
  perf --> verdict
  audit --> verdict
  cov --> verdict
  mut --> verdict
  check --> verdict
  week["weekly"] --> mfull["mutants, 4 shards<br/>every mutant, against a baseline"]
  tag["tag v*"] --> vcheck["release: tag names the version,<br/>and its commit is on main?"]
  vcheck --> rgate["gate, .deb, coverage, docs site"]
  rgate --> sign["SBOMs, SHA256SUMS signed and checked"]
  sign --> draft["draft release<br/>.deb, docs, SBOMs, signed SHA256SUMS"]
  draft --> pub["published"]
```

Development happens on a self-hosted Forgejo forge, whose CI runs on every branch push: the gate,
then the Linux end-to-end leg, the coverage, mutation, audit, fuzz, performance and Rust-version jobs side by side, each in a bare Ubuntu 24.04
container. The job logic lives in
`tools/forge-job.sh`, so a job runs the same way in a local container (`tools/ci-local.sh`). The
runner keeps rustup, cargo, npm's cache and the build tree between jobs, so with the cache warm the
gate takes about two minutes and the end-to-end leg about three, including the release build.

`main` is protected: nobody pushes to it, including the maintainer. A change arrives as a pull
request from a short-lived branch, merges only when CI's `verdict` job (which needs every other job)
is green on the branch's head and the branch is current with `main`, and needs one approval.
Changes are often prepared by an automation account, and the forge does not let an author approve
their own pull request, so that approval is a human reading the diff. Merges are fast-forward only,
so the commit CI tested is the commit on `main`, by hash. The weekly and daily workflows run from
`main`, the default branch.

Tauri draws with each platform's own web view, and WebGL2 is where engines differ most, so the GitHub
workflows in `.github/` run the same end-to-end check on Linux, macOS and Windows and compare each
platform's snapshots with Linux's in 8 px blocks.

## Releases

On the forge, a version tag runs the gate, builds the Debian package and publishes it as a release
with its SHA256SUMS, and the API reference and this book with it (`tools/forge-job.sh release`). The tag must name the version in `tauri.conf.json` (`v0.1.0`; `v0.1.0-rc.1` publishes a
prerelease), and the tagged commit must be on `main`, so a release is always something that went
through review and CI. Both are checked before any setup, so a wrong tag fails in seconds. Version
tags are protected too: only the maintainer can push one. The release is
created as a draft, filled, then published, so a failed upload never leaves a public release with half
its files.

Each release also says what it contains and proves where it came from. Two software bills of
materials (CycloneDX, `tools/sbom.sh`) list every crate and npm package in the build, so a later
advisory can be checked against a release without rebuilding it. SHA256SUMS lists every asset and is
signed with the project's key (`tools/sign-release.sh`); the signature is checked with the committed
`release-signing.pub` before anything is published, and a release without the key fails rather than
going out unsigned. The key is a minisign key made by Tauri's signer, the format Tauri's updater
checks, so the same key can later sign updates. To check a download:

```text
minisign -Vm SHA256SUMS -p release-signing.pub && sha256sum -c --ignore-missing SHA256SUMS
```

### Updates

An installed copy can update itself (`src-tauri/src/update.rs`, Tauri's updater plugin). A release
also publishes `latest.json`, a manifest naming the newest version and where its `.deb` is, and a
signature of that `.deb` by the release key (`tools/update-manifest.sh`). The app asks the manifest
when its window opens; if a newer version exists it shows a notice, and only the user's click
downloads it, checks the signature against the release key compiled into the app, installs it with
`dpkg` through the desktop's administrator prompt, and restarts.

The signature is the only thing trusted. The manifest comes over TLS but is not signed, so the
signature also names the version it was made for, and the app refuses a package whose signed
version differs from the one the manifest announces. Without that, a manifest could pair a new
version number with an older, genuinely signed release, and the app would install a downgrade.
The plugin compares the two whenever the signature names a version; `requireSignedVersion` (on)
also refuses a signature that names none, the kind older Tauri CLIs made, which could otherwise
be replayed under any version number.

Where the app looks is fixed at build time (`MAPS_UPDATE_URL`). The forge cannot be reached from
outside, so until the releases have a public home the variable is unset, release builds have no
updater, and they make no request for one; the manifest and signature are published anyway.

`tools/update-e2e.sh` checks the whole path as a user would meet it, weekly and on demand
(`update.yml`), in a fresh Ubuntu container: it builds 0.1.0 and 0.1.1 with a throwaway key,
installs 0.1.0 from its package, serves manifests over local HTTPS, and runs the installed app.

| Offered | Result |
|---|---|
| 0.1.1, signed for 0.1.1 | installed |
| the same package with one byte appended | refused: the signature fails |
| 0.1.1 signed by another key | refused: another key |
| announced as 0.1.2, the package signed for 0.1.1 | refused: the versions differ |
| announced as 0.1.2, the signature naming no version | refused: no signed version |
| 0.1.0 | nothing newer |

Switching `requireSignedVersion` off was the planted defect. The first version of the test passed
anyway: its only replay case used a signature that names a version, and the plugin compares those
whatever the setting. The unversioned case was added for that reason, and with the setting off it
failed the test, the old package installed as 0.1.2.
