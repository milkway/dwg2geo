#![cfg(feature = "native-backend")]

//! Reader-to-output coverage for native entity conversion and accounting.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use acadrust::{CadDocument, DxfVersion, entities::EntityType, io::dwg::DwgWriter};
use serde_json::Value;
use tempfile::TempDir;

struct FixtureOutput {
    geojson: Value,
    report: Value,
}

struct EntityCase {
    fixture_name: &'static str,
    entity: EntityType,
    expected_entity_type: &'static str,
}

/// Write one logical model-space entity to AC1027, then exercise the same
/// native conversion path a user invokes. Classic polylines additionally
/// produce owned VERTEX/SEQEND records in the DWG, but remain one top-level
/// source entity after acadrust reconstructs them.
fn convert_one(fixture_name: &str, entity: EntityType) -> FixtureOutput {
    let dir = TempDir::new().expect("create temporary fixture directory");
    let fixture = write_one_entity_fixture(dir.path(), fixture_name, entity);
    let output_path = dir.path().join(format!("{fixture_name}.geojson"));
    let output = Command::new(env!("CARGO_BIN_EXE_dwg2geo"))
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&output_path)
        .args([
            "--backend",
            "native",
            "--allow-local-coordinates",
            "--force",
        ])
        .output()
        .unwrap_or_else(|error| panic!("run native conversion for {fixture_name}: {error}"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "native conversion for {fixture_name} exited with {}\nstderr:\n{stderr}",
        output.status
    );

    let geojson_bytes = fs::read(&output_path).unwrap_or_else(|error| {
        panic!(
            "read GeoJSON for {fixture_name} at {}: {error}\nstderr:\n{stderr}",
            output_path.display()
        )
    });
    let geojson = serde_json::from_slice(&geojson_bytes).unwrap_or_else(|error| {
        panic!("parse GeoJSON for {fixture_name}: {error}\nstderr:\n{stderr}")
    });

    let report_path = PathBuf::from(format!("{}.report.json", output_path.display()));
    let report_bytes = fs::read(&report_path).unwrap_or_else(|error| {
        panic!(
            "read report for {fixture_name} at {}: {error}\nstderr:\n{stderr}",
            report_path.display()
        )
    });
    let report = serde_json::from_slice(&report_bytes).unwrap_or_else(|error| {
        panic!("parse report for {fixture_name}: {error}\nstderr:\n{stderr}")
    });

    FixtureOutput { geojson, report }
}

fn write_one_entity_fixture(dir: &Path, fixture_name: &str, entity: EntityType) -> PathBuf {
    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    document
        .add_entity(entity)
        .unwrap_or_else(|error| panic!("add {fixture_name} entity: {error}"));

    let path = dir.join(format!("{fixture_name}.dwg"));
    DwgWriter::write_to_file(&path, &document)
        .unwrap_or_else(|error| panic!("write {fixture_name} DWG fixture: {error}"));
    path
}

fn convert_supported(case: EntityCase, expected_geometry_type: &str) -> FixtureOutput {
    let fixture_name = case.fixture_name;
    let expected_entity_type = case.expected_entity_type;
    let output = convert_one(fixture_name, case.entity);
    let features = output.geojson["features"]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: GeoJSON features must be an array"));
    assert_eq!(
        features.len(),
        1,
        "{fixture_name}: expected exactly one output feature: {}",
        output.geojson
    );
    let feature = &features[0];
    assert_eq!(
        feature["properties"]["entity_type"], expected_entity_type,
        "{fixture_name}: wrong entity_type: {feature}"
    );
    assert_eq!(
        feature["geometry"]["type"], expected_geometry_type,
        "{fixture_name}: wrong geometry type: {feature}"
    );
    assert_accounted(fixture_name, &output.report);
    output
}

fn assert_deliberately_unsupported(case: EntityCase) {
    let fixture_name = case.fixture_name;
    let expected_entity_type = case.expected_entity_type;
    let output = convert_one(fixture_name, case.entity);
    let features = output.geojson["features"]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: GeoJSON features must be an array"));
    assert!(
        features.is_empty(),
        "{fixture_name}: unsupported entity emitted features: {features:?}"
    );

    let skipped = output.report["native"]["skipped"]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: native.skipped must be an array"));
    let entry = skipped
        .iter()
        .find(|entry| entry["entity_type"] == expected_entity_type)
        .unwrap_or_else(|| {
            panic!(
                "{fixture_name}: {expected_entity_type} missing from native.skipped: {skipped:?}"
            )
        });
    assert_eq!(entry["count"], 1, "{fixture_name}: wrong skipped count");
    assert!(
        entry["reason"]
            .as_str()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "{fixture_name}: skipped entry must carry a reason: {entry}"
    );
    assert_accounted(fixture_name, &output.report);
}

fn only_feature(output: &FixtureOutput) -> &Value {
    &output.geojson["features"][0]
}

fn assert_accounted(fixture_name: &str, report: &Value) {
    let accounting = &report["native"]["accounting"];
    assert_eq!(
        accounting["model_space_entities"], 1,
        "{fixture_name}: fixture must contain one model-space entity: {accounting}"
    );
    assert_eq!(
        accounting["top_level_accounted"], 1,
        "{fixture_name}: top-level accounting must balance: {accounting}"
    );
    assert_eq!(
        accounting["unaccounted"], 0,
        "{fixture_name}: source entity was not accounted for: {accounting}"
    );
}

fn line_positions<'a>(fixture_name: &str, feature: &'a Value) -> &'a [Value] {
    feature["geometry"]["coordinates"]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: LineString coordinates must be an array"))
}

fn exterior_ring<'a>(fixture_name: &str, feature: &'a Value) -> &'a [Value] {
    feature["geometry"]["coordinates"][0]
        .as_array()
        .unwrap_or_else(|| panic!("{fixture_name}: Polygon exterior ring must be an array"))
}

fn assert_closed(fixture_name: &str, positions: &[Value]) {
    assert!(
        positions.len() >= 4,
        "{fixture_name}: a closed ring needs at least four positions: {positions:?}"
    );
    assert_eq!(
        positions.first(),
        positions.last(),
        "{fixture_name}: ring must close: {positions:?}"
    );
}

fn signed_area(fixture_name: &str, ring: &[Value]) -> f64 {
    ring.windows(2)
        .map(|edge| {
            let first = edge[0]
                .as_array()
                .unwrap_or_else(|| panic!("{fixture_name}: position must be an array"));
            let second = edge[1]
                .as_array()
                .unwrap_or_else(|| panic!("{fixture_name}: position must be an array"));
            let x1 = first[0]
                .as_f64()
                .unwrap_or_else(|| panic!("{fixture_name}: x coordinate must be numeric"));
            let y1 = first[1]
                .as_f64()
                .unwrap_or_else(|| panic!("{fixture_name}: y coordinate must be numeric"));
            let x2 = second[0]
                .as_f64()
                .unwrap_or_else(|| panic!("{fixture_name}: x coordinate must be numeric"));
            let y2 = second[1]
                .as_f64()
                .unwrap_or_else(|| panic!("{fixture_name}: y coordinate must be numeric"));
            x1 * y2 - x2 * y1
        })
        .sum::<f64>()
        / 2.0
}

#[test]
fn arc_survives_reader_to_output_conversion() {
    use acadrust::entities::Arc;

    let output = convert_supported(
        EntityCase {
            fixture_name: "arc",
            entity: EntityType::Arc(Arc::from_coords(
                10.0,
                20.0,
                0.0,
                5.0,
                0.0,
                std::f64::consts::FRAC_PI_2,
            )),
            expected_entity_type: "ARC",
        },
        "LineString",
    );
    let feature = only_feature(&output);
    assert!(
        line_positions("arc", feature).len() > 2,
        "ARC must be tessellated into interior positions: {feature}"
    );
}

#[test]
fn ellipse_survives_reader_to_output_conversion() {
    use acadrust::{entities::Ellipse, types::Vector3};

    let ellipse = Ellipse::from_center_axes(
        Vector3::new(10.0, 20.0, 0.0),
        Vector3::new(6.0, 0.0, 0.0),
        0.5,
    );
    let output = convert_supported(
        EntityCase {
            fixture_name: "ellipse",
            entity: EntityType::Ellipse(ellipse),
            expected_entity_type: "ELLIPSE",
        },
        "LineString",
    );
    let feature = only_feature(&output);
    assert_closed("ellipse", line_positions("ellipse", feature));
}

#[test]
fn spline_survives_reader_to_output_conversion() {
    use acadrust::{entities::Spline, types::Vector3};

    let spline = Spline::from_control_points(
        3,
        vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(4.0, 8.0, 0.0),
            Vector3::new(8.0, -2.0, 0.0),
            Vector3::new(12.0, 4.0, 0.0),
        ],
    );
    let output = convert_supported(
        EntityCase {
            fixture_name: "spline",
            entity: EntityType::Spline(spline),
            expected_entity_type: "SPLINE",
        },
        "LineString",
    );
    let feature = only_feature(&output);
    assert!(
        line_positions("spline", feature).len() >= 17,
        "SPLINE must retain the converter's minimum sampling: {feature}"
    );
}

#[test]
fn mtext_survives_reader_to_output_conversion() {
    use acadrust::{entities::MText, types::Vector3};

    let mut mtext = MText::with_value("READER TO OUTPUT", Vector3::new(4.0, 6.0, 0.0));
    mtext.height = 2.5;
    let output = convert_supported(
        EntityCase {
            fixture_name: "mtext",
            entity: EntityType::MText(mtext),
            expected_entity_type: "MTEXT",
        },
        "Point",
    );
    let feature = only_feature(&output);
    assert_eq!(
        feature["properties"]["text"], "READER TO OUTPUT",
        "MTEXT plain text must survive the DWG round trip: {feature}"
    );
}

#[test]
fn polyline2d_survives_reader_to_output_conversion() {
    use acadrust::{
        entities::{Polyline2D, Vertex2D},
        types::Vector3,
    };

    let mut polyline = Polyline2D::new();
    for point in [
        Vector3::new(0.0, 0.0, 0.0),
        Vector3::new(5.0, 0.0, 0.0),
        Vector3::new(7.0, 3.0, 0.0),
    ] {
        polyline.add_vertex(Vertex2D::new(point));
    }
    let output = convert_supported(
        EntityCase {
            fixture_name: "polyline2d",
            entity: EntityType::Polyline2D(polyline),
            expected_entity_type: "POLYLINE",
        },
        "LineString",
    );
    let feature = only_feature(&output);
    assert_eq!(
        line_positions("polyline2d", feature).len(),
        3,
        "Polyline2D vertices must survive the DWG round trip: {feature}"
    );
}

#[test]
fn polyline3d_survives_reader_to_output_conversion() {
    use acadrust::{entities::Polyline3D, types::Vector3};

    let polyline = Polyline3D::from_points(vec![
        Vector3::new(0.0, 0.0, 2.0),
        Vector3::new(5.0, 1.0, 3.0),
        Vector3::new(8.0, 4.0, 4.0),
    ]);
    let output = convert_supported(
        EntityCase {
            fixture_name: "polyline3d",
            entity: EntityType::Polyline3D(polyline),
            expected_entity_type: "POLYLINE",
        },
        "LineString",
    );
    let feature = only_feature(&output);
    let positions = line_positions("polyline3d", feature);
    assert_eq!(
        positions.len(),
        3,
        "Polyline3D vertices must survive the DWG round trip: {feature}"
    );
    assert!(
        positions.iter().all(|position| position
            .as_array()
            .is_some_and(|position| position.len() == 2)),
        "Polyline3D must follow the converter's XY output policy: {feature}"
    );
}

#[test]
fn face3d_survives_reader_to_output_conversion() {
    use acadrust::{entities::Face3D, types::Vector3};

    let face = Face3D::new(
        Vector3::new(0.0, 0.0, 2.0),
        Vector3::new(0.0, 6.0, 2.0),
        Vector3::new(8.0, 6.0, 2.0),
        Vector3::new(8.0, 0.0, 2.0),
    );
    let output = convert_supported(
        EntityCase {
            fixture_name: "face3d",
            entity: EntityType::Face3D(face),
            expected_entity_type: "3DFACE",
        },
        "Polygon",
    );
    let feature = only_feature(&output);
    let ring = exterior_ring("face3d", feature);
    assert_closed("face3d", ring);
    assert!(
        signed_area("face3d", ring) > 0.0,
        "3DFACE exterior ring must be counter-clockwise: {ring:?}"
    );
}

#[test]
fn solid_survives_reader_to_output_conversion() {
    use acadrust::{entities::Solid, types::Vector3};

    // SOLID's third/fourth stored corners are deliberately in DXF bow-tie
    // order; the converter must untwist and orient the resulting square.
    let solid = Solid::new(
        Vector3::new(0.0, 0.0, 0.0),
        Vector3::new(8.0, 0.0, 0.0),
        Vector3::new(0.0, 6.0, 0.0),
        Vector3::new(8.0, 6.0, 0.0),
    );
    let output = convert_supported(
        EntityCase {
            fixture_name: "solid",
            entity: EntityType::Solid(solid),
            expected_entity_type: "SOLID",
        },
        "Polygon",
    );
    let feature = only_feature(&output);
    let ring = exterior_ring("solid", feature);
    assert_closed("solid", ring);
    assert!(
        signed_area("solid", ring) > 0.0,
        "SOLID exterior ring must be counter-clockwise: {ring:?}"
    );
}

#[test]
fn hatch_survives_reader_to_output_conversion() {
    use acadrust::{
        entities::{BoundaryEdge, BoundaryPath, Hatch, LineEdge},
        types::Vector2,
    };

    let corners = [
        Vector2::new(0.0, 0.0),
        Vector2::new(10.0, 0.0),
        Vector2::new(10.0, 7.0),
        Vector2::new(0.0, 7.0),
    ];
    let mut path = BoundaryPath::external();
    for index in 0..corners.len() {
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: corners[index],
            end: corners[(index + 1) % corners.len()],
        }));
    }
    let mut hatch = Hatch::solid();
    hatch.add_path(path);

    let output = convert_supported(
        EntityCase {
            fixture_name: "hatch",
            entity: EntityType::Hatch(hatch),
            expected_entity_type: "HATCH",
        },
        "Polygon",
    );
    let feature = only_feature(&output);
    assert_closed("hatch", exterior_ring("hatch", feature));
}

#[test]
fn xline_is_reported_as_deliberately_unsupported() {
    use acadrust::{entities::XLine, types::Vector3};

    assert_deliberately_unsupported(EntityCase {
        fixture_name: "xline",
        entity: EntityType::XLine(XLine::new(Vector3::new(2.0, 3.0, 0.0), Vector3::UNIT_X)),
        expected_entity_type: "XLINE",
    });
}

#[test]
fn solid3d_is_reported_as_deliberately_unsupported() {
    use acadrust::entities::Solid3D;

    assert_deliberately_unsupported(EntityCase {
        fixture_name: "solid3d",
        entity: EntityType::Solid3D(Solid3D::new()),
        expected_entity_type: "3DSOLID",
    });
}

#[test]
fn leader_is_reported_as_not_yet_converted() {
    use acadrust::{entities::Leader, types::Vector3};

    assert_deliberately_unsupported(EntityCase {
        fixture_name: "leader",
        entity: EntityType::Leader(Leader::from_vertices(vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(4.0, 5.0, 0.0),
            Vector3::new(8.0, 5.0, 0.0),
        ])),
        expected_entity_type: "LEADER",
    });
}

// `Surface::new` is public, but acadrust 0.4.1's DWG writer only serializes a
// Surface when it already carries reader-preserved `raw_dwg_data`. A newly
// constructed Surface is therefore dropped by the writer and cannot serve as
// a reader-to-output fixture. The native ACIS accounting path is covered here
// with constructible `Solid3D` instead.
