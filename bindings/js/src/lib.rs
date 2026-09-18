//! WebAssembly bindings for `dwg2geo` — the npm package `dwg2geo`.
//!
//! ```js
//! import init, { convert } from 'dwg2geo';
//! await init();
//! const result = convert(new Uint8Array(dwgBytes), false, undefined);
//! const fc = JSON.parse(result.geojson); // FeatureCollection, local coords
//! ```

use wasm_bindgen::prelude::*;

/// Convert DWG bytes to a GeoJSON FeatureCollection (local drawing
/// coordinates) plus a conversion report. Returns a JS object shaped like
/// `dwg2geo::backend::native::EmbedResult`: `{ geojson, feature_count,
/// model_space_entities, converted, skipped, failed, warnings, bbox,
/// source_sha256, geodata, crs_text_hints }`. Reprojection is the caller's
/// responsibility (e.g. proj4js) — this function never guesses a CRS. What
/// it does surface is every CRS *declaration* the drawing carries: the
/// GEODATA object (`geodata`, or `null`) and text strings from any space
/// that mention a CRS keyword (`crs_text_hints`, e.g. a title block's
/// "UTM - SIRGAS 2000 - FUSO 25 SUL"), for the caller to interpret.
#[wasm_bindgen]
pub fn convert(
    bytes: &[u8],
    polygonize_closed: bool,
    curve_tolerance: Option<f64>,
) -> Result<JsValue, JsValue> {
    console_error_panic_hook::set_once();
    let result = dwg2geo::backend::native::convert_bytes(bytes, polygonize_closed, curve_tolerance)
        .map_err(|error| JsValue::from_str(&format!("{error:#}")))?;
    serde_wasm_bindgen::to_value(&result).map_err(|error| JsValue::from_str(&error.to_string()))
}
