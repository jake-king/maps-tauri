#!/usr/bin/env python3
"""Print a memory probe log's [bench] line as one row: total and per-process PSS in MB."""

import json
import sys

for path in sys.argv[1:]:
    with open(path, encoding="utf-8") as fh:
        lines = [line for line in fh.read().split("\n") if line.startswith("[bench] ")]
    if len(lines) != 1:
        print(f"{path}: expected one [bench] line, found {len(lines)}")
        continue
    d = json.loads(lines[0][len("[bench] ") :])
    total = sum(m["pss_kb"] for m in d["memory"]) // 1024
    rows = ", ".join(f"{m['name']} {m['pss_kb'] // 1024}" for m in d["memory"])
    print(f"{d['probe']:6} total {total:4} MB | {rows}")
    for m in d["memory"]:
        k = m.get("kinds")
        if k:
            print(
                f"         {m['name']:<16} anon {k['anon_kb'] // 1024:4}  gpu-mapped {k['gpu_kb'] // 1024:4}"
                f"  shared-mem {k['shmem_kb'] // 1024:4}  files {k['file_kb'] // 1024:4}  (MB)"
            )
    for b in d.get("blocks", []):
        print(f"         {b['label']:<26} {b['bytes'] / 1048576:7.1f} MB  {b['note']}")
