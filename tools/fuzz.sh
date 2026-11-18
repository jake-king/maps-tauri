#!/usr/bin/env bash
# Coverage-guided fuzzing of the parsers that read bytes from elsewhere (fuzz/, cargo-fuzz with
# libFuzzer, on the pinned nightly below: the sanitizer flags it needs are nightly-only).
#   tools/fuzz.sh seeds [TAPE_DIR] [ARCHIVE]  seed every target's corpus from a recorded tape and a
#                                             map archive (default fixtures/tape, data/westla.pmtiles)
#   tools/fuzz.sh run SECONDS [TARGET...]     fuzz each target (default: all) for SECONDS, from its
#                                             corpus and its committed regressions
#   tools/fuzz.sh cmin [TARGET...]            shrink each corpus to the inputs that add coverage
# The corpus grows as the fuzzer finds new paths; it lives in $FUZZ_CORPUS (default logs/fuzz/corpus;
# CI keeps it in the runner's cache, so each run starts where the last stopped). An input that
# crashes a target, or runs it out of time or memory, is saved under logs/fuzz/artifacts/TARGET and
# fails the run. Once fixed, it goes in fuzz/regressions/TARGET, which every run replays.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
NIGHTLY=nightly-2026-07-11   # rustc 1.99.0-nightly: the stable 1.99 line the app is built with
CARGO_FUZZ=0.13.2
TARGETS=(archive pmtiles_dir mvt_mesh grib radar_gunzip wx_text coords)
corpus=${FUZZ_CORPUS:-$PWD/logs/fuzz/corpus}
artifacts=$PWD/logs/fuzz/artifacts

setup() {
  rustup toolchain list | grep -q "^$NIGHTLY" || rustup toolchain install "$NIGHTLY" --profile minimal
  if [[ $(cargo fuzz --version 2>/dev/null) != "cargo-fuzz $CARGO_FUZZ" ]]; then
    cargo install cargo-fuzz --version "$CARGO_FUZZ" --locked
  fi
}

case ${1:-} in
  seeds)
    tape=${2:-fixtures/tape} archive=${3:-data/westla.pmtiles}
    python3 tools/fuzz_seeds.py "$tape" "$corpus"
    cargo run -q --release -p vtile --example fuzz_seeds "$archive" "$corpus"
    ;;
  run)
    secs=${2:?usage: tools/fuzz.sh run SECONDS [TARGET...]}
    shift 2
    (($#)) && TARGETS=("$@")
    setup
    # -a: debug assertions and overflow checks on, so arithmetic that would wrap in a release build
    # (an offset, a count) is a finding, not a silent wrong answer
    cargo "+$NIGHTLY" fuzz build -O -a
    failed=()
    for t in "${TARGETS[@]}"; do
      mkdir -p "$corpus/$t" "$artifacts/$t" "fuzz/regressions/$t"
      echo "fuzz: $t, ${secs}s, corpus $(find "$corpus/$t" -type f | wc -l) inputs"
      # -rss_limit_mb: a parser that allocates gigabytes on a small input is a finding too
      if ! cargo "+$NIGHTLY" fuzz run -O -a "$t" "$corpus/$t" "fuzz/regressions/$t" -- \
          -max_total_time="$secs" -timeout=10 -rss_limit_mb=2048 -print_final_stats=1 \
          -artifact_prefix="$artifacts/$t/" >"logs/fuzz/$t.log" 2>&1; then
        failed+=("$t")
        grep -E '^(==[0-9]+==ERROR|thread .* panicked|attempt to|SUMMARY|Test unit written)' \
          "logs/fuzz/$t.log" | head -8
        # CI keeps no files from a job, so a small input is printed: base64 -d it into a file and
        # replay it with cargo +$NIGHTLY fuzz run -O -a TARGET FILE
        for f in "$artifacts/$t"/*; do
          [[ -f $f && $(stat -c %s "$f") -le 65536 ]] || continue
          echo "  ${f##*/} (base64):"
          base64 -w 100 "$f" | sed 's/^/    /'
        done
      fi
      grep -E '^stat::(number_of_executed_units|peak_rss_mb|new_units_added)' "logs/fuzz/$t.log" \
        | sed 's/^/  /' || true
    done
    ((${#failed[@]} == 0)) || { echo "fuzz: FAILED: ${failed[*]} (inputs in $artifacts)" >&2; exit 1; }
    echo "fuzz: no crash in ${#TARGETS[@]} targets"
    ;;
  cmin)
    shift
    (($#)) && TARGETS=("$@")
    setup
    for t in "${TARGETS[@]}"; do
      [[ -d $corpus/$t ]] || continue
      before=$(find "$corpus/$t" -type f | wc -l)
      cargo "+$NIGHTLY" fuzz cmin -O -a "$t" "$corpus/$t" >"logs/fuzz/$t-cmin.log" 2>&1
      echo "fuzz: $t corpus $before -> $(find "$corpus/$t" -type f | wc -l) inputs"
    done
    ;;
  *) echo "usage: tools/fuzz.sh seeds [TAPE_DIR] [ARCHIVE] | run SECONDS [TARGET...] | cmin [TARGET...]" >&2; exit 2 ;;
esac
