#![cfg(feature = "native-backend")]

//! Golden GeoJSON coverage for the native converter.
//! Regenerate the committed fixture with:
//! `UPDATE_GOLDEN=1 cargo test --features native-backend --test golden`

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Number, Value};
use tempfile::TempDir;

const COORDINATE_TOLERANCE: f64 = 1e-9;
const GOLDEN_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/golden/native-basic.geojson"
);

fn write_golden_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        BlockRecord, CadDocument, DxfVersion,
        entities::{Circle, EntityType, Insert, Line, LwPolyline, Point, Text},
        io::dwg::DwgWriter,
        tables::Layer,
        types::{Vector2, Vector3},
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    for name in ["GEOMETRY", "ANNOTATION", "BLOCKS"] {
        let mut layer = Layer::new(name);
        layer.handle = document.allocate_handle();
        document.layers.add(layer).expect("add fixture layer");
    }

    let mut line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 10.0, 5.0, 0.0));
    line.common_mut().layer = "GEOMETRY".to_string();
    document.add_entity(line).expect("add LINE");

    let mut point = EntityType::Point(Point::from_coords(2.5, -1.0, 0.0));
    point.common_mut().layer = "GEOMETRY".to_string();
    document.add_entity(point).expect("add POINT");

    let mut closed = LwPolyline::new();
    for (x, y) in [(20.0, 0.0), (30.0, 0.0), (30.0, 8.0), (20.0, 8.0)] {
        closed.add_point(Vector2::new(x, y));
    }
    closed.is_closed = true;
    let mut closed = EntityType::LwPolyline(closed);
    closed.common_mut().layer = "GEOMETRY".to_string();
    document.add_entity(closed).expect("add closed LWPOLYLINE");

    let mut bulged = LwPolyline::new();
    bulged.add_point_with_bulge(Vector2::new(40.0, 0.0), 0.5);
    bulged.add_point(Vector2::new(50.0, 0.0));
    let mut bulged = EntityType::LwPolyline(bulged);
    bulged.common_mut().layer = "GEOMETRY".to_string();
    document.add_entity(bulged).expect("add bulged LWPOLYLINE");

    let mut circle = EntityType::Circle(Circle::from_coords(65.0, 5.0, 0.0, 3.0));
    circle.common_mut().layer = "GEOMETRY".to_string();
    document.add_entity(circle).expect("add CIRCLE");

    let mut text = Text::new();
    text.value = "GOLDEN LABEL".to_string();
    text.insertion_point = Vector3::new(5.0, 20.0, 0.0);
    text.height = 2.5;
    text.rotation = std::f64::consts::FRAC_PI_6;
    let mut text = EntityType::Text(text);
    text.common_mut().layer = "ANNOTATION".to_string();
    document.add_entity(text).expect("add TEXT");

    let mut block_record = BlockRecord::new("GOLDEN_BLOCK");
    block_record.handle = document.allocate_handle();
    block_record.base_point = Vector3::ZERO;
    let block_owner = block_record.handle;
    document
        .block_records
        .add(block_record)
        .expect("add block record");

    let mut block_line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 4.0, 0.0, 0.0));
    block_line.common_mut().owner_handle = block_owner;
    document.add_entity(block_line).expect("add block LINE");

    let mut insert = Insert::new("GOLDEN_BLOCK", Vector3::new(80.0, 10.0, 0.0));
    insert.set_x_scale(2.0);
    insert.set_y_scale(2.0);
    insert.set_z_scale(2.0);
    insert.rotation = std::f64::consts::FRAC_PI_2;
    let mut insert = EntityType::Insert(insert);
    insert.common_mut().layer = "BLOCKS".to_string();
    document.add_entity(insert).expect("add INSERT");

    let path = dir.join("native-basic.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write golden DWG fixture");
    path
}

fn converted_geojson() -> Value {
    let dir = TempDir::new().expect("create temporary directory");
    let fixture = write_golden_fixture(dir.path());
    let output_path = dir.path().join("native-basic.geojson");
    let output = Command::new(env!("CARGO_BIN_EXE_dwg2geo"))
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&output_path)
        .args(["--backend", "native", "--allow-local-coordinates"])
        .output()
        .expect("run native conversion");

    assert!(
        output.status.success(),
        "native conversion failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&output_path).expect("read native GeoJSON output");
    serde_json::from_slice(&bytes).expect("native conversion must emit valid JSON")
}

fn normalized_for_golden(mut value: Value) -> Value {
    let metadata = value
        .get_mut("dwg2geo")
        .and_then(Value::as_object_mut)
        .expect("GeoJSON must contain a top-level dwg2geo object");
    metadata.insert(
        "source_sha256".to_string(),
        Value::String("<ignored>".to_string()),
    );
    value
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn index_path(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn shown_path(path: &str) -> &str {
    if path.is_empty() { "<root>" } else { path }
}

fn compare_json(expected: &Value, actual: &Value) -> Result<(), String> {
    compare_at(expected, actual, "", false)
}

fn compare_at(
    expected: &Value,
    actual: &Value,
    path: &str,
    inside_coordinates: bool,
) -> Result<(), String> {
    match (expected, actual) {
        (Value::Null, Value::Null) => Ok(()),
        (Value::Bool(expected), Value::Bool(actual)) if expected == actual => Ok(()),
        (Value::String(expected), Value::String(actual)) if expected == actual => Ok(()),
        (Value::Number(expected), Value::Number(actual)) => {
            if inside_coordinates {
                let expected = expected
                    .as_f64()
                    .expect("JSON coordinate number must be representable as f64");
                let actual = actual
                    .as_f64()
                    .expect("JSON coordinate number must be representable as f64");
                if (expected - actual).abs() <= COORDINATE_TOLERANCE {
                    Ok(())
                } else {
                    Err(format!(
                        "{}: {expected} != {actual} (tolerance {COORDINATE_TOLERANCE})",
                        shown_path(path)
                    ))
                }
            } else if expected == actual {
                Ok(())
            } else {
                Err(format!(
                    "{}: {expected} != {actual} (exact comparison)",
                    shown_path(path)
                ))
            }
        }
        (Value::Array(expected), Value::Array(actual)) => {
            if expected.len() != actual.len() {
                return Err(format!(
                    "{}: array length {} != {}",
                    shown_path(path),
                    expected.len(),
                    actual.len()
                ));
            }
            for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
                compare_at(
                    expected,
                    actual,
                    &index_path(path, index),
                    inside_coordinates,
                )?;
            }
            Ok(())
        }
        (Value::Object(expected), Value::Object(actual)) => {
            let ignored_source_hash = path == "dwg2geo";
            let expected_keys: BTreeSet<&str> = expected
                .keys()
                .map(String::as_str)
                .filter(|key| !(ignored_source_hash && *key == "source_sha256"))
                .collect();
            let actual_keys: BTreeSet<&str> = actual
                .keys()
                .map(String::as_str)
                .filter(|key| !(ignored_source_hash && *key == "source_sha256"))
                .collect();
            if let Some(key) = expected_keys.difference(&actual_keys).next() {
                return Err(format!("{}: missing object member", child_path(path, key)));
            }
            if let Some(key) = actual_keys.difference(&expected_keys).next() {
                return Err(format!(
                    "{}: unexpected object member",
                    child_path(path, key)
                ));
            }
            for key in expected_keys {
                let next_path = child_path(path, key);
                compare_at(
                    &expected[key],
                    &actual[key],
                    &next_path,
                    inside_coordinates || key == "coordinates",
                )?;
            }
            Ok(())
        }
        _ => Err(format!("{}: {expected} != {actual}", shown_path(path))),
    }
}

fn perturb_first_coordinate(
    value: &mut Value,
    path: &str,
    inside_coordinates: bool,
) -> Option<String> {
    match value {
        Value::Number(number) if inside_coordinates => {
            let perturbed = number.as_f64().expect("coordinate must be numeric") + 1e-6;
            *number = Number::from_f64(perturbed).expect("perturbed coordinate must be finite");
            Some(path.to_string())
        }
        Value::Array(values) => values.iter_mut().enumerate().find_map(|(index, value)| {
            perturb_first_coordinate(value, &index_path(path, index), inside_coordinates)
        }),
        Value::Object(values) => values.iter_mut().find_map(|(key, value)| {
            let next_path = child_path(path, key);
            perturb_first_coordinate(
                value,
                &next_path,
                inside_coordinates || key == "coordinates",
            )
        }),
        _ => None,
    }
}

#[test]
fn native_basic_matches_golden_geojson() {
    let actual = converted_geojson();
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        let normalized = normalized_for_golden(actual);
        let mut json = serde_json::to_string_pretty(&normalized).expect("serialize golden JSON");
        json.push('\n');
        fs::create_dir_all(Path::new(GOLDEN_PATH).parent().expect("golden parent"))
            .expect("create golden directory");
        fs::write(GOLDEN_PATH, json).expect("rewrite golden GeoJSON");
        return;
    }

    let expected: Value = serde_json::from_slice(
        &fs::read(GOLDEN_PATH).expect("read golden GeoJSON; regenerate with UPDATE_GOLDEN=1"),
    )
    .expect("golden file must contain valid JSON");
    if let Err(mismatch) = compare_json(&expected, &actual) {
        panic!("native GeoJSON differs from golden file at {mismatch}");
    }
}

#[test]
fn golden_comparator_detects_coordinate_changes_above_tolerance() {
    let expected = converted_geojson();
    let mut perturbed = expected.clone();
    let path = perturb_first_coordinate(&mut perturbed, "", false)
        .expect("fixture output must contain at least one coordinate");

    let mismatch = compare_json(&expected, &perturbed)
        .expect_err("a 1e-6 coordinate change must exceed the 1e-9 tolerance");
    assert!(
        mismatch.contains(&path),
        "mismatch path missing: {mismatch}"
    );
    assert!(
        mismatch.contains("tolerance 0.000000001"),
        "mismatch tolerance missing: {mismatch}"
    );
}
