#!/usr/bin/env bash
# What a fresh Ubuntu 24.04 machine needs before tools/check.sh can run: the WebKitGTK and other
# system libraries Tauri links against, shellcheck (Ubuntu's 0.9; the dev box runs 0.11, and the
# scripts must pass both), and ruff at the version the dev box uses. On a
# bare container (tools/ci-local.sh) it also installs rustup and Node; GitHub's runner has rustup,
# and the workflow sets Node up itself. Rust's version comes from rust-toolchain.toml either way.
set -euo pipefail
RUFF=0.17.3
NODE=22
sudo=; [[ $(id -u) == 0 ]] || sudo=sudo
export DEBIAN_FRONTEND=noninteractive
$sudo apt-get update -q
$sudo apt-get install -y -q --no-install-recommends \
  build-essential curl ca-certificates git pkg-config python3 pipx shellcheck xz-utils \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf
pipx install --force "ruff==$RUFF"
if ! command -v rustup >/dev/null; then
  curl -fsS https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal
fi
if ! command -v node >/dev/null; then
  curl -fsS "https://nodejs.org/dist/latest-v$NODE.x/SHASUMS256.txt" -o /tmp/node.sums
  tarball=$(grep -o "node-v${NODE}\.[0-9.]*-linux-x64\.tar\.xz" /tmp/node.sums | head -1)
  curl -fsS "https://nodejs.org/dist/latest-v$NODE.x/$tarball" -o "/tmp/$tarball"
  (cd /tmp && grep " $tarball\$" node.sums | sha256sum -c -)
  $sudo tar -xJf "/tmp/$tarball" -C /usr/local --strip-components=1
fi
export PATH=$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")/.."
rustup toolchain install   # the one rust-toolchain.toml names, with its components and targets
