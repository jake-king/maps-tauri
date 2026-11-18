#!/usr/bin/env python3
"""Seed inputs for the weather and search fuzz targets (tools/fuzz.sh seeds).

    fuzz_seeds.py TAPE_DIR CORPUS_DIR

From a recorded tape (wx::tape: index.tsv maps each URL to its response) it writes one or two real
answers of each kind into the targets' corpus directories, cut down where a whole one would only
slow the fuzzer: text to its first lines, GRIB to its first message. The coordinate seeds are the
shapes the search box accepts. A seed only gives the fuzzer a valid starting point; it mutates from
there.
"""

import gzip
import pathlib
import re
import sys

TEXT_BYTES = 16 << 10

COORDS = [
    "34.05 N, 118.25 W",
    "34.05, -118.25",
    "34°03'N 118°15'W",
    "34° 3' 0\" N 118° 15' 0\" W",
    "N34.05 W118.25",
    "-33.8688 151.2093",
    "51 30 N 0 7 W",
    "Los Angeles",
    "Santa Mon",
]


def first_lines(b, limit=TEXT_BYTES):
    """The first `limit` bytes, cut back to the end of a line."""
    if len(b) <= limit:
        return b
    cut = b.rfind(b"\n", 0, limit)
    return b[: cut + 1 if cut > 0 else limit]


def first_grib_message(b):
    """The first GRIB2 message (its total length is bytes 8-16 of the indicator section)."""
    if b[:4] != b"GRIB" or len(b) < 16:
        return b
    return b[: int.from_bytes(b[8:16], "big")]


def kind(url):
    """Which target an answer seeds, and how it is cut, or None for answers no target reads."""
    if url.endswith(".grib2.gz"):
        return "radar"
    if "filter_gfs" in url:
        return "gfs"
    if "metars.cache.csv.gz" in url:
        return "metar"
    if url.rstrip("/").endswith("MergedBaseReflectivityQC"):
        return "radar_index"
    if "GetCapabilities" in url:
        return "capabilities"
    if "/api/data/" in url:
        return "json"
    return None


def main(tape, out):
    tape, out = pathlib.Path(tape), pathlib.Path(out)
    seeds = {}  # (target, name) -> bytes; the smallest answer of each kind wins
    # split as wx::tape writes it, on \n only (splitlines would also split on \r and U+2028)
    for line in (tape / "index.tsv").read_text().split("\n"):
        if not line:
            continue
        key, url = line.split("\t")[:2]
        k = kind(url)
        path = tape / key
        if k is None or not path.exists():
            continue
        b = path.read_bytes()
        if k == "radar":
            add = [
                ("radar_gunzip", "mrms.gz", b),
                ("grib", "mrms", first_grib_message(gzip.decompress(b))),
            ]
        elif k == "gfs":
            add = [("grib", "gfs-wind", first_grib_message(b))]
        elif k == "metar":
            add = [("wx_text", "metars.csv", first_lines(gzip.decompress(b)))]
        elif k == "json":
            name = re.sub(r"\W+", "-", url.split("/api/data/")[1].split("&date")[0])
            add = [("wx_text", name + ".json", b)]
        else:
            add = [("wx_text", k, first_lines(b))]
        for target, name, data in add:
            if (target, name) not in seeds or len(data) < len(seeds[target, name]):
                seeds[target, name] = data
    for i, c in enumerate(COORDS):
        seeds["coords", f"c{i}"] = c.encode()
    for (target, name), data in sorted(seeds.items()):
        d = out / target
        d.mkdir(parents=True, exist_ok=True)
        (d / name).write_bytes(data)
    counts = {}
    for target, _ in seeds:
        counts[target] = counts.get(target, 0) + 1
    print("fuzz_seeds:", ", ".join(f"{t} {n}" for t, n in sorted(counts.items())))


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
