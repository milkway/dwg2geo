use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::TempDir;

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_dwg2geo"))
}

fn write_fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"AC1027synthetic-cli-fixture").expect("write fixture");
    path
}

#[test]
fn inspect_emits_ac1027_json() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("inspect")
        .arg(&fixture)
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("AC1027"));
    assert!(stdout.contains("AutoCAD 2013"));
}

#[test]
fn inspect_handles_spaces_and_non_ascii_paths() {
    let dir = TempDir::new().expect("temporary directory");
    let sub = dir.path().join("peça técnica nº 1");
    fs::create_dir_all(&sub).expect("create non-ascii directory");
    let fixture = write_fixture(&sub, "corredor sul – trecho ç.dwg");

    let output = binary()
        .arg("inspect")
        .arg(&fixture)
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("inspect --json must emit valid JSON");
    assert_eq!(parsed["signature"], "AC1027");
}

#[test]
fn convert_requires_coordinate_policy() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("coordinate policy is required"), "{stderr}");
    assert!(stderr.contains("--source-crs"), "{stderr}");
}

#[test]
fn conflicting_coordinate_flags_are_a_usage_error() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--source-crs", "EPSG:31985", "--allow-local-coordinates"])
        .output()
        .expect("run binary");

    // clap reports conflicting arguments as a usage error.
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn layer_filters_require_the_gdal_route() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--allow-local-coordinates", "--include-layers", "EIXO"])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--source-crs"));
}

#[test]
fn convert_refuses_existing_output_without_force() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");
    let out = dir.path().join("out.geojson");
    fs::write(&out, "precious previous output").expect("seed output");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(["--source-crs", "EPSG:31985"])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--force"));
    assert_eq!(
        fs::read_to_string(&out).expect("output must survive"),
        "precious previous output"
    );
}

#[test]
fn doctor_json_reports_both_tools() {
    let output = binary()
        .args(["doctor", "--json"])
        .output()
        .expect("run binary");

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("doctor --json must emit valid JSON");
    assert!(parsed["healthy"].is_boolean());

    let tools = parsed["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(names, ["dwgread", "ogr2ogr"]);
    for tool in tools {
        assert!(tool["status"].is_string());
        assert!(tool["required"].is_boolean());
    }
}

#[cfg(unix)]
#[test]
fn missing_tools_fail_actionably_and_leave_no_partial_output() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");
    let out = dir.path().join("out.geojson");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .arg("--allow-local-coordinates")
        .env("PATH", "")
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("dwgread"), "stderr: {stderr}");
    assert!(stderr.contains("dwg2geo doctor"), "stderr: {stderr}");
    assert!(stderr.contains("LibreDWG"), "stderr: {stderr}");

    assert!(!out.exists());
    assert!(!dir.path().join("out.geojson.partial").exists());
    assert!(!dir.path().join("out.geojson.report.json").exists());
}

/// Install a stand-in for dwgread/ogr2ogr that records its arguments and
/// writes a small FeatureCollection to the destination path (the
/// second-to-last argument in every invocation this program issues).
#[cfg(unix)]
fn install_stub(dir: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(name);
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "{name} stub 1.0.0"
  exit 0
fi
printf '%s\n' "$@" > "$0.args"
prev=""
dst=""
for a in "$@"; do
  dst="$prev"
  prev="$a"
done
printf '{{"type":"FeatureCollection","features":[]}}' > "$dst"
"#
    );
    fs::write(&path, script).expect("write stub");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
}

#[cfg(unix)]
#[test]
fn stubbed_conversion_writes_output_report_and_intermediate() {
    let stub_dir = TempDir::new().expect("stub directory");
    install_stub(stub_dir.path(), "dwgread");
    install_stub(stub_dir.path(), "ogr2ogr");

    let dir = TempDir::new().expect("temporary directory");
    let workspace = dir.path().join("saída ç");
    fs::create_dir_all(&workspace).expect("create workspace");
    let fixture = write_fixture(&workspace, "corredor sul.dwg");
    let out = workspace.join("corredor sul.geojson");
    fs::write(&out, "old output").expect("seed output for --force");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args([
            "--source-crs",
            "EPSG:31985",
            "--include-layers",
            "EIXO,PISTA SUL",
            "--keep-intermediate",
            "--force",
        ])
        .env("PATH", stub_dir.path())
        .output()
        .expect("run binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let geojson = fs::read_to_string(&out).expect("output exists");
    assert_eq!(geojson, r#"{"type":"FeatureCollection","features":[]}"#);
    assert!(
        workspace
            .join("corredor sul.geojson.intermediate.dxf")
            .exists()
    );
    assert!(!workspace.join("corredor sul.geojson.partial").exists());

    let report_text = fs::read_to_string(workspace.join("corredor sul.geojson.report.json"))
        .expect("report exists");
    let report: serde_json::Value = serde_json::from_str(&report_text).expect("report is JSON");
    assert_eq!(report["report_version"], 1);
    assert_eq!(report["source"]["signature"], "AC1027");
    assert_eq!(report["source"]["sha256"].as_str().expect("hash").len(), 64);
    assert_eq!(report["options"]["source_crs"], "EPSG:31985");
    assert_eq!(report["options"]["target_crs"], "EPSG:4326");
    assert_eq!(report["options"]["include_layers"][1], "PISTA SUL");
    assert_eq!(report["steps"].as_array().expect("steps").len(), 2);
    assert_eq!(
        report["output"]["size_bytes"].as_u64(),
        Some(geojson.len() as u64)
    );
    for tool in report["external_tools"].as_array().expect("tools") {
        assert_eq!(tool["status"], "available");
        assert!(
            tool["version"]
                .as_str()
                .expect("version")
                .contains("stub 1.0.0")
        );
    }

    let ogr_args =
        fs::read_to_string(stub_dir.path().join("ogr2ogr.args")).expect("ogr2ogr was invoked");
    assert!(
        ogr_args.contains("Layer IN ('EIXO', 'PISTA SUL')"),
        "ogr2ogr args: {ogr_args}"
    );
}

#[cfg(not(feature = "native-backend"))]
#[test]
fn layers_without_native_backend_explains_rebuild() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("layers")
        .arg(&fixture)
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--features native-backend"));
}

/// Write a small synthetic AC1027 drawing with two custom layers, three
/// model-space entities, and one paper-space entity.
#[cfg(feature = "native-backend")]
fn write_native_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, Line, Point},
        io::dwg::DwgWriter,
        tables::Layer,
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);

    let mut eixo = Layer::new("EIXO");
    eixo.handle = document.allocate_handle();
    document.layers.add(eixo).expect("add EIXO layer");

    let mut apoio = Layer::new("APOIO");
    apoio.handle = document.allocate_handle();
    apoio.flags.frozen = true;
    document.layers.add(apoio).expect("add APOIO layer");

    let mut line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 100.0, 50.0, 0.0));
    line.common_mut().layer = "EIXO".to_string();
    document.add_entity(line).expect("add line");

    let mut second = EntityType::Line(Line::from_coords(10.0, 0.0, 0.0, 10.0, 90.0, 0.0));
    second.common_mut().layer = "EIXO".to_string();
    document.add_entity(second).expect("add second line");

    let mut point = EntityType::Point(Point::from_coords(5.0, 5.0, 0.0));
    point.common_mut().layer = "APOIO".to_string();
    document.add_entity(point).expect("add point");

    let mut circle = EntityType::Circle(Circle::from_coords(50.0, 25.0, 0.0, 12.5));
    circle.common_mut().layer = "EIXO".to_string();
    document
        .add_paper_space_entity(circle)
        .expect("add paper-space circle");

    let path = dir.join("native fixture ç.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write DWG fixture");
    path
}

#[cfg(feature = "native-backend")]
#[test]
fn native_inspect_extends_json_with_histogram() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_native_fixture(dir.path());

    let output = binary()
        .arg("inspect")
        .arg(&fixture)
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("inspect --json must emit valid JSON");

    assert_eq!(parsed["signature"], "AC1027");
    let native = &parsed["native"];
    assert_eq!(native["dwg_version"], "AC1027");
    assert_eq!(native["read_mode"], "strict");
    assert_eq!(native["layer_count"], 3);
    assert_eq!(native["entity_counts"]["model_space"], 3);
    assert_eq!(native["entity_counts"]["paper_space"], 1);

    let histogram = native["entity_histogram"].as_array().expect("histogram");
    let types: Vec<&str> = histogram
        .iter()
        .map(|entry| entry["entity_type"].as_str().expect("type"))
        .collect();
    assert_eq!(types, ["CIRCLE", "LINE", "POINT"]);
}

#[cfg(feature = "native-backend")]
#[test]
fn native_inspect_reports_parse_failure_without_failing_file_inspection() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fake.dwg");

    let output = binary()
        .arg("inspect")
        .arg(&fixture)
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("inspect --json must emit valid JSON");
    assert_eq!(parsed["signature"], "AC1027");
    assert!(parsed.get("native").is_none());
    assert!(
        parsed["native_error"]
            .as_str()
            .expect("native_error")
            .contains("strict error")
    );
}

#[cfg(feature = "native-backend")]
#[test]
fn layers_json_lists_sorted_layers_with_counts() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_native_fixture(dir.path());

    let output = binary()
        .arg("layers")
        .arg(&fixture)
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("layers --json must emit valid JSON");

    let names: Vec<&str> = parsed["layers"]
        .as_array()
        .expect("layers array")
        .iter()
        .map(|layer| layer["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["0", "APOIO", "EIXO"]);

    let eixo = &parsed["layers"][2];
    assert_eq!(eixo["entity_counts"]["model_space"], 2);
    assert_eq!(eixo["entity_counts"]["paper_space"], 1);
    let apoio = &parsed["layers"][1];
    assert_eq!(apoio["frozen"], true);
    assert_eq!(apoio["entity_types"][0]["entity_type"], "POINT");
}

#[test]
fn polygonize_closed_requires_native_backend() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--allow-local-coordinates", "--polygonize-closed"])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--backend native"), "stderr: {stderr}");
}

#[test]
fn geojson_seq_requires_native_backend() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojsonseq"))
        .args([
            "--allow-local-coordinates",
            "--output-format",
            "geojson-seq",
        ])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--backend native"), "stderr: {stderr}");
}

#[test]
fn block_modes_require_native_backend_and_are_mutually_exclusive() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_fixture(dir.path(), "fixture.dwg");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--allow-local-coordinates", "--preserve-inserts"])
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--backend native"), "stderr: {stderr}");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args([
            "--allow-local-coordinates",
            "--explode-blocks",
            "--preserve-inserts",
        ])
        .output()
        .expect("run binary");
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot be used with"), "stderr: {stderr}");
}

/// Fixture for conversion tests: three convertible model-space entities, a
/// bulged polyline and a circle that must be skipped with reasons, and one
/// paper-space entity that must be excluded.
#[cfg(feature = "native-backend")]
fn write_convert_fixture(dir: &Path) -> PathBuf {
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, Line, LwPolyline, Point},
        io::dwg::DwgWriter,
        tables::Layer,
        types::Vector2,
    };

    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    for name in ["EIXO", "PISTA"] {
        let mut layer = Layer::new(name);
        layer.handle = document.allocate_handle();
        document.layers.add(layer).expect("add layer");
    }

    let mut line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 100.0, 50.0, 0.0));
    line.common_mut().layer = "EIXO".to_string();
    document.add_entity(line).expect("add line");

    let mut point = EntityType::Point(Point::from_coords(5.0, 5.0, 0.0));
    point.common_mut().layer = "EIXO".to_string();
    document.add_entity(point).expect("add point");

    let mut square = LwPolyline::new();
    for (x, y) in [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)] {
        square.add_point(Vector2::new(x, y));
    }
    square.is_closed = true;
    let mut square = EntityType::LwPolyline(square);
    square.common_mut().layer = "PISTA".to_string();
    document.add_entity(square).expect("add closed polyline");

    let mut bulged = LwPolyline::new();
    bulged.add_point_with_bulge(Vector2::new(20.0, 0.0), 0.5);
    bulged.add_point(Vector2::new(30.0, 0.0));
    let mut bulged = EntityType::LwPolyline(bulged);
    bulged.common_mut().layer = "PISTA".to_string();
    document.add_entity(bulged).expect("add bulged polyline");

    let mut circle = EntityType::Circle(Circle::from_coords(50.0, 25.0, 0.0, 12.5));
    circle.common_mut().layer = "EIXO".to_string();
    document.add_entity(circle).expect("add circle");

    let mut paper_point = EntityType::Point(Point::from_coords(1.0, 1.0, 0.0));
    paper_point.common_mut().layer = "EIXO".to_string();
    document
        .add_paper_space_entity(paper_point)
        .expect("add paper point");

    let path = dir.join("convert fixture ç.dwg");
    DwgWriter::write_to_file(&path, &document).expect("write DWG fixture");
    path
}

#[cfg(feature = "native-backend")]
fn run_native_convert(fixture: &Path, out: &Path) -> std::process::Output {
    binary()
        .arg("convert")
        .arg(fixture)
        .arg("--output")
        .arg(out)
        .args([
            "--backend",
            "native",
            "--allow-local-coordinates",
            "--polygonize-closed",
        ])
        .output()
        .expect("run binary")
}

#[cfg(feature = "native-backend")]
fn run_native_geojson_seq_convert(fixture: &Path, out: &Path) -> std::process::Output {
    binary()
        .arg("convert")
        .arg(fixture)
        .arg("--output")
        .arg(out)
        .args([
            "--backend",
            "native",
            "--allow-local-coordinates",
            "--output-format",
            "geojson-seq",
        ])
        .output()
        .expect("run binary")
}

#[cfg(feature = "native-backend")]
#[test]
fn native_convert_writes_geojson_and_accounted_report() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("saída ç.geojson");

    let output = run_native_convert(&fixture, &out);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let geojson: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&out).expect("output exists"))
            .expect("output is JSON");
    assert_eq!(geojson["type"], "FeatureCollection");
    assert_eq!(
        geojson["dwg2geo"]["coordinate_status"],
        "local-unreferenced"
    );

    let features = geojson["features"].as_array().expect("features");
    assert_eq!(features.len(), 5);
    let kinds: Vec<(&str, &str)> = features
        .iter()
        .map(|feature| {
            (
                feature["properties"]["entity_type"].as_str().expect("type"),
                feature["geometry"]["type"].as_str().expect("geom type"),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("LINE", "LineString"),
            ("POINT", "Point"),
            ("LWPOLYLINE", "Polygon"),
            ("LWPOLYLINE", "LineString"),
            ("CIRCLE", "Polygon"),
        ]
    );
    for feature in features {
        assert_eq!(feature["properties"]["space"], "model");
        assert!(feature["id"].is_string(), "feature must have a stable id");
    }
    // The bulged polyline is tessellated and marked as approximated.
    let bulged = &features[3];
    assert_eq!(bulged["properties"]["approximated"], true);
    assert!(
        bulged["geometry"]["coordinates"]
            .as_array()
            .expect("coordinates")
            .len()
            > 2
    );

    let report_path = dir.path().join("saída ç.geojson.report.json");
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_path).expect("report exists"))
            .expect("report is JSON");
    assert_eq!(report["options"]["backend"], "native");
    assert_eq!(report["options"]["polygonize_closed"], true);
    assert_eq!(report["options"]["curve_tolerance"], 0.05);

    let native = &report["native"];
    assert_eq!(native["read_mode"], "strict");
    assert_eq!(native["features_written"], 5);
    assert_eq!(native["approximated_features"], 2);
    assert_eq!(native["excluded"]["paper_space"], 1);
    assert_eq!(native["skipped"].as_array().expect("skipped").len(), 0);

    // Milestone 6: histogram accounting must balance, bounding boxes are
    // reported (identical without a transform), and the geometry validity
    // pass finds no invariant violations.
    let accounting = &native["accounting"];
    assert_eq!(accounting["unaccounted"], 0);
    assert_eq!(
        accounting["model_space_entities"],
        accounting["top_level_accounted"]
    );
    let bbox = native["bbox_drawing"].as_array().expect("bbox_drawing");
    assert_eq!(bbox.len(), 4);
    assert_eq!(native["bbox_drawing"], native["bbox_output"]);
    assert!(bbox[0].as_f64().expect("min x") <= bbox[2].as_f64().expect("max x"));
    let checks = &native["geometry_checks"];
    assert_eq!(checks["features_checked"], 5);
    assert_eq!(checks["non_finite_coordinates"], 0);
    assert_eq!(checks["empty_geometries"], 0);
    assert_eq!(checks["unclosed_rings"], 0);
    assert_eq!(checks["misoriented_rings"], 0);
    assert_eq!(checks["degenerate_rings"], 0);
    assert!(checks["rings_checked"].as_u64().expect("rings") >= 2);
}

#[cfg(feature = "native-backend")]
#[test]
fn native_convert_output_is_deterministic() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let first_out = dir.path().join("first.geojson");
    let second_out = dir.path().join("second.geojson");

    assert!(run_native_convert(&fixture, &first_out).status.success());
    assert!(run_native_convert(&fixture, &second_out).status.success());

    let first = fs::read(&first_out).expect("first output");
    let second = fs::read(&second_out).expect("second output");
    assert_eq!(first, second, "GeoJSON output must be byte-identical");
}

#[cfg(feature = "native-backend")]
#[test]
fn native_convert_writes_geojson_seq_and_records_format() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("saída ç.geojsonseq");

    let output = run_native_geojson_seq_convert(&fixture, &out);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {stderr}");

    let contents = fs::read_to_string(&out).expect("output exists");
    let lines: Vec<&str> = contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert!(!lines.is_empty());
    for line in &lines {
        let feature: serde_json::Value =
            serde_json::from_str(line).expect("each line must be valid JSON");
        assert!(feature.is_object());
        assert_eq!(feature["type"], "Feature");
    }

    let report_path = dir.path().join("saída ç.geojsonseq.report.json");
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_path).expect("report exists"))
            .expect("report is JSON");
    assert_eq!(report["options"]["output_format"], "geojson-seq");
    assert_eq!(
        lines.len() as u64,
        report["native"]["features_written"]
            .as_u64()
            .expect("features_written")
    );
}

#[cfg(feature = "native-backend")]
#[test]
fn native_geojson_seq_output_is_deterministic() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let first_out = dir.path().join("first.geojsonseq");
    let second_out = dir.path().join("second.geojsonseq");

    assert!(
        run_native_geojson_seq_convert(&fixture, &first_out)
            .status
            .success()
    );
    assert!(
        run_native_geojson_seq_convert(&fixture, &second_out)
            .status
            .success()
    );

    let first = fs::read(&first_out).expect("first output");
    let second = fs::read(&second_out).expect("second output");
    assert_eq!(first, second, "GeoJSONSeq output must be byte-identical");
}

#[cfg(all(feature = "native-backend", not(feature = "native-reproject")))]
#[test]
fn native_convert_rejects_source_crs_without_the_reproject_feature() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--backend", "native", "--source-crs", "EPSG:31985"])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("native-reproject"), "stderr: {stderr}");
    assert!(!dir.path().join("out.geojson").exists());
}

#[cfg(feature = "native-reproject")]
#[test]
fn native_reprojection_requires_units_when_the_header_is_ambiguous() {
    let dir = TempDir::new().expect("temporary directory");
    // The generated fixture leaves $INSUNITS at 0 (unitless).
    let fixture = write_convert_fixture(dir.path());

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(dir.path().join("out.geojson"))
        .args(["--backend", "native", "--source-crs", "EPSG:31982"])
        .output()
        .expect("run binary");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--source-units"), "stderr: {stderr}");
    assert!(stderr.contains("unitless"), "stderr: {stderr}");
    assert!(!dir.path().join("out.geojson").exists());
}

#[cfg(feature = "native-reproject")]
#[test]
fn native_reprojection_transforms_and_records_provenance() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("out.geojson");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args([
            "--backend",
            "native",
            "--source-crs",
            "EPSG:31982",
            "--target-crs",
            "EPSG:4326",
            "--source-units",
            "m",
        ])
        .output()
        .expect("run binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    let geojson: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read output"))
            .expect("valid GeoJSON");
    let marker = &geojson["dwg2geo"];
    assert_eq!(marker["coordinate_status"], "georeferenced");
    assert_eq!(marker["source_crs"], "EPSG:31982");
    assert_eq!(marker["target_crs"], "EPSG:4326");
    // Every coordinate must be plausible longitude/latitude now.
    let first = &geojson["features"][0]["geometry"]["coordinates"];
    let probe = if first[0].is_array() {
        &first[0]
    } else {
        first
    };
    let lon = probe[0].as_f64().expect("finite lon");
    assert!((-180.0..=180.0).contains(&lon), "lon {lon}");

    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("out.geojson.report.json")).expect("read report"),
    )
    .expect("valid report");
    let reprojection = &report["native"]["reprojection"];
    assert_eq!(reprojection["unit_source"], "override");
    assert_eq!(reprojection["drawing_unit"], "meters");
    assert_eq!(reprojection["meters_per_drawing_unit"], 1.0);
    assert!(
        reprojection["proj_version"]
            .as_str()
            .expect("proj version")
            .starts_with('9'),
        "{reprojection}"
    );
    assert_eq!(report["options"]["source_units"], "m");
    assert_eq!(report["options"]["source_crs"], "EPSG:31982");
}

#[cfg(feature = "native-backend")]
#[test]
fn control_point_calibration_transforms_and_reports_residuals() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("out.geojson");

    // Pure translation: drawing (x, y) -> (x + 1000, y + 2000).
    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args([
            "--backend",
            "native",
            "--target-crs",
            "EPSG:31982",
            "--control-point",
            "0,0=1000,2000",
            "--control-point",
            "100,50=1100,2050",
            "--control-point",
            "10,10=1010,2010",
        ])
        .output()
        .expect("run binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    let geojson: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).expect("read output"))
            .expect("valid GeoJSON");
    assert_eq!(geojson["dwg2geo"]["coordinate_status"], "calibrated");
    assert_eq!(geojson["dwg2geo"]["control_points"], 3);
    // The fixture's point entity sits at (5, 5) -> (1005, 2005).
    let features = geojson["features"].as_array().expect("features");
    let point = features
        .iter()
        .find(|f| f["properties"]["entity_type"] == "POINT")
        .expect("point feature");
    let x = point["geometry"]["coordinates"][0].as_f64().expect("x");
    let y = point["geometry"]["coordinates"][1].as_f64().expect("y");
    assert!(
        (x - 1005.0).abs() < 1e-9 && (y - 2005.0).abs() < 1e-9,
        "({x}, {y})"
    );

    let report: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("out.geojson.report.json")).expect("read report"),
    )
    .expect("valid report");
    let calibration = &report["native"]["calibration"];
    assert_eq!(calibration["control_points"], 3);
    let scale = calibration["scale"].as_f64().expect("scale");
    assert!((scale - 1.0).abs() < 1e-12, "scale {scale}");
    let rotation = calibration["rotation_deg"].as_f64().expect("rotation");
    assert!(rotation.abs() < 1e-9, "rotation {rotation}");
    assert_eq!(
        calibration["residuals"]
            .as_array()
            .expect("residuals")
            .len(),
        3
    );
    let rms = calibration["rms_error"].as_f64().expect("rms");
    assert!(rms < 1e-9, "rms {rms}");
    assert_eq!(calibration["target_crs"], "EPSG:31982");
}

#[cfg(feature = "native-backend")]
#[test]
fn validate_boundary_reports_containment() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("out.geojson");

    // Boundary covering only part of the fixture (x <= 20).
    let boundary_path = dir.path().join("boundary.geojson");
    fs::write(
        &boundary_path,
        r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[-1,-1],[20,-1],[20,60],[-1,60],[-1,-1]]]}}"#,
    )
    .expect("write boundary");

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(["--backend", "native", "--allow-local-coordinates"])
        .arg("--validate-boundary")
        .arg(&boundary_path)
        .output()
        .expect("run binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");

    let report: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join("out.geojson.report.json")).expect("read report"),
    )
    .expect("valid report");
    let check = &report["native"]["boundary_check"];
    assert_eq!(check["polygons"], 1);
    let inside = check["features_inside"].as_u64().expect("inside");
    let partial = check["features_partial"].as_u64().expect("partial");
    let outside = check["features_outside"].as_u64().expect("outside");
    assert_eq!(inside + partial + outside, 5);
    assert!(inside >= 2, "square and point are inside: {check}");
    assert!(partial + outside >= 1, "line to x=100 leaves: {check}");
    let warnings = report["warnings"].as_array().expect("warnings");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("boundary_check")),
        "{warnings:?}"
    );

    // Native-only flag.
    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(["--allow-local-coordinates", "--force"])
        .arg("--validate-boundary")
        .arg(&boundary_path)
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--backend native"), "stderr: {stderr}");
}

#[cfg(feature = "native-backend")]
#[test]
fn calibration_validation_fails_closed() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("out.geojson");

    // One control point cannot determine the transform.
    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(["--backend", "native", "--control-point", "0,0=10,10"])
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("at least two"), "stderr: {stderr}");

    // UTM-magnitude targets delivered as the default EPSG:4326 must trip
    // the extent check.
    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args([
            "--backend",
            "native",
            "--control-point",
            "0,0=248000,7396000",
            "--control-point",
            "100,50=248100,7396050",
        ])
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("implausible"), "stderr: {stderr}");
    assert!(!out.exists());
}

#[cfg(feature = "native-reproject")]
#[test]
fn implausible_wgs84_extents_fail_closed_unless_overridden() {
    let dir = TempDir::new().expect("temporary directory");
    let fixture = write_convert_fixture(dir.path());
    let out = dir.path().join("out.geojson");

    // Degrees scaled by 1000: the identity transform then yields longitudes
    // in the tens of thousands — exactly the wrong-units mistake the check
    // exists to catch.
    let args = [
        "--backend",
        "native",
        "--source-crs",
        "EPSG:4326",
        "--target-crs",
        "EPSG:4326",
        "--source-units",
        "km",
    ];
    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(args)
        .output()
        .expect("run binary");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("implausible"), "stderr: {stderr}");
    assert!(
        stderr.contains("--allow-suspect-extents"),
        "stderr: {stderr}"
    );
    assert!(!out.exists());

    let output = binary()
        .arg("convert")
        .arg(&fixture)
        .arg("--output")
        .arg(&out)
        .args(args)
        .arg("--allow-suspect-extents")
        .output()
        .expect("run binary");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    assert!(out.exists());
}
