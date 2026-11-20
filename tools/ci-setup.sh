#!/usr/bin/env bash
# What a fresh Ubuntu 24.04 machine needs before tools/check.sh can run: the WebKitGTK and other
# system libraries Tauri links against, shellcheck (Ubuntu's 0.9; the dev box runs 0.11, and the
# scripts must pass both), ruff at the version the dev box uses, and mdBook with its Mermaid preprocessor for the book. On a
# bare container (every CI job, tools/ci-local.sh) it also installs rustup and Node; GitHub's own
# runner images (platforms.yml) have rustup, and that workflow sets Node up itself. Rust's version comes from rust-toolchain.toml either way.
set -euo pipefail
RUFF=0.17.3
MDBOOK=0.5.4
MDBOOK_MERMAID=0.17.1
CARGO_DENY=0.21.0
CARGO_MACHETE=0.10.1
NODE=22
GITLEAKS=8.30.1
# the published sha256 of gitleaks_8.30.1_linux_x64.tar.gz (its release checksums file)
GITLEAKS_SHA256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
sudo=; [[ $(id -u) == 0 ]] || sudo=sudo
export DEBIAN_FRONTEND=noninteractive
$sudo apt-get update -q
$sudo apt-get install -y -q --no-install-recommends \
  build-essential curl ca-certificates git pkg-config python3 pipx shellcheck xz-utils minisign valgrind \
  libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev patchelf
if ! command -v rustup >/dev/null; then
  curl -fsS https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal
fi
export PATH=$HOME/.cargo/bin:$PATH
cd "$(dirname "$0")/.."
# The toolchain goes in before anything else can start a rustup proxy. pip (under pipx, just below)
# runs `rustc --version` for its user agent and kills it after 0.5 s; with no toolchain installed
# yet, that rustc auto-installs the one rust-toolchain.toml names and is killed partway, and the
# half-written toolchain then fails this install ("detected conflict: 'bin/cargo-clippy'") or
# leaves it without cargo. Installed first, that rustc only prints its version.
rustup toolchain install   # the one rust-toolchain.toml names, with its components and targets
pipx install --force "ruff==$RUFF"
if ! command -v node >/dev/null; then
  curl -fsS "https://nodejs.org/dist/latest-v$NODE.x/SHASUMS256.txt" -o /tmp/node.sums
  tarball=$(grep -o "node-v${NODE}\.[0-9.]*-linux-x64\.tar\.xz" /tmp/node.sums | head -1)
  curl -fsS "https://nodejs.org/dist/latest-v$NODE.x/$tarball" -o "/tmp/$tarball"
  (cd /tmp && grep " $tarball\$" node.sums | sha256sum -c -)
  $sudo tar -xJf "/tmp/$tarball" -C /usr/local --strip-components=1
fi
if [[ $(gitleaks version 2>/dev/null) != "$GITLEAKS" ]]; then
  t=gitleaks_${GITLEAKS}_linux_x64.tar.gz
  curl -fsSL "https://github.com/gitleaks/gitleaks/releases/download/v$GITLEAKS/$t" -o "/tmp/$t"
  echo "$GITLEAKS_SHA256  /tmp/$t" | sha256sum -c --quiet -
  tar -xzf "/tmp/$t" -C /tmp gitleaks
  $sudo install -m 755 /tmp/gitleaks /usr/local/bin/gitleaks
fi
if [[ $(mdbook --version 2>/dev/null) != "mdbook v$MDBOOK" ]]; then
  cargo install mdbook --version "$MDBOOK" --locked
fi
if [[ $(mdbook-mermaid --version 2>/dev/null) != "mdbook-mermaid $MDBOOK_MERMAID" ]]; then
  cargo install mdbook-mermaid --version "$MDBOOK_MERMAID" --locked
fi
if [[ $(cargo deny --version 2>/dev/null) != "cargo-deny $CARGO_DENY" ]]; then
  cargo install cargo-deny --version "$CARGO_DENY" --locked
fi
if [[ $(cargo machete --version 2>/dev/null) != "$CARGO_MACHETE" ]]; then
  cargo install cargo-machete --version "$CARGO_MACHETE" --locked
fi
