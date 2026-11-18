#!/usr/bin/env bash
# One job of the self-hosted forge's CI (its workflow, .forgejo/, is kept out of the public copy), run
# from the checked-out tree inside a bare Ubuntu 24.04 container on the forge's runner:
#   tools/forge-job.sh check   setup, then tools/check.sh (the gate the dev box runs before a commit)
#   tools/forge-job.sh e2e     setup, unit tests, the release build, then the end-to-end checks on the
#                              recorded tape in a virtual display (the Linux leg of platforms.yml)
#   tools/forge-job.sh coverage setup, then tools/coverage.sh with its floor, the HTML report kept in
#                              target/coverage for the docs site
#   tools/forge-job.sh mutants  cargo-mutants on the lines this push changed (since $BEFORE, else the
#                              previous commit); a mutant no test catches fails the job
#   tools/forge-job.sh mutants-full  one shard of every mutant ($SHARD, k/n from 0), its missed list kept
#                              in the cache under the workflow run
#   tools/forge-job.sh mutants-total the shards' missed lists together; fails if more are missed than the
#                              recorded baseline
#   tools/forge-job.sh fuzz    setup, seeds from the fixtures, then every fuzz target for $FUZZ_SECONDS
#                              (default 30) on a corpus kept in the cache; FUZZ_CMIN=1 minimises it after
#   tools/forge-job.sh perf    setup, then tools/perf.sh: instruction counts of the benchmarks against
#                              the merge base with main (on main, against $BEFORE); +3% fails
#   tools/forge-job.sh msrv    setup, then tools/msrv.sh: the workspace builds with Cargo.toml's
#                              rust-version, not only with rust-toolchain.toml's newer Rust
#   tools/forge-job.sh audit   setup, then the RustSec advisories for Cargo.lock (cargo-deny, deny.toml)
#                              and npm's for package-lock.json (tools/npm_audit.py), both fetched fresh
#   tools/forge-job.sh release for a pushed tag v<version>: the gate, the Debian package, then a forge
#                              release holding it, the documentation site (book + API reference) as a
#                              tarball, and their SHA256SUMS (RELEASE_DRY_RUN=1 builds, lists what it
#                              would publish, and stops)
# The runner mounts a directory that outlives jobs at $CI_CACHE: rustup, cargo, npm's cache and the
# build tree are kept there, so only the first job builds from nothing. Without $CI_CACHE (a local
# run through tools/ci-local.sh) everything stays in the container. The e2e job takes the fixtures
# from ./e2e-fixtures.tar.gz when present (tools/ci-local.sh's FIXTURES=), else from the cache, else
# from the forge's e2e-fixtures branch with the job's token in $TOKEN.
set -euo pipefail
cd "$(dirname "$0")/.."
job=${1:-}
[[ $job =~ ^(check|e2e|coverage|fuzz|perf|msrv|audit|mutants|mutants-full|mutants-total|release)$ ]] || { echo "usage: tools/forge-job.sh check|e2e|coverage|fuzz|perf|msrv|audit|mutants|mutants-full|mutants-total|release" >&2; exit 2; }

if [[ $job == release ]]; then
  # Before any setup, so a wrong tag fails in seconds. The tag names the version in tauri.conf.json:
  # v<version> is a release, v<version>-<suffix> (v0.2.0-rc.1) a prerelease. The bare container has
  # no python3 yet, hence sed.
  conf=$(sed -n 's/^  "version": "\([^"]*\)",$/\1/p' src-tauri/tauri.conf.json)
  [[ -n $conf ]] || { echo "forge-job: no version found in src-tauri/tauri.conf.json" >&2; exit 1; }
  tag=${GITHUB_REF_NAME:-v$conf}
  case ${tag#v} in
    "$conf") prerelease=false ;;
    "$conf"-?*) prerelease=true ;;
    *) echo "forge-job: tag $tag does not name the app version $conf" >&2; exit 1 ;;
  esac
  [[ $tag == v* ]] || { echo "forge-job: tag $tag does not start with v" >&2; exit 1; }
  # A release is built from what main holds, merged through a pull request with CI green, never from
  # a branch nobody reviewed: the tagged commit must be on main (release.yml fetches it into this ref)
  main=${RELEASE_MAIN_REF:-refs/remotes/origin/main}
  sha=${GITHUB_SHA:-$(git rev-parse HEAD)}
  git rev-parse -q --verify "$main^{commit}" >/dev/null ||
    { echo "forge-job: $main not found: cannot check that $tag is on main" >&2; exit 1; }
  git merge-base --is-ancestor "$sha" "$main" ||
    { echo "forge-job: $tag ($sha) is not on main: merge it first, then tag" >&2; exit 1; }
  echo "forge-job: $tag ($sha) is on main"
fi

# The full run is sharded (one job may not outlast the runner's 3-hour limit) and its shards' missed
# lists meet in the cache, under the workflow run's id
shards=${CI_CACHE:-$PWD/logs}/maps-tauri-mutants/${GITHUB_RUN_ID:-local}
if [[ $job == mutants-total ]]; then
  n=${SHARDS:?SHARDS: how many shards the run had}
  for ((k = 0; k < n; k++)); do
    [[ -f $shards/shard-$k ]] || { echo "forge-job: shard $k/$n left no result in $shards" >&2; exit 1; }
  done
  missed=$(cat "$shards"/shard-* | grep -c . || true)
  total=$(cat "$shards"/total-* | awk '{ t += $1 } END { print t + 0 }')
  echo "forge-job: $missed of $total viable mutants missed, over $n shards"
  sort "$shards"/shard-*
  # results of runs older than a week go
  find "${shards%/*}" -mindepth 1 -maxdepth 1 -mtime +7 -exec rm -rf {} +
  if [[ ! -f tools/mutants-baseline.txt ]]; then
    echo "forge-job: no baseline recorded yet; this is the number to record"
    exit 0
  fi
  allowed=$(grep -v '^#' tools/mutants-baseline.txt)
  echo "forge-job: the baseline allows $allowed"
  ((missed <= allowed)) || { echo "forge-job: more mutants missed than the baseline allows" >&2; exit 1; }
  exit 0
fi

if [[ -n ${CI_CACHE:-} ]]; then
  c=$CI_CACHE/maps-tauri
  export COV_DIR=$c/cov
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

gate() {
  local out rc=0
  out=$(mktemp)
  tools/check.sh | tee "$out" || rc=$?
  # the step logs stay in the container, so a failed step's tail is printed here
  if ((rc)); then
    for log in $(grep -o 'logs/check-[A-Za-z0-9_.-]*\.log' "$out" || true); do
      echo "==== $log (last 60 lines)"
      tail -n 60 "$log"
    done
  fi
  return "$rc"
}
wasm_pack() {
  if [[ $(wasm-pack --version 2>/dev/null) != "wasm-pack 0.15.1" ]]; then
    cargo install wasm-pack --version 0.15.1 --locked
  fi
}

if [[ $job == check ]]; then
  npm ci --no-audit --no-fund
  gate
  exit
fi

if [[ $job == coverage ]]; then
  npm ci --no-audit --no-fund
  tools/coverage.sh --floor --html target/coverage
  exit
fi

if [[ $job == audit ]]; then
  # both run, so one red does not hide the other
  rc=0
  cargo deny check advisories || rc=1
  python3 tools/npm_audit.py || rc=1
  exit "$rc"
fi

if [[ $job == mutants || $job == mutants-full ]]; then
  MUTANTS=27.1.0
  if [[ $(cargo mutants --version 2>/dev/null) != "cargo-mutants $MUTANTS" ]]; then
    cargo install cargo-mutants --version "$MUTANTS" --locked
  fi
  # A run that finds nothing to mutate exits 0 without writing a report: never read an older one
  rm -rf logs/mutants/mutants.out logs/mutants/mutants.out.old
  if [[ $job == mutants ]]; then
    # The base: the commit the branch was at before this push, when the checkout has it (a new
    # branch or a force-push may not), else the previous commit.
    base=${BEFORE:-}
    if [[ -z $base || $base =~ ^0+$ ]] || ! git cat-file -e "$base^{commit}" 2>/dev/null; then
      base=HEAD~1
    fi
    mkdir -p logs
    git diff "$base" HEAD >logs/mutants.diff
    echo "forge-job: mutants on the lines changed since $(git rev-parse --short "$base")"
    # In place, one at a time, on the runner's warm build tree: a copied tree would build Tauri
    # from nothing for every job. The checkout is thrown away after the job.
    rc=0
    tools/mutants.sh --in-place --in-diff logs/mutants.diff || rc=$?
  else
    rc=0
    tools/mutants.sh --shard "${SHARD:?SHARD: which shard, k/n from 0}" || rc=$?
  fi
  out=logs/mutants/mutants.out
  # exit 2: missed mutants; 3: timeouts only (a mutant that hangs the tests is caught by the timeout)
  case $rc in
    0 | 3) ;;
    2) ;;
    *) echo "forge-job: cargo-mutants failed (exit $rc)" >&2; exit "$rc" ;;
  esac
  missed=$(grep -c . "$out/missed.txt" 2>/dev/null || true)
  if [[ $job == mutants ]]; then
    if ((missed)); then
      echo "forge-job: $missed mutant(s) on changed lines that no test catches:"
      cat "$out/missed.txt"
      exit 1
    fi
  else
    mkdir -p "$shards"
    k=${SHARD%/*}
    cat "$out"/{caught,missed,timeout}.txt 2>/dev/null | grep -c . >"$shards/total-$k" || true
    cp "$out/missed.txt" "$shards/shard-$k" 2>/dev/null || : >"$shards/shard-$k"
    echo "forge-job: shard $SHARD: $(grep -c . "$shards/shard-$k" || true) missed of $(cat "$shards/total-$k") viable"
  fi
  exit 0
fi

if [[ $job == release ]]; then
  wasm_pack
  npm ci --no-audit --no-fund
  gate
  npx tauri build --bundles deb
  out=logs/release
  rm -rf "$out"
  mkdir -p "$out"
  cp target/release/bundle/deb/*.deb "$out/"
  # the coverage report goes into the documentation site (tools/docs.sh picks up target/coverage)
  tools/coverage.sh --floor --html target/coverage
  tools/docs.sh --archive
  cp target/maps-tauri-docs.tar.gz "$out/maps-tauri-docs-$tag.tar.gz"
  # what the build contains, for anyone checking it against an advisory later
  tools/sbom.sh "$out" "maps-tauri-$tag"
  (cd "$out" && sha256sum -- *.deb *.tar.gz *.cdx.json >SHA256SUMS)
  # one signature over the sums covers every asset; checked with the committed public key before
  # anything is published. A dry run without the key (a local one) says so; a release cannot
  if [[ ${RELEASE_DRY_RUN:-} == 1 && -z ${RELEASE_SIGNING_KEY:-} ]]; then
    echo "forge-job: RELEASE_DRY_RUN=1 without RELEASE_SIGNING_KEY: SHA256SUMS not signed"
  else
    tools/sign-release.sh "$out"
  fi
  echo "forge-job: $tag (prerelease $prerelease):"
  (cd "$out" && ls -l && cat SHA256SUMS)
  if [[ ${RELEASE_DRY_RUN:-} == 1 ]]; then
    echo "forge-job: RELEASE_DRY_RUN=1, nothing published"
    exit 0
  fi
  # The release is created as a draft, filled, then published, so a failed upload never leaves a
  # public release with half its files. A tag that already has a release fails here: delete that
  # release on the forge to build it again. The job's token is accepted by the API (measured).
  api=$GITHUB_SERVER_URL/api/v1/repos/$GITHUB_REPOSITORY
  auth() { printf 'Authorization: token %s\n' "${TOKEN:?}"; }
  python3 - "$tag" "$GITHUB_SHA" "$prerelease" >"$out/create.json" <<'EOF'
import json, sys
tag, sha, pre = sys.argv[1:]
print(json.dumps({"tag_name": tag, "target_commitish": sha, "name": f"maps-tauri {tag}",
                  "body": open(".github/release-notes.md").read(), "draft": True,
                  "prerelease": pre == "true"}))
EOF
  code=$(auth | curl -sS -H @- -H 'Content-Type: application/json' --data-binary @"$out/create.json" \
    -o "$out/release.json" -w '%{http_code}' -X POST "$api/releases")
  [[ $code == 201 ]] || { echo "forge-job: creating the release answered $code" >&2; cat "$out/release.json" >&2; exit 1; }
  id=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["id"])' "$out/release.json")
  for f in "$out"/*.deb "$out"/*.tar.gz "$out"/*.cdx.json "$out/SHA256SUMS" "$out/SHA256SUMS.minisig"; do
    code=$(auth | curl -sS -H @- -F "attachment=@$f" -o "$out/asset.json" -w '%{http_code}' \
      "$api/releases/$id/assets?name=$(basename "$f")")
    [[ $code == 201 ]] || { echo "forge-job: uploading $(basename "$f") answered $code" >&2; cat "$out/asset.json" >&2; exit 1; }
  done
  code=$(auth | curl -sS -H @- -H 'Content-Type: application/json' -d '{"draft": false}' \
    -o "$out/release.json" -w '%{http_code}' -X PATCH "$api/releases/$id")
  [[ $code == 200 ]] || { echo "forge-job: publishing the release answered $code" >&2; cat "$out/release.json" >&2; exit 1; }
  echo "forge-job: published $(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["html_url"])' "$out/release.json")"
  exit 0
fi

# The end-to-end fixtures (tape and map, tools/fixtures.lock) into fixtures/ and data/
fixtures() {
  local archive want kept
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
}

if [[ $job == fuzz ]]; then
  # the corpus outlives the job in the cache, so each run starts from what earlier ones found
  [[ -n ${CI_CACHE:-} ]] && export FUZZ_CORPUS=$c/fuzz-corpus
  fixtures
  tools/fuzz.sh seeds
  tools/fuzz.sh run "${FUZZ_SECONDS:-30}"
  [[ ${FUZZ_CMIN:-} == 1 ]] && tools/fuzz.sh cmin
  exit 0
fi

if [[ $job == msrv ]]; then
  tools/msrv.sh
  exit 0
fi

if [[ $job == perf ]]; then
  fixtures
  tools/perf.sh
  exit 0
fi

sudo=
[[ $(id -u) == 0 ]] || sudo=sudo
$sudo apt-get install -y -q --no-install-recommends xvfb libgl1-mesa-dri
wasm_pack
npm ci --no-audit --no-fund
cargo test --workspace --locked
npm test
npx tauri build --no-bundle
fixtures
# a virtual display (not xvfb-run: its ready signal was seen lost), and WebKitGTK's DMA-BUF renderer
# off, as it needs a GPU and Xvfb has none
export WEBKIT_DISABLE_DMABUF_RENDERER=1
python3 tools/e2e.py --tape fixtures/tape --no-timing --xvfb --out e2e-out
