Linux build (Debian/Ubuntu package), built and published by the release workflow after the gate
passed. The map data is not included: point `MAPS_DATA` at a directory holding `westla.pmtiles`
(see the README's Data section), then run `maps-tauri`; `MAPS_RENDERER=rust` selects the native
renderer.

Each release carries SHA256SUMS, signed (SHA256SUMS.minisig), and software bills of materials
(CycloneDX: `*.rust.cdx.json`, `*.js.cdx.json`) listing every crate and package in the build. To check
a download, with the public key from the repository (`release-signing.pub`):

    minisign -Vm SHA256SUMS -p release-signing.pub && sha256sum -c --ignore-missing SHA256SUMS
