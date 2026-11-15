#!/usr/bin/env bash
# Fetch the experiment's data from the Datastrike maps service on the LAN (never the internet):
#   data/basemap.pmtiles   a small Santa Monica cutout of the planet archive (the benchmarks' reference)
#   data/westla.pmtiles    a larger West LA cutout, what the app opens by default
#   public/assets/         Protomaps glyphs + sprites, from the service's pinned basemaps-assets tarball
#   data/geonames.db       the production GeoNames FTS5 gazetteer (1.6 GB), only with --gazetteer
set -euo pipefail
cd "$(dirname "$0")/.."
SRC=${MAPS_SRC:?set MAPS_SRC to the service base URL}
BBOX=${BBOX:--118.55,33.95,-118.35,34.10}
PMTILES=${PMTILES:-$HOME/go/bin/go-pmtiles}
mkdir -p data public/assets

[[ -s data/basemap.pmtiles ]] || "$PMTILES" extract "$SRC/tiles/basemap.pmtiles" data/basemap.pmtiles --bbox="$BBOX" --maxzoom=14
# the larger area the app opens by default: Malibu to downtown, the mountains to LAX (~25 MB)
[[ -s data/westla.pmtiles ]] || "$PMTILES" extract "$SRC/tiles/basemap.pmtiles" data/westla.pmtiles --bbox=-118.70,33.85,-118.15,34.20 --maxzoom=14

if [[ ! -d public/assets/fonts ]]; then
  tmp=$(mktemp -d -p data); trap 'rm -rf "$tmp"' EXIT
  curl -fsS -o "$tmp/a.tgz" "$SRC/assets/basemaps-assets.tar.gz"
  tar -xzf "$tmp/a.tgz" -C "$tmp"
  root=$(find "$tmp" -maxdepth 2 -type d -name fonts -printf '%h\n' | head -1)
  mkdir -p public/assets/sprites
  cp -r "$root/fonts" public/assets/fonts
  cp -r "$root/sprites/v4" public/assets/sprites/v4
fi

if [[ ${1:-} == --gazetteer && ! -s data/geonames.db ]]; then
  curl -fS -o data/geonames.db.part "$SRC/gazetteer/geonames.db" && mv data/geonames.db.part data/geonames.db
fi
ls -la data public/assets
