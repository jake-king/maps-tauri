#!/usr/bin/env bash
# Per-process memory (PSS) of the release build at one stage: blank | idle. Env passes through.
#   tools/memprobe.sh blank      MAPS_WORKERS=1 tools/memprobe.sh idle
set -euo pipefail
cd "$(dirname "$0")/.."
stage=${1:?blank or idle}
log="logs/memprobe-$stage.log"
MAPS_BENCH=1 MAPS_MEMPROBE=$stage systemd-run --user --scope -q -p MemoryMax=10G timeout 60 ./target/release/maps-tauri >"$log" 2>&1 || true
python3 tools/mem_summary.py "$log"
