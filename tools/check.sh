#!/usr/bin/env bash
# The project's gate: everything that must hold before a commit, in one command, each step under a
# time limit and a memory cap. Exit status is 0 only if every step passed.
#   tools/check.sh            all steps
#   tools/check.sh --fast     skip the whole-data checks (decoder diff, archive mesh check, search parity)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
fast=0
[[ ${1:-} == --fast ]] && fast=1
failed=()
# Each step runs in its own memory-capped scope where a user systemd session exists (the dev box);
# a CI runner has none and is thrown away after the job, so there the cap is skipped.
cap=()
if systemd-run --user --scope -q true 2>/dev/null; then cap=(systemd-run --user --scope -q -p MemoryMax=10G); fi
run() {
  local name=$1 limit=$2
  shift 2
  printf '%-34s ' "$name"
  local t0=$SECONDS
  if "${cap[@]}" timeout "$limit" "$@" >"logs/check-$name.log" 2>&1; then
    echo "ok ($((SECONDS - t0)) s)"
  else
    echo "FAILED (exit $?; logs/check-$name.log)"
    failed+=("$name")
  fi
}
skip() { printf '%-34s skipped: %s\n' "$1" "$2"; }
mkdir -p logs

# the fuzz targets are a workspace of their own (fuzz/), so cargo fmt --all does not reach them
run rustfmt 120 sh -c 'cargo fmt --all -- --check && rustfmt --edition 2021 --check fuzz/fuzz_targets/*.rs'
run clippy 900 cargo clippy --workspace --all-targets --all-features -q -- -D warnings
# the API docs build with every rustdoc warning fatal (the lints are in the workspace Cargo.toml)
# the dependency tree against deny.toml: licenses, banned crates, sources. Offline: the advisory
# databases change daily, so advisories are CI's `audit` job, not a verdict that moves under a commit.
# cargo-deny reads the crates of every platform deny.toml names (windows-sys, Android's), which no
# build here downloads: on a fresh CARGO_HOME --offline failed on them until they were fetched first
run deps 300 sh -c 'cargo fetch --locked -q && cargo deny --offline check licenses bans sources'
# a dependency no code uses, in both workspaces (cargo-machete reads the sources, not a build). It
# sees [dependencies] only: an unused dev- or build-dependency passes (its own TODO, 0.10.1)
run unused-deps 60 cargo machete
# secrets in every commit and in the working tree about to be committed (gitleaks, .gitleaks.toml)
run secrets 120 tools/secrets.sh
run rustdoc 900 env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked -q
# the book builds without a warning, and the site it is published as has every API page it links to
run docs-site 300 tools/docs.sh
run eslint 300 npx eslint .
run ruff 120 sh -c 'ruff check tools/*.py && ruff format --check tools/*.py'
run shellcheck 120 sh -c 'shellcheck tools/*.sh'
# generated files must match a fresh run of their generator (a hand edit or a stale file fails here)
# The generator reads the sprite sheet from public/assets, which is not in the repository.
if [[ -s public/assets/sprites/v4/light.json ]]; then
  # shellcheck disable=SC2016 # $d is meant to expand in the inner sh, not here
  run palette-fresh 120 sh -c 'd=$(mktemp -d) && cp crates/vtile/src/palette.rs src/rust-renderer/palette.json "$d"/ &&
    node tools/gen_palette.mjs >/dev/null &&
    cmp crates/vtile/src/palette.rs "$d"/palette.rs && cmp src/rust-renderer/palette.json "$d"/palette.json'
else
  skip palette-fresh "no public/assets/sprites (tools/fetch-data.sh)"
fi

run cargo-test 900 cargo test -q --workspace
run js-test 300 npm test --silent

if ((fast)); then
  skip whole-data "--fast"
else
  if [[ -s data/basemap.pmtiles ]]; then
    run decoder-diff 600 node tools/mvt_diff.mjs data/basemap.pmtiles
  else
    skip decoder-diff "no data/basemap.pmtiles (tools/fetch-data.sh)"
  fi
  if [[ -s data/westla.pmtiles ]]; then
    # every tile of the larger area meshed, under a time limit: an infinite loop on a wiggly road
    # (the canyon bug) fails here instead of in the app
    run mesh-westla 300 sh -c 'cargo build -q --release -p vtile --example meshcheck && timeout 200 ./target/release/examples/meshcheck data/westla.pmtiles'
  else
    skip mesh-westla "no data/westla.pmtiles (tools/fetch-data.sh)"
  fi
  if [[ -s data/geonames.db && -d ../Infrastructure ]]; then
    run search-parity 600 python3 tools/search_diff.py
  else
    skip search-parity "no data/geonames.db or ../Infrastructure"
  fi
fi

if ((${#failed[@]})); then
  echo "check: FAILED: ${failed[*]}"
  exit 1
fi
echo "check: all passed"
