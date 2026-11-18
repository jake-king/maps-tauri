#!/usr/bin/env bash
# Sign a release: tools/sign-release.sh DIR signs DIR/SHA256SUMS, which lists every asset's hash, so
# one signature covers them all, and writes DIR/SHA256SUMS.minisig. Then it checks the signature with
# the committed public key (release-signing.pub) using minisign itself, so a release whose
# signature would not verify for its users is never published.
#   key: $RELEASE_SIGNING_KEY, the private key file's contents (a forge secret; never on a command
#        line). Tauri's signer makes the key (`npx tauri signer generate`), in minisign's format,
#        the format Tauri's updater checks, so the same key can sign app updates later.
# A user checks a download with:  minisign -Vm SHA256SUMS -p release-signing.pub && sha256sum -c SHA256SUMS
set -euo pipefail
cd "$(dirname "$0")/.."
dir=${1:?usage: tools/sign-release.sh DIR}
pub=${RELEASE_SIGNING_PUB:-release-signing.pub}
[[ -n ${RELEASE_SIGNING_KEY:-} ]] || { echo "sign-release: no RELEASE_SIGNING_KEY: refusing to publish unsigned" >&2; exit 1; }
[[ -f $pub ]] || { echo "sign-release: no public key at $pub" >&2; exit 1; }
command -v minisign >/dev/null || { echo "sign-release: minisign is needed to check the signature" >&2; exit 1; }
rm -f "$dir/SHA256SUMS.sig" "$dir/SHA256SUMS.minisig"
TAURI_SIGNING_PRIVATE_KEY=$RELEASE_SIGNING_KEY TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
  node_modules/.bin/tauri signer sign "$dir/SHA256SUMS" >/dev/null
# Tauri writes the minisign signature base64-encoded (the updater's form); minisign reads it plain
base64 -d "$dir/SHA256SUMS.sig" >"$dir/SHA256SUMS.minisig"
rm "$dir/SHA256SUMS.sig"
minisign -Vm "$dir/SHA256SUMS" -x "$dir/SHA256SUMS.minisig" -p "$pub"
