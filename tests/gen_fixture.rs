//! Test-only helper: writes a synthetic DWG to the path in DWG2GEO_FIXTURE_OUT.
//! Ignored unless the env var is set; used for manual smoke runs.
#[cfg(feature = "native-backend")]
#[test]
fn generate_fixture_for_manual_smoke() {
    let Some(path) = std::env::var_os("DWG2GEO_FIXTURE_OUT") else {
        return;
    };
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{EntityType, Line},
        io::dwg::DwgWriter,
        tables::Layer,
    };
    let mut document = CadDocument::with_version(DxfVersion::AC1027);
    let mut eixo = Layer::new("EIXO");
    eixo.handle = document.allocate_handle();
    document.layers.add(eixo).expect("layer");
    let mut line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 100.0, 50.0, 0.0));
    line.common_mut().layer = "EIXO".to_string();
    document.add_entity(line).expect("entity");
    DwgWriter::write_to_file(std::path::Path::new(&path), &document).expect("write");
}
