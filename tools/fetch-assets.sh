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
# One font is another's files under a second name: the 252 glyph ranges of Noto Sans Devanagari
# Regular v1 are symlinks into Noto Sans Regular. Copied as links they break on Windows (Git Bash
# cannot make a link before its target is copied, and git may write a link as a text file), so git
# writes every link as a file holding its target, on every platform, and each such file is replaced
# by a copy of the file it names. A link to another link is refused: its copy would be link text.
git -C "$tmp" -c core.symlinks=false checkout -q FETCH_HEAD
# every link in the tree, wherever it is (a link to a link elsewhere is refused all the same), and
# the ones among the files copied below
all=$tmp/.git/links-all links=$tmp/.git/links
git -C "$tmp" ls-tree -r -z FETCH_HEAD | while IFS= read -r -d '' e; do
  if [[ $e == "120000 "* ]]; then printf '%s\n' "${e#*$'\t'}"; fi
done >"$all"
grep -E '^(fonts|sprites/v4)/' "$all" >"$links" || (($? == 1))   # 1: no links there, 2: an error
root=$(cd "$tmp" && pwd -P)
while IFS= read -r link; do
  to=$(<"$tmp/$link")
  dir=$(cd "$tmp/$(dirname "$link")/$(dirname "$to")" && pwd -P)
  target=${dir#"$root"/}/$(basename "$to")
  if grep -Fxq -- "$target" "$all" || [[ ! -f $tmp/$target ]]; then
    echo "assets: $link -> $to is not a link to a file" >&2
    exit 1
  fi
  cp -- "$tmp/$target" "$tmp/$link"
done <"$links"
mkdir -p public/assets/sprites
rm -rf public/assets/fonts public/assets/sprites/v4
cp -r "$tmp/fonts" public/assets/fonts
cp -r "$tmp/sprites/v4" public/assets/sprites/v4
echo "assets: protomaps/basemaps-assets@${REF:0:7} -> public/assets"
