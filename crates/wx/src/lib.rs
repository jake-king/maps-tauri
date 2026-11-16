//! NOAA weather for the native renderer, decoded in Rust so the webview only uploads textures and
//! draws outlines:
//! * [`grib`]: the GRIB2 subset MRMS radar mosaics use.
//! * [`radar`]: a decoded mosaic reduced to one byte per cell for the GPU.
//! * [`hazards`]: G-AIRMET hazard areas (turbulence, icing, ...) from aviationweather.gov GeoJSON.
//! * `fetch` (feature `http`): getting both from NOAA.

#[cfg(feature = "http")]
pub mod fetch;
pub mod grib;
pub mod hazards;
pub mod radar;
