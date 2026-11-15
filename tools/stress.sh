#!/usr/bin/env bash
# The stress benchmark (src/devtools/stress.js) on the release build: a seeded script of interrupted
# pans, zooms and turns, wheel bursts, drags and teleports. Each renderer runs twice: once for timing,
# once with the pixel check (reading every frame back costs time, so it would distort the first).
#   tools/stress.sh [rust] [maplibre]   default both
#   OPS=150 SEED=1                       script length and seed
#   REMOTE=1                             the native renderer reads the planet over the LAN
#                                        (MapLibre always reads the local file)
# The archive is data/westla.pmtiles unless MAPS_ARCHIVE says otherwise. JSON lines go to
# logs/stress-<renderer>-<timing|pixels>.log; the worst pixel-check frame to logs/snapshot-stress-*.png.
set -euo pipefail
cd "$(dirname "$0")/.."
export MAPS_ARCHIVE=${MAPS_ARCHIVE:-westla.pmtiles}
if [ "${REMOTE:-0}" = 1 ]; then unset MAPS_LOCAL_ONLY; else export MAPS_LOCAL_ONLY=1; fi
ops=${OPS:-150}
seed=${SEED:-1}
renderers=("$@")
[ ${#renderers[@]} -gt 0 ] || renderers=(rust maplibre)
for r in "${renderers[@]}"; do
  for mode in timing pixels; do
    log="logs/stress-$r-$mode.log"
    pixel=0
    [ "$mode" = pixels ] && pixel=1
    MAPS_BENCH=1 MAPS_STRESS=$ops MAPS_STRESS_SEED=$seed MAPS_PIXEL_CHECK=$pixel MAPS_RENDERER=$r \
      systemd-run --user --scope -q -p MemoryMax=10G timeout 300 ./target/release/maps-tauri >"$log" 2>&1 || true
  done
done
python3 - "$ops" "$seed" "${renderers[@]}" <<'PY'
import json, sys
ops, seed, renderers = sys.argv[1], sys.argv[2], sys.argv[3:]

def result(path):
    # split on the producer's own newline: str.splitlines() also breaks at U+2028 inside JSON strings
    lines = [l for l in open(path, encoding="utf-8").read().split("\n") if l.startswith("[bench] ")]
    if len(lines) != 1:
        sys.exit(f"{path}: expected one [bench] line, found {len(lines)} (see the log)")
    return json.loads(lines[0][8:])

print(f"stress: {ops} operations, seed {seed}")
for r in renderers:
    t, p = result(f"logs/stress-{r}-timing.log"), result(f"logs/stress-{r}-pixels.log")
    fi, rd, px = t["frame_interval"], t["render"], p["pixel"]
    mem = t["memory_mb"]
    print(f"{r}: {t['chaos_s']} s of chaos, {t['frames']} frames ({t['rendered']} rendered)"
          f" | input events with no effect: {t['input_no_effect'] or 'none'} of {({k: v for k, v in t['op_counts'].items() if k in ('wheel', 'drag', 'rotdrag')})}")
    print(f"  missed refreshes {t['dropped']} ({100 * t['dropped'] / max(1, t['frames']):.1f}%), over 50 ms {t['long_frames_over_50ms']}"
          f" | frame interval p50 {fi['p50']} p99 {fi['p99']} worst {fi['worst']} ms")
    print(f"  render p50 {rd['p50']} p95 {rd['p95']} p99 {rd['p99']} worst {rd['worst']} ms"
          + (f" | labels p99 {t['labels']['p99']} ms" if t.get("labels") else ""))
    print(f"  background (pixel run): {px['gray_frames']} frames ({px['gray_pct']}%), area p99 {px['area_pct']['p99']}% worst {px['area_pct']['worst']}%"
          f" | at rest {px['at_rest_pct']}% | stand-in frames {p['stand_in_frames']}, coarser {p['coarse_frames']}")
    print(f"  camera calls that threw: {t['op_errors']} (pixel run {p['op_errors']}) | first non-finite view: {t['first_bad_view'] or 'none'}")
    print(f"  settled {t['settled']} in {t['settle_ms']} ms | listener errors {t['listener_errors']}, uncaught {t['uncaught_errors']}"
          f" (pixel run: {p['listener_errors']}, {p['uncaught_errors']})")
    print(f"  memory MB: start {mem[0]['mb']}, peak {max(m['mb'] for m in mem)}, end {mem[-2]['mb']}, 10 s later {mem[-1]['mb']}")
    if px.get("worst_frame"):
        print(f"  worst frame: {px['worst_frame']}")
PY
