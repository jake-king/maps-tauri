#!/usr/bin/env bash
# The zoom benchmark for one renderer (release build): zoom out 15->10 and in 10->16 at the pier,
# background on screen measured by the pixel check, worst frame saved as a PNG.
#   tools/zoombench.sh rust|maplibre        env passes through (e.g. MAPS_NO_FALLBACK=1)
set -euo pipefail
cd "$(dirname "$0")/.."
# benchmarks run on the small reference cutout unless told otherwise, so results stay comparable
export MAPS_ARCHIVE=${MAPS_ARCHIVE:-basemap.pmtiles}
r=${1:?rust or maplibre}
log="logs/zoombench-$r.log"
MAPS_BENCH=1 MAPS_ZOOM_BENCH=1 MAPS_PIXEL_CHECK=1 MAPS_RENDERER=$r systemd-run --user --scope -q -p MemoryMax=10G \
  timeout 120 ./target/release/maps-tauri >"$log" 2>&1 || true
python3 - "$log" "$r" <<'PY'
import json, sys
lines = [l for l in open(sys.argv[1], encoding="utf-8").read().split("\n") if l.startswith("[bench] ")]
if len(lines) != 1:
    sys.exit(f"{sys.argv[1]}: expected one [bench] line, found {len(lines)}")
d = json.loads(lines[0][8:])
print(f"{sys.argv[2]}: worst frame {d['worst_frame_pct']}% background -> {d['worst_frame']}")
for l in d["legs"]:
    a = l["area_pct"]
    print(f"  zoom {l['leg']:7} {l['frames']:3} frames: background in {l['gray_frames']:3}, avg {a['avg']}% p95 {a['p95']}% worst {a['worst']}%"
          f" | stand-in frames {l['fallback_frames']} (coarser ancestor in {l.get('coarse_frames', '?')}) | at rest after: {l['at_rest_pct']}%")
    if l.get("not_ready"):
        print(f"      tiles not ready when visible, by state just before: {l['not_ready']}")
PY
