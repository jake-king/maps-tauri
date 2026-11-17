#!/usr/bin/env bash
# The weather layer end to end, on the release build, against NOAA (needs the internet, so it is not
# part of tools/check.sh): both layers on, every radar frame on the GPU and the hazards loaded,
# then the same 3 hours back, then these checks:
#   - both times fully loaded (radar loop, satellite image, hazards), nothing from after the chosen time,
#     and the flight-level filter narrowing the hazards
#   - the wind trails through a pan and a wheel zoom (a second run): kept, not started again
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
# the METAR history file sits beside it: start without one, so "nothing collected yet" is certain
rm -f "$db" "$db-wal" "$db-shm" logs/metars.sqlite logs/metars.sqlite-wal logs/metars.sqlite-shm
MAPS_ARCHIVE=westla.pmtiles MAPS_LOCAL_ONLY=1 MAPS_BENCH=1 MAPS_F4=0 MAPS_RENDERER=rust MAPS_WEATHER=1 MAPS_WEATHER_CACHE=$db \
  systemd-run --user --scope -q -p MemoryMax=10G timeout 200 ./target/release/maps-tauri >logs/weather-e2e.log 2>&1 || true
python3 tools/e2e_check.py weather logs/weather-e2e.log

# the trails through a pan and a wheel zoom: they must stay, not start again with every move
MAPS_ARCHIVE=westla.pmtiles MAPS_LOCAL_ONLY=1 MAPS_BENCH=1 MAPS_F4=0 MAPS_RENDERER=rust MAPS_WIND_MOTION=1 MAPS_WEATHER_CACHE=$db \
  systemd-run --user --scope -q -p MemoryMax=10G timeout 120 ./target/release/maps-tauri >logs/weather-wind-motion.log 2>&1 || true
python3 tools/e2e_check.py wind logs/weather-wind-motion.log
