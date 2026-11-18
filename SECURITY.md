# Security policy

## Reporting a vulnerability

Please report a vulnerability privately, not in a public issue.

Email **security@king-data.org**. If the report holds anything sensitive (a working exploit, a
crafted file), encrypt it to the project's OpenPGP key, [`security-key.asc`](security-key.asc) in
this repository:

```text
A593 660A FF7D 1EA6 BF31  D219 78BF 11F6 1616 03D7
```

Check the fingerprint of the key you use against the one above. The key expires on 2028-11-17 and
is extended before then; a key with this address and another fingerprint is not this project's.

A useful report says what is affected (version or commit), how to reproduce it, and what an
attacker gains. A crafted input file is welcome: the parsers that read untrusted data are listed
below.

## What happens next

This is a project maintained by one person, so the times are what one person can promise:

- an acknowledgement within 7 days;
- an assessment (is it real, how severe, which versions) within 30 days;
- a fix in a new release, and a public advisory naming the reporter if they wish, once users can
  update. The default for disclosure is 90 days from the report, sooner when a fix ships sooner,
  and later only by agreement.

## Supported versions

Fixes go into the newest release only. The project has not reached 1.0, and older releases are not
patched.

## Scope

In scope: the app and its Rust crates in this repository, and the release artifacts built from it.
The code that reads data from outside is the most likely place for a flaw, and is fuzzed for that
reason (`fuzz/`): the PMTiles archive and vector tile decoders (`crates/vtile`), NOAA's GRIB2, radar,
METAR, SIGMET and G-AIRMET formats (`crates/wx`), and the search box's coordinate and query parsing
(`crates/coords`).

A flaw in a dependency belongs with its project. Report it there; if this project is affected in a
way the dependency's advisory does not cover, report that here too.

## Verifying a release

Every release ships a `SHA256SUMS` signed with the project's minisign key, `release-signing.pub` in
this repository:

```text
minisign -Vm SHA256SUMS -p release-signing.pub && sha256sum -c --ignore-missing SHA256SUMS
```

The release key and the reporting key are separate: one signs what the project publishes, the other
receives what is sent to it.
