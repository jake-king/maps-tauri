#!/usr/bin/env python3
"""The end-to-end checks on the app's own report: the `[bench]` JSON line a self-driving run prints.
Plain Python, no dependencies, so the same checks run on Linux, macOS and Windows.

  e2e_check.py weather LOG [--no-timing]   the weather run (MAPS_WEATHER=1)
  e2e_check.py wind LOG                    the wind-trail motion run (MAPS_WIND_MOTION=1)

--no-timing drops the limits that measure speed (buffering, gaps between time-lapse steps, total
time): CI's runners draw WebGL in software, so only correctness is checked there. Exit 1 on failure.
"""

import json
import math
import re
import sys
from itertools import pairwise
from pathlib import Path


def weather(path, timing=True):
    log = Path(path).read_text(encoding="utf-8").split("\n")
    bad = [
        l
        for l in log
        if re.match(
            r"\[frontend\] (uncaught error|unhandled rejection|map error|csp violation)",
            l,
        )
    ]
    bench = [l for l in log if l.startswith("[bench] ")]
    fail = []
    if bad:
        fail += bad[:5]
    if not bench:
        sys.exit(f"weather: FAILED: no [bench] line ({path})")
    r = json.loads(bench[0][8:])
    w, past, p = r["weather"], r["past"], r["probe"]
    for name, x in (("live", w), ("3 h back", past)):
        if x["errors"]:
            fail.append(f"{name}: fetch errors: {x['errors']}")
        if x["last_error"]:
            fail.append(f"{name}: the layer reported: {x['last_error']}")
        sel = x["selection"] or {}
        if len(sel.get("radar") or []) < 4:
            fail.append(f"{name}: radar loop of {len(sel.get('radar') or [])} frames")
        if x["shown_sat"] is None:
            fail.append(f"{name}: no satellite image drawn")
        if name == "live" and x["hazards"] < 1:
            fail.append(f"{name}: no hazards")
        if (sel.get("hazards") is None) != (x["hazards"] == 0):
            fail.append(
                f"{name}: hazards {x['hazards']} for a snapshot of {sel.get('hazards')}"
            )
    if any(t > r["back"] for t in past["selection"]["radar"]):
        fail.append(
            f"3 h back: a radar frame from after the chosen time: {past['selection']['radar']} > {r['back']}"
        )
    if (
        past["selection"]["satellite"] is None
        or past["selection"]["satellite"] > r["back"]
    ):
        fail.append(
            f"3 h back: satellite {past['selection']['satellite']} is not at or before {r['back']}"
        )
    if (w["metars"] or 0) < 4000:
        fail.append(f"live: {w['metars']} METAR stations (the world has about 5,000)")
    wo = r["world"]
    lb = wo.get("labels") or {}
    if not (lb.get("placed") or 0) >= 5:
        fail.append(
            f"the world view: {lb.get('placed')} of {lb.get('wanted')} hazard labels placed"
        )
    if wo["shown_global"] is None:
        fail.append("the world view: no global satellite image drawn")
    if (wo["sigmets"] or 0) < 20:
        fail.append(f"the world view: {wo['sigmets']} SIGMETs in force")
    if not (r["sigmetPopup"] or "").startswith(("SIGMET", "CONVECTIVE")):
        fail.append(f"a click inside a SIGMET did not open it: {r['sigmetPopup']!r}")
    if (past["sigmets"] or 0) < 10:
        fail.append(f"3 h back: {past['sigmets']} SIGMETs (the history fetch)")
    if not (r["popup"] or "").startswith("KLAX"):
        fail.append(f"a click on KLAX did not open its report: {r['popup']!r}")
    if past["metars"] != 0 or "collected from" not in past["stamp"]:
        fail.append(
            f"3 h back, before any METAR was collected: {past['metars']} stations, stamp {past['stamp']!r}"
        )
    wd = r["wind"] or {}
    if "run" not in wd:
        fail.append(f"no wind field: {wd}")
    else:
        # the 0.5° files are every 3 hours: the nearest is at most 90 minutes away
        if abs(wd["valid"] - w["at"] if w["at"] else 0) > 5400 and w["at"]:
            fail.append(f"wind valid {wd['valid']} is not near {w['at']}")
        if wd["hpa"] != 250 or wd["segments"] < 1000:
            fail.append(f"winds at FL340: {wd}")
    if r["wind180"]["hpa"] != 500:
        fail.append(f"FL180 did not switch the winds to 500 hPa: {r['wind180']}")
    pl = r["play"]
    steps = pl["steps"]
    if not pl["ended_live"]:
        fail.append(f"the time-lapse did not end at live: {pl}")
    if len(steps) < 10 or any(b - a != 600 for a, b in pairwise(steps)):
        fail.append(
            f"the time-lapse did not step 10 minutes at a time from 2 hours back: {steps}"
        )
    if pl["incomplete"]:
        fail.append(
            f"the time-lapse left steps before they were on screen: {pl['incomplete']}"
        )
    if timing and pl["buffer_ms"] > 2000:
        fail.append(
            f"the time-lapse buffered {pl['buffer_ms']} ms before playing: its steps should have been fetched ahead"
        )
    if pl["blank_frames"]:
        fail.append(
            f"the time-lapse showed a layer blank in {pl['blank_frames']} of {pl['frames']} frames (the flash)"
        )
    if timing and pl["max_gap_ms"] is not None and pl["max_gap_ms"] > 1200:
        fail.append(
            f"the time-lapse stalled: {pl['max_gap_ms']} ms between steps (buffered, it should not)"
        )
    if timing and pl["ms"] > 60000:
        fail.append(f"the time-lapse took {pl['ms']} ms for {len(steps)} steps")
    c = r["changed"]
    if c["at"] != c["want"] or not c["ready"]:
        fail.append(f"a change of time mid-load did not settle on the new time: {c}")
    if c["six_on_gpu"]:
        fail.append(
            f"{c['six_on_gpu']} textures uploaded for the time abandoned mid-load"
        )
    lv = r["levels"]
    if not (
        lv["all"] >= lv["180"] and lv["all"] > min(lv["100"], lv["180"], lv["340"])
    ):
        fail.append(f"the flight-level filter changed nothing: {lv}")
    cell = p.get("cell_css") or 0
    if not p.get("changed"):
        fail.append("probe: the lit cell drew nothing")
    else:
        dx, dy = p["off_by_css"]
        bw, bh = p["box_css"]
        tall = cell / math.cos(math.radians(34.0105))
        if abs(dx) > cell / 2 or abs(dy) > tall / 2:
            fail.append(
                f"probe: cell drawn {dx}, {dy} px from the pier (half a cell is {cell / 2:.0f} x {tall / 2:.0f})"
            )
        if abs(bw - cell) > 2 or abs(bh - tall) > 2:
            fail.append(
                f"probe: cell drawn {bw} x {bh} px, expected {cell} x {tall:.0f}"
            )
    print(
        f"weather: time-lapse buffer {pl['buffer_ms']} ms, then {len(pl['steps'])} steps in {pl['ms']} ms, longest gap {pl['max_gap_ms']} ms, blank frames {pl['blank_frames']} of {pl['frames']}"
    )
    print(
        f"weather: world: global sat {wo['shown_global']}, SIGMETs {wo['sigmets']} (3 h back {past['sigmets']}), METARs {w['metars']}, SIGMET popup {(r['sigmetPopup'] or '')[:50]!r}; winds {wd}, FL180 {r['wind180']}, cost {r['windCost']}; time-lapse {len(steps)} steps in {pl['ms'] / 1000:.1f} s, ended live {pl['ended_live']}; METARs {w['metars']} stations, popup {(r['popup'] or '')[:60]!r}; live ready in {r['live_ms'] / 1000:.1f} s, 3 h back in {r['past_ms'] / 1000:.1f} s, changed mid-load {c}; GPU {past['gpu_bytes'] / 1e6:.0f} MB; hazards by level {lv}; probe {p}"
    )
    if fail:
        print("weather: FAILED:\n  " + "\n  ".join(fail))
        sys.exit(1)
    print("weather: all passed")


def wind(path):
    bench = [
        l
        for l in Path(path).read_text(encoding="utf-8").split("\n")
        if l.startswith("[bench] ")
    ]
    if not bench:
        sys.exit(f"wind motion: FAILED: no [bench] line ({path})")
    r = json.loads(bench[0][8:])
    c, st = r["coverage"], r["stats"]
    before = c["0-before"]
    # the slow moves keep their trails; the fast ones start some again in the revealed area, which is fine,
    # but at every moment the particles must cover the screen evenly and no trail may be stretched
    slow = [
        "1-pan-mid",
        "2-pan-after",
        "3-zoom-mid",
        "4-zoom-after",
        "6-wheel-mid",
        "7-wheel-after",
        "16-atlantic",
        "17-atlantic-fastpan-after",
    ]
    low = {k: c[k] for k in slow if c[k] < 0.6 * before}
    bunched = {k: v["cv"] for k, v in st.items() if v["cv"] is None or v["cv"] > 0.35}
    stretched = {k: v["long"] for k, v in st.items() if v["long"]}
    print(
        "wind motion: "
        + ", ".join(
            f"{k} {v['segments']} seg cv {v['cv']}"
            for k, v in sorted(st.items(), key=lambda x: int(x[0].split("-")[0]))
        )
    )
    fail = []
    if before < 10000 or low:
        fail.append(f"trails lost through a slow move (under 60% of {before}): {low}")
    if bunched:
        fail.append(f"particles bunched (evenness over 0.35): {bunched}")
    if stretched:
        fail.append(f"trail segments stretched by the camera: {stretched}")
    if fail:
        sys.exit("wind motion: FAILED:\n  " + "\n  ".join(fail))
    print(
        "wind motion: trails kept, even, unstretched through slow and fast pans and zooms"
    )


if __name__ == "__main__":
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    if len(args) != 2 or args[0] not in ("weather", "wind"):
        sys.exit(__doc__)
    if args[0] == "weather":
        weather(args[1], timing="--no-timing" not in sys.argv)
    else:
        wind(args[1])
