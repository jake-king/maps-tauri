#!/usr/bin/env bash
# The performance gate: the benchmarks (crates/vtile/benches, crates/wx/benches) run on this commit
# and on the commit it is compared with, and any benchmark that executes more than 3% more
# instructions fails the gate.
#   tools/perf.sh          compare HEAD with its merge base on main (or, on main itself, with the
#                          commit main was at before this push: $BEFORE, else HEAD~1)
#   PERF_BASE=REV tools/perf.sh    compare with REV instead
# Exit 0: no regression, or each one accepted; 1: a regression; 2: the gate could not measure.
#
# Instructions are counted under Valgrind's Callgrind (gungraun), not timed: the forge's runner is
# shared, and a wall-clock benchmark there measures its neighbours. A count is repeatable to the
# instruction for the decoders and within 0.03% for the mesher (its label placement hashes), so 3%
# is a real change. It does not see what a count cannot: cache misses, memory bandwidth, SIMD
# width. The inputs are the end-to-end fixtures (data/westla.pmtiles, fixtures/tape).
#
# A regression that is the price of a change (a check added on a hot path) is accepted by a commit
# on the branch carrying the trailer `Perf-accepted: <why>`; the reason is printed with the result.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
GUNGRAUN=0.20.0
LIMIT=${PERF_LIMIT:-3%}
# package and bench target of each benchmark suite
SUITES=("vtile tiles" "wx decode")

command -v valgrind >/dev/null || { echo "perf: valgrind is needed (apt install valgrind)" >&2; exit 2; }
if [[ $(gungraun-runner --version 2>/dev/null) != "gungraun-runner $GUNGRAUN" ]]; then
  cargo install gungraun-runner --version "$GUNGRAUN" --locked
fi
[[ -f data/westla.pmtiles && -f fixtures/tape/index.tsv ]] ||
  { echo "perf: no fixtures (python3 tools/fixtures.py unpack ARCHIVE)" >&2; exit 2; }
# gungraun runs Valgrind under `setarch -R` for a fixed address layout; a container's seccomp
# policy refuses that personality() call. Measured without it: the counts above.
export GUNGRAUN_ALLOW_ASLR=yes

# --- what to compare with
main=""
for ref in refs/remotes/origin/main refs/remotes/forge/main; do
  git rev-parse -q --verify "$ref^{commit}" >/dev/null && { main=$ref; break; }
done
if [[ -n ${PERF_BASE:-} ]]; then
  base=$(git rev-parse --verify "$PERF_BASE^{commit}")
else
  [[ -n $main ]] || { echo "perf: no main to compare with (fetch it, or set PERF_BASE)" >&2; exit 2; }
  base=$(git merge-base HEAD "$main")
  if [[ $base == $(git rev-parse HEAD) ]]; then
    base=${BEFORE:-}
    if [[ -z $base || $base =~ ^0+$ ]] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
      base=$(git rev-parse HEAD~1)
    fi
  fi
fi
echo "perf: $(git rev-parse --short HEAD) against $(git rev-parse --short "$base"), limit +$LIMIT instructions"

work=$PWD/logs/perf
home=$work/gungraun
rm -rf "$home"
mkdir -p "$work"
tree=$work/base
git worktree remove --force "$tree" 2>/dev/null || rm -rf "$tree"
git worktree prune
trap 'git worktree remove --force "$tree" 2>/dev/null || true' EXIT
git worktree add -q --detach "$tree" "$base"
# the base tree reads the same fixtures
export MAPS_BENCH_ARCHIVE=$PWD/data/westla.pmtiles MAPS_BENCH_TAPE=$PWD/fixtures/tape
# gungraun clears the environment before it starts Valgrind: these two are passed through by name
envs=(--envs="MAPS_BENCH_ARCHIVE MAPS_BENCH_TAPE")
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
# The base builds in a target directory of its own. Shared, cargo names both builds' artifacts the
# same, and its freshness check, reading the dependency list the base build wrote, finds the base
# tree's files unchanged and never rebuilds the head: both sides would measure the base (seen: a
# planted slowdown read as "No change").
base_target=${PERF_BASE_TARGET:-$CARGO_TARGET_DIR/perf-base}

rc=0
for suite in "${SUITES[@]}"; do
  read -r pkg bench <<<"$suite"
  file=crates/$pkg/benches/$bench.rs
  if ! git cat-file -e "$base:$file" 2>/dev/null; then
    echo "perf: $pkg/$bench is new: measured, nothing to compare with"
    (cargo bench -q -p "$pkg" --bench "$bench" -- --home="$home" "${envs[@]}") || exit 2
    continue
  fi
  (cd "$tree" && CARGO_TARGET_DIR=$base_target cargo bench -q -p "$pkg" --bench "$bench" -- --home="$home" "${envs[@]}" --save-baseline=base) \
    >"$work/$bench-base.log" 2>&1 || { cat "$work/$bench-base.log"; echo "perf: the base run of $pkg/$bench failed" >&2; exit 2; }
  # gungraun, given --baseline with no saved baseline, saves one and compares nothing: a base run
  # that wrote nothing would pass every benchmark. Fail instead.
  [[ -n $(find "$home/$pkg/$bench" -name '*.out.base@base' -print -quit 2>/dev/null) ]] ||
    { echo "perf: the base run of $pkg/$bench saved no baseline" >&2; exit 2; }
  # the guard for the trap above: a head build whose dependency lists name the base tree is stale
  if grep -qsF "$tree/" "$CARGO_TARGET_DIR"/release/deps/*.d; then
    echo "perf: the head build was made from the base tree's sources: rebuilding it"
    cargo clean -q --release -p "$pkg"
  fi
  r=0
  cargo bench -q -p "$pkg" --bench "$bench" -- --home="$home" "${envs[@]}" --baseline=base \
    --callgrind-limits="ir=$LIMIT" >"$work/$bench.log" 2>&1 || r=$?
  grep -E '^[a-z_]+::|Instructions:|Regression|exceeded' "$work/$bench.log" || true
  case $r in
    0) ;;
    3) rc=1 ;;
    *) cat "$work/$bench.log"; echo "perf: $pkg/$bench failed to run (exit $r)" >&2; exit 2 ;;
  esac
done

if ((rc)); then
  accepted=$(git log --format='%(trailers:key=Perf-accepted,valueonly)' "$base..HEAD" | grep . || true)
  if [[ -n $accepted ]]; then
    echo "perf: regression accepted by a Perf-accepted trailer:"
    while IFS= read -r line; do echo "  $line"; done <<<"$accepted"
    exit 0
  fi
  echo "perf: an instruction count rose by more than $LIMIT. Make it faster, or, if the cost is the" >&2
  echo "perf: point of the change, say so in a commit trailer: Perf-accepted: <why>" >&2
  exit 1
fi
echo "perf: no benchmark over +$LIMIT"
