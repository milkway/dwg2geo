//! Synthetic DWG round-trip for ACAD_TABLE (fixture-independent).
//!
//! Writes a table with text cells, column widths and row heights, reads it
//! back, and checks the structure and every cell's text survive — at both a
//! pre-R2010 version (flat format) and an R2010+ version (inline table
//! content), exercising both writer and reader paths.

use std::io::Cursor;

use acadrust::entities::{EntityType, Table, TableCell};
use acadrust::types::{DxfVersion, Vector3};
use acadrust::{CadDocument, DwgReader, DwgWriter};

fn sample_table() -> Table {
    let mut t = Table::new(Vector3::new(1.0, 2.0, 0.0), 2, 3);
    t.columns[0].width = 10.0;
    t.columns[1].width = 20.0;
    t.columns[2].width = 30.0;
    t.rows[0].height = 5.0;
    t.rows[1].height = 7.0;
    t.rows[0].cells[0] = TableCell::text("Name");
    t.rows[0].cells[1] = TableCell::text("Qty");
    t.rows[0].cells[2] = TableCell::text("Cost");
    t.rows[1].cells[0] = TableCell::text("Bolt");
    t.rows[1].cells[1] = TableCell::text("10");
    t.rows[1].cells[2] = TableCell::text("2.50");
    t
}

fn roundtrip(version: DxfVersion) -> Table {
    let mut doc = CadDocument::with_version(version);
    doc.add_entity(EntityType::Table(sample_table())).unwrap();
    let bytes = DwgWriter::write_to_vec(&doc).expect("DWG write");
    let rt = DwgReader::from_stream(Cursor::new(bytes))
        .read()
        .expect("DWG read");
    let found = rt.entities().find_map(|e| match e {
        EntityType::Table(t) => Some(t.clone()),
        _ => None,
    });
    found.expect("table missing after DWG roundtrip")
}

fn assert_table(t: &Table, label: &str) {
    assert_eq!(t.rows.len(), 2, "{label}: rows");
    assert_eq!(t.columns.len(), 3, "{label}: columns");
    assert_eq!(t.columns[1].width, 20.0, "{label}: column width");
    assert_eq!(t.rows[1].height, 7.0, "{label}: row height");
    let text = |r: usize| -> Vec<String> {
        t.rows[r]
            .cells
            .iter()
            .map(|c| c.text_value().to_string())
            .collect()
    };
    assert_eq!(text(0), vec!["Name", "Qty", "Cost"], "{label}: header");
    assert_eq!(text(1), vec!["Bolt", "10", "2.50"], "{label}: data");
}

#[test]
fn table_dwg_roundtrip_flat_r2007() {
    // AC1021 = R2007 → pre-R2010 flat cell format.
    let t = roundtrip(DxfVersion::AC1021);
    assert_table(&t, "R2007");
}

#[test]
fn table_dwg_roundtrip_content_r2018() {
    // AC1032 = R2018 → R2010+ inline table content.
    let t = roundtrip(DxfVersion::AC1032);
    assert_table(&t, "R2018");
}
