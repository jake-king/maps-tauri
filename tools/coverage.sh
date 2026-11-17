#!/usr/bin/env bash
# Test coverage for both halves. Rust: source-based coverage (-C instrument-coverage) on the pinned
# toolchain (rust-toolchain.toml, the gate's compiler), read with the llvm-cov and llvm-profdata of
# its llvm-tools component; built in logs/cov/target ($COV_DIR) so the normal build cache is untouched.
# JS: node's built-in coverage over tests/*.test.js.
#   tools/coverage.sh                per-file summary of both, and the totals
#   tools/coverage.sh FILE.rs        also the uncovered lines of one Rust file
#   tools/coverage.sh --html DIR     also the Rust report as HTML (per file, line by line) in DIR
#   tools/coverage.sh --floor        fail if either total is below tools/coverage-floor.txt
# Only the unit and integration tests are instrumented. The gate's decoder diff, whole-archive mesh
# check, search parity and the end-to-end tape exercise more than this reports.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
html='' floor=0 file=''
while (($#)); do
  case $1 in
    --html) html=${2:?--html needs a directory}; shift 2 ;;
    --floor) floor=1; shift ;;
    *.rs) file=$1; shift ;;
    *) echo "usage: tools/coverage.sh [FILE.rs] [--html DIR] [--floor]" >&2; exit 2 ;;
  esac
done

rustup component list --installed | grep -q '^llvm-tools' || rustup component add llvm-tools
BIN=$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin
# COV_DIR: where the instrumented build and profiles go (CI points it at the cache between jobs)
OUT=${COV_DIR:-$PWD/logs/cov}
mkdir -p "$OUT"
rm -rf "$OUT"/*.profraw "$OUT"/all.profdata   # left by the earlier layout
# This run's profiles and logs in a directory of its own: two runs may share the build tree (a
# second CI job on the same cache), never each other's profiles. The raw profiles go at exit.
find "$OUT" -maxdepth 1 -name "run.*" -mmin +1440 -exec rm -rf {} +   # a day's runs are kept
RUN=$(mktemp -d -p "$OUT" run.XXXX)
trap 'rm -f "$RUN"/*.profraw' EXIT
# Source paths are remapped to the repository root, so the report names crates/... and not where the
# checkout happens to live (the HTML report is published with the docs).
export CARGO_TARGET_DIR=$OUT/target LLVM_PROFILE_FILE=$RUN/%p-%m.profraw
export RUSTFLAGS="-C instrument-coverage --remap-path-prefix=$PWD=."
# The test binaries of THIS build, as cargo reports them: a reused target directory also holds the
# binaries of earlier builds, and mixing those in gives llvm-cov "mismatched data" and wrong counts.
cargo test -q --workspace --all-features --no-run --message-format=json 2>"$RUN/build.log" \
  | python3 -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") and m.get("executable"):
        print(m["executable"])' >"$RUN/binaries" || { tail -30 "$RUN/build.log"; exit 1; }
cargo test -q --workspace --all-features >"$RUN/test.log" 2>&1 || { tail -30 "$RUN/test.log"; exit 1; }
"$BIN/llvm-profdata" merge -sparse "$RUN"/*.profraw -o "$RUN/all.profdata"
# the first binary is positional (llvm-cov reads any later positional as a source file), the rest -object
objs=()
while IFS= read -r f; do
  if ((${#objs[@]} == 0)); then objs=("$f"); else objs+=(-object "$f"); fi
done <"$RUN/binaries"
((${#objs[@]})) || { echo "coverage: cargo reported no test binaries" >&2; exit 1; }
# Only the workspace's own files are reported. Their paths were remapped to relative ones at build
# time; everything else (dependencies wherever cargo keeps its registry, the standard library) keeps
# an absolute path, so the rule needs no list of where those live. An earlier pattern, `\.cargo`,
# missed CI's registry under the runner's cache and counted every dependency: 6% instead of 81%.
cov=("${objs[@]}" -instr-profile="$RUN/all.profdata" --ignore-filename-regex='^/|(^|/)tests/')

echo "Rust (unit and integration tests; line coverage)"
"$BIN/llvm-cov" report -use-color=false "${cov[@]}" >"$RUN/report.txt" 2>"$RUN/report.err"
# A crate whose functions differ under cfg(test) is compiled twice (its own unit-test binary, and
# the library its integration tests link), and llvm-cov cannot merge the two: a few "mismatched
# data" functions are expected. Before the binaries came from cargo's list, stale ones made 17.
sed 's/^/  note: /' "$RUN/report.err"
awk 'NF > 8 && $1 != "Filename" { printf "  %-32s %8s\n", $1, $10 }' "$RUN/report.txt"
rust_total=$(awk '$1 == "TOTAL" { sub(/%/, "", $10); print $10 }' "$RUN/report.txt")
if [[ -n $file ]]; then
  "$BIN/llvm-cov" show "${cov[@]}" --show-line-counts-or-regions "$file" | grep -E '^ *[0-9]+\| *0\|' || true
fi
if [[ -n $html ]]; then
  rm -rf "$html"
  "$BIN/llvm-cov" show "${cov[@]}" -format=html -output-dir="$html" -show-line-counts-or-regions \
    -project-title="maps-tauri: Rust unit and integration tests"
  echo "  HTML report: $html/index.html"
fi

echo "JS (node --test)"
node --test --experimental-test-coverage tests/*.test.js >"$RUN/js.log" 2>&1 || { tail -30 "$RUN/js.log"; exit 1; }
sed -n '/start of coverage/,/end of coverage/p' "$RUN/js.log" | grep -E '\.js +\|' | grep -v test.js \
  | awk -F'|' '{ gsub(/^# +/, "", $1); printf "  %-32s %7s%%\n", $1, $2 }'
js_total=$(sed -n '/start of coverage/,/end of coverage/p' "$RUN/js.log" | awk -F'|' '/all files/ { gsub(/ /, "", $2); print $2 }')
echo "TOTAL lines: Rust $rust_total%, JS $js_total%"

if ((floor)); then
  # The floor only moves up, by an edit that says why: tighten it when coverage rises.
  read -r rust_floor js_floor < <(grep -v '^#' tools/coverage-floor.txt)
  ok=1
  awk -v t="$rust_total" -v f="$rust_floor" 'BEGIN { exit !(t + 0 >= f + 0) }' \
    || { echo "coverage: Rust lines $rust_total% is below the floor $rust_floor%" >&2; ok=0; }
  awk -v t="$js_total" -v f="$js_floor" 'BEGIN { exit !(t + 0 >= f + 0) }' \
    || { echo "coverage: JS lines $js_total% is below the floor $js_floor%" >&2; ok=0; }
  ((ok)) || exit 1
  echo "coverage: at or above the floor (Rust $rust_floor%, JS $js_floor%)"
fi
