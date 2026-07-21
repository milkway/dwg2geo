//! Native DWG -> GeoJSON conversion (Milestone 3).
//!
//! Current slice: `POINT`, `LINE`, and `LWPOLYLINE` without bulge arcs,
//! model space only, in raw drawing coordinates (reprojection arrives with
//! the `native-reproject` feature in Milestone 5). Every entity that is not
//! converted is counted in the report with a reason — nothing is silently
//! dropped. Output is deterministic: features follow model-space document
//! order and identifiers come from entity handles.

use std::{collections::BTreeMap, fs, time::Instant};

use acadrust::{
    CadDocument,
    entities::EntityType,
    types::{Matrix3, Vector3},
};
use anyhow::{Context, Result, bail};
use geojson::{
    Feature, FeatureCollection, Geometry, GeometryValue, JsonObject, JsonValue, feature::Id,
};

use super::{ReadMode, read_document};
use crate::{
    backend::{
        ConvertRequest, append_suffix, check_output_collision, ensure_nonempty_output,
        ensure_parent_directory, remove_stale, validate_input,
    },
    dwg,
    report::{
        self, ConversionOptions, ConversionReport, ConvertedCount, ExcludedCounts, Generator,
        NativeConversionSummary, OutcomeCount, OutputInfo, Step,
    },
};

const MAX_HANDLE_SAMPLES: usize = 10;
const Z_EPSILON: f64 = 1e-9;

pub fn convert(request: &ConvertRequest<'_>) -> Result<()> {
    let started = Instant::now();

    if let Some(source_crs) = request.source_crs {
        bail!(
            "the native backend cannot reproject from {source_crs} yet; reprojection arrives with the `native-reproject` feature (Milestone 5). Use --backend external for CRS-aware conversion, or --allow-local-coordinates for raw drawing coordinates"
        );
    }
    if !request.allow_local_coordinates {
        bail!("internal validation error: native conversion reached without a coordinate policy");
    }

    validate_input(request.input)?;
    check_output_collision(request.output, request.force)?;
    ensure_parent_directory(request.output)?;

    let source = dwg::inspect(request.input)
        .with_context(|| format!("cannot inspect input {}", request.input.display()))?;

    let mut warnings = Vec::new();
    if source.autocad_generation.contains("unknown") {
        warnings.push(format!(
            "input signature {:?} is not a known DWG generation",
            source.signature
        ));
    }
    warnings
        .push("output uses raw drawing coordinates; no geographic CRS was established".to_string());
    if request.keep_intermediate {
        warnings.push(
            "the native backend produces GeoJSON directly; there is no intermediate DXF to keep"
                .to_string(),
        );
    }

    let mut steps = Vec::new();

    let parse_started = Instant::now();
    let (document, read_mode, read_errors) = read_document(request.input)?;
    steps.push(Step {
        purpose: "native DWG parse".to_string(),
        command: "(in-process acadrust reader)".to_string(),
        duration_ms: parse_started.elapsed().as_millis() as u64,
    });

    let extract_started = Instant::now();
    let extraction = extract(&document, request.polygonize_closed)?;
    steps.push(Step {
        purpose: "entity extraction and GeoJSON mapping".to_string(),
        command: "(in-process converter)".to_string(),
        duration_ms: extract_started.elapsed().as_millis() as u64,
    });

    if extraction.features.is_empty() {
        warnings.push(
            "no features were converted; see the native section of the report for reasons"
                .to_string(),
        );
    }

    let features_written = extraction.features.len();
    let collection = FeatureCollection {
        bbox: None,
        features: extraction.features,
        foreign_members: Some(foreign_members(&source)),
    };

    let partial = append_suffix(request.output, ".partial");
    remove_stale(&partial)?;

    let mut json = serde_json::to_string_pretty(&collection)
        .context("cannot serialize GeoJSON feature collection")?;
    json.push('\n');
    if let Err(error) = fs::write(&partial, json)
        .with_context(|| format!("cannot write output {}", partial.display()))
        .and_then(|()| ensure_nonempty_output(&partial))
    {
        let _ = fs::remove_file(&partial);
        return Err(error);
    }

    fs::rename(&partial, request.output).with_context(|| {
        format!(
            "cannot move finished output into place at {}",
            request.output.display()
        )
    })?;

    let output_size = fs::metadata(request.output).map(|m| m.len()).unwrap_or(0);
    let conversion_report = ConversionReport {
        report_version: report::REPORT_VERSION,
        generator: Generator::current(),
        source,
        options: ConversionOptions {
            backend: "native",
            source_crs: None,
            target_crs: None,
            allow_local_coordinates: true,
            force: request.force,
            keep_intermediate: request.keep_intermediate,
            include_layers: request.include_layers.to_vec(),
            exclude_layers: request.exclude_layers.to_vec(),
            polygonize_closed: request.polygonize_closed,
        },
        external_tools: Vec::new(),
        steps,
        warnings,
        native: Some(NativeConversionSummary {
            read_mode: match read_mode {
                ReadMode::Strict => "strict".to_string(),
                ReadMode::FailsafeRecovery => "failsafe_recovery".to_string(),
            },
            read_errors,
            features_written,
            converted: extraction
                .converted
                .into_iter()
                .map(|(entity_type, count)| ConvertedCount { entity_type, count })
                .collect(),
            skipped: outcome_counts(extraction.skipped),
            failed: outcome_counts(extraction.failed),
            excluded: ExcludedCounts {
                paper_space: extraction.excluded_paper_space,
                block_definitions: extraction.excluded_block_definitions,
                unowned: extraction.excluded_unowned,
            },
            feature_warnings: extraction.feature_warnings,
        }),
        output: OutputInfo {
            path: request.output.display().to_string(),
            size_bytes: output_size,
        },
        total_duration_ms: started.elapsed().as_millis() as u64,
    };

    let report_file = report::report_path(request.output);
    report::write(&conversion_report, &report_file)?;

    eprintln!("wrote {}", request.output.display());
    eprintln!("wrote report {}", report_file.display());

    Ok(())
}

/// GeoJSON foreign members marking the output as non-geographic, per the
/// local-coordinates contract (RFC 7946 output is otherwise assumed WGS 84).
fn foreign_members(source: &dwg::DwgInfo) -> JsonObject {
    let mut members = JsonObject::new();
    members.insert(
        "dwg2geo".to_string(),
        serde_json::json!({
            "coordinate_status": "local-unreferenced",
            "note": "coordinates are raw drawing units; no geographic CRS was established",
            "source_sha256": source.sha256,
        }),
    );
    members
}

#[derive(Default)]
struct HandleSamples {
    count: usize,
    samples: Vec<String>,
}

#[derive(Default)]
struct Extraction {
    features: Vec<Feature>,
    converted: BTreeMap<String, usize>,
    skipped: BTreeMap<(String, String), HandleSamples>,
    failed: BTreeMap<(String, String), HandleSamples>,
    excluded_paper_space: usize,
    excluded_block_definitions: usize,
    excluded_unowned: usize,
    feature_warnings: usize,
}

fn outcome_counts(map: BTreeMap<(String, String), HandleSamples>) -> Vec<OutcomeCount> {
    map.into_iter()
        .map(|((entity_type, reason), samples)| OutcomeCount {
            entity_type,
            reason,
            count: samples.count,
            sample_handles: samples.samples,
        })
        .collect()
}

enum EntityOutcome {
    Converted {
        geometry: GeometryValue,
        extra_properties: Vec<(&'static str, JsonValue)>,
        warnings: Vec<String>,
    },
    Skipped(String),
    Failed(String),
}

/// Convert every model-space entity in document order; count paper-space,
/// block-definition, and unowned entities as excluded by the documented
/// model-space filter.
fn extract(document: &CadDocument, polygonize_closed: bool) -> Result<Extraction> {
    let mut extraction = Extraction::default();
    let mut visited: std::collections::HashSet<u64> = std::collections::HashSet::new();
    let mut model_index: usize = 0;
    let mut found_model_space = false;

    for record in document.block_records.iter() {
        let is_model = record.is_model_space();
        let is_paper = record.is_paper_space();
        found_model_space |= is_model;

        for handle in &record.entity_handles {
            let Some(entity) = document.get_entity(*handle) else {
                // Inspection reports unresolved handles; conversion counts
                // them as failed so the totals still add up.
                record_outcome(
                    &mut extraction.failed,
                    "UNRESOLVED".to_string(),
                    "entity handle does not resolve to an entity".to_string(),
                    &format!("{handle}"),
                );
                continue;
            };
            if matches!(entity, EntityType::Block(_) | EntityType::BlockEnd(_)) {
                continue;
            }
            visited.insert(handle.value());

            if is_paper {
                extraction.excluded_paper_space += 1;
                continue;
            }
            if !is_model {
                extraction.excluded_block_definitions += 1;
                continue;
            }

            process_entity(entity, model_index, polygonize_closed, &mut extraction);
            model_index += 1;
        }
    }

    for entity in document.entities() {
        if !visited.contains(&entity.common().handle.value()) {
            extraction.excluded_unowned += 1;
        }
    }

    if !found_model_space {
        bail!("the drawing has no model-space block record; cannot convert");
    }

    Ok(extraction)
}

fn process_entity(
    entity: &EntityType,
    index: usize,
    polygonize_closed: bool,
    extraction: &mut Extraction,
) {
    let entity_type = entity.as_entity().entity_type().to_string();
    let handle_text = format!("{}", entity.common().handle);

    match convert_entity(entity, polygonize_closed) {
        EntityOutcome::Converted {
            geometry,
            extra_properties,
            warnings,
        } => {
            extraction.feature_warnings += warnings.len();
            *extraction.converted.entry(entity_type.clone()).or_default() += 1;
            extraction.features.push(build_feature(
                entity,
                index,
                &entity_type,
                geometry,
                extra_properties,
                warnings,
            ));
        }
        EntityOutcome::Skipped(reason) => {
            record_outcome(&mut extraction.skipped, entity_type, reason, &handle_text);
        }
        EntityOutcome::Failed(reason) => {
            record_outcome(&mut extraction.failed, entity_type, reason, &handle_text);
        }
    }
}

fn record_outcome(
    map: &mut BTreeMap<(String, String), HandleSamples>,
    entity_type: String,
    reason: String,
    handle_text: &str,
) {
    let entry = map.entry((entity_type, reason)).or_default();
    entry.count += 1;
    if entry.samples.len() < MAX_HANDLE_SAMPLES {
        entry.samples.push(handle_text.to_string());
    }
}

fn build_feature(
    entity: &EntityType,
    index: usize,
    entity_type: &str,
    geometry: GeometryValue,
    extra_properties: Vec<(&'static str, JsonValue)>,
    warnings: Vec<String>,
) -> Feature {
    let common = entity.common();

    let id = if common.handle.is_null() {
        format!("model-{index}")
    } else {
        format!("{}", common.handle)
    };

    let mut properties = JsonObject::new();
    properties.insert("layer".to_string(), JsonValue::from(common.layer.clone()));
    properties.insert(
        "entity_type".to_string(),
        JsonValue::from(entity_type.to_string()),
    );
    properties.insert("space".to_string(), JsonValue::from("model"));
    properties.insert("handle".to_string(), JsonValue::from(id.clone()));
    for (key, value) in extra_properties {
        properties.insert(key.to_string(), value);
    }
    if !warnings.is_empty() {
        properties.insert("warnings".to_string(), JsonValue::from(warnings));
    }

    Feature {
        bbox: None,
        geometry: Some(Geometry::new(geometry)),
        id: Some(Id::String(id)),
        properties: Some(properties),
        foreign_members: None,
    }
}

fn convert_entity(entity: &EntityType, polygonize_closed: bool) -> EntityOutcome {
    match entity {
        EntityType::Point(point) => convert_point(point),
        EntityType::Line(line) => convert_line(line),
        EntityType::LwPolyline(polyline) => convert_lwpolyline(polyline, polygonize_closed),
        _ => EntityOutcome::Skipped(
            "entity type is not converted by the native backend yet".to_string(),
        ),
    }
}

fn convert_point(point: &acadrust::entities::Point) -> EntityOutcome {
    let location = point.location;
    if !is_finite(&location) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, location.z.abs());

    EntityOutcome::Converted {
        geometry: GeometryValue::new_point((location.x, location.y)),
        extra_properties: Vec::new(),
        warnings,
    }
}

fn convert_line(line: &acadrust::entities::Line) -> EntityOutcome {
    if !is_finite(&line.start) || !is_finite(&line.end) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if line.start.x == line.end.x && line.start.y == line.end.y {
        return EntityOutcome::Skipped("degenerate line: identical XY endpoints".to_string());
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, line.start.z.abs().max(line.end.z.abs()));

    EntityOutcome::Converted {
        geometry: GeometryValue::new_line_string(vec![
            (line.start.x, line.start.y),
            (line.end.x, line.end.y),
        ]),
        extra_properties: Vec::new(),
        warnings,
    }
}

fn convert_lwpolyline(
    polyline: &acadrust::entities::LwPolyline,
    polygonize_closed: bool,
) -> EntityOutcome {
    if polyline.vertices.len() < 2 {
        return EntityOutcome::Skipped("polyline has fewer than two vertices".to_string());
    }
    if polyline.vertices.iter().any(|vertex| vertex.bulge != 0.0) {
        return EntityOutcome::Skipped("bulge arc segments are not tessellated yet".to_string());
    }

    // LWPOLYLINE vertices live in OCS; lift them to WCS via the arbitrary
    // axis algorithm (identity for the default normal).
    let ocs_to_wcs = Matrix3::arbitrary_axis(polyline.normal);
    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(polyline.vertices.len() + 1);
    let mut max_abs_z: f64 = 0.0;
    for vertex in &polyline.vertices {
        let wcs = ocs_to_wcs.transform_point(Vector3::new(
            vertex.location.x,
            vertex.location.y,
            polyline.elevation,
        ));
        if !is_finite(&wcs) {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        }
        max_abs_z = max_abs_z.max(wcs.z.abs());
        coordinates.push((wcs.x, wcs.y));
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    if polyline.is_closed && polygonize_closed {
        let distinct = count_distinct(&coordinates);
        if distinct < 3 {
            return EntityOutcome::Skipped(
                "closed polyline has fewer than three distinct vertices; cannot form a polygon ring"
                    .to_string(),
            );
        }
        let mut ring = coordinates;
        if ring.first() != ring.last() {
            ring.push(ring[0]);
        }
        // RFC 7946: exterior rings are counter-clockwise.
        if signed_area(&ring) < 0.0 {
            ring.reverse();
        }
        return EntityOutcome::Converted {
            geometry: GeometryValue::new_polygon(vec![ring]),
            extra_properties: vec![("is_closed", JsonValue::from(true))],
            warnings,
        };
    }

    let is_closed = polyline.is_closed;
    if is_closed && coordinates.first() != coordinates.last() {
        coordinates.push(coordinates[0]);
    }

    EntityOutcome::Converted {
        geometry: GeometryValue::new_line_string(coordinates),
        extra_properties: vec![("is_closed", JsonValue::from(is_closed))],
        warnings,
    }
}

fn push_z_warning(warnings: &mut Vec<String>, max_abs_z: f64) {
    if max_abs_z > Z_EPSILON {
        warnings.push("non-zero z coordinates dropped (output is 2D)".to_string());
    }
}

fn is_finite(vector: &Vector3) -> bool {
    vector.x.is_finite() && vector.y.is_finite() && vector.z.is_finite()
}

fn count_distinct(coordinates: &[(f64, f64)]) -> usize {
    let mut distinct: Vec<(f64, f64)> = Vec::new();
    for coordinate in coordinates {
        if !distinct.contains(coordinate) {
            distinct.push(*coordinate);
        }
    }
    distinct.len()
}

/// Shoelace formula; positive for counter-clockwise rings.
fn signed_area(ring: &[(f64, f64)]) -> f64 {
    let mut sum = 0.0;
    for pair in ring.windows(2) {
        sum += (pair[1].0 - pair[0].0) * (pair[1].1 + pair[0].1);
    }
    -sum / 2.0
}

#[cfg(test)]
mod tests {
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, Line, LwPolyline, Point},
        types::Vector2,
    };
    use geojson::GeometryValue;

    use super::{EntityOutcome, convert_entity, extract, signed_area};

    fn lwpolyline(points: &[(f64, f64)], closed: bool) -> LwPolyline {
        let mut polyline = LwPolyline::new();
        for (x, y) in points {
            polyline.add_point(Vector2::new(*x, *y));
        }
        polyline.is_closed = closed;
        polyline
    }

    #[test]
    fn point_and_line_map_to_2d_geometries() {
        let point = EntityType::Point(Point::from_coords(1.0, 2.0, 0.0));
        match convert_entity(&point, false) {
            EntityOutcome::Converted { geometry, .. } => {
                assert_eq!(geometry, GeometryValue::new_point((1.0, 2.0)));
            }
            _ => panic!("point must convert"),
        }

        let line = EntityType::Line(Line::from_coords(0.0, 0.0, 3.0, 4.0, 5.0, 3.0));
        match convert_entity(&line, false) {
            EntityOutcome::Converted {
                geometry, warnings, ..
            } => {
                assert_eq!(
                    geometry,
                    GeometryValue::new_line_string(vec![(0.0, 0.0), (4.0, 5.0)])
                );
                assert_eq!(warnings.len(), 1, "z must be reported as dropped");
            }
            _ => panic!("line must convert"),
        }
    }

    #[test]
    fn degenerate_line_is_skipped_with_reason() {
        let line = EntityType::Line(Line::from_coords(1.0, 1.0, 0.0, 1.0, 1.0, 0.0));
        match convert_entity(&line, false) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("degenerate")),
            _ => panic!("degenerate line must be skipped"),
        }
    }

    #[test]
    fn closed_polyline_becomes_closed_linestring_by_default() {
        let polyline =
            EntityType::LwPolyline(lwpolyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], true));
        match convert_entity(&polyline, false) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    assert_eq!(coordinates.len(), 4);
                    assert_eq!(coordinates.first(), coordinates.last());
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("closed polyline must convert"),
        }
    }

    #[test]
    fn polygonize_closed_produces_ccw_ring_even_from_cw_input() {
        // Clockwise square.
        let polyline = EntityType::LwPolyline(lwpolyline(
            &[(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)],
            true,
        ));
        match convert_entity(&polyline, true) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::Polygon { coordinates } => {
                    assert_eq!(coordinates.len(), 1);
                    let ring = &coordinates[0];
                    assert_eq!(ring.first(), ring.last());
                    let tuples: Vec<(f64, f64)> = ring
                        .iter()
                        .map(|position| (position[0], position[1]))
                        .collect();
                    assert!(signed_area(&tuples) > 0.0, "ring must be CCW");
                }
                other => panic!("expected Polygon, got {other:?}"),
            },
            _ => panic!("closed polyline must polygonize"),
        }
    }

    #[test]
    fn bulged_polyline_is_skipped_not_flattened() {
        let mut polyline = lwpolyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false);
        polyline.vertices[1].bulge = 0.5;
        match convert_entity(&EntityType::LwPolyline(polyline), false) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("bulge")),
            _ => panic!("bulged polyline must be skipped, not approximated silently"),
        }
    }

    #[test]
    fn unsupported_types_are_counted_and_paper_space_is_excluded() {
        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        document
            .add_entity(EntityType::Circle(Circle::from_coords(0.0, 0.0, 0.0, 5.0)))
            .expect("add circle");
        document
            .add_entity(EntityType::Point(Point::from_coords(1.0, 1.0, 0.0)))
            .expect("add point");
        document
            .add_paper_space_entity(EntityType::Point(Point::from_coords(2.0, 2.0, 0.0)))
            .expect("add paper point");

        let extraction = extract(&document, false).expect("extract");

        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.converted.get("POINT"), Some(&1));
        assert_eq!(extraction.excluded_paper_space, 1);
        let skipped: Vec<_> = extraction.skipped.keys().collect();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, "CIRCLE");
        let samples = extraction.skipped.values().next().expect("skip entry");
        assert_eq!(samples.count, 1);
        assert_eq!(samples.samples.len(), 1);
    }

    #[test]
    fn feature_order_and_ids_are_stable() {
        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        for i in 0..3 {
            let x = f64::from(i);
            document
                .add_entity(EntityType::Point(Point::from_coords(x, 0.0, 0.0)))
                .expect("add point");
        }

        let first = extract(&document, false).expect("extract");
        let second = extract(&document, false).expect("extract");

        let ids = |extraction: &super::Extraction| -> Vec<String> {
            extraction
                .features
                .iter()
                .map(|feature| format!("{:?}", feature.id))
                .collect()
        };
        assert_eq!(ids(&first), ids(&second));
        assert_eq!(first.features.len(), 3);
    }
}
