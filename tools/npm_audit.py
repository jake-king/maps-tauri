#!/usr/bin/env python3
"""npm's security advisories for the JavaScript dependencies, with a reviewed allowlist.

`npm audit` has no way to accept an advisory, so a single unfixable one would leave it red forever,
and a check that is always red gets ignored. This runs `npm audit --json` and fails on every
advisory except those in tools/npm-audit-allow.json, where each entry names the advisory, the
reason it is accepted, and a date after which it fails again and has to be looked at. An entry
that has expired or no longer matches anything also fails, so the list cannot outlive its reasons.

    python3 tools/npm_audit.py                 run npm audit and judge it
    python3 tools/npm_audit.py --report FILE   judge a saved `npm audit --json` report

Exit 0: nothing outside the allowlist. 1: an advisory to act on, or a stale entry. 2: npm audit
itself failed (no network, no lockfile), never read as a pass.
"""

import datetime
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
ALLOW = ROOT / "tools" / "npm-audit-allow.json"


def advisories(report):
    """The advisories in a report, by id: {GHSA id: (package, severity, title)}."""
    if "vulnerabilities" not in report:
        raise ValueError(
            report.get("error", {}).get("summary")
            or "no 'vulnerabilities' in the report"
        )
    found = {}
    for vuln in report["vulnerabilities"].values():
        for via in vuln["via"]:
            # a string names another vulnerable package (the path to it); a dict is an advisory
            if isinstance(via, dict):
                ghsa = via["url"].rsplit("/", 1)[-1]
                found[ghsa] = (via["name"], via["severity"], via["title"])
    return found


def judge(found, allow, today):
    """The problems, one line each: advisories not allowed, and allowlist entries that are stale."""
    problems = []
    for ghsa, (pkg, severity, title) in sorted(found.items()):
        entry = allow.get(ghsa)
        if entry is None:
            problems.append(f"{ghsa} {pkg} ({severity}): {title}")
        elif datetime.date.fromisoformat(entry["until"]) < today:
            problems.append(
                f"{ghsa} {pkg}: accepted until {entry['until']}, look at it again"
            )
    for ghsa in sorted(set(allow) - set(found)):
        problems.append(f"{ghsa}: in the allowlist but no longer reported, remove it")
    return problems


def main(argv):
    if argv[:1] == ["--report"]:
        text = pathlib.Path(argv[1]).read_text()
    else:
        # npm audit exits 1 when it finds anything, so its output, not its status, is the verdict
        run = subprocess.run(
            ["npm", "audit", "--json"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )
        text = run.stdout
    try:
        found = advisories(json.loads(text))
    except (ValueError, KeyError) as e:
        print(f"npm-audit: npm audit gave no usable report: {e}", file=sys.stderr)
        return 2
    allow = {e["id"]: e for e in json.loads(ALLOW.read_text())["accepted"]}
    for entry in allow.values():
        if not entry.get("reason") or not entry.get("until"):
            print(
                f"npm-audit: {entry['id']} needs a reason and an until date",
                file=sys.stderr,
            )
            return 1
    problems = judge(found, allow, datetime.date.today())
    for line in problems:
        print(f"npm-audit: {line}", file=sys.stderr)
    if problems:
        return 1
    print(
        f"npm-audit: {len(found)} advisories, all accepted in {ALLOW.relative_to(ROOT)}"
        if found
        else "npm-audit: no advisories"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
