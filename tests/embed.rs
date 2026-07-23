#![cfg(feature = "native-backend")]

//! End-to-end test of the in-memory embedding API (`convert_bytes`), the
//! entry point used by the WebAssembly build.

use acadrust::{
    CadDocument, DxfVersion,
    entities::{Circle, EntityType, Line, Point},
    io::dwg::DwgWriter,
};
use tempfile::TempDir;

/// Build a tiny AC1027 drawing, write it, and return its bytes (the acadrust
/// writer only writes to a path, so round-trip through a temp file).
fn fixture_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    document
        .add_entity(EntityType::Point(Point::from_coords(5.0, 6.0, 0.0)))
        .expect("point");
    document
        .add_entity(EntityType::Line(Line::from_coords(
            0.0, 0.0, 0.0, 10.0, 4.0, 0.0,
        )))
        .expect("line");
    document
        .add_entity(EntityType::Circle(Circle::from_coords(
            20.0, 20.0, 0.0, 5.0,
        )))
        .expect("circle");
    let path = dir.join("embed-fixture.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write dwg");
    std::fs::read(&path).expect("read bytes")
}

#[test]
fn convert_bytes_produces_valid_local_geojson() {
    let dir = TempDir::new().expect("temp dir");
    let bytes = fixture_bytes(dir.path());

    let result =
        dwg2geo::backend::native::convert_bytes(&bytes, false, None).expect("conversion succeeds");

    // The GeoJSON parses and is a FeatureCollection with the expected features.
    let value: serde_json::Value = serde_json::from_str(&result.geojson).expect("valid GeoJSON");
    assert_eq!(value["type"], "FeatureCollection");
    assert_eq!(
        value["dwg2geo"]["coordinate_status"], "local-unreferenced",
        "output must be marked non-geographic"
    );

    let features = value["features"].as_array().expect("features array");
    assert_eq!(features.len(), 3);
    assert_eq!(result.feature_count, 3);

    let kinds: Vec<&str> = features
        .iter()
        .map(|f| f["properties"]["entity_type"].as_str().unwrap())
        .collect();
    assert!(kinds.contains(&"POINT") && kinds.contains(&"LINE") && kinds.contains(&"CIRCLE"));

    // The point sits at (5, 6) in drawing coordinates.
    let point = features
        .iter()
        .find(|f| f["properties"]["entity_type"] == "POINT")
        .expect("point feature");
    assert_eq!(point["geometry"]["coordinates"][0], 5.0);
    assert_eq!(point["geometry"]["coordinates"][1], 6.0);

    // Summary fields are populated.
    assert!(result.bbox.is_some());
    assert_eq!(result.model_space_entities, 3);
    assert!(!result.source_sha256.is_empty());
    assert!(result.converted.iter().any(|c| c.entity_type == "CIRCLE"));

    // Deterministic: same bytes → same GeoJSON.
    let again = dwg2geo::backend::native::convert_bytes(&bytes, false, None).expect("again");
    assert_eq!(result.geojson, again.geojson);
}

#[test]
fn convert_bytes_rejects_garbage() {
    assert!(dwg2geo::backend::native::convert_bytes(b"not a dwg file", false, None).is_err());
}
