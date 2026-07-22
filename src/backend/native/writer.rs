//! The single place where the CAD-neutral model becomes GeoJSON (ADR-004).
//!
//! Property names and their insertion order are the canonical v1 schema
//! (ADR-014), pinned by `tests/golden/native-basic.geojson`.

use geojson::{Feature, Geometry, GeometryValue, JsonObject, JsonValue, feature::Id};

use super::model::{CadFeature, CadGeometry};

pub fn to_geojson(feature: &CadFeature) -> Feature {
    let mut properties = JsonObject::new();
    properties.insert("layer".to_string(), JsonValue::from(feature.layer.clone()));
    if let Some(source_layer) = &feature.source_layer {
        properties.insert(
            "source_layer".to_string(),
            JsonValue::from(source_layer.clone()),
        );
    }
    properties.insert(
        "entity_type".to_string(),
        JsonValue::from(feature.entity_type.clone()),
    );
    properties.insert("space".to_string(), JsonValue::from("model"));
    properties.insert(
        "handle".to_string(),
        JsonValue::from(feature.handle.clone()),
    );
    if !feature.block_path.is_empty() {
        properties.insert(
            "block_path".to_string(),
            JsonValue::from(feature.block_path.join("/")),
        );
    }
    for (key, value) in &feature.extra_properties {
        properties.insert((*key).to_string(), value.clone());
    }
    if !feature.warnings.is_empty() {
        properties.insert(
            "warnings".to_string(),
            JsonValue::from(feature.warnings.clone()),
        );
    }

    Feature {
        bbox: None,
        geometry: Some(Geometry::new(geometry_to_geojson(&feature.geometry))),
        id: Some(Id::String(feature.id.clone())),
        properties: Some(properties),
        foreign_members: None,
    }
}

fn geometry_to_geojson(geometry: &CadGeometry) -> GeometryValue {
    match geometry {
        CadGeometry::Point(position) => GeometryValue::new_point(*position),
        CadGeometry::Line(line) => GeometryValue::new_line_string(line.clone()),
        CadGeometry::Polygon(rings) => GeometryValue::new_polygon(rings.clone()),
        CadGeometry::MultiPolygon(polygons) => GeometryValue::new_multi_polygon(polygons.clone()),
    }
}
