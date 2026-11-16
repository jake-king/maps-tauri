#!/usr/bin/env bash
# The weather layer end to end, on the release build, against NOAA (needs the internet, so it is not
# part of tools/check.sh): both layers on, every radar frame on the GPU and the hazards loaded,
# then the same 3 hours back, then these checks:
#   - both times fully loaded (radar loop, satellite image, hazards), nothing from after the chosen time,
#     and the flight-level filter narrowing the hazards
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
MAPS_ARCHIVE=westla.pmtiles MAPS_LOCAL_ONLY=1 MAPS_BENCH=1 MAPS_F4=0 MAPS_RENDERER=rust MAPS_WEATHER=1 MAPS_WEATHER_CACHE=$db \
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
w, past, p = r["weather"], r["past"], r["probe"]
for name, x in (("live", w), ("3 h back", past)):
    if x["errors"]:
        fail.append(f"{name}: fetch errors: {x['errors']}")
    if x["last_error"]:
        fail.append(f"{name}: the layer reported: {x['last_error']}")
    sel = x["selection"] or {}
    if len(sel.get("radar") or []) < 4:
        fail.append(f"{name}: radar loop of {len(sel.get('radar') or [])} frames")
    if x["shown_sat"] is None:
        fail.append(f"{name}: no satellite image drawn")
    if x["hazards"] < 1:
        fail.append(f"{name}: no hazards")
if any(t > r["back"] for t in past["selection"]["radar"]):
    fail.append(f"3 h back: a radar frame from after the chosen time: {past['selection']['radar']} > {r['back']}")
if past["selection"]["satellite"] is None or past["selection"]["satellite"] > r["back"]:
    fail.append(f"3 h back: satellite {past['selection']['satellite']} is not at or before {r['back']}")
lv = r["levels"]
if not (lv["all"] >= lv["180"] and lv["all"] > min(lv["100"], lv["180"], lv["340"])):
    fail.append(f"the flight-level filter changed nothing: {lv}")
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
print(f"weather: live ready in {r['live_ms'] / 1000:.1f} s, 3 h back in {r['past_ms'] / 1000:.1f} s; GPU {past['gpu_bytes'] / 1e6:.0f} MB; hazards by level {lv}; probe {p}")
if fail:
    print("weather: FAILED:\n  " + "\n  ".join(fail))
    sys.exit(1)
print("weather: all passed")
PY
