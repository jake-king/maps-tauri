#!/usr/bin/env python3
"""The end-to-end fixtures: a tape of NOAA's answers (wx::tape) and the map archive the checks draw
on, in one archive too large for git (about 90 MB), checked by its sha256 in tools/fixtures.lock.

  fixtures.py pack TAPE_DIR OUT.tar.gz   build it from a recorded tape and data/westla.pmtiles,
                                         and write tools/fixtures.lock
  fixtures.py unpack ARCHIVE             verify it against the lock, then put the tape in
                                         fixtures/tape and the map in data/

GitHub's CI downloads the archive from the release the lock names (`gh release download`); the
forge's takes it from the repository's e2e-fixtures branch (tools/forge-job.sh). The map is
OpenStreetMap data (ODbL, attributed in the app); NOAA's data is public domain.
"""

import hashlib
import json
import sys
import tarfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "tools" / "fixtures.lock"
MAP = "westla.pmtiles"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def pack(tape, out):
    tape, out = Path(tape), Path(out)
    if not (tape / "now.txt").is_file():
        sys.exit(f"fixtures: {tape} is not a tape (no now.txt)")
    with tarfile.open(out, "w:gz") as t:
        for p in sorted(tape.iterdir()):
            t.add(p, arcname=f"tape/{p.name}")
        t.add(ROOT / "data" / MAP, arcname=f"data/{MAP}")
    lock = json.loads(LOCK.read_text()) if LOCK.exists() else {}
    lock.update(
        file=out.name,
        sha256=sha256(out),
        bytes=out.stat().st_size,
        recorded=int((tape / "now.txt").read_text().strip()),
    )
    lock.setdefault("release", "e2e-fixtures-1")
    LOCK.write_text(json.dumps(lock, indent=2) + "\n")
    print(
        f"fixtures: {out} {lock['bytes']} bytes sha256 {lock['sha256']}; lock written"
    )


def unpack(archive):
    lock = json.loads(LOCK.read_text())
    got = sha256(archive)
    if got != lock["sha256"]:
        sys.exit(
            f"fixtures: {archive} has sha256 {got}, the lock says {lock['sha256']}"
        )
    with tarfile.open(archive, "r:gz") as t:
        for m in t.getmembers():
            if not m.isfile() or m.name.startswith("/") or ".." in Path(m.name).parts:
                sys.exit(f"fixtures: unexpected member {m.name!r}")
            top = m.name.split("/", 1)[0]
            if top not in ("tape", "data"):
                sys.exit(f"fixtures: unexpected member {m.name!r}")
            dest = ROOT / ("fixtures" if top == "tape" else "") / m.name
            dest.parent.mkdir(parents=True, exist_ok=True)
            with t.extractfile(m) as src, open(dest, "wb") as f:
                f.write(src.read())
    print(f"fixtures: unpacked into fixtures/tape and data/{MAP}")


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "pack":
        pack(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 3 and sys.argv[1] == "unpack":
        unpack(sys.argv[2])
    else:
        sys.exit(__doc__)
