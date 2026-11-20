#!/usr/bin/env bash
# Record the README's demo: the release build's MAPS_DEMO=1 tour (src/rust-renderer/app.js), on live
# NOAA data and the West LA extract, in a virtual display, captured by ffmpeg until the app exits.
#   tools/demo-video.sh            writes logs/demo/demo.mp4 (the whole tour, under 10 MB) and
#                                  logs/demo/demo.gif (the US with every layer coming on, for the README)
#   SKIP_BUILD=1                   reuse target/release/maps-tauri
# Needs Xvfb and ffmpeg. The app is kept off a Wayland session (GDK would open the window there,
# not in the virtual display). The display has no GPU, so WebGL runs in software: frame rate is lower
# than on a desktop.
set -euo pipefail
cd "$(dirname "$0")/.."
out=logs/demo
mkdir -p "$out"
if [[ ${SKIP_BUILD:-0} != 1 ]]; then
  systemd-run --user --scope -q -p MemoryMax=12G npx tauri build --no-bundle >"$out/build.log" 2>&1 \
    || { tail -20 "$out/build.log" >&2; exit 1; }
fi
w=1200 h=800 disp=:87
Xvfb "$disp" -screen 0 "${w}x${h}x24" -nolisten tcp >"$out/xvfb.log" 2>&1 &
xvfb=$!
trap 'kill "$xvfb" 2>/dev/null || true' EXIT
for _ in $(seq 50); do xdpyinfo -display "$disp" >/dev/null 2>&1 && break; sleep 0.1; done
rm -f "$out"/weather.sqlite*
env -u WAYLAND_DISPLAY GDK_BACKEND=x11 DISPLAY=$disp WEBKIT_DISABLE_DMABUF_RENDERER=1 MAPS_BENCH=1 MAPS_DEMO=1 MAPS_F4=0 \
  MAPS_ARCHIVE=westla.pmtiles MAPS_LOCAL_ONLY=1 MAPS_WEATHER_CACHE="$PWD/$out/weather.sqlite" \
  systemd-run --user --scope -q -p MemoryMax=10G timeout 600 ./target/release/maps-tauri >"$out/app.log" 2>&1 &
app=$!
rec_t0=$(date +%s.%N)
# wall-clock timestamps: with WebGL in software the capture drops frames, and without them the video
# would run shorter than the tour and the step marks below would land on the wrong frames
ffmpeg -loglevel error -y -use_wallclock_as_timestamps 1 -f x11grab -draw_mouse 0 -video_size "${w}x${h}" \
  -framerate 20 -i "$disp" -fps_mode cfr -r 20 -c:v libx264 -preset ultrafast -qp 0 "$out/raw.mkv" &
rec=$!
wait "$app" || true
kill -INT "$rec" 2>/dev/null || true
wait "$rec" || true
grep -q '"demo":true' "$out/app.log" || { echo "demo-video: the tour did not finish (see $out/app.log)" >&2; exit 1; }
# the first 2 s are the window coming up
ffmpeg -loglevel error -y -ss 2 -i "$out/raw.mkv" -vf "scale=960:-2:flags=lanczos" -c:v libx264 -preset slow \
  -crf 30 -pix_fmt yuv420p -movflags +faststart "$out/demo.mp4"
# the README's loop: from the flight out to the country until the winds have run for 5 s, by the
# tour's own marks (wall-clock seconds; its waits follow NOAA's answers, so they move from run to run)
mark() { sed -n "s/^\[frontend\] demo $1 \([0-9.]*\)$/\1/p" "$out/app.log" | head -1; }
from=$(mark country) to=$(mark winds-shown)
[[ -n $from && -n $to ]] || { echo "demo-video: no step marks in $out/app.log" >&2; exit 1; }
ffmpeg -loglevel error -y -ss "$(awk "BEGIN { print $from - $rec_t0 }")" -t "$(awk "BEGIN { print $to - $from }")" -i "$out/raw.mkv" \
  -vf "fps=8,scale=640:-1:flags=lanczos,split[a][b];[a]palettegen=max_colors=96:stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=3:diff_mode=rectangle" \
  "$out/demo.gif"
ls -l "$out/demo.mp4" "$out/demo.gif"
