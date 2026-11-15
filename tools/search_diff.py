#!/usr/bin/env python3
"""Parity test for the whole search path: serve.py's search() against the Rust port, both over the
same GeoNames index, compared feature by feature (ids, order, labels, coordinates, properties).

    tools/search_diff.py [data/geonames.db]        SERVE_PY overrides the path to serve.py
"""

import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from diff import NAMES, ROOT, SERVE, load

QUERIES = [
    "london",
    "London",
    "santa mon",
    "santa monica",
    "lake tahoe",
    "reykjavik",
    "Reykjavík",
    "zurich",
    "mount everest",
    "new york",
    "york new",
    "los angeles",
    "lax",
    "st. john's",
    "coeur d'alene",
    "san francisco intl",
    "heathrow",
    "ohare",
    "o'hare",
    "denver",
    "dallas",
    "sao paulo",
    "são paulo",
    "münchen",
    "munchen",
    "kyiv",
    "tokyo",
    "東京",
    "delhi",
    "नई दिल्ली",
    "القاهرة",
    "",
    " ",
    '"',
    "*",
    "NEAR",
    'a" OR "b',
    "a*b*c",
    "café*",
    "(",
    "51.5074N 0.1278W",
    "-0.1278, 51.5074",
    "110 00",
    "N1 9",
    "34.0195, -118.4912",
]


def main():
    db_path = (
        sys.argv[1] if len(sys.argv) > 1 else os.path.join(ROOT, "data/geonames.db")
    )
    serve = load(SERVE, "serve")
    db = serve.open_db(db_path)
    queries = list(QUERIES)
    for name in NAMES[:12] + ["Santa Monica", "Lake Tahoe", "Londonderry"]:
        queries += [name[:i] for i in range(1, len(name) + 1)]
    queries = list(dict.fromkeys(queries))

    proc = subprocess.run(
        [
            "cargo",
            "run",
            "-q",
            "--release",
            "-p",
            "maps-tauri",
            "--example",
            "search_eval",
            db_path,
        ],
        input="".join(json.dumps(q) + "\n" for q in queries),
        capture_output=True,
        text=True,
        cwd=ROOT,
        check=True,
    )
    rust = [json.loads(line) for line in proc.stdout.split("\n") if line]
    assert len(rust) == len(queries), f"rust answered {len(rust)} of {len(queries)}"

    bad = 0
    for q, r in zip(queries, rust):
        p = json.loads(json.dumps(serve.search(db, q, 8)))
        if p != r:
            bad += 1
            if bad <= 5:
                print(f"DISAGREE {q!r}")
                print(f"  python {[f['id'] for f in p]}")
                print(f"  rust   {[f['id'] for f in r]}")
                for fp, fr in zip(p, r):
                    if fp != fr:
                        print(f"  first differing feature:\n    {fp}\n    {fr}")
                        break
    results = sum(len(r) for r in rust)
    print(
        f"{len(queries)} queries, {results} features: "
        f"{'all identical' if not bad else f'{bad} queries DISAGREE'}"
    )
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
