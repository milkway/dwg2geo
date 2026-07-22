#![cfg(feature = "native-backend")]

//! Differential coverage between the native converter and LibreDWG's direct
//! GeoJSON writer. These tests deliberately compare spatial and count
//! invariants, not serialization or vertex ordering.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;
use tempfile::TempDir;

const COORDINATE_TOLERANCE: f64 = 1e-6;

#[derive(Debug)]
struct OutputSummary {
    total_features: usize,
    bbox: [f64; 4],
    geometry_counts: BTreeMap<String, usize>,
    entity_counts: BTreeMap<String, usize>,
}

fn dwgread_available() -> bool {
    match Command::new("dwgread").arg("--version").output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "skipping differential test: `dwgread --version` exited with {}",
                output.status
            );
            false
        }
        Err(error) => {
            eprintln!("skipping differential test: cannot run `dwgread --version`: {error}");
            false
        }
    }
}

fn add_layer(document: &mut acadrust::CadDocument, name: &str) {
    use acadrust::tables::Layer;

    let mut layer = Layer::new(name);
    layer.handle = document.allocate_handle();
    document.layers.add(layer).expect("add fixture layer");
}

fn write_points_and_lines_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{EntityType, Line, Point},
        io::dwg::DwgWriter,
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    add_layer(&mut document, "PRIMITIVES");

    for (x, y) in [(-4.0, -3.0), (12.0, 9.0)] {
        let mut point = EntityType::Point(Point::from_coords(x, y, 0.0));
        point.common_mut().layer = "PRIMITIVES".to_string();
        document.add_entity(point).expect("add POINT");
    }

    for (start, end) in [((-4.0, 1.0), (8.0, 9.0)), ((2.0, -3.0), (12.0, 4.0))] {
        let mut line =
            EntityType::Line(Line::from_coords(start.0, start.1, 0.0, end.0, end.1, 0.0));
        line.common_mut().layer = "PRIMITIVES".to_string();
        document.add_entity(line).expect("add LINE");
    }

    let path = dir.join("points-lines.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write points/lines DWG fixture");
    path
}

fn write_curves_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, LwPolyline, Point},
        io::dwg::DwgWriter,
        types::Vector2,
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    add_layer(&mut document, "CURVES");

    // The outer closed polyline pins the drawing-wide bbox independently of
    // the two converters' legitimate curve tessellation differences.
    let mut closed = LwPolyline::new();
    for (x, y) in [(0.0, 0.0), (40.0, 0.0), (40.0, 20.0), (0.0, 20.0)] {
        closed.add_point(Vector2::new(x, y));
    }
    closed.is_closed = true;
    let mut closed = EntityType::LwPolyline(closed);
    closed.common_mut().layer = "CURVES".to_string();
    document.add_entity(closed).expect("add closed LWPOLYLINE");

    let mut bulged = LwPolyline::new();
    bulged.add_point_with_bulge(Vector2::new(6.0, 10.0), 0.5);
    bulged.add_point(Vector2::new(16.0, 10.0));
    let mut bulged = EntityType::LwPolyline(bulged);
    bulged.common_mut().layer = "CURVES".to_string();
    document.add_entity(bulged).expect("add bulged LWPOLYLINE");

    let mut circle = EntityType::Circle(Circle::from_coords(28.0, 10.0, 0.0, 3.0));
    circle.common_mut().layer = "CURVES".to_string();
    document.add_entity(circle).expect("add CIRCLE");

    // dwgread 0.14 emits malformed JSON when the generated CIRCLE is the
    // drawing's final entity (an unseparated null feature follows it). Keep a
    // comparable trailing POINT so this test exercises the successful direct
    // GeoJSON comparison path; docs/DIFFERENTIAL.md records the serializer bug.
    let mut trailing_point = EntityType::Point(Point::from_coords(20.0, 20.0, 0.0));
    trailing_point.common_mut().layer = "CURVES".to_string();
    document
        .add_entity(trailing_point)
        .expect("add trailing POINT");

    let path = dir.join("closed-bulged-circle.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write curves DWG fixture");
    path
}

fn write_text_and_insert_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        BlockRecord, CadDocument, DxfVersion,
        entities::{EntityType, Insert, Line, Point, Text},
        io::dwg::DwgWriter,
        types::Vector3,
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    add_layer(&mut document, "ANNOTATION");
    add_layer(&mut document, "SYMBOLS");

    let mut text = Text::new();
    text.value = "DIFFERENTIAL LABEL".to_string();
    text.insertion_point = Vector3::new(25.0, 22.0, 0.0);
    text.height = 1.5;
    let mut text = EntityType::Text(text);
    text.common_mut().layer = "ANNOTATION".to_string();
    document.add_entity(text).expect("add TEXT");

    // The explicit point is a comparable primitive and pins the union bbox to
    // include LibreDWG's untransformed block-definition child. LibreDWG also
    // emits the INSERT anchor itself, which contains the native exploded
    // child's first vertex.
    let mut anchor = EntityType::Point(Point::from_coords(0.0, 0.0, 0.0));
    anchor.common_mut().layer = "SYMBOLS".to_string();
    document.add_entity(anchor).expect("add anchor POINT");

    let mut block = BlockRecord::new("DIFF_SYMBOL");
    block.handle = document.allocate_handle();
    block.base_point = Vector3::ZERO;
    let block_owner = block.handle;
    document.block_records.add(block).expect("add block record");

    let mut child = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 5.0, 2.0, 0.0));
    child.common_mut().owner_handle = block_owner;
    document.add_entity(child).expect("add block LINE");

    let mut insert = EntityType::Insert(Insert::new("DIFF_SYMBOL", Vector3::new(20.0, 20.0, 0.0)));
    insert.common_mut().layer = "SYMBOLS".to_string();
    document.add_entity(insert).expect("add INSERT");

    let path = dir.join("text-insert.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write text/INSERT DWG fixture");
    path
}

fn run_conversion(fixture: &Path, output: &Path, native: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_dwg2geo"));
    command
        .arg("convert")
        .arg(fixture)
        .arg("--output")
        .arg(output);
    if native {
        command.args(["--backend", "native"]);
    }
    let result = command
        .arg("--allow-local-coordinates")
        .output()
        .expect("run dwg2geo conversion");
    assert!(
        result.status.success(),
        "{} conversion failed:\n{}",
        if native { "native" } else { "external" },
        String::from_utf8_lossy(&result.stderr)
    );

    serde_json::from_slice(&fs::read(output).expect("read conversion output"))
        .expect("conversion output must be valid JSON")
}

fn features(geojson: &Value) -> &[Value] {
    geojson["features"]
        .as_array()
        .expect("GeoJSON FeatureCollection must contain a features array")
}

fn collect_coordinates(value: &Value, coordinates: &mut Vec<[f64; 2]>) {
    let Some(values) = value.as_array() else {
        return;
    };
    if values.len() >= 2 && values[0].is_number() && values[1].is_number() {
        coordinates.push([
            values[0].as_f64().expect("finite x coordinate"),
            values[1].as_f64().expect("finite y coordinate"),
        ]);
        return;
    }
    for child in values {
        collect_coordinates(child, coordinates);
    }
}

fn all_coordinates(geojson: &Value) -> Vec<[f64; 2]> {
    let mut coordinates = Vec::new();
    for feature in features(geojson) {
        collect_coordinates(&feature["geometry"]["coordinates"], &mut coordinates);
    }
    coordinates
}

fn bbox(coordinates: &[[f64; 2]]) -> [f64; 4] {
    let first = coordinates
        .first()
        .expect("output must contain coordinates");
    coordinates.iter().skip(1).fold(
        [first[0], first[1], first[0], first[1]],
        |[min_x, min_y, max_x, max_y], [x, y]| {
            [min_x.min(*x), min_y.min(*y), max_x.max(*x), max_y.max(*y)]
        },
    )
}

fn canonical_external_entity_type(subclasses: &str) -> String {
    let class = subclasses.rsplit(':').next().unwrap_or(subclasses).trim();
    let class = class.strip_prefix("AcDb").unwrap_or(class);
    match class {
        "BlockReference" => "INSERT".to_string(),
        "Polyline" => "LWPOLYLINE".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

fn summarize(geojson: &Value, native: bool) -> OutputSummary {
    let mut geometry_counts = BTreeMap::new();
    let mut entity_counts = BTreeMap::new();
    for feature in features(geojson) {
        let geometry_type = feature["geometry"]["type"].as_str().expect("geometry type");
        *geometry_counts
            .entry(geometry_type.to_string())
            .or_default() += 1;

        let entity_type = if native {
            feature["properties"]["entity_type"]
                .as_str()
                .expect("native entity_type")
                .to_string()
        } else {
            canonical_external_entity_type(
                feature["properties"]["SubClasses"]
                    .as_str()
                    .expect("LibreDWG SubClasses"),
            )
        };
        *entity_counts.entry(entity_type).or_default() += 1;
    }

    OutputSummary {
        total_features: features(geojson).len(),
        bbox: bbox(&all_coordinates(geojson)),
        geometry_counts,
        entity_counts,
    }
}

fn assert_bbox_matches(native: [f64; 4], external: [f64; 4]) {
    for (index, (native, external)) in native.into_iter().zip(external).enumerate() {
        assert!(
            (native - external).abs() <= COORDINATE_TOLERANCE,
            "bbox ordinate {index} differs: native={native}, external={external}, tolerance={COORDINATE_TOLERANCE}"
        );
    }
}

fn first_vertex(feature: &Value) -> Option<[f64; 2]> {
    let coordinates = &feature["geometry"]["coordinates"];
    match feature["geometry"]["type"].as_str()? {
        "Point" => Some([coordinates[0].as_f64()?, coordinates[1].as_f64()?]),
        "LineString" => Some([coordinates[0][0].as_f64()?, coordinates[0][1].as_f64()?]),
        _ => None,
    }
}

fn assert_native_first_vertices_are_contained(native: &Value, external: &Value) {
    let external_coordinates = all_coordinates(external);
    for feature in features(native) {
        let Some(vertex) = first_vertex(feature) else {
            continue;
        };
        assert!(
            external_coordinates.iter().any(|candidate| {
                (vertex[0] - candidate[0]).abs() <= COORDINATE_TOLERANCE
                    && (vertex[1] - candidate[1]).abs() <= COORDINATE_TOLERANCE
            }),
            "native feature {:?} first vertex {vertex:?} is absent from external coordinates",
            feature.get("id")
        );
    }
}

fn convert_pair(dir: &Path, fixture: &Path) -> (Value, Value) {
    let native = run_conversion(fixture, &dir.join("native.geojson"), true);
    let external = run_conversion(fixture, &dir.join("external.geojson"), false);
    (native, external)
}

fn assert_shared_spatial_invariants(native: &Value, external: &Value) {
    assert_bbox_matches(
        summarize(native, true).bbox,
        summarize(external, false).bbox,
    );
    assert_native_first_vertices_are_contained(native, external);
}

#[test]
fn points_and_lines_match_exactly() {
    if !dwgread_available() {
        return;
    }
    let dir = TempDir::new().expect("create temporary directory");
    let fixture = write_points_and_lines_fixture(dir.path());
    let (native, external) = convert_pair(dir.path(), &fixture);
    let native_summary = summarize(&native, true);
    let external_summary = summarize(&external, false);
    eprintln!("points/lines native={native_summary:?} external={external_summary:?}");

    assert_bbox_matches(native_summary.bbox, [-4.0, -3.0, 12.0, 9.0]);
    assert_eq!(native_summary.total_features, 4);
    assert_eq!(external_summary.total_features, 4);
    assert_eq!(native_summary.entity_counts, external_summary.entity_counts);
    assert_eq!(
        native_summary.geometry_counts,
        external_summary.geometry_counts
    );
    assert_shared_spatial_invariants(&native, &external);
}

#[test]
fn closed_bulged_polylines_and_circle_preserve_corpus_invariants() {
    if !dwgread_available() {
        return;
    }
    let dir = TempDir::new().expect("create temporary directory");
    let fixture = write_curves_fixture(dir.path());
    let (native, external) = convert_pair(dir.path(), &fixture);
    let native_summary = summarize(&native, true);
    let external_summary = summarize(&external, false);
    eprintln!("curves native={native_summary:?} external={external_summary:?}");

    assert_bbox_matches(native_summary.bbox, [0.0, 0.0, 40.0, 20.0]);
    assert_eq!(native_summary.total_features, 4);
    assert_eq!(external_summary.total_features, 4);
    assert_eq!(native_summary.entity_counts, external_summary.entity_counts);
    assert_eq!(
        native_summary.geometry_counts,
        BTreeMap::from([("LineString".to_string(), 3), ("Point".to_string(), 1)])
    );
    assert_eq!(
        external_summary.geometry_counts,
        BTreeMap::from([
            ("LineString".to_string(), 1),
            ("Point".to_string(), 1),
            ("Polygon".to_string(), 2),
        ])
    );
    assert_shared_spatial_invariants(&native, &external);
}

#[test]
fn text_and_insert_pin_the_documented_block_divergence() {
    if !dwgread_available() {
        return;
    }
    let dir = TempDir::new().expect("create temporary directory");
    let fixture = write_text_and_insert_fixture(dir.path());
    let (native, external) = convert_pair(dir.path(), &fixture);
    let native_summary = summarize(&native, true);
    let external_summary = summarize(&external, false);
    eprintln!("text/INSERT native={native_summary:?} external={external_summary:?}");

    assert_bbox_matches(native_summary.bbox, [0.0, 0.0, 25.0, 22.0]);
    assert_eq!(native_summary.total_features, 3);
    assert_eq!(external_summary.total_features, 4);
    assert_eq!(native_summary.entity_counts.get("POINT"), Some(&1));
    assert_eq!(external_summary.entity_counts.get("POINT"), Some(&1));
    assert_eq!(native_summary.entity_counts.get("LINE"), Some(&1));
    assert_eq!(external_summary.entity_counts.get("LINE"), Some(&1));
    assert_eq!(native_summary.entity_counts.get("TEXT"), Some(&1));
    assert_eq!(external_summary.entity_counts.get("TEXT"), Some(&1));
    assert_eq!(native_summary.entity_counts.get("INSERT"), None);
    assert_eq!(external_summary.entity_counts.get("INSERT"), Some(&1));
    assert_eq!(
        native_summary.geometry_counts,
        BTreeMap::from([("LineString".to_string(), 1), ("Point".to_string(), 2)])
    );
    assert_eq!(
        external_summary.geometry_counts,
        BTreeMap::from([("LineString".to_string(), 1), ("Point".to_string(), 3)])
    );
    assert_shared_spatial_invariants(&native, &external);
}
