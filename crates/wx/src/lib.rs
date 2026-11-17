//! NOAA weather for the native renderer, decoded in Rust so the webview only uploads textures and
//! draws outlines:
//! * [`grib`]: the GRIB2 subset MRMS radar mosaics use.
//! * [`metar`]: airport observations by flight category, from the AWC cache file.
//! * [`radar`]: a decoded mosaic reduced to one byte per cell for the GPU.
//! * [`hazards`]: G-AIRMET hazard areas (turbulence, icing, ...) from aviationweather.gov GeoJSON.
//! * [`satellite`]: GOES infrared clouds from nowCOAST, one byte of grey per pixel.
//! * [`sigmet`]: international and US convective SIGMETs, worldwide.
//! * [`timeline`]: what to show at a chosen time (radar loop, satellite image, hazard snapshot).
//! * [`wind`]: winds aloft from the GFS model at a flight level.
//! * `fetch` (feature `http`): getting it all from NOAA.

#[cfg(feature = "http")]
pub mod fetch;
pub mod grib;
pub mod hazards;
pub mod metar;
pub mod radar;
pub mod satellite;
pub mod sigmet;
pub mod timeline;
pub mod wind;
