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
    print(f"{label}: cache_zoom_levels={d.get('cache_zoom_levels')}")
    for k in ("cold", "warm"):
        v = d[k]
        print(f"  {k}: missing {v['missing_pct']:5}% of frames | tiles loaded {v['load']['n']:3}"
              f" (avg {v['load']['avg']} ms) | unique {v['unique_tiles']} | cache max {v['cache_max']}"
              f" | over budget {v['over_budget']} | mem {v['memory_mb']} MB")


if __name__ == "__main__":
    main()
