#!/usr/bin/env bash
# Software bills of materials for a release, CycloneDX 1.5 JSON:
#   tools/sbom.sh OUT_DIR NAME   writes OUT_DIR/NAME.rust.cdx.json (the app crate and every crate it
#                                builds with on Linux x86-64, cargo-cyclonedx) and NAME.js.cdx.json
#                                (the frontend's runtime packages, npm sbom; build tools omitted)
# cargo-cyclonedx names the workspace's own crates by their absolute path (path+file:///...); those
# are rewritten to the repository root, and the run fails if any absolute path to the checkout is
# left, so a release never carries the builder's home directory.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
out=${1:?usage: tools/sbom.sh OUT_DIR NAME} name=${2:?usage: tools/sbom.sh OUT_DIR NAME}
CYCLONEDX=0.5.10
if [[ $(cargo cyclonedx --version 2>/dev/null) != "cargo-cyclonedx-cyclonedx $CYCLONEDX" ]]; then
  cargo install cargo-cyclonedx --version "$CYCLONEDX" --locked
fi
mkdir -p "$out"
# cargo-cyclonedx writes a report beside every workspace member's manifest, not only the one asked
# for; the raw reports hold absolute paths, so all of them go whether the run passes or not
mapfile -t raw < <(cargo metadata -q --no-deps --format-version 1 --manifest-path src-tauri/Cargo.toml |
  python3 -c 'import json, os, sys; [print(os.path.dirname(p["manifest_path"]) + "/sbom-tmp.json") for p in json.load(sys.stdin)["packages"]]')
[[ ${#raw[@]} -gt 0 ]] || { echo "sbom: cargo metadata listed no packages" >&2; exit 1; }
trap 'rm -f "${raw[@]}"' EXIT
cargo cyclonedx -q --manifest-path src-tauri/Cargo.toml -f json --spec-version 1.5 \
  --target x86_64-unknown-linux-gnu --override-filename sbom-tmp
python3 - "$PWD" src-tauri/sbom-tmp.json "$out/$name.rust.cdx.json" <<'PY'
import re, sys
root, src, dst = sys.argv[1:]
text = open(src).read().replace("path+file://" + root + "/", "path+file:")
# the checkout's path as a path (followed by / or the end of a string), not as any substring:
# a short root such as a container's /w is also the start of every "/wasm..." in the text
if re.search(re.escape(root) + r'(/|")', text):
    sys.exit(f"sbom: {src} still names {root} after the rewrite")
open(dst, "w").write(text)
PY
npm sbom --sbom-format cyclonedx --omit dev >"$out/$name.js.cdx.json"
for f in "$out/$name".{rust,js}.cdx.json; do
  # a local path as a path (followed by / or the end of a string), as in the rewrite above
  if ! python3 -c 'import re, sys; t = open(sys.argv[1]).read(); sys.exit(any(re.search(re.escape(p) + r"(/|\")", t) for p in sys.argv[2:]))' "$f" "$PWD" "$HOME"; then
    echo "sbom: $f names a local path" >&2
    exit 1
  fi
  python3 -c 'import json, sys; d = json.load(open(sys.argv[1])); print("sbom:", sys.argv[1], "CycloneDX", d["specVersion"], len(d["components"]), "components")' "$f"
done
