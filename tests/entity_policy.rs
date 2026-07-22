#![cfg(feature = "native-backend")]

// This list must track every EntityType variant in acadrust 0.4.1. Acadrust has
// no enum iterator, so upgrading the dependency requires updating this list and
// the exhaustive policy tables in docs/ENTITY_MAPPING.md together.
const ENTITY_TYPE_VARIANTS: [&str; 44] = [
    "Point",
    "Line",
    "Circle",
    "Arc",
    "Ellipse",
    "Polyline",
    "Polyline2D",
    "Polyline3D",
    "LwPolyline",
    "Text",
    "MText",
    "Spline",
    "Helix",
    "Dimension",
    "Hatch",
    "Solid",
    "Face3D",
    "Insert",
    "Block",
    "BlockEnd",
    "Ray",
    "XLine",
    "Viewport",
    "AttributeDefinition",
    "AttributeEntity",
    "Leader",
    "MultiLeader",
    "MLine",
    "Mesh",
    "RasterImage",
    "Solid3D",
    "Region",
    "Body",
    "Surface",
    "Table",
    "Tolerance",
    "PolyfaceMesh",
    "Wipeout",
    "Shape",
    "Underlay",
    "Seqend",
    "Ole2Frame",
    "PolygonMesh",
    "Unknown",
];

#[test]
fn every_acadrust_entity_type_has_exactly_one_documented_policy() {
    let policy = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/ENTITY_MAPPING.md"
    ));

    for variant in ENTITY_TYPE_VARIANTS {
        let marker = format!("`EntityType::{variant}`");
        assert_eq!(
            policy.matches(&marker).count(),
            1,
            "{marker} must appear in exactly one entity-policy table"
        );
    }
}
