//! The WebAssembly face of the crate, for the frontend's search box.

use wasm_bindgen::prelude::*;

/// The readings flattened as `[lat0, lon0, lat1, lon1]`; empty when the input is not a coordinate.
/// JavaScript receives a `Float64Array`.
#[wasm_bindgen(js_name = parseCoords)]
pub fn parse_coords(raw: &str) -> Vec<f64> {
    crate::parse_coords(raw)
        .into_iter()
        .flat_map(|(lat, lon)| [lat, lon])
        .collect()
}
