#!/usr/bin/env bash
# One job of the forge's CI (.forgejo/workflows/ci.yml), run from the checked-out tree inside a bare
# Ubuntu 24.04 container on the forge's runner:
#   tools/forge-job.sh check   setup, then tools/check.sh (the gate the dev box runs before a commit)
#   tools/forge-job.sh e2e     setup, unit tests, the release build, then the end-to-end checks on the
#                              recorded tape in a virtual display (the Linux leg of platforms.yml)
# The runner mounts a directory that outlives jobs at $CI_CACHE: rustup, cargo, npm's cache and the
# build tree are kept there, so only the first job builds from nothing. Without $CI_CACHE (a local
# run through tools/ci-local.sh) everything stays in the container. The e2e job takes the fixtures
# from ./e2e-fixtures.tar.gz when present (tools/ci-local.sh's FIXTURES=), else from the cache, else
# from the forge's e2e-fixtures branch with the job's token in $TOKEN.
set -euo pipefail
cd "$(dirname "$0")/.."
job=${1:-}
[[ $job == check || $job == e2e ]] || { echo "usage: tools/forge-job.sh check|e2e" >&2; exit 2; }

if [[ -n ${CI_CACHE:-} ]]; then
  c=$CI_CACHE/maps-tauri
  mkdir -p "$c/rustup" "$c/cargo" "$c/npm" "$c/target"
  export RUSTUP_HOME=$c/rustup CARGO_HOME=$c/cargo npm_config_cache=$c/npm
  # The scripts name ./target, so the kept build tree is linked there. The runner's disk is shared
  # by every repository's CI and cargo never shrinks a build tree: past 20 GB it starts over.
  if (($(du -sm "$c/target" | cut -f1) > 20000)); then
    echo "forge-job: build tree over 20 GB, emptied"
    find "$c/target" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
  fi
  [[ -e target ]] || ln -s "$c/target" target
fi
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$HOME/.local/bin:$PATH

tools/ci-setup.sh
tools/fetch-assets.sh

if [[ $job == check ]]; then
  npm ci --no-audit --no-fund
  out=$(mktemp)
  rc=0
  tools/check.sh | tee "$out" || rc=$?
  # the step logs stay in the container, so a failed step's tail is printed here
  if ((rc)); then
    for log in $(grep -o 'logs/check-[A-Za-z0-9_.-]*\.log' "$out" || true); do
      echo "==== $log (last 60 lines)"
      tail -n 60 "$log"
    done
  fi
  exit "$rc"
fi

sudo=
[[ $(id -u) == 0 ]] || sudo=sudo
$sudo apt-get install -y -q --no-install-recommends xvfb libgl1-mesa-dri
if [[ $(wasm-pack --version 2>/dev/null) != "wasm-pack 0.15.1" ]]; then
  cargo install wasm-pack --version 0.15.1 --locked
fi
npm ci --no-audit --no-fund
cargo test --workspace --locked
npm test
npx tauri build --no-bundle
archive=e2e-fixtures.tar.gz
if [[ ! -f $archive ]]; then
  want=$(python3 -c 'import json; print(json.load(open("tools/fixtures.lock"))["sha256"])')
  kept=${CI_CACHE:+$CI_CACHE/maps-tauri/fixtures/$want.tar.gz}
  if [[ -n $kept && -f $kept ]]; then
    archive=$kept
  else
    # The archive is kept on the forge's e2e-fixtures branch, away from the code branches (and so
    # from the public copy). The job's token is accepted by git and the API, not by the forge's
    # web downloads, so a release asset is out of reach; the token goes in git's environment.
    mkdir -p logs
    archive=logs/e2e-fixtures.tar.gz
    (
      export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.extraHeader GIT_CONFIG_VALUE_0="Authorization: token ${TOKEN:?}"
      git fetch -q --depth 1 "$GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git" e2e-fixtures
    )
    git show FETCH_HEAD:e2e-fixtures.tar.gz >"$archive"
    echo "$want  $archive" | sha256sum -c --quiet
    if [[ -n $kept ]]; then
      mkdir -p "${kept%/*}"
      cp "$archive" "$kept.$$" && mv "$kept.$$" "$kept"
    fi
  fi
fi
python3 tools/fixtures.py unpack "$archive"
# a virtual display (not xvfb-run: its ready signal was seen lost), and WebKitGTK's DMA-BUF renderer
# off, as it needs a GPU and Xvfb has none
export WEBKIT_DISABLE_DMABUF_RENDERER=1
python3 tools/e2e.py --tape fixtures/tape --no-timing --xvfb --out e2e-out
