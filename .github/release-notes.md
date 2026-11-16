Linux build (Debian/Ubuntu package), built and published by the release workflow after the gate
passed. The map data is not included: point `MAPS_DATA` at a directory holding `westla.pmtiles`
(see the README's Data section), then run `maps-tauri`; `MAPS_RENDERER=rust` selects the native
renderer.
