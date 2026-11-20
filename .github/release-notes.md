Linux package (Ubuntu 24.04 or newer, amd64), built by the release workflow after the gate passed.

Install it, download the West LA map and place-name index from the `map-westla-20261118` release
into `~/.local/share/local.maps-tauri` (the README's Install section has the commands), then run
`maps-tauri`. The native renderer is the default; `MAPS_RENDERER=maplibre` selects MapLibre.

This release is the package alone: it is not signed and carries no SBOMs. Releases built on the
project's own forge add a SHA256SUMS signed with the key in `release-signing.pub` and CycloneDX
SBOMs.
