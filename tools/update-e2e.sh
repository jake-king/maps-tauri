#!/usr/bin/env bash
# The updater, end to end, as root in a throwaway Ubuntu 24.04 container (tools/ci-local.sh --
# tools/update-e2e.sh): version 0.1.0 is installed from its .deb, a local HTTPS server offers
# 0.1.1, and the installed app (MAPS_UPDATE=install) must install a genuine update and refuse
# every forged one, leaving 0.1.0 in place:
#   good       0.1.1, signed by the trusted key for 0.1.1               installed
#   tampered   the same package with one byte appended                  refused
#   wrong key  0.1.1 signed by another key                              refused
#   downgrade  the manifest says 0.1.2, the package is signed for 0.1.1 refused (the versions differ)
#   unversioned  the manifest says 0.1.2, the signature names no version  refused (requireSignedVersion)
#   current    the manifest offers 0.1.0                                 nothing newer
# Both builds use a throwaway key (the release key never leaves the forge) and accept the test
# server's self-signed certificate; neither setting is in any release build.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH=${CARGO_HOME:-$HOME/.cargo}/bin:$PATH
work=$PWD/logs/update-e2e
rm -rf "$work"
mkdir -p "$work/www"
need=()
for t in xvfb-run sudo openssl dpkg-deb; do command -v $t >/dev/null || need+=("$t"); done
if ((${#need[@]})); then
  apt-get install -y -q --no-install-recommends xvfb sudo openssl libgl1-mesa-dri >/dev/null
fi

# the release build runs the frontend's build, which builds the WebAssembly crate
if [[ $(wasm-pack --version 2>/dev/null) != "wasm-pack 0.15.1" ]]; then
  cargo install wasm-pack --version 0.15.1 --locked
fi
tauri=node_modules/.bin/tauri
$tauri signer generate --ci -p '' -w "$work/trusted.key" >/dev/null
$tauri signer generate --ci -p '' -w "$work/other.key" >/dev/null
port=8443
export MAPS_UPDATE_URL=https://127.0.0.1:$port/latest.json
# tauri's .pub file is the minisign public key file, base64-encoded
MAPS_UPDATE_PUBKEY=$(base64 -d "$work/trusted.key.pub")
export MAPS_UPDATE_PUBKEY

build() {
  local v=$1
  $tauri build --bundles deb --config "{\"version\":\"$v\",\"plugins\":{\"updater\":{\"dangerousAcceptInvalidCerts\":true}}}" \
    >"$work/build-$v.log" 2>&1 || { tail -40 "$work/build-$v.log"; exit 2; }
  cp target/release/bundle/deb/maps-tauri_"$v"_amd64.deb "$work/"
}
build 0.1.0
build 0.1.1
# the bundler marks the binary with its package type, which the updater reads to install a .deb
dpkg-deb -x "$work/maps-tauri_0.1.0_amd64.deb" "$work/unpacked"
[[ -x $work/unpacked/usr/bin/maps-tauri ]] ||
  { dpkg-deb -c "$work/maps-tauri_0.1.0_amd64.deb" >&2; echo "update-e2e: no usr/bin/maps-tauri in the package" >&2; exit 2; }
grep -qa __TAURI_BUNDLE_TYPE_VAR_DEB "$work/unpacked/usr/bin/maps-tauri" ||
  { echo "update-e2e: the 0.1.0 binary is not marked as a .deb install" >&2; exit 2; }

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 -subj /CN=127.0.0.1 \
  -keyout "$work/tls.key" -out "$work/tls.crt" 2>/dev/null
python3 - "$work/www" "$work/tls.crt" "$work/tls.key" "$port" >"$work/server.log" 2>&1 <<'EOF' &
import functools, http.server, ssl, sys
root, crt, key, port = sys.argv[1:]
handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=root)
httpd = http.server.ThreadingHTTPServer(("127.0.0.1", int(port)), handler)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(crt, key)
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
httpd.serve_forever()
EOF
server=$!
trap 'kill $server 2>/dev/null || true' EXIT

installed() { dpkg-query -W -f '${Version}' maps-tauri; }
# offer PKG as VERSION, signed by KEY for SIGNED_AS
offer() {
  local pkg=$1 version=$2 key=$3 signed_as=$4
  cp "$pkg" "$work/www/update.deb"
  RELEASE_SIGNING_KEY=$(cat "$key") tools/update-manifest.sh "$work/www/update.deb" "$signed_as" \
    "https://127.0.0.1:$port/update.deb" "$work/www/latest.json" >/dev/null
  # the version announced may differ from the one signed: that is the downgrade case
  python3 -c 'import json,sys; p=sys.argv[1]; m=json.load(open(p)); m["version"]=sys.argv[2]; json.dump(m,open(p,"w"))' \
    "$work/www/latest.json" "$version"
}
fail=0
case_() {
  local name=$1 want_rc=$2 want_version=$3 rc=0
  dpkg -i "$work/maps-tauri_0.1.0_amd64.deb" >/dev/null 2>&1
  MAPS_UPDATE=install timeout 120 xvfb-run -a /usr/bin/maps-tauri >"$work/$name.log" 2>&1 || rc=$?
  local said
  said=$(grep -m1 '^update: ' "$work/$name.log" || echo "update: (no outcome printed)")
  if [[ $rc == "$want_rc" && $(installed) == "$want_version" ]]; then
    printf 'ok    %-10s exit %s, installed %s  (%s)\n' "$name" "$rc" "$(installed)" "$said"
  else
    printf 'FAIL  %-10s exit %s (want %s), installed %s (want %s)  (%s)\n' "$name" "$rc" "$want_rc" "$(installed)" "$want_version" "$said"
    tail -20 "$work/$name.log"
    fail=1
  fi
}

new=$work/maps-tauri_0.1.1_amd64.deb
offer "$new" 0.1.1 "$work/trusted.key" 0.1.1
case_ good 0 0.1.1

cp "$new" "$work/tampered.deb"
offer "$work/tampered.deb" 0.1.1 "$work/trusted.key" 0.1.1
printf 'x' >>"$work/www/update.deb" # after signing
case_ tampered 1 0.1.0

offer "$new" 0.1.1 "$work/other.key" 0.1.1
case_ wrong-key 1 0.1.0

offer "$new" 0.1.2 "$work/trusted.key" 0.1.1
case_ downgrade 1 0.1.0

# a signature made without --app-version, as older Tauri CLIs made them: it binds no version, so
# only requireSignedVersion stops a manifest announcing any version it likes
cp "$new" "$work/www/update.deb"
TAURI_SIGNING_PRIVATE_KEY=$(cat "$work/trusted.key") TAURI_SIGNING_PRIVATE_KEY_PASSWORD='' \
  $tauri signer sign "$work/www/update.deb" >/dev/null
python3 -c 'import json,sys; json.dump({"version": "0.1.2", "platforms": {"linux-x86_64-deb": {"url": sys.argv[1], "signature": open(sys.argv[2]).read().strip()}}}, open(sys.argv[3], "w"))' \
  "https://127.0.0.1:$port/update.deb" "$work/www/update.deb.sig" "$work/www/latest.json"
case_ unversioned 1 0.1.0

offer "$work/maps-tauri_0.1.0_amd64.deb" 0.1.0 "$work/trusted.key" 0.1.0
case_ current 0 0.1.0

((fail)) && { echo "update-e2e: FAILED" >&2; exit 1; }
echo "update-e2e: a genuine update installed; a tampered package, another key and both replays refused"
