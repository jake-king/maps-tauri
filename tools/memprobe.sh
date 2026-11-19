#!/usr/bin/env bash
# Per-process memory (PSS) of the release build at one stage: blank | idle. Env passes through.
#   tools/memprobe.sh blank      MAPS_WORKERS=1 tools/memprobe.sh idle
set -euo pipefail
cd "$(dirname "$0")/.."
# benchmarks run on the small reference cutout unless told otherwise, so results stay comparable
export MAPS_ARCHIVE=${MAPS_ARCHIVE:-basemap.pmtiles}
export MAPS_F4=${MAPS_F4:-0} # the dev overlay closed: open, it draws every frame and skews timings
export MAPS_LOCAL_ONLY=${MAPS_LOCAL_ONLY:-1} # never the remote planet unless asked
# MapLibre unless told otherwise (MAPS_RENDERER=rust): the earlier results in bench/ were taken on it
export MAPS_RENDERER=${MAPS_RENDERER:-maplibre}
stage=${1:?blank or idle}
log="logs/memprobe-$stage.log"
MAPS_BENCH=1 MAPS_MEMPROBE=$stage systemd-run --user --scope -q -p MemoryMax=10G timeout 60 ./target/release/maps-tauri >"$log" 2>&1 || true
python3 tools/mem_summary.py "$log"
