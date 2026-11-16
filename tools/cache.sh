#!/usr/bin/env bash
# The disk cache end to end, on the release build, against a local Range server (tools/rangeserve.py)
# serving data/westla.pmtiles with an ETag, so the test can take the network away and replace the
# archive without touching the LAN:
#   cold      empty cache, a seeded stress run: everything comes over the network
#   warm      the same run again: only the header should reach the server
#   offline   server stopped, the same run with the pixel check: the cached copy is drawn
#   replaced  server back under a new ETag: the old version's ranges are dropped
#   corrupt   the cache file overwritten with noise: the planet is still read, without a cache
#   tools/cache.sh            build, then run
#   SKIP_BUILD=1 OPS=60       reuse the build; the stress script's length
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p logs
if [ "${SKIP_BUILD:-0}" != 1 ]; then
  systemd-run --user --scope -q -p MemoryMax=12G npx tauri build --no-bundle >logs/cache-build.log 2>&1 \
    || { tail -20 logs/cache-build.log >&2; exit 1; }
fi
ops=${OPS:-60}
port=18765
db=logs/cache-e2e.sqlite
rm -f "$db" "$db-wal" "$db-shm"
export MAPS_ARCHIVE_URL="http://127.0.0.1:$port/westla.pmtiles" MAPS_DISK_CACHE=$db MAPS_BENCH=1 \
  MAPS_RENDERER=rust MAPS_STRESS=$ops MAPS_STRESS_SEED=${SEED:-1}
unset MAPS_LOCAL_ONLY
server=
serve() { # etag, log
  python3 tools/rangeserve.py data/westla.pmtiles "$port" "$1" >"$2" 2>&1 &
  server=$!
  for _ in $(seq 50); do
    curl -s -o /dev/null -r 0-0 "$MAPS_ARCHIVE_URL" && return 0
    sleep 0.1
  done
  echo "rangeserve did not start (see $2)" >&2
  exit 1
}
stop() { [ -n "$server" ] && kill "$server" 2>/dev/null && wait "$server" 2>/dev/null || true; server=; }
trap stop EXIT
app() { # name, then env assignments
  env "${@:2}" systemd-run --user --scope -q -p MemoryMax=10G timeout 300 ./target/release/maps-tauri >"logs/cache-$1.log" 2>&1 || true
}
ERRORS='^\[frontend\] (uncaught error|unhandled rejection|search failed|map error|csp violation)'
fail=0
check() { # name, what its log must contain (fixed string)
  if grep -qE "$ERRORS" "logs/cache-$1.log"; then
    echo "$1: FAILED: errors:"; grep -E "$ERRORS" "logs/cache-$1.log" | head -5; fail=1
  elif ! grep -q '^\[bench\] ' "logs/cache-$1.log"; then
    echo "$1: FAILED: no [bench] line (logs/cache-$1.log)"; fail=1
  elif ! grep -qF -- "$2" "logs/cache-$1.log"; then
    echo "$1: FAILED: no line with '$2' (logs/cache-$1.log)"; fail=1
  fi
}
requests() { grep -c '^\[range\] 206' "$1" || true; }

serve '"v1"' logs/cache-server-1.log
app cold
check cold '(0 MB stored for this version)'
cold=$(requests logs/cache-server-1.log)
app warm
check warm '[archive] remote planet'
warm=$(($(requests logs/cache-server-1.log) - cold))
echo "cold: $cold requests | warm: $warm requests | cache $(du -h "$db" | cut -f1)"
[ "$warm" -lt "$cold" ] || { echo "warm: FAILED: the cache saved nothing"; fail=1; }

stop
app offline MAPS_PIXEL_CHECK=1
check offline 'reading its cached copy (version "v1"'
python3 - logs/cache-offline.log <<'PY'
import json, sys
line = [l for l in open(sys.argv[1], encoding="utf-8").read().split("\n") if l.startswith("[bench] ")][0]
px = json.loads(line[8:])["pixel"]
print(f"offline: background in {px['gray_frames']} frames ({px['gray_pct']}%), worst {px['area_pct']['worst']}%, at rest {px['at_rest_pct']}%")
PY

serve '"v2"' logs/cache-server-2.log
app replaced MAPS_STRESS=5
check replaced '(0 MB stored for this version)'
grep -q "if-range=\"v2\"" logs/cache-server-2.log || { echo "replaced: FAILED: requests did not carry the new version"; fail=1; }

# a cache file that is not a database costs the cache, not the planet
rm -f "$db-wal" "$db-shm"
head -c 4096 /dev/urandom >"$db"
app corrupt MAPS_STRESS=5
check corrupt 'will not open'
check corrupt '[archive] remote planet'
stop
((fail)) || echo "cache: all passed"
exit $fail
