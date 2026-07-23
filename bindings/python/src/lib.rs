//! Python bindings for `dwg2geo` — the PyPI package `dwg2geo`.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

/// Convert DWG bytes to GeoJSON.
///
/// Returns a dict shaped like dwg2geo's `EmbedResult`: ``geojson`` (str,
/// FeatureCollection in local drawing coordinates), ``feature_count``,
/// ``model_space_entities``, ``converted``/``skipped``/``failed`` (lists of
/// per-entity-type outcomes), ``warnings``, ``bbox`` and ``source_sha256``.
/// Coordinates are local — reproject with your own tooling (pyproj etc.);
/// dwg2geo never guesses a CRS.
#[pyfunction]
#[pyo3(signature = (data, polygonize_closed = false, curve_tolerance = None))]
fn convert(
    py: Python<'_>,
    data: &[u8],
    polygonize_closed: bool,
    curve_tolerance: Option<f64>,
) -> PyResult<Py<PyAny>> {
    let result =
        ::dwg2geo::backend::native::convert_bytes(data, polygonize_closed, curve_tolerance)
            .map_err(|error| PyValueError::new_err(format!("{error:#}")))?;
    let object = pythonize::pythonize(py, &result)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    Ok(object.unbind())
}

/// Convenience wrapper: read ``path`` and convert it (same result dict).
#[pyfunction]
#[pyo3(signature = (path, polygonize_closed = false, curve_tolerance = None))]
fn convert_file(
    py: Python<'_>,
    path: std::path::PathBuf,
    polygonize_closed: bool,
    curve_tolerance: Option<f64>,
) -> PyResult<Py<PyAny>> {
    let bytes = std::fs::read(&path).map_err(|error| {
        PyValueError::new_err(format!("cannot read {}: {error}", path.display()))
    })?;
    convert(py, &bytes, polygonize_closed, curve_tolerance)
}

#[pymodule]
fn dwg2geo(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(convert, module)?)?;
    module.add_function(wrap_pyfunction!(convert_file, module)?)?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
