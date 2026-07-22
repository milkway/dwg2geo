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

/// Default maximum chord error for arc tessellation, in drawing units.
const DEFAULT_CURVE_TOLERANCE: f64 = 0.05;
/// Angular safety cap per tessellated segment (15 degrees).
const MAX_ANGLE_STEP: f64 = std::f64::consts::PI / 12.0;
/// Hard cap on segments per arc; reaching it emits a warning.
const MAX_ARC_SEGMENTS: usize = 256;

/// Geometry-mapping options resolved from the CLI.
struct GeometryOptions {
    polygonize_closed: bool,
    curve_tolerance: f64,
}

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

    let geometry_options = GeometryOptions {
        polygonize_closed: request.polygonize_closed,
        curve_tolerance: request.curve_tolerance.unwrap_or(DEFAULT_CURVE_TOLERANCE),
    };

    let extract_started = Instant::now();
    let extraction = extract(&document, &geometry_options)?;
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
            curve_tolerance: Some(geometry_options.curve_tolerance),
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
            approximated_features: extraction.approximated_features,
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
    approximated_features: usize,
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
fn extract(document: &CadDocument, options: &GeometryOptions) -> Result<Extraction> {
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

            process_entity(entity, model_index, options, &mut extraction);
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
    options: &GeometryOptions,
    extraction: &mut Extraction,
) {
    let entity_type = entity.as_entity().entity_type().to_string();
    let handle_text = format!("{}", entity.common().handle);

    match convert_entity(entity, options) {
        EntityOutcome::Converted {
            geometry,
            extra_properties,
            warnings,
        } => {
            extraction.feature_warnings += warnings.len();
            if extra_properties
                .iter()
                .any(|(key, value)| *key == "approximated" && *value == JsonValue::Bool(true))
            {
                extraction.approximated_features += 1;
            }
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

fn convert_entity(entity: &EntityType, options: &GeometryOptions) -> EntityOutcome {
    match entity {
        EntityType::Point(point) => convert_point(point),
        EntityType::Line(line) => convert_line(line),
        EntityType::LwPolyline(polyline) => convert_lwpolyline(polyline, options),
        EntityType::Polyline2D(polyline) => convert_polyline2d(polyline, options),
        EntityType::Polyline3D(polyline) => convert_polyline3d(polyline, options),
        EntityType::Polyline(polyline) => convert_polyline_generic(polyline, options),
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

/// One OCS polyline vertex: 2D location plus the bulge of the segment that
/// starts at it (`bulge = tan(included_angle / 4)`).
struct OcsVertex {
    x: f64,
    y: f64,
    bulge: f64,
}

fn convert_lwpolyline(
    polyline: &acadrust::entities::LwPolyline,
    options: &GeometryOptions,
) -> EntityOutcome {
    let vertices: Vec<OcsVertex> = polyline
        .vertices
        .iter()
        .map(|vertex| OcsVertex {
            x: vertex.location.x,
            y: vertex.location.y,
            bulge: vertex.bulge,
        })
        .collect();
    finish_ocs_path(
        &vertices,
        polyline.is_closed,
        polyline.elevation,
        polyline.normal,
        options,
    )
}

fn convert_polyline2d(
    polyline: &acadrust::entities::Polyline2D,
    options: &GeometryOptions,
) -> EntityOutcome {
    use acadrust::entities::PolylineFlags;

    let flags = polyline.flags.bits();
    if flags & (PolylineFlags::CURVE_FIT.bits() | PolylineFlags::SPLINE_FIT.bits()) != 0 {
        return EntityOutcome::Skipped(
            "curve-fit/spline-fit polyline smoothing is not evaluated yet".to_string(),
        );
    }

    // 2D POLYLINE vertices share the polyline elevation; some files carry it
    // on the vertices' z instead.
    let elevation = if polyline.elevation != 0.0 {
        polyline.elevation
    } else {
        polyline
            .vertices
            .first()
            .map(|vertex| vertex.location.z)
            .unwrap_or(0.0)
    };
    let vertices: Vec<OcsVertex> = polyline
        .vertices
        .iter()
        .map(|vertex| OcsVertex {
            x: vertex.location.x,
            y: vertex.location.y,
            bulge: vertex.bulge,
        })
        .collect();
    finish_ocs_path(
        &vertices,
        polyline.is_closed(),
        elevation,
        polyline.normal,
        options,
    )
}

fn convert_polyline3d(
    polyline: &acadrust::entities::Polyline3D,
    options: &GeometryOptions,
) -> EntityOutcome {
    if polyline.flags.spline_fit {
        return EntityOutcome::Skipped(
            "curve-fit/spline-fit polyline smoothing is not evaluated yet".to_string(),
        );
    }
    if polyline.flags.is_3d_mesh || polyline.flags.is_polyface_mesh {
        return EntityOutcome::Skipped(
            "polygon/polyface meshes are not converted by the native backend yet".to_string(),
        );
    }

    let points: Vec<Vector3> = polyline
        .vertices
        .iter()
        .map(|vertex| vertex.position)
        .collect();
    finish_wcs_path(&points, polyline.flags.closed, options)
}

fn convert_polyline_generic(
    polyline: &acadrust::entities::Polyline,
    options: &GeometryOptions,
) -> EntityOutcome {
    use acadrust::entities::PolylineFlags;

    let flags = polyline.flags.bits();
    if flags & (PolylineFlags::CURVE_FIT.bits() | PolylineFlags::SPLINE_FIT.bits()) != 0 {
        return EntityOutcome::Skipped(
            "curve-fit/spline-fit polyline smoothing is not evaluated yet".to_string(),
        );
    }
    if flags & (PolylineFlags::POLYGON_MESH.bits() | PolylineFlags::POLYFACE_MESH.bits()) != 0 {
        return EntityOutcome::Skipped(
            "polygon/polyface meshes are not converted by the native backend yet".to_string(),
        );
    }

    let points: Vec<Vector3> = polyline
        .vertices
        .iter()
        .map(|vertex| vertex.location)
        .collect();
    finish_wcs_path(&points, polyline.is_closed(), options)
}

/// Expand bulge arcs in the OCS plane, lift to WCS via the arbitrary axis
/// algorithm (identity for the default normal), and build the line/polygon.
fn finish_ocs_path(
    vertices: &[OcsVertex],
    closed: bool,
    elevation: f64,
    normal: Vector3,
    options: &GeometryOptions,
) -> EntityOutcome {
    if vertices.len() < 2 {
        return EntityOutcome::Skipped("polyline has fewer than two vertices".to_string());
    }
    if vertices
        .iter()
        .any(|v| !v.x.is_finite() || !v.y.is_finite() || !v.bulge.is_finite())
        || !elevation.is_finite()
    {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }

    let mut warnings = Vec::new();
    let mut approximated = false;

    // Expand each segment: its start vertex, then tessellated interior
    // points when the segment is an arc. The closing segment of a closed
    // polyline can carry a bulge too.
    let mut ocs_points: Vec<(f64, f64)> = Vec::with_capacity(vertices.len());
    let segment_count = if closed {
        vertices.len()
    } else {
        vertices.len() - 1
    };
    for index in 0..segment_count {
        let start = &vertices[index];
        let end = &vertices[(index + 1) % vertices.len()];
        ocs_points.push((start.x, start.y));
        if start.bulge != 0.0 {
            let interior = tessellate_bulge(
                (start.x, start.y),
                (end.x, end.y),
                start.bulge,
                options.curve_tolerance,
                &mut warnings,
            );
            if !interior.is_empty() {
                approximated = true;
            }
            ocs_points.extend(interior);
        }
    }
    if !closed {
        let last = vertices.last().expect("length checked above");
        ocs_points.push((last.x, last.y));
    }

    let ocs_to_wcs = Matrix3::arbitrary_axis(normal);
    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(ocs_points.len() + 1);
    let mut max_abs_z: f64 = 0.0;
    for (x, y) in ocs_points {
        let wcs = ocs_to_wcs.transform_point(Vector3::new(x, y, elevation));
        if !is_finite(&wcs) {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        }
        max_abs_z = max_abs_z.max(wcs.z.abs());
        coordinates.push((wcs.x, wcs.y));
    }

    push_z_warning(&mut warnings, max_abs_z);
    if approximated {
        warnings.push(format!(
            "arc segments tessellated with chord tolerance {} drawing units",
            options.curve_tolerance
        ));
    }
    finish_coordinates(coordinates, closed, approximated, options, warnings)
}

/// 3D polylines carry WCS positions and no bulges; drop z with a warning.
fn finish_wcs_path(points: &[Vector3], closed: bool, options: &GeometryOptions) -> EntityOutcome {
    if points.len() < 2 {
        return EntityOutcome::Skipped("polyline has fewer than two vertices".to_string());
    }
    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(points.len() + 1);
    let mut max_abs_z: f64 = 0.0;
    for point in points {
        if !is_finite(point) {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        }
        max_abs_z = max_abs_z.max(point.z.abs());
        coordinates.push((point.x, point.y));
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);
    finish_coordinates(coordinates, closed, false, options, warnings)
}

fn finish_coordinates(
    mut coordinates: Vec<(f64, f64)>,
    closed: bool,
    approximated: bool,
    options: &GeometryOptions,
    warnings: Vec<String>,
) -> EntityOutcome {
    let mut extra_properties = vec![("is_closed", JsonValue::from(closed))];
    if approximated {
        extra_properties.push(("approximated", JsonValue::from(true)));
    }

    if closed && options.polygonize_closed {
        if count_distinct(&coordinates) < 3 {
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
            extra_properties,
            warnings,
        };
    }

    if closed && coordinates.first() != coordinates.last() {
        coordinates.push(coordinates[0]);
    }

    EntityOutcome::Converted {
        geometry: GeometryValue::new_line_string(coordinates),
        extra_properties,
        warnings,
    }
}

/// Interior points of a bulge arc between `start` and `end` (both endpoints
/// excluded), tessellated so the chord error stays within `tolerance` drawing
/// units, capped at [`MAX_ANGLE_STEP`] per segment and [`MAX_ARC_SEGMENTS`]
/// segments per arc. Deterministic: pure arithmetic on the inputs.
fn tessellate_bulge(
    start: (f64, f64),
    end: (f64, f64),
    bulge: f64,
    tolerance: f64,
    warnings: &mut Vec<String>,
) -> Vec<(f64, f64)> {
    let chord_x = end.0 - start.0;
    let chord_y = end.1 - start.1;
    let chord = (chord_x * chord_x + chord_y * chord_y).sqrt();
    if chord <= f64::EPSILON {
        warnings.push("arc segment with coincident endpoints collapsed to a point".to_string());
        return Vec::new();
    }

    // bulge = tan(theta / 4); theta is the included angle, signed CCW.
    let theta = 4.0 * bulge.atan();
    let half_chord = chord / 2.0;
    let sagitta = bulge.abs() * half_chord;
    let radius = (half_chord * half_chord + sagitta * sagitta) / (2.0 * sagitta);
    let apothem = radius - sagitta;

    // Center sits on the perpendicular bisector; for positive bulge (CCW)
    // it lies on the left of start->end, mirrored for negative bulge.
    let left_x = -chord_y / chord;
    let left_y = chord_x / chord;
    let side = if bulge > 0.0 { 1.0 } else { -1.0 };
    let center_x = (start.0 + end.0) / 2.0 + left_x * apothem * side;
    let center_y = (start.1 + end.1) / 2.0 + left_y * apothem * side;

    let chord_limited_step = if tolerance >= radius {
        std::f64::consts::TAU
    } else {
        2.0 * (1.0 - tolerance / radius).acos()
    };
    let step = chord_limited_step.clamp(f64::EPSILON, MAX_ANGLE_STEP);
    let mut segments = (theta.abs() / step).ceil() as usize;
    segments = segments.max(1);
    if segments > MAX_ARC_SEGMENTS {
        segments = MAX_ARC_SEGMENTS;
        warnings.push(format!(
            "arc tessellation capped at {MAX_ARC_SEGMENTS} segments; chord tolerance not met"
        ));
    }

    let start_angle = (start.1 - center_y).atan2(start.0 - center_x);
    (1..segments)
        .map(|i| {
            let angle = start_angle + theta * (i as f64) / (segments as f64);
            (
                center_x + radius * angle.cos(),
                center_y + radius * angle.sin(),
            )
        })
        .collect()
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

    use super::{
        DEFAULT_CURVE_TOLERANCE, EntityOutcome, GeometryOptions, convert_entity, extract,
        signed_area, tessellate_bulge,
    };

    fn opts(polygonize_closed: bool) -> GeometryOptions {
        GeometryOptions {
            polygonize_closed,
            curve_tolerance: DEFAULT_CURVE_TOLERANCE,
        }
    }

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
        match convert_entity(&point, &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => {
                assert_eq!(geometry, GeometryValue::new_point((1.0, 2.0)));
            }
            _ => panic!("point must convert"),
        }

        let line = EntityType::Line(Line::from_coords(0.0, 0.0, 3.0, 4.0, 5.0, 3.0));
        match convert_entity(&line, &opts(false)) {
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
        match convert_entity(&line, &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("degenerate")),
            _ => panic!("degenerate line must be skipped"),
        }
    }

    #[test]
    fn closed_polyline_becomes_closed_linestring_by_default() {
        let polyline =
            EntityType::LwPolyline(lwpolyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], true));
        match convert_entity(&polyline, &opts(false)) {
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
        match convert_entity(&polyline, &opts(true)) {
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
    fn quarter_circle_bulge_tessellates_on_the_unit_circle() {
        let mut warnings = Vec::new();
        let bulge = (std::f64::consts::PI / 8.0).tan();
        let interior = tessellate_bulge((1.0, 0.0), (0.0, 1.0), bulge, 0.05, &mut warnings);

        assert!(warnings.is_empty());
        assert!((4..=6).contains(&interior.len()), "got {}", interior.len());
        for (x, y) in &interior {
            let radius = (x * x + y * y).sqrt();
            assert!((radius - 1.0).abs() < 1e-9, "point off circle: {x},{y}");
            assert!(
                *x > 0.0 && *y > 0.0,
                "point outside first quadrant: {x},{y}"
            );
        }
    }

    #[test]
    fn bulge_sign_selects_arc_side() {
        let mut warnings = Vec::new();
        let ccw = tessellate_bulge((0.0, 0.0), (10.0, 0.0), 1.0, 0.01, &mut warnings);
        let min_y = ccw.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
        assert!(
            (min_y + 5.0).abs() < 0.05,
            "CCW semicircle apex, got {min_y}"
        );

        let cw = tessellate_bulge((0.0, 0.0), (10.0, 0.0), -1.0, 0.01, &mut warnings);
        let max_y = cw.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (max_y - 5.0).abs() < 0.05,
            "CW semicircle apex, got {max_y}"
        );

        for (x, y) in ccw.iter().chain(cw.iter()) {
            let radius = ((x - 5.0).powi(2) + y.powi(2)).sqrt();
            assert!((radius - 5.0).abs() < 1e-9);
        }
    }

    #[test]
    fn bulged_polyline_is_tessellated_and_marked_approximated() {
        let mut polyline = lwpolyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false);
        polyline.vertices[1].bulge = 0.5;
        match convert_entity(&EntityType::LwPolyline(polyline), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                warnings,
            } => {
                match geometry {
                    GeometryValue::LineString { coordinates } => {
                        assert!(coordinates.len() > 3, "arc must add interior points");
                    }
                    other => panic!("expected LineString, got {other:?}"),
                }
                assert!(
                    extra_properties
                        .iter()
                        .any(|(key, value)| *key == "approximated"
                            && *value == serde_json::Value::Bool(true))
                );
                assert!(warnings.iter().any(|w| w.contains("chord tolerance")));
            }
            _ => panic!("bulged polyline must convert via tessellation"),
        }
    }

    #[test]
    fn closing_segment_bulge_forms_a_full_circle() {
        let mut polyline = lwpolyline(&[(0.0, 0.0), (10.0, 0.0)], true);
        polyline.vertices[0].bulge = 1.0;
        polyline.vertices[1].bulge = 1.0;
        match convert_entity(&EntityType::LwPolyline(polyline), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    assert_eq!(coordinates.first(), coordinates.last(), "ring must close");
                    assert!(coordinates.len() > 12);
                    for position in &coordinates {
                        let radius = ((position[0] - 5.0).powi(2) + position[1].powi(2)).sqrt();
                        assert!((radius - 5.0).abs() < 1e-9);
                    }
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("circle-shaped polyline must convert"),
        }
    }

    #[test]
    fn arc_segment_cap_is_reported() {
        let mut warnings = Vec::new();
        let interior = tessellate_bulge((0.0, 0.0), (10.0, 0.0), 1.0, 1e-7, &mut warnings);
        assert_eq!(interior.len(), super::MAX_ARC_SEGMENTS - 1);
        assert!(warnings.iter().any(|w| w.contains("capped")));
    }

    #[test]
    fn classic_2d_polyline_converts_and_smoothing_is_skipped() {
        use acadrust::entities::{Polyline2D, PolylineFlags, Vertex2D};
        use acadrust::types::Vector3;

        let mut plain = Polyline2D::new();
        plain.vertices = vec![
            Vertex2D::new(Vector3::new(0.0, 0.0, 0.0)),
            Vertex2D::new(Vector3::new(10.0, 0.0, 0.0)),
            Vertex2D::new(Vector3::new(10.0, 10.0, 0.0)),
        ];
        plain.flags = PolylineFlags::from_bits(PolylineFlags::CLOSED.bits());
        match convert_entity(&EntityType::Polyline2D(plain), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    assert_eq!(coordinates.len(), 4);
                    assert_eq!(coordinates.first(), coordinates.last());
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("classic 2D polyline must convert"),
        }

        let mut smoothed = Polyline2D::new();
        smoothed.vertices = vec![
            Vertex2D::new(Vector3::new(0.0, 0.0, 0.0)),
            Vertex2D::new(Vector3::new(10.0, 0.0, 0.0)),
        ];
        smoothed.flags = PolylineFlags::from_bits(PolylineFlags::SPLINE_FIT.bits());
        match convert_entity(&EntityType::Polyline2D(smoothed), &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("smoothing")),
            _ => panic!("spline-fit polyline must be skipped"),
        }
    }

    #[test]
    fn polyline3d_drops_z_with_warning() {
        use acadrust::entities::{Polyline3D, Vertex3DPolyline};
        use acadrust::types::{Handle, Vector3};

        let mut polyline = Polyline3D::new();
        polyline.flags.closed = true;
        polyline.vertices = [(0.0, 0.0, 2.0), (10.0, 0.0, 2.0), (10.0, 10.0, 2.0)]
            .into_iter()
            .map(|(x, y, z)| Vertex3DPolyline {
                handle: Handle::NULL,
                layer: "0".to_string(),
                position: Vector3::new(x, y, z),
                flags: 0,
            })
            .collect();
        match convert_entity(&EntityType::Polyline3D(polyline), &opts(false)) {
            EntityOutcome::Converted {
                geometry, warnings, ..
            } => {
                match geometry {
                    GeometryValue::LineString { coordinates } => {
                        assert_eq!(coordinates.len(), 4);
                        assert_eq!(coordinates.first(), coordinates.last());
                    }
                    other => panic!("expected LineString, got {other:?}"),
                }
                assert!(warnings.iter().any(|w| w.contains("z coordinates dropped")));
            }
            _ => panic!("3D polyline must convert"),
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

        let extraction = extract(&document, &opts(false)).expect("extract");

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

        let first = extract(&document, &opts(false)).expect("extract");
        let second = extract(&document, &opts(false)).expect("extract");

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
