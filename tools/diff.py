#!/usr/bin/env python3
"""Differential test: the Rust `coords` crate against the Python `serve.py` it was ported from.

Generates a deterministic corpus (the selftest's case tables, multi-script place names, and seeded
random strings), asks both implementations about every input, and prints each disagreement
grouped by function. Exit status is 0 only when they agree on everything.

    tools/diff.py [--fuzz N] [--seed S]       SERVE_PY overrides the path to serve.py

The Python original is not in this repository; by default it is looked for in a sibling
checkout of the private repository it lives in.
"""

import argparse
import importlib.util
import json
import os
import random
import sqlite3
import subprocess
import sys
import unicodedata
from collections import defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
INFRA = os.path.join(os.path.dirname(ROOT), "Infrastructure")
SERVE = os.environ.get(
    "SERVE_PY", os.path.join(INFRA, "containers/epoch/maps/gazetteer/serve.py")
)
SELFTEST = os.path.join(INFRA, "ci/maps-search-selftest.py")

NAMES = [
    "Reykjavík",
    "São Paulo",
    "Zürich",
    "Đà Nẵng",
    "İstanbul",
    "Straße",
    "ΟΔΟΣ",
    "Ḩajjah",
    "नई दिल्ली",
    "กรุงเทพมหานคร",
    "서울특별시",
    "東京都",
    "Ｔｏｋｙｏ",
    "ﬁnland",
    "Kelvin",
    "القاهرة",
    "قَاهِرَة",
    "ירושלים",
    "יְרוּשָׁלַיִם",
    "Ελληνικά",
    "Ǆemal",
    "Ŀlorenç",
    "١٢٣",
    "१२३",
    "x²",
    "Ⅻ",
    "St. John's",
    "Coeur d'Alene",
    "Wrocław",
    "Øresund",
    "Þingvellir",
]
FUZZ_ALPHABET = list("0123456789.-+ ,;°º'\"NSEWnsewa/e") + [
    "′",
    "″",
    "ſ",
    " ",
    " ",
    "\u001c",
    "\u0085",
    " ",
    "٥",
    "١",
    "１",
    "\t",
    "\n",
]


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def corpus(fuzz, seed):
    st = load(SELFTEST, "selftest")
    items = (
        [q for q, _ in st.COORD_CASES] + st.COORD_REFUSED + st.COORD_REFUSED_REALISTIC
    )
    items += st.COORD_HOSTILE + st.ADVERSARIAL + st.FOLD_CASES + NAMES
    for name in [row[1] for row in st.FIXTURE] + NAMES:
        items += [name[:i] for i in range(1, len(name) + 1)]
    rng = random.Random(seed)
    for _ in range(fuzz):
        items.append(
            "".join(rng.choice(FUZZ_ALPHABET) for _ in range(rng.randint(1, 28)))
        )
    seen, out = set(), []
    for q in items:
        if q not in seen:
            seen.add(q)
            out.append(q)
    return out


def python_side(serve, q):
    return {
        "coords": [list(p) for p in serve.parse_coords(q)],
        "fold": serve.fold(q),
        "fts": serve.fts_query(q),
    }


def rust_side(inputs):
    proc = subprocess.run(
        ["cargo", "run", "-q", "--release", "-p", "coords", "--example", "eval"],
        input="".join(json.dumps(q) + "\n" for q in inputs),
        capture_output=True,
        text=True,
        cwd=ROOT,
        check=True,
    )
    # Split on "\n" only: serde_json leaves U+2028 and U+0085 unescaped, and splitlines() would
    # break a row at them.
    rows = [json.loads(line) for line in proc.stdout.split("\n") if line]
    if len(rows) != len(inputs):
        sys.exit(f"rust side answered {len(rows)} of {len(inputs)} inputs")
    return rows


def fts_tokens(query):
    """The tokens SQLite's FTS5 sees in a phrase query, using the gazetteer's own tokenizer. Two
    query strings with the same tokens find the same rows, whatever their text."""
    if query is None:
        return None
    db = sqlite3.connect(":memory:")
    db.execute(
        'CREATE VIRTUAL TABLE t USING fts5(x, tokenize="unicode61 remove_diacritics 2")'
    )
    db.execute("CREATE VIRTUAL TABLE v USING fts5vocab(t, instance)")
    db.execute("INSERT INTO t VALUES (?)", (query.removesuffix("*").strip('"'),))
    return [row[0] for row in db.execute("SELECT term FROM v ORDER BY offset")]


def has_non_ascii_digit(q):
    return any(unicodedata.category(c) == "Nd" and not c.isascii() for c in q)


# Disagreements the port makes on purpose. Each is reported, counted, and kept out of the exit
# status; anything else fails the run.
EXPLAINED = {
    # Python's \d and float() accept any Unicode decimal digit ("١", "５"); the port reads ASCII
    # digits only. Refusing sends the text to the gazetteer, the safe direction by serve.py's own
    # rule, and nobody types an Arabic-Indic coordinate on purpose.
    "coords": (
        "non-ASCII digits refused by the port",
        lambda q, p, r: has_non_ascii_digit(q) and r == [],
    ),
    # Python splits words at combining marks (Indic vowel signs, Arabic harakat, Hebrew niqqud);
    # the port keeps them in the word. FTS5 re-tokenizes the phrase either way, so the two
    # queries find the same rows.
    "fts": (
        "same FTS5 tokens, different query text",
        lambda q, p, r: fts_tokens(p) == fts_tokens(r),
    ),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fuzz", type=int, default=200_000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--show", type=int, default=6, help="examples printed per function")
    args = ap.parse_args()

    serve = load(SERVE, "serve")
    inputs = corpus(args.fuzz, args.seed)
    rust = rust_side(inputs)
    diffs = defaultdict(list)
    accepted = 0
    for q, r in zip(inputs, rust):
        p = python_side(serve, q)
        accepted += bool(p["coords"])
        for key in ("coords", "fold", "fts"):
            if p[key] != r[key]:
                diffs[key].append((q, p[key], r[key]))

    print(
        f"{len(inputs)} inputs (seed {args.seed}); python accepted {accepted} as coordinates"
    )
    failed = False
    for key in ("coords", "fold", "fts"):
        rows = diffs[key]
        why, ok = EXPLAINED.get(key, ("", lambda *_: False))
        explained = [d for d in rows if ok(*d)]
        unexplained = [d for d in rows if not ok(*d)]
        failed |= bool(unexplained)
        line = "agree" if not rows else f"{len(unexplained)} DISAGREE"
        if explained:
            line += f" (+{len(explained)} explained: {why})"
        print(f"  {key:6} {line}")
        for q, p, r in unexplained[: args.show]:
            print(f"         {q!r}\n           python {p!r}\n           rust   {r!r}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
