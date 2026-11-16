#!/usr/bin/env bash
# The weather layer end to end, on the release build, against NOAA (needs the internet, so it is not
# part of tools/check.sh): both layers on, every radar frame on the GPU and the hazards loaded,
# then two checks:
#   - registration: a synthetic one-cell frame at Santa Monica Pier must land where the map's own
#     projection puts the pier, within half a cell, and be one cell wide and 1/cos(lat) cells tall
#   - nothing reported an error, and real data arrived (frames, hazards)
#   tools/weather.sh             build, then run
#   SKIP_BUILD=1 tools/weather.sh
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
if [[ ${SKIP_BUILD:-0} != 1 ]]; then
  systemd-run --user --scope -q -p MemoryMax=12G npx tauri build --no-bundle >logs/weather-build.log 2>&1 \
    || { tail -20 logs/weather-build.log >&2; exit 1; }
fi
db=logs/weather-e2e.sqlite
rm -f "$db" "$db-wal" "$db-shm"
MAPS_ARCHIVE=westla.pmtiles MAPS_LOCAL_ONLY=1 MAPS_BENCH=1 MAPS_RENDERER=rust MAPS_WEATHER=1 MAPS_WEATHER_CACHE=$db \
  systemd-run --user --scope -q -p MemoryMax=10G timeout 200 ./target/release/maps-tauri >logs/weather-e2e.log 2>&1 || true
python3 - logs/weather-e2e.log <<'PY'
import json, math, re, sys
log = open(sys.argv[1], encoding="utf-8").read().split("\n")
bad = [l for l in log if re.match(r"\[frontend\] (uncaught error|unhandled rejection|map error|csp violation)", l)]
bench = [l for l in log if l.startswith("[bench] ")]
fail = []
if bad:
    fail += bad[:5]
if not bench:
    sys.exit("weather: FAILED: no [bench] line (logs/weather-e2e.log)")
r = json.loads(bench[0][8:])
w, p = r["weather"], r["probe"]
if w["errors"]:
    fail.append(f"fetch errors: {w['errors']}")
if w["frames"] < 1 or w["hazards"] < 1:
    fail.append(f"no data: {w['frames']} frames, {w['hazards']} hazards")
cell = p.get("cell_css") or 0
if not p.get("changed"):
    fail.append("probe: the lit cell drew nothing")
else:
    dx, dy = p["off_by_css"]
    bw, bh = p["box_css"]
    tall = cell / math.cos(math.radians(34.0105))
    if abs(dx) > cell / 2 or abs(dy) > tall / 2:
        fail.append(f"probe: cell drawn {dx}, {dy} px from the pier (half a cell is {cell / 2:.0f} x {tall / 2:.0f})")
    if abs(bw - cell) > 2 or abs(bh - tall) > 2:
        fail.append(f"probe: cell drawn {bw} x {bh} px, expected {cell} x {tall:.0f}")
print(f"weather: {w['frames']} frames ({w['gpu_bytes'] / 1e6:.0f} MB on the GPU), {w['hazards']} hazard areas, ready in {r['wait_ms'] / 1000:.1f} s; probe {p}")
if fail:
    print("weather: FAILED:\n  " + "\n  ".join(fail))
    sys.exit(1)
print("weather: all passed")
PY
