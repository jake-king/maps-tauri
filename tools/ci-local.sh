#!/usr/bin/env bash
# The CI job, run locally in the image GitHub's runner is built on (Ubuntu 24.04), from a clean
# clone of HEAD: the same setup script, the same gate. A pipeline change is proven here before it
# is pushed; keep the two in step. Needs podman.
#   tools/ci-local.sh            setup + npm ci + tools/check.sh
#   tools/ci-local.sh -- CMD     setup + npm ci, then CMD instead of the gate
#   FIXTURES=logs/e2e-fixtures.tar.gz tools/ci-local.sh -- CMD   with the e2e fixtures in the clone
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d -p "$PWD/logs" ci-local.XXXX)
trap 'rm -rf "$work"' EXIT
git clone -q . "$work/src"
# FIXTURES=<archive>: the end-to-end fixtures, copied in for a `-- CMD` that unpacks them
if [[ -n ${FIXTURES:-} ]]; then cp "$FIXTURES" "$work/src/e2e-fixtures.tar.gz"; fi
cmd='tools/check.sh'
[[ ${1:-} == -- ]] && { shift; cmd="$*"; }
systemd-run --user --scope -q -p MemoryMax=12G \
  podman run --rm -v "$work/src:/w" -w /w docker.io/library/ubuntu:24.04 bash -c "
    set -euo pipefail
    tools/ci-setup.sh >/tmp/setup.log 2>&1 || { tail -30 /tmp/setup.log; exit 1; }
    export PATH=\$HOME/.cargo/bin:\$HOME/.local/bin:\$PATH
    tools/fetch-assets.sh
    npm ci --no-audit --no-fund >/tmp/npm.log 2>&1 || { tail -30 /tmp/npm.log; exit 1; }
    $cmd"
