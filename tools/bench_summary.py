#!/usr/bin/env python3
"""Read a benchmark log's [bench] line, save it as JSON with its label, print a summary.

    tools/bench_summary.py <log> <label> <out.json>
"""
import json
import sys


def main():
    log, label, out = sys.argv[1:4]
    lines = [l for l in open(log, encoding="utf-8").read().split("\n") if l.startswith("[bench] ")]
    if len(lines) != 1:
        sys.exit(f"{log}: expected one [bench] line, found {len(lines)}")
    d = json.loads(lines[0][len("[bench] "):])
    d["label"] = label
    with open(out, "w", encoding="utf-8") as f:
        json.dump(d, f, indent=1)
    print(f"{label}: workers={d.get('workers')} of {d.get('cores')} cores, prefetch={d.get('prefetch_zoom_delta')},"
          f" cache_zoom_levels={d.get('cache_zoom_levels')}, warm_zoom={d.get('warm_zoom')}, labels={d.get('labels')},"
          f" fade_ms={d.get('fade_ms')}, cached_parents={d.get('cached_parents')}")
    if d.get("pixel_check"):
        print(f"  pixel self-test: {d.get('pixel_selftest_outside_pct')}% background with every other layer hidden"
              " (must be near 100, or the pixel check is blind)")
    for b in d.get("blocks", []):
        print(f"  after: {b['label']:<26} {b['bytes'] / 1048576:7.1f} MB  {b['note']}")
    for k in ("cold", "warm"):
        v = d[k]
        if v.get("rendered_ratio", 1) < 0.9 or v.get("listener_errors", 0):
            print(f"  WARNING {k}: only {v.get('rendered_ratio')} of frames rendered, {v.get('listener_errors')} listener errors: figures suspect")
        print(f"  {k}: GRAY {v['gray_pct']:5}% of frames | loading {v['missing_pct']:5}% | tiles loaded {v['load']['n']:3}"
              f" (avg {v['load']['avg']} ms) | unique {v['unique_tiles']} | cache max {v['cache_max']}"
              f" | dropped frames {v['dropped']}/{v['frames']['n']} (+{v['jitter_only']} within timer rounding)"
              f" | mem {v['memory_mb']} MB")
        w = v.get("visible_wait")
        if w:
            mc = v.get("mesh_cache") or {}
            print(f"     visible wait: {w['n']} tiles, {w['not_ready']} not ready when visible; wait avg {w['avg']} ms, p95 {w['p95']}, worst {w['worst']}"
                  f" | Rust mesh cache: {mc.get('hits', 0)} hits / {mc.get('misses', 0)} misses, {mc.get('entries', 0)} meshes, {mc.get('bytes', 0) / 1048576:.1f} MB")
        s = v.get("stages")
        if s:
            print(f"     handler stages, ms/request: wait {s['wait']}, read {s['read']}, decode {s['decode']}, mesh {s['mesh']},"
                  f" serialize {s['serialize']}, copy {s['copy']} | in flight avg {s['inflight_avg']:.1f}, max {s['inflight_max']}")
        if d.get("pixel_check"):
            px = v["pixel"]
            print(f"     pixels: GRAY {px['gray_pct']}% of frames, area {px['area_pct']} %"
                  f" | agree {px['agree_gray']}, tile-only {px['tile_says_gray_pixels_clean']},"
                  f" pixel-only {px['pixels_gray_tile_says_clean']}")


if __name__ == "__main__":
    main()
