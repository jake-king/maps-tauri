# Measuring

Most decisions in this project were made from a measurement, and several measurements were wrong
before they were right. The tools are in `tools/` and `src/devtools/`; each script describes itself
at the top. F4 in the app opens the profiler overlay.

## The stress benchmark

`tools/stress.sh` plays a seeded script of 150 camera operations in about 52 seconds: pans, zooms,
turns and all three at once, most interrupted by the next; bursts of wheel notches, drags and
right-drags sent as real DOM events so the input handlers run; and teleports to views nothing has
loaded. The script is a pure function of the seed (unit-tested), so both renderers get exactly the
same input. Each renderer runs twice: once for timing, once with a pixel check that reads every
frame back from the GPU and counts frames where any of the screen is not yet drawn.

West LA extract, seed 1:

| | native | MapLibre |
|---|---|---|
| frames with background showing | 0 of 2380 | 325 of 3066 (10.6%) |
| worst frame | 0% | 100% (after a teleport) |
| render p50 / p99 | 2 / 7 ms | 5 / 13 ms |
| memory start / peak | 435 / 619 MB | 494 / 1501 MB |

A control keeps the zero honest: with stand-in tiles switched off, the pixel check sees background
in 15 frames of a short run. Against the remote planet over a LAN, the disk cache took frames with
background from 7.8% to 0.4%.

The harness was wrong twice before its numbers meant anything. MapLibre threw on every frame after
the third operation, because a pan passed `bearing: undefined` and MapLibre turns an explicit
`undefined` into a NaN bearing. And MapLibre ignored most synthetic drags, because mouse moves sent
to `window` never reach its listeners on `document`, so it was getting an easier test. The run now
counts input events that did not move the map.

## Findings that came from measuring

- **A frame loop that silently stalled.** The first native runs showed 0% of frames with tiles
  loading, which could not be right with 150 ms tile delays. A dev-tools listener called a
  MapLibre-only method, threw inside the render, and the next frame was never scheduled, exactly
  while tiles were loading. The renderer now isolates listener errors, and the benchmark reports the
  share of frames rendered.
- **A resting CPU, not slow code.** A tile took 10.3 ms in the app's handler but 1.7 ms offline. The
  same work with a 16 ms sleep between tiles took 7.75 ms, and with the gap busy-waited instead,
  1.76 ms: the CPU clocks down between requests. Rather than keep it awake, meshing moved off the
  critical path with a prefetch ring, and tiles not ready when they came into view went from 40 of
  40 to 0 of 40. A Rust mesh cache built for the same problem had zero hits and is off by default.
- **The benchmark that was missing.** Every benchmark panned at a fixed zoom; zooming by hand showed
  grey areas. A zoom benchmark found that empty tiles past a cutout's edge drew nothing, and the fix
  (fall back to a coarser ancestor, prefetch along the zoom) took the worst frames to zero on the
  West LA archive.
- **A label hitch.** Slow label frames were the ones measuring new text, and their cost did not scale
  with the amount measured, which pointed at setting the canvas font. Text is now measured when a
  tile arrives, and the worst label frame went from 8-11 ms to 3-4 ms.
- **Memory drift, bisected.** Idle memory had drifted from 296 to 375 MB. Building eight commits and
  probing each showed 26 MB was the platform's blank-window floor moving, and 17 MB was the
  coordinate parser's WebAssembly compiled at start-up; it now loads on the first search keystroke.

## Memory accounting

The dev tools break memory down by owner: a GPU ledger (every WebGL allocation call wrapped), a walk
of the tile buffers, and the process's memory map by kind. A blank window with MapLibre's code loaded
but no map measured 293 MB on the dev box, which bounds what any optimisation of the map itself can
reach.
