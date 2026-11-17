#!/usr/bin/env python3
"""Run the app's self-driving end-to-end checks on any platform and judge its report.

  e2e.py [--tape DIR | --record DIR] [--no-timing] [--xvfb] [--app PATH] [--out DIR]

Two runs: the weather run (MAPS_WEATHER=1) and the wind-trail motion run (MAPS_WIND_MOTION=1), each
judged by e2e_check.py. --tape replays NOAA's answers from a recorded tape (wx::tape), so every
platform draws the same data with no network; --record makes one. Snapshots and logs go to --out.
The app is the release build in target/release unless --app says otherwise. --xvfb (Linux, no
display) starts a virtual display first and waits for it through Xvfb's -displayfd pipe: xvfb-run's
ready handshake (a SIGUSR1 from Xvfb to the script) was seen lost in CI's container, leaving the run
parked forever with the display already up.
"""

import argparse
import os
import platform
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))
import e2e_check


def run(app, env, log, timeout):
    with open(log, "w", encoding="utf-8") as f:
        try:
            subprocess.run(
                [str(app)],
                env=env,
                stdout=f,
                stderr=subprocess.STDOUT,
                timeout=timeout,
                cwd=ROOT,
                check=False,
            )
        except subprocess.TimeoutExpired:
            pass  # judged by its report: a run that never reported fails the check


def xvfb():
    """Start Xvfb on a free display and return (process, ":N") once it accepts connections."""
    r, w = os.pipe()
    x = subprocess.Popen(
        [
            "Xvfb",
            "-displayfd",
            str(w),
            "-screen",
            "0",
            "1280x900x24",
            "-nolisten",
            "tcp",
        ],
        pass_fds=(w,),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    os.close(w)
    with os.fdopen(r) as f:
        n = f.readline().strip()  # written once the server is ready; EOF if it died
    if not n.isdigit():
        x.kill()
        sys.exit(f"Xvfb did not start (exit {x.wait()})")
    return x, f":{n}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tape")
    ap.add_argument("--record")
    ap.add_argument("--no-timing", action="store_true")
    ap.add_argument("--xvfb", action="store_true")
    ap.add_argument("--app")
    ap.add_argument("--out", default=str(ROOT / "logs" / "e2e"))
    a = ap.parse_args()
    exe = "maps-tauri.exe" if platform.system() == "Windows" else "maps-tauri"
    app = Path(a.app) if a.app else ROOT / "target" / "release" / exe
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    db = out / "weather.sqlite"
    for p in out.glob("*.sqlite*"):
        p.unlink()
    env = dict(
        os.environ,
        MAPS_ARCHIVE="westla.pmtiles",
        MAPS_LOCAL_ONLY="1",
        MAPS_BENCH="1",
        MAPS_F4="0",
        MAPS_RENDERER="rust",
        MAPS_WEATHER_CACHE=str(db),
        MAPS_SNAPSHOT_DIR=str(out),
    )
    if a.tape:
        if not (Path(a.tape) / "now.txt").is_file():
            sys.exit(f"{a.tape}: not a tape (no now.txt)")
        env["MAPS_TAPE_REPLAY"] = str(Path(a.tape).resolve())
    if a.record:
        env["MAPS_TAPE_RECORD"] = str(Path(a.record).resolve())
    display = None
    if a.xvfb:
        display, env["DISPLAY"] = xvfb()
    ok = True
    for name, extra, timeout, check in (
        (
            "weather",
            {"MAPS_WEATHER": "1"},
            400,
            lambda log: e2e_check.weather(log, timing=not a.no_timing),
        ),
        (
            "wind",
            {"MAPS_WIND_MOTION": "1"},
            180,
            lambda log: e2e_check.wind(log, timing=not a.no_timing),
        ),
    ):
        log = out / f"{name}.log"
        run(app, dict(env, **extra), log, timeout)
        try:
            check(str(log))
        except SystemExit as e:
            if e.code not in (0, None):
                print(e.code if isinstance(e.code, str) else f"{name}: FAILED")
                ok = False
    if display:
        display.terminate()
        display.wait()
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
