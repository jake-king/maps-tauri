//! Reading the basemap without JavaScript: a PMTiles v3 archive reader ([`pmtiles`]) and a Mapbox
//! Vector Tile decoder ([`mvt`]). Both are hand-rolled: each format is a few pages of spec, and
//! owning the decoder is what lets the renderer control its allocation.

pub mod mvt;
pub mod pmtiles;
