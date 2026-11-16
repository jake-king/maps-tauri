#!/usr/bin/env bash
# The page may reach only its own assets, the Rust backend and tiles:// (the content security policy
# in src-tauri/tauri.conf.json). This runs the release build through a stress run on each renderer
# and the search box end to end, and fails if the policy refused anything: a refused request means
# something tried to leave the app. A control run makes one request the policy must refuse and fails
# unless exactly that one is reported, so a policy that stopped being applied cannot pass silently.
#   tools/offline.sh          build, then check
#   SKIP_BUILD=1              reuse target/release/maps-tauri
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  systemd-run --user --scope -q -p MemoryMax=12G npx tauri build --no-bundle >logs/offline-build.log 2>&1 \
    || { tail -20 logs/offline-build.log >&2; exit 1; }
fi
export MAPS_ARCHIVE=${MAPS_ARCHIVE:-westla.pmtiles} MAPS_LOCAL_ONLY=1 MAPS_BENCH=1
fail=0
app() { # name, then env assignments
  local name=$1
  shift
  env "$@" systemd-run --user --scope -q -p MemoryMax=10G timeout 180 ./target/release/maps-tauri >"logs/offline-$name.log" 2>&1 || true
}
# what a run must not print, whatever else it got right: checked before any result is looked at
ERRORS='^\[frontend\] (uncaught error|unhandled rejection|search failed|map error)'
violations() { grep -c '^\[frontend\] csp violation: ' "logs/offline-$1.log" || true; }
expect() { # name, wanted violation count, what the run must have produced (a [bench] line pattern)
  local name=$1 want=$2 pattern=$3 got
  got=$(violations "$name")
  if grep -qE "$ERRORS" "logs/offline-$name.log"; then
    echo "$name: FAILED: the run reported errors:"
    grep -E "$ERRORS" "logs/offline-$name.log" | head -5
    fail=1
  elif ! grep -q "^\[bench\] .*$pattern" "logs/offline-$name.log"; then
    echo "$name: FAILED: no [bench] line matching '$pattern' (logs/offline-$name.log)"
    fail=1
  elif [ "$got" != "$want" ]; then
    echo "$name: FAILED: $got policy violations, expected $want:"
    grep '^\[frontend\] csp violation: ' "logs/offline-$name.log" | sort | uniq -c | head -5
    fail=1
  else
    echo "$name: ok ($got violations)"
  fi
}

app stress-rust MAPS_RENDERER=rust MAPS_STRESS=40
expect stress-rust 0 '"csp_violations":0'
app stress-maplibre MAPS_RENDERER=maplibre MAPS_STRESS=40
expect stress-maplibre 0 '"csp_violations":0'
app search-name MAPS_RENDERER=rust MAPS_SEARCH="santa mon"
expect search-name 0 '"box":{"list":"none","padding":"10px"}.*"credit":{"inside":true,"text":"© OpenStreetMap contributors, Protomaps"}.*"results":\["Santa Monica'
app search-coords MAPS_RENDERER=rust MAPS_SEARCH="34.0105, -118.4912"
expect search-coords 0 '"results":\["34'

# control: the probe's one request must be refused and reported, and nothing else
app control MAPS_RENDERER=rust MAPS_SEARCH="santa mon" MAPS_CSP_PROBE=1
expect control 1 '"results":\["Santa Monica'
grep -q '^\[frontend\] csp violation: connect-src http://192.0.2.1/csp-probe$' logs/offline-control.log \
  || { echo "control: FAILED: the probe's request was not the one reported"; fail=1; }

exit $fail
