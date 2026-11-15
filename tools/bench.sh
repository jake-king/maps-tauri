#!/usr/bin/env bash
# Run the pan benchmark once and print a summary. Uses the Vite dev server on :1420 if one is
# running, else starts one for the run and stops its whole process group afterwards.
#   tools/bench.sh [label]          env such as MAPS_CACHE_ZOOMS=20 passes through to the app
# The full JSON goes to bench/<date>-<label>.json.
set -euo pipefail
cd "$(dirname "$0")/.."
label=${1:-run}
out="bench/$(date +%F)-$label.json"
log="logs/bench-$label.log"
cargo build -q -p maps-tauri

if ! ss -ltn | grep -q ':1420 '; then
  npm run -s wasm >/dev/null
  setsid npx vite >logs/bench-vite.log 2>&1 &
  vite_pgid=$!
  trap 'kill -TERM -- "-$vite_pgid" 2>/dev/null || true' EXIT
  for _ in $(seq 100); do ss -ltn | grep -q ':1420 ' && break; sleep 0.1; done
  ss -ltn | grep -q ':1420 ' || { echo "vite did not start; see logs/bench-vite.log" >&2; exit 1; }
fi
# timeout runs INSIDE the scope, as the app's parent, so a hung run cannot outlive it.
MAPS_BENCH=1 systemd-run --user --scope -q -p MemoryMax=10G timeout 180 ./target/debug/maps-tauri >"$log" 2>&1 || true
python3 tools/bench_summary.py "$log" "$label" "$out"
