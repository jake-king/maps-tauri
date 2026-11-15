#!/usr/bin/env python3
"""Print a memory probe log's [bench] line as one row: total and per-process PSS in MB."""
import json
import sys

for path in sys.argv[1:]:
    lines = [l for l in open(path, encoding="utf-8").read().split("\n") if l.startswith("[bench] ")]
    if len(lines) != 1:
        print(f"{path}: expected one [bench] line, found {len(lines)}")
        continue
    d = json.loads(lines[0][len("[bench] "):])
    total = sum(m["pss_kb"] for m in d["memory"]) // 1024
    rows = ", ".join(f"{m['name']} {m['pss_kb'] // 1024}" for m in d["memory"])
    print(f"{d['probe']:6} total {total:4} MB | {rows}")
