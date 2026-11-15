#!/usr/bin/env bash
# Test coverage for both halves. Rust: source-based coverage (-C instrument-coverage) with the
# llvm-cov and llvm-profdata the nightly toolchain ships, so nothing extra is installed; built in
# logs/cov/target so the normal build cache is untouched. JS: node's built-in coverage.
#   tools/coverage.sh            per-file summary of both
#   tools/coverage.sh FILE.rs    also the uncovered lines of one Rust file
set -euo pipefail
cd "$(dirname "$0")/.."
TC=${COV_TOOLCHAIN:-nightly-2026-04-16}
BIN=$(rustc +"$TC" --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin
OUT=logs/cov
rm -rf "$OUT"/*.profraw "$OUT"/all.profdata
mkdir -p "$OUT"
CARGO_TARGET_DIR=$PWD/$OUT/target RUSTFLAGS="-C instrument-coverage" LLVM_PROFILE_FILE=$PWD/$OUT/%p-%m.profraw \
  cargo +"$TC" test -q --workspace --all-features > "$OUT/test.log" 2>&1 || { tail -30 "$OUT/test.log"; exit 1; }
"$BIN/llvm-profdata" merge -sparse "$OUT"/*.profraw -o "$OUT/all.profdata"
# the first binary is positional (llvm-cov reads any later positional as a source file), the rest -object
objs=()
while IFS= read -r f; do
  if [ ${#objs[@]} -eq 0 ]; then objs=("$f"); else objs+=(-object "$f"); fi
done < <(find "$OUT/target/debug/deps" -maxdepth 1 -type f -executable ! -name '*.so')
ignore='(\.cargo|rustc|/target/|/tests/)'
echo "Rust (unit and integration tests; line coverage)"
"$BIN/llvm-cov" report "${objs[@]}" -instr-profile="$OUT/all.profdata" --ignore-filename-regex="$ignore" \
  | awk 'NF > 8 && $1 != "Filename" { printf "  %-32s %8s\n", $1, $10 }'
if [ $# -gt 0 ]; then
  "$BIN/llvm-cov" show "${objs[@]}" -instr-profile="$OUT/all.profdata" --show-line-counts-or-regions "$1" \
    | grep -E '^ *[0-9]+\| *0\|' || true
fi
echo "JS (node --test)"
node --test --experimental-test-coverage tests/*.test.js 2>&1 \
  | sed -n '/start of coverage/,/end of coverage/p' | grep -E '\.js +\|' | grep -v test.js \
  | awk -F'|' '{ gsub(/^# +/, "", $1); printf "  %-32s %7s%%\n", $1, $2 }'
