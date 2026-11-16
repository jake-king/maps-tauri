#!/usr/bin/env bash
# The Protomaps glyphs and sprites the frontend serves from public/assets, from the public
# protomaps/basemaps-assets repository at a pinned commit (the same files the dev box has: its
# sprites/v4/light.json was compared byte for byte). CI uses it so the palette check runs and the
# release build carries its fonts.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO=https://github.com/protomaps/basemaps-assets
REF=028c18f713baecad011301ff7a69acc39bcc2ae7
[[ -s public/assets/sprites/v4/light.json && -d public/assets/fonts ]] && exit 0
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
git -C "$tmp" init -q
git -C "$tmp" fetch -q --depth 1 "$REPO" "$REF"
git -C "$tmp" checkout -q FETCH_HEAD
mkdir -p public/assets/sprites
rm -rf public/assets/fonts public/assets/sprites/v4
cp -r "$tmp/fonts" public/assets/fonts
cp -r "$tmp/sprites/v4" public/assets/sprites/v4
echo "assets: protomaps/basemaps-assets@${REF:0:7} -> public/assets"
