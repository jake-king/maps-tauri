#!/usr/bin/env bash
# The updater's half of a release: sign the .deb for the updater and write the manifest installed
# copies read (src-tauri/src/update.rs).
#   tools/update-manifest.sh DEB VERSION URL OUT
#     DEB      the release's Debian package; DEB.sig is written beside it
#     VERSION  the version it was built as (the signature is bound to it: requireSignedVersion)
#     URL      where installed copies will download DEB from
#     OUT      the manifest to write (latest.json)
#   key: $RELEASE_SIGNING_KEY, the private key file's contents, as for tools/sign-release.sh
#        (never on a command line); $RELEASE_SIGNING_KEY_PASSWORD if it has one
# The release job and the end-to-end check (tools/update-e2e.sh) both call this, so the check
# exercises the manifest a release publishes.
set -euo pipefail
cd "$(dirname "$0")/.."
deb=${1:?usage: tools/update-manifest.sh DEB VERSION URL OUT}
version=${2:?version}
url=${3:?url}
out=${4:?out}
[[ -n ${RELEASE_SIGNING_KEY:-} ]] || { echo "update-manifest: no RELEASE_SIGNING_KEY" >&2; exit 1; }
[[ -f $deb ]] || { echo "update-manifest: no package at $deb" >&2; exit 1; }
rm -f "$deb.sig"
TAURI_SIGNING_PRIVATE_KEY=$RELEASE_SIGNING_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD=${RELEASE_SIGNING_KEY_PASSWORD:-} \
  node_modules/.bin/tauri signer sign --app-version "$version" "$deb" >/dev/null
# The updater looks for "{os}-{arch}-{bundle type}" first: an installed .deb gets the .deb
python3 - "$version" "$url" "$deb.sig" "$out" <<'EOF'
import datetime, json, sys
version, url, sig, out = sys.argv[1:]
manifest = {
    "version": version,
    "pub_date": datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat(),
    "platforms": {"linux-x86_64-deb": {"url": url, "signature": open(sig).read().strip()}},
}
with open(out, "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
EOF
echo "update-manifest: $out offers $version at $url"
