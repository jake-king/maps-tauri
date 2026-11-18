#!/usr/bin/env bash
# Mutation testing with cargo-mutants (version pinned in tools/forge-job.sh; settings in .cargo/mutants.toml):
# each mutant is a small deliberate bug, and a mutant the tests do not catch is a gap in the tests.
#   tools/mutants.sh                    every mutant (~2,600; hours; CI shards it: --shard k/n)
#   tools/mutants.sh --in-diff FILE     only mutants on lines a diff touches (what CI runs per push)
#   tools/mutants.sh -f PATH ...        any other cargo-mutants arguments
# Results go to logs/mutants/mutants.out (missed.txt lists the mutants no test caught).
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
# Scratch trees on disk under logs/, never /tmp (memory on the dev box): one per job
export TMPDIR=$PWD/logs/mutants-tmp
mkdir -p "$TMPDIR" logs/mutants
# --in-place mutates the checkout itself, one mutant at a time, and cargo-mutants refuses --jobs with it
jobs=(-j "${MUTANTS_JOBS:-4}")
for a in "$@"; do [[ $a == --in-place ]] && jobs=(); done
# A copied tree builds from nothing, once per job: without debuginfo it is 1.7 GB, not several, and
# links faster. Not in place, where the build tree is the one the gate keeps warm.
((${#jobs[@]})) && export CARGO_PROFILE_DEV_DEBUG=0
exec cargo mutants --output logs/mutants --cap-lints true "${jobs[@]}" "$@"
