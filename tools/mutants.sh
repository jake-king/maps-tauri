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
# A mutant can turn a loop into one that never ends and allocates on every pass (dash_polyline in
# crates/vtile/src/mesh.rs with `d += take` made `d -= take` grows by about 0.5 GB a second). It
# reaches the machine's memory long before the test timeout, and the kernel then kills whatever it
# picks: on GitHub the runner, and the whole shard with it. So every test binary (doctests too) runs
# under a 4 GiB address-space cap, through cargo's target runner: the allocation that crosses it
# fails, the test aborts, and the mutant counts as caught. The runner is used only to run what was
# built, so rustc, the linker and build scripts stay uncapped. The largest test binary (the app
# crate's) needs about 1.2 GiB of address space at 16 to 32 test threads (measured; it grows with
# the thread count), so 4 GiB leaves room, and four mutants at a time stay under a 16 GB runner.
command -v prlimit >/dev/null || { echo "mutants: prlimit (util-linux) not found: no memory cap for the tests" >&2; exit 1; }
host=$(rustc -vV | sed -n 's/^host: //p')
[[ -n $host ]] || { echo "mutants: no host triple from rustc -vV" >&2; exit 1; }
export "CARGO_TARGET_$(tr 'a-z-' 'A-Z_' <<<"$host")_RUNNER=prlimit --as=$((4 << 30)) --"
exec cargo mutants --output logs/mutants --cap-lints true "${jobs[@]}" "$@"
