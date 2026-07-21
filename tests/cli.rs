use std::{io::Write, process::Command};

use tempfile::NamedTempFile;

#[test]
fn inspect_emits_ac1027_json() {
    let mut fixture = NamedTempFile::new().expect("temporary file");
    fixture
        .write_all(b"AC1027synthetic-cli-fixture")
        .expect("write fixture");

    let output = Command::new(env!("CARGO_BIN_EXE_dwg2geo"))
        .arg("inspect")
        .arg(fixture.path())
        .arg("--json")
        .output()
        .expect("run binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("AC1027"));
    assert!(stdout.contains("AutoCAD 2013"));
}

#[test]
fn convert_requires_coordinate_policy() {
    let mut fixture = NamedTempFile::new().expect("temporary file");
    fixture
        .write_all(b"AC1027synthetic-cli-fixture")
        .expect("write fixture");

    let output_path = fixture.path().with_extension("geojson");
    let output = Command::new(env!("CARGO_BIN_EXE_dwg2geo"))
        .arg("convert")
        .arg(fixture.path())
        .arg("--output")
        .arg(output_path)
        .output()
        .expect("run binary");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("source CRS is required"));
}
