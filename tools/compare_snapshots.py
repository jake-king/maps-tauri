#!/usr/bin/env python3
"""Compare one platform's end-to-end snapshots with a reference platform's, without dependencies.

  compare_snapshots.py REF_DIR DIR [--limits FILE] [--diff-dir OUT]

Each snapshot-<name>.png in both is reduced to 8 x 8 px blocks (so anti-aliasing and font hinting,
which differ legitimately between engines, average out) and scored by the share of blocks whose mean
colour differs by more than 24 levels in any channel. A score over the name's limit fails. The block
map of differences is written as a PGM for each pair when --diff-dir is given. Exit 1 on failure.
"""

import json
import struct
import sys
import zlib
from pathlib import Path

BLOCK = 8
TOL = 24


def read_png(path):
    """8-bit RGB or RGBA, non-interlaced (what a canvas's toDataURL writes) -> (w, h, rows of RGB)."""
    data = Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos, idat, w = 8, b"", 0
    while pos < len(data):
        n, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + n]
        if kind == b"IHDR":
            w, h, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if depth != 8 or color not in (2, 6) or interlace:
                raise ValueError(
                    f"{path}: depth {depth} colour {color} interlace {interlace} not supported"
                )
            bpp = 4 if color == 6 else 3
        elif kind == b"IDAT":
            idat += body
        pos += 12 + n
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        f = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1 : (y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - bpp] if i >= bpp else 0
            b = prev[i]
            c = prev[i - bpp] if i >= bpp else 0
            if f == 1:
                line[i] = (line[i] + a) & 255
            elif f == 2:
                line[i] = (line[i] + b) & 255
            elif f == 3:
                line[i] = (line[i] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                line[i] = (
                    line[i] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)
                ) & 255
        rows.append(bytes(line))
        prev = line
    return w, h, bpp, rows


def blocks(png):
    w, h, bpp, rows = png
    bw, bh = w // BLOCK, h // BLOCK
    out = []
    for by in range(bh):
        for bx in range(bw):
            s = [0, 0, 0]
            for y in range(by * BLOCK, (by + 1) * BLOCK):
                r = rows[y]
                for x in range(bx * BLOCK, (bx + 1) * BLOCK):
                    o = x * bpp
                    s[0] += r[o]
                    s[1] += r[o + 1]
                    s[2] += r[o + 2]
            out.append(tuple(v // (BLOCK * BLOCK) for v in s))
    return bw, bh, out


def score(a, b):
    """Share of blocks differing by more than TOL, and the per-block map."""
    (aw, ah, ab), (bw, bh, bb) = blocks(a), blocks(b)
    if (aw, ah) != (bw, bh):
        return None, None, (aw, ah)
    diff = [max(abs(p - q) for p, q in zip(x, y)) > TOL for x, y in zip(ab, bb)]
    return sum(diff) / len(diff), diff, (aw, ah)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    opts = dict(zip(sys.argv[1::1], sys.argv[2::1]))
    if len(args) < 2:
        sys.exit(__doc__)
    ref, cur = Path(args[0]), Path(args[1])
    limits = (
        json.loads(Path(opts["--limits"]).read_text()) if "--limits" in opts else {}
    )
    out = Path(opts["--diff-dir"]) if "--diff-dir" in opts else None
    names = sorted(p.name for p in ref.glob("snapshot-*.png"))
    if not names:
        sys.exit(f"compare: no snapshots in {ref}")
    fail = []
    for n in names:
        key = n[len("snapshot-") : -4]
        limit = limits.get(key, limits.get("*", 0.02))
        if limit is None:
            print(f"compare: {key}: not compared (random content)")
            continue
        if not (cur / n).exists():
            fail.append(f"{key}: missing")
            continue
        a, b = read_png(ref / n), read_png(cur / n)
        s, diff, (bw, bh) = score(a, b)
        if s is None:
            # a window the OS shrank to fit its screen, say: not a rendering difference
            print(f"compare: {key}: sizes differ, {a[0]}x{a[1]} against {b[0]}x{b[1]}")
            fail.append(f"{key}: sizes differ ({a[0]}x{a[1]} against {b[0]}x{b[1]})")
            continue
        print(f"compare: {key}: {s:.4f} of blocks differ (limit {limit})")
        if s > limit:
            fail.append(f"{key}: {s:.4f} > {limit}")
        if out and diff:
            out.mkdir(parents=True, exist_ok=True)
            (out / f"diff-{key}.pgm").write_bytes(
                f"P5 {bw} {bh} 255\n".encode() + bytes(255 if d else 0 for d in diff)
            )
    if fail:
        sys.exit("compare: FAILED:\n  " + "\n  ".join(fail))
    print("compare: all within limits")


if __name__ == "__main__":
    main()
