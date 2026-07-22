#![cfg(feature = "native-backend")]

//! Machine-checked entity-support policy (audit finding B4).
//!
//! Every `EntityType` variant in acadrust 0.4.1 must have exactly one
//! reviewed disposition. This test is the in-repo source of truth: upgrading
//! acadrust with new variants fails compilation here (the count) or the
//! exhaustiveness assertion until each new variant is categorized. The
//! human-readable prose lives in the (local) `docs/ENTITY_MAPPING.md`.

/// How the native backend treats a CAD entity type.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Policy {
    /// Converted to a GeoJSON feature.
    Converted,
    /// Intentionally never a feature (infinite geometry, ACIS solids,
    /// structural markers, templates); reported, never guessed.
    DeliberatelyUnsupported,
    /// Not converted yet; reported with a reason.
    NotYetConverted,
}

/// Every acadrust 0.4.1 `EntityType` variant and its disposition. acadrust has
/// no enum iterator, so this list must be updated when the dependency changes.
const POLICY: [(&str, Policy); 44] = [
    // Converted (16)
    ("Point", Policy::Converted),
    ("Line", Policy::Converted),
    ("LwPolyline", Policy::Converted),
    ("Polyline", Policy::Converted),
    ("Polyline2D", Policy::Converted),
    ("Polyline3D", Policy::Converted),
    ("Arc", Policy::Converted),
    ("Circle", Policy::Converted),
    ("Ellipse", Policy::Converted),
    ("Spline", Policy::Converted),
    ("Hatch", Policy::Converted),
    ("Face3D", Policy::Converted),
    ("Text", Policy::Converted),
    ("MText", Policy::Converted),
    ("Insert", Policy::Converted),
    ("Solid", Policy::Converted),
    // Deliberately unsupported with policy (11)
    ("Ray", Policy::DeliberatelyUnsupported),
    ("XLine", Policy::DeliberatelyUnsupported),
    ("Solid3D", Policy::DeliberatelyUnsupported),
    ("Region", Policy::DeliberatelyUnsupported),
    ("Body", Policy::DeliberatelyUnsupported),
    ("Surface", Policy::DeliberatelyUnsupported),
    ("Block", Policy::DeliberatelyUnsupported),
    ("BlockEnd", Policy::DeliberatelyUnsupported),
    ("Seqend", Policy::DeliberatelyUnsupported),
    ("AttributeDefinition", Policy::DeliberatelyUnsupported),
    ("Unknown", Policy::DeliberatelyUnsupported),
    // Not yet converted (17)
    ("Helix", Policy::NotYetConverted),
    ("Dimension", Policy::NotYetConverted),
    ("Viewport", Policy::NotYetConverted),
    ("AttributeEntity", Policy::NotYetConverted),
    ("Leader", Policy::NotYetConverted),
    ("MultiLeader", Policy::NotYetConverted),
    ("MLine", Policy::NotYetConverted),
    ("Mesh", Policy::NotYetConverted),
    ("RasterImage", Policy::NotYetConverted),
    ("Table", Policy::NotYetConverted),
    ("Tolerance", Policy::NotYetConverted),
    ("PolyfaceMesh", Policy::NotYetConverted),
    ("Wipeout", Policy::NotYetConverted),
    ("Shape", Policy::NotYetConverted),
    ("Underlay", Policy::NotYetConverted),
    ("Ole2Frame", Policy::NotYetConverted),
    ("PolygonMesh", Policy::NotYetConverted),
];

#[test]
fn every_acadrust_entity_type_has_exactly_one_documented_policy() {
    // No duplicate variant names.
    for (i, (name, _)) in POLICY.iter().enumerate() {
        assert!(
            !POLICY[..i].iter().any(|(other, _)| other == name),
            "{name} appears more than once in the policy table"
        );
    }

    // The three categories partition the whole set.
    let converted = POLICY
        .iter()
        .filter(|(_, p)| *p == Policy::Converted)
        .count();
    let unsupported = POLICY
        .iter()
        .filter(|(_, p)| *p == Policy::DeliberatelyUnsupported)
        .count();
    let not_yet = POLICY
        .iter()
        .filter(|(_, p)| *p == Policy::NotYetConverted)
        .count();

    assert_eq!(converted, 16, "converted variant count changed");
    assert_eq!(
        unsupported, 11,
        "deliberately-unsupported variant count changed"
    );
    assert_eq!(not_yet, 17, "not-yet-converted variant count changed");
    assert_eq!(
        converted + unsupported + not_yet,
        44,
        "policy must cover all 44 variants"
    );
}
