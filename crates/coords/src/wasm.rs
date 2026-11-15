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

/// The search-box answer for a typed coordinate: a JSON array of Carmen features, one per reading
/// (at most `limit`), built by the same `coord_feature` the backend uses. `[]` when the input is
/// not a coordinate and belongs to the gazetteer.
#[wasm_bindgen(js_name = coordFeatures)]
pub fn coord_features(raw: &str, limit: usize) -> String {
    let feats: Vec<serde_json::Value> = crate::parse_coords(raw)
        .into_iter()
        .take(limit)
        .map(|(lat, lon)| crate::coord_feature(lat, lon))
        .collect();
    serde_json::Value::Array(feats).to_string()
}
