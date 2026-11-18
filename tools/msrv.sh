#!/usr/bin/env bash
# The minimum supported Rust version: the workspace, every target and feature, and the WebAssembly
# crate build with the Rust named by `rust-version` in Cargo.toml, not only with the newer one
# rust-toolchain.toml pins for the gate.
#   tools/msrv.sh
# Exit 0: it builds; 1: it does not (a newer language feature or std API slipped in); 2: the check
# could not run (no rust-version, members that disagree, the toolchain would not install).
#
# The build is `cargo check`: what the version decides is whether the code compiles, and a test run
# would only repeat the gate on an older std. Its build tree is target/msrv, so it does not throw
# away the gate's.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH

# every workspace member names the same rust-version (the workspace's, by inheritance)
versions=$(cargo metadata --format-version 1 --no-deps --locked | python3 -c '
import json, sys
d = json.load(sys.stdin)
for p in d["packages"]:
    print(p["name"], p.get("rust_version") or "-")')
msrv=$(awk '{print $2}' <<<"$versions" | sort -u)
if [[ $msrv == *-* || $(wc -l <<<"$msrv") != 1 ]]; then
  echo "msrv: every member needs the workspace's rust-version (rust-version.workspace = true):" >&2
  echo "$versions" >&2
  exit 2
fi
echo "msrv: building with Rust $msrv ($(wc -l <<<"$versions") members)"

rustup toolchain install "$msrv" --profile minimal --target wasm32-unknown-unknown >/dev/null 2>&1 ||
  { echo "msrv: rustup could not install Rust $msrv" >&2; exit 2; }
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}/msrv
mkdir -p logs
rc=0
cargo "+$msrv" check --workspace --all-targets --all-features --locked >logs/msrv-host.log 2>&1 || rc=1
# the search box's crate is also built for the browser
cargo "+$msrv" check -p coords --lib --target wasm32-unknown-unknown --locked >logs/msrv-wasm.log 2>&1 || rc=1
if ((rc)); then
  grep -h -E -A12 '^error' logs/msrv-host.log logs/msrv-wasm.log | head -80 >&2 || true
  echo "msrv: the workspace does not build with Rust $msrv. Use what $msrv has, or raise" >&2
  echo "msrv: rust-version in Cargo.toml on purpose (and say why in the commit)" >&2
  exit 1
fi
echo "msrv: builds with Rust $msrv (host, all targets and features; coords for wasm32)"
