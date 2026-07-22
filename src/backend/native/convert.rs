//! Native DWG -> GeoJSON conversion (Milestone 3).
//!
//! Converts model-space geometry (points, lines, polylines with bulge arcs,
//! arcs, circles, ellipses, splines, and text anchors) in raw drawing
//! coordinates; reprojection arrives with the `native-reproject` feature in
//! Milestone 5. Every entity that is not converted is counted in the report
//! with a reason — nothing is silently dropped. Output is deterministic:
//! features follow model-space document order, identifiers come from entity
//! handles, and all curve approximation uses pure arithmetic on the inputs.

use std::{collections::BTreeMap, fs, time::Instant};

use acadrust::{
    CadDocument,
    entities::EntityType,
    types::{Color, Matrix3, Vector3},
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

/// Hard cap on nested INSERT expansion depth.
const MAX_BLOCK_DEPTH: usize = 16;

/// Geometry-mapping options resolved from the CLI.
struct GeometryOptions {
    polygonize_closed: bool,
    curve_tolerance: f64,
    preserve_inserts: bool,
}

/// Row-major affine transform: `linear * v + translation`.
#[derive(Clone, Copy, Debug)]
struct Affine {
    linear: [[f64; 3]; 3],
    translation: [f64; 3],
}

impl Affine {
    const IDENTITY: Affine = Affine {
        linear: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        translation: [0.0, 0.0, 0.0],
    };

    fn from_linear(linear: [[f64; 3]; 3]) -> Affine {
        Affine {
            linear,
            translation: [0.0, 0.0, 0.0],
        }
    }

    fn from_translation(v: Vector3) -> Affine {
        Affine {
            linear: Affine::IDENTITY.linear,
            translation: [v.x, v.y, v.z],
        }
    }

    fn rotation_z(angle: f64) -> Affine {
        let (sin, cos) = angle.sin_cos();
        Affine::from_linear([[cos, -sin, 0.0], [sin, cos, 0.0], [0.0, 0.0, 1.0]])
    }

    fn scale(x: f64, y: f64, z: f64) -> Affine {
        Affine::from_linear([[x, 0.0, 0.0], [0.0, y, 0.0], [0.0, 0.0, z]])
    }

    fn apply(&self, v: Vector3) -> Vector3 {
        let m = &self.linear;
        Vector3::new(
            m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z + self.translation[0],
            m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z + self.translation[1],
            m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z + self.translation[2],
        )
    }

    /// Apply only the linear part (no translation); for directions.
    fn apply_linear(&self, v: Vector3) -> Vector3 {
        let m = &self.linear;
        Vector3::new(
            m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
            m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
            m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
        )
    }

    /// `self` after `inner` (apply `inner` first).
    fn compose(&self, inner: &Affine) -> Affine {
        let mut linear = [[0.0; 3]; 3];
        for (row, out) in linear.iter_mut().enumerate() {
            for (col, cell) in out.iter_mut().enumerate() {
                *cell = (0..3)
                    .map(|k| self.linear[row][k] * inner.linear[k][col])
                    .sum();
            }
        }
        let t = self.apply(Vector3::new(
            inner.translation[0],
            inner.translation[1],
            inner.translation[2],
        ));
        Affine {
            linear,
            translation: [t.x, t.y, t.z],
        }
    }
}

/// Where an entity is being placed: identity for direct model-space
/// entities, or the composed INSERT transform chain for block content.
struct Placement {
    matrix: Affine,
    /// Block-name chain, outermost first; used for provenance and to detect
    /// recursive references.
    block_path: Vec<String>,
    /// Insert-handle chain prefix so feature ids stay unique per instance.
    id_prefix: String,
    /// Layer substituted for block entities on layer "0" (the conventional
    /// BYBLOCK-style inheritance); None outside block content.
    inherited_layer: Option<String>,
    /// Resolved color of the enclosing insert, substituted for ByBlock
    /// entity colors; None outside block content or when unresolvable.
    inherited_color: Option<Color>,
    /// Resolved linetype of the enclosing insert, substituted for ByBlock
    /// entity linetypes; None outside block content or when unresolvable.
    inherited_linetype: Option<String>,
    /// Largest accumulated |scale| factor, for tessellation-error warnings.
    max_scale: f64,
}

impl Placement {
    fn model_space() -> Placement {
        Placement {
            matrix: Affine::IDENTITY,
            block_path: Vec::new(),
            id_prefix: String::new(),
            inherited_layer: None,
            inherited_color: None,
            inherited_linetype: None,
            max_scale: 1.0,
        }
    }
}

/// Layer "0" content inside a block takes the insert's effective layer.
fn effective_layer(source_layer: &str, placement: &Placement) -> String {
    if source_layer == "0" {
        placement
            .inherited_layer
            .clone()
            .unwrap_or_else(|| source_layer.to_string())
    } else {
        source_layer.to_string()
    }
}

/// Resolve a color policy to a concrete color: ByLayer through the effective
/// layer's table entry, ByBlock through the enclosing insert. None when the
/// policy cannot be resolved (missing layer, or ByBlock outside a block).
fn resolve_color(
    document: &CadDocument,
    color: Color,
    layer: &str,
    placement: &Placement,
) -> Option<Color> {
    match color {
        Color::Index(_) | Color::Rgb { .. } => Some(color),
        Color::ByLayer => document
            .layers
            .get(layer)
            .map(|entry| entry.color)
            .filter(|resolved| matches!(resolved, Color::Index(_) | Color::Rgb { .. })),
        Color::ByBlock => placement.inherited_color,
    }
}

/// Resolve a linetype name the same way; an empty name means ByLayer.
fn resolve_linetype(
    document: &CadDocument,
    linetype: &str,
    layer: &str,
    placement: &Placement,
) -> Option<String> {
    if linetype.is_empty() || linetype.eq_ignore_ascii_case("bylayer") {
        document
            .layers
            .get(layer)
            .map(|entry| entry.line_type.clone())
    } else if linetype.eq_ignore_ascii_case("byblock") {
        placement.inherited_linetype.clone()
    } else {
        Some(linetype.to_string())
    }
}

/// Style properties for a feature: resolved color (ACI index and/or RGB) and
/// linetype. Unresolvable policies are emitted verbatim ("ByLayer" or
/// "ByBlock") instead of being dropped.
fn style_properties(
    document: &CadDocument,
    common: &acadrust::entities::EntityCommon,
    layer: &str,
    placement: &Placement,
) -> Vec<(&'static str, JsonValue)> {
    let mut properties = Vec::new();
    match resolve_color(document, common.color, layer, placement) {
        Some(color) => {
            if let Some(index) = color.index() {
                properties.push(("color_index", JsonValue::from(index)));
            }
            if let Some((r, g, b)) = color.rgb() {
                properties.push((
                    "color_rgb",
                    JsonValue::from(format!("#{r:02X}{g:02X}{b:02X}")),
                ));
            }
        }
        None => properties.push(("color", JsonValue::from(common.color.to_string()))),
    }
    match resolve_linetype(document, &common.linetype, layer, placement) {
        Some(name) => properties.push(("linetype", JsonValue::from(name))),
        None => properties.push(("linetype", JsonValue::from("ByBlock"))),
    }
    properties
}

/// Apply the placement and project to 2D, tracking dropped |z|. None on
/// non-finite results.
fn project(placement: &Placement, point: Vector3, max_abs_z: &mut f64) -> Option<(f64, f64)> {
    let placed = placement.matrix.apply(point);
    if !is_finite(&placed) {
        return None;
    }
    *max_abs_z = max_abs_z.max(placed.z.abs());
    Some((placed.x, placed.y))
}

/// Block-reference transform: translate to the insertion point, orient by
/// the insert normal, rotate, offset the MINSERT cell (rotated, unscaled),
/// scale, and shift by the block base point.
fn insert_matrix(
    insert: &acadrust::entities::Insert,
    base_point: Vector3,
    column: u16,
    row: u16,
) -> Affine {
    let to_insertion = Affine::from_translation(insert.insert_point);
    let orient = Affine::from_linear(Matrix3::arbitrary_axis(insert.normal).m);
    let rotate = Affine::rotation_z(insert.rotation);
    let cell = Affine::from_translation(Vector3::new(
        f64::from(column) * insert.column_spacing,
        f64::from(row) * insert.row_spacing,
        0.0,
    ));
    let scale = Affine::scale(insert.x_scale(), insert.y_scale(), insert.z_scale());
    let from_base = Affine::from_translation(base_point * -1.0);

    to_insertion
        .compose(&orient)
        .compose(&rotate)
        .compose(&cell)
        .compose(&scale)
        .compose(&from_base)
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
        preserve_inserts: request.preserve_inserts,
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
            block_mode: Some(
                if request.preserve_inserts {
                    "preserve-inserts"
                } else {
                    "explode"
                }
                .to_string(),
            ),
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
            inserts_expanded: extraction.inserts_expanded,
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
    inserts_expanded: usize,
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
    let model_placement = Placement::model_space();

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

            process_entity(
                document,
                entity,
                model_index,
                options,
                &mut extraction,
                &model_placement,
                0,
            );
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

#[allow(clippy::too_many_arguments)]
fn process_entity(
    document: &CadDocument,
    entity: &EntityType,
    index: usize,
    options: &GeometryOptions,
    extraction: &mut Extraction,
    placement: &Placement,
    depth: usize,
) {
    if let EntityType::Insert(insert) = entity {
        process_insert(
            document, entity, insert, options, extraction, placement, depth,
        );
        return;
    }

    let entity_type = entity.as_entity().entity_type().to_string();
    let feature_id = feature_id(entity, index, placement);

    match convert_entity(entity, options, placement) {
        EntityOutcome::Converted {
            geometry,
            mut extra_properties,
            warnings,
        } => {
            let layer = effective_layer(&entity.common().layer, placement);
            extra_properties.extend(style_properties(
                document,
                entity.common(),
                &layer,
                placement,
            ));
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
                feature_id,
                &entity_type,
                geometry,
                extra_properties,
                warnings,
                placement,
            ));
        }
        EntityOutcome::Skipped(reason) => {
            record_outcome(&mut extraction.skipped, entity_type, reason, &feature_id);
        }
        EntityOutcome::Failed(reason) => {
            record_outcome(&mut extraction.failed, entity_type, reason, &feature_id);
        }
    }
}

/// Unique, stable feature id: the insert-handle chain plus the entity's own
/// handle (or its model-space position when the handle is null).
fn feature_id(entity: &EntityType, index: usize, placement: &Placement) -> String {
    let handle = entity.common().handle;
    if handle.is_null() {
        format!("{}model-{index}", placement.id_prefix)
    } else {
        format!("{}{}", placement.id_prefix, handle)
    }
}

#[allow(clippy::too_many_arguments)]
fn process_insert(
    document: &CadDocument,
    entity: &EntityType,
    insert: &acadrust::entities::Insert,
    options: &GeometryOptions,
    extraction: &mut Extraction,
    placement: &Placement,
    depth: usize,
) {
    let id = feature_id(entity, 0, placement);

    // Entities on layer "0" inside a block conventionally take the insert's
    // layer; resolve the insert's own effective layer, color, and linetype
    // first so the rules compose through nesting.
    let insert_layer = effective_layer(&entity.common().layer, placement);
    let insert_color = resolve_color(document, entity.common().color, &insert_layer, placement);
    let insert_linetype = resolve_linetype(
        document,
        &entity.common().linetype,
        &insert_layer,
        placement,
    );

    let attributes: BTreeMap<String, String> = insert
        .attributes
        .iter()
        .map(|attribute| (attribute.tag.clone(), attribute.value.clone()))
        .collect();

    if options.preserve_inserts {
        emit_insert_anchor(document, entity, insert, &attributes, extraction, placement);
        return;
    }

    let Some(record) = document.block_records.get(&insert.block_name) else {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            format!(
                "references missing block definition {:?}",
                insert.block_name
            ),
            &id,
        );
        return;
    };
    if placement
        .block_path
        .iter()
        .any(|name| name.eq_ignore_ascii_case(&insert.block_name))
    {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            format!("recursive reference to block {:?}", insert.block_name),
            &id,
        );
        return;
    }
    if depth >= MAX_BLOCK_DEPTH {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            format!("block nesting deeper than {MAX_BLOCK_DEPTH} levels"),
            &id,
        );
        return;
    }

    let columns = insert.column_count.max(1);
    let rows = insert.row_count.max(1);
    let multi = columns > 1 || rows > 1;
    let instance_scale = insert
        .x_scale()
        .abs()
        .max(insert.y_scale().abs())
        .max(insert.z_scale().abs());

    let mut block_path = placement.block_path.clone();
    block_path.push(insert.block_name.clone());

    for row in 0..rows {
        for column in 0..columns {
            let cell_suffix = if multi {
                format!("[{row},{column}]")
            } else {
                String::new()
            };
            let child_placement = Placement {
                matrix: placement.matrix.compose(&insert_matrix(
                    insert,
                    record.base_point,
                    column,
                    row,
                )),
                block_path: block_path.clone(),
                id_prefix: format!("{id}{cell_suffix}/"),
                inherited_layer: Some(insert_layer.clone()),
                inherited_color: insert_color,
                inherited_linetype: insert_linetype.clone(),
                max_scale: placement.max_scale * instance_scale,
            };

            for handle in &record.entity_handles {
                let Some(child) = document.get_entity(*handle) else {
                    record_outcome(
                        &mut extraction.failed,
                        "UNRESOLVED".to_string(),
                        "entity handle does not resolve to an entity".to_string(),
                        &format!("{}{}", child_placement.id_prefix, handle),
                    );
                    continue;
                };
                if matches!(child, EntityType::Block(_) | EntityType::BlockEnd(_)) {
                    continue;
                }
                if matches!(child, EntityType::AttributeDefinition(_)) {
                    record_outcome(
                        &mut extraction.skipped,
                        "ATTDEF".to_string(),
                        "attribute definition template; values are read from the INSERT"
                            .to_string(),
                        &format!("{}{}", child_placement.id_prefix, child.common().handle),
                    );
                    continue;
                }
                process_entity(
                    document,
                    child,
                    0,
                    options,
                    extraction,
                    &child_placement,
                    depth + 1,
                );
            }
        }
    }

    extraction.inserts_expanded += 1;
    if !attributes.is_empty() {
        emit_insert_anchor(document, entity, insert, &attributes, extraction, placement);
    }
}

/// Point feature at the (transformed) insertion point carrying the block
/// name and attribute values.
fn emit_insert_anchor(
    document: &CadDocument,
    entity: &EntityType,
    insert: &acadrust::entities::Insert,
    attributes: &BTreeMap<String, String>,
    extraction: &mut Extraction,
    placement: &Placement,
) {
    let id = feature_id(entity, 0, placement);
    let mut max_abs_z: f64 = 0.0;
    let Some(position) = project(placement, insert.insert_point, &mut max_abs_z) else {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            "non-finite coordinates".to_string(),
            &id,
        );
        return;
    };

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    let mut extra_properties = vec![
        ("block_name", JsonValue::from(insert.block_name.clone())),
        (
            "rotation_deg",
            JsonValue::from(insert.rotation.to_degrees()),
        ),
    ];
    if !attributes.is_empty() {
        let map: JsonObject = attributes
            .iter()
            .map(|(tag, value)| (tag.clone(), JsonValue::from(value.clone())))
            .collect();
        extra_properties.push(("attributes", JsonValue::Object(map)));
    }
    let layer = effective_layer(&entity.common().layer, placement);
    extra_properties.extend(style_properties(
        document,
        entity.common(),
        &layer,
        placement,
    ));

    extraction.feature_warnings += warnings.len();
    *extraction
        .converted
        .entry("INSERT".to_string())
        .or_default() += 1;
    extraction.features.push(build_feature(
        entity,
        id,
        "INSERT",
        GeometryValue::new_point(position),
        extra_properties,
        warnings,
        placement,
    ));
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
    feature_id: String,
    entity_type: &str,
    geometry: GeometryValue,
    extra_properties: Vec<(&'static str, JsonValue)>,
    warnings: Vec<String>,
    placement: &Placement,
) -> Feature {
    let common = entity.common();

    let source_layer = common.layer.clone();
    let layer = effective_layer(&source_layer, placement);

    let mut properties = JsonObject::new();
    properties.insert("layer".to_string(), JsonValue::from(layer.clone()));
    if layer != source_layer {
        properties.insert("source_layer".to_string(), JsonValue::from(source_layer));
    }
    properties.insert(
        "entity_type".to_string(),
        JsonValue::from(entity_type.to_string()),
    );
    properties.insert("space".to_string(), JsonValue::from("model"));
    properties.insert(
        "handle".to_string(),
        JsonValue::from(format!("{}", common.handle)),
    );
    if !placement.block_path.is_empty() {
        properties.insert(
            "block_path".to_string(),
            JsonValue::from(placement.block_path.join("/")),
        );
    }
    for (key, value) in extra_properties {
        properties.insert(key.to_string(), value);
    }
    if !warnings.is_empty() {
        properties.insert("warnings".to_string(), JsonValue::from(warnings));
    }

    Feature {
        bbox: None,
        geometry: Some(Geometry::new(geometry)),
        id: Some(Id::String(feature_id)),
        properties: Some(properties),
        foreign_members: None,
    }
}

fn convert_entity(
    entity: &EntityType,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    match entity {
        EntityType::Point(point) => convert_point(point, placement),
        EntityType::Line(line) => convert_line(line, placement),
        EntityType::LwPolyline(polyline) => convert_lwpolyline(polyline, options, placement),
        EntityType::Polyline2D(polyline) => convert_polyline2d(polyline, options, placement),
        EntityType::Polyline3D(polyline) => convert_polyline3d(polyline, options, placement),
        EntityType::Polyline(polyline) => convert_polyline_generic(polyline, options, placement),
        EntityType::Circle(circle) => convert_circle(circle, options, placement),
        EntityType::Arc(arc) => convert_arc(arc, options, placement),
        EntityType::Ellipse(ellipse) => convert_ellipse(ellipse, options, placement),
        EntityType::Spline(spline) => convert_spline(spline, options, placement),
        EntityType::Text(text) => convert_text(text, placement),
        EntityType::MText(mtext) => convert_mtext(mtext, placement),
        _ => EntityOutcome::Skipped(
            "entity type is not converted by the native backend yet".to_string(),
        ),
    }
}

fn convert_point(point: &acadrust::entities::Point, placement: &Placement) -> EntityOutcome {
    if !is_finite(&point.location) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    let mut max_abs_z: f64 = 0.0;
    let Some(position) = project(placement, point.location, &mut max_abs_z) else {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    };

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    EntityOutcome::Converted {
        geometry: GeometryValue::new_point(position),
        extra_properties: Vec::new(),
        warnings,
    }
}

fn convert_line(line: &acadrust::entities::Line, placement: &Placement) -> EntityOutcome {
    if !is_finite(&line.start) || !is_finite(&line.end) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    let mut max_abs_z: f64 = 0.0;
    let (Some(start), Some(end)) = (
        project(placement, line.start, &mut max_abs_z),
        project(placement, line.end, &mut max_abs_z),
    ) else {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    };
    if start == end {
        return EntityOutcome::Skipped("degenerate line: identical XY endpoints".to_string());
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    EntityOutcome::Converted {
        geometry: GeometryValue::new_line_string(vec![start, end]),
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
    placement: &Placement,
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
        placement,
    )
}

fn convert_polyline2d(
    polyline: &acadrust::entities::Polyline2D,
    options: &GeometryOptions,
    placement: &Placement,
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
        placement,
    )
}

fn convert_polyline3d(
    polyline: &acadrust::entities::Polyline3D,
    options: &GeometryOptions,
    placement: &Placement,
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
    finish_wcs_path(&points, polyline.flags.closed, options, placement)
}

fn convert_polyline_generic(
    polyline: &acadrust::entities::Polyline,
    options: &GeometryOptions,
    placement: &Placement,
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
    finish_wcs_path(&points, polyline.is_closed(), options, placement)
}

/// Expand bulge arcs in the OCS plane, lift to WCS via the arbitrary axis
/// algorithm (identity for the default normal), and build the line/polygon.
fn finish_ocs_path(
    vertices: &[OcsVertex],
    closed: bool,
    elevation: f64,
    normal: Vector3,
    options: &GeometryOptions,
    placement: &Placement,
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
        let Some(position) = project(placement, wcs, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        coordinates.push(position);
    }

    push_z_warning(&mut warnings, max_abs_z);
    if approximated {
        warnings.push(format!(
            "arc segments tessellated with chord tolerance {} drawing units",
            options.curve_tolerance
        ));
    }
    finish_coordinates(
        coordinates,
        closed,
        approximated,
        options,
        placement,
        warnings,
    )
}

/// 3D polylines carry WCS positions and no bulges; drop z with a warning.
fn finish_wcs_path(
    points: &[Vector3],
    closed: bool,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    if points.len() < 2 {
        return EntityOutcome::Skipped("polyline has fewer than two vertices".to_string());
    }
    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(points.len() + 1);
    let mut max_abs_z: f64 = 0.0;
    for point in points {
        if !is_finite(point) {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        }
        let Some(position) = project(placement, *point, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        coordinates.push(position);
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);
    finish_coordinates(coordinates, closed, false, options, placement, warnings)
}

fn finish_coordinates(
    mut coordinates: Vec<(f64, f64)>,
    closed: bool,
    approximated: bool,
    options: &GeometryOptions,
    placement: &Placement,
    mut warnings: Vec<String>,
) -> EntityOutcome {
    if approximated && placement.max_scale > 1.0 + 1e-9 {
        warnings.push(format!(
            "placed by an insert with scale {}; the effective chord error scales accordingly",
            placement.max_scale
        ));
    }
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

    let start_angle = (start.1 - center_y).atan2(start.0 - center_x);
    let mut points = arc_points(
        (center_x, center_y),
        radius,
        start_angle,
        theta,
        tolerance,
        warnings,
    );
    // Interior points only: the polyline vertices already provide both
    // endpoints.
    points.pop();
    if !points.is_empty() {
        points.remove(0);
    }
    points
}

/// Points of a circular arc from `start_angle` sweeping `sweep` radians
/// (signed CCW), inclusive of both endpoints, tessellated so the chord error
/// stays within `tolerance` drawing units, capped at [`MAX_ANGLE_STEP`] per
/// segment and [`MAX_ARC_SEGMENTS`] segments. Deterministic.
fn arc_points(
    center: (f64, f64),
    radius: f64,
    start_angle: f64,
    sweep: f64,
    tolerance: f64,
    warnings: &mut Vec<String>,
) -> Vec<(f64, f64)> {
    let chord_limited_step = if tolerance >= radius {
        std::f64::consts::TAU
    } else {
        2.0 * (1.0 - tolerance / radius).acos()
    };
    let step = chord_limited_step.clamp(f64::EPSILON, MAX_ANGLE_STEP);
    let mut segments = (sweep.abs() / step).ceil() as usize;
    segments = segments.max(1);
    if segments > MAX_ARC_SEGMENTS {
        segments = MAX_ARC_SEGMENTS;
        warnings.push(format!(
            "arc tessellation capped at {MAX_ARC_SEGMENTS} segments; chord tolerance not met"
        ));
    }

    (0..=segments)
        .map(|i| {
            let angle = start_angle + sweep * (i as f64) / (segments as f64);
            (
                center.0 + radius * angle.cos(),
                center.1 + radius * angle.sin(),
            )
        })
        .collect()
}

/// DXF circles live in the OCS plane of their normal; tessellate CCW and
/// close the ring.
fn convert_circle(
    circle: &acadrust::entities::Circle,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    if !is_finite(&circle.center) || !circle.radius.is_finite() {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if circle.radius <= 0.0 {
        return EntityOutcome::Skipped("degenerate circle: non-positive radius".to_string());
    }

    let mut warnings = Vec::new();
    let mut ocs_points = arc_points(
        (circle.center.x, circle.center.y),
        circle.radius,
        0.0,
        std::f64::consts::TAU,
        options.curve_tolerance,
        &mut warnings,
    );
    // The finisher closes the ring; drop the duplicated end point.
    ocs_points.pop();

    finish_curve(
        ocs_points,
        circle.center.z,
        circle.normal,
        true,
        options,
        placement,
        warnings,
    )
}

/// DXF arcs sweep counter-clockwise from start to end angle in the OCS plane.
fn convert_arc(
    arc: &acadrust::entities::Arc,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    if !is_finite(&arc.center)
        || !arc.radius.is_finite()
        || !arc.start_angle.is_finite()
        || !arc.end_angle.is_finite()
    {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if arc.radius <= 0.0 {
        return EntityOutcome::Skipped("degenerate arc: non-positive radius".to_string());
    }

    let mut sweep = (arc.end_angle - arc.start_angle).rem_euclid(std::f64::consts::TAU);
    if sweep <= f64::EPSILON {
        sweep = std::f64::consts::TAU;
    }

    let mut warnings = Vec::new();
    let ocs_points = arc_points(
        (arc.center.x, arc.center.y),
        arc.radius,
        arc.start_angle,
        sweep,
        options.curve_tolerance,
        &mut warnings,
    );

    finish_curve(
        ocs_points,
        arc.center.z,
        arc.normal,
        false,
        options,
        placement,
        warnings,
    )
}

/// DXF ellipses are parametric in WCS: center and major-axis vector are
/// world coordinates and the minor axis is `normal x major * ratio`.
fn convert_ellipse(
    ellipse: &acadrust::entities::Ellipse,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    if !is_finite(&ellipse.center)
        || !is_finite(&ellipse.major_axis)
        || !ellipse.minor_axis_ratio.is_finite()
        || !ellipse.start_parameter.is_finite()
        || !ellipse.end_parameter.is_finite()
    {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    let major_length = ellipse.major_axis.length();
    if major_length <= 0.0 || ellipse.minor_axis_ratio <= 0.0 {
        return EntityOutcome::Skipped(
            "degenerate ellipse: non-positive axis length or ratio".to_string(),
        );
    }
    let minor_direction = ellipse.normal.cross(&ellipse.major_axis);
    let minor_direction_length = minor_direction.length();
    if minor_direction_length <= 0.0 {
        return EntityOutcome::Skipped(
            "degenerate ellipse: normal is parallel to the major axis".to_string(),
        );
    }
    let minor_axis =
        minor_direction * (major_length * ellipse.minor_axis_ratio / minor_direction_length);

    let mut sweep =
        (ellipse.end_parameter - ellipse.start_parameter).rem_euclid(std::f64::consts::TAU);
    let closed = sweep <= f64::EPSILON;
    if closed {
        sweep = std::f64::consts::TAU;
    }

    // The circle step formula with the major radius bounds the ellipse chord
    // error: local error is ~step^2 * axis / 8 and the major axis dominates.
    let mut warnings = Vec::new();
    let parameters = arc_points(
        (0.0, 0.0),
        1.0,
        ellipse.start_parameter,
        sweep,
        options.curve_tolerance / major_length,
        &mut warnings,
    );

    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(parameters.len());
    let mut max_abs_z: f64 = 0.0;
    for (cos_t, sin_t) in parameters {
        let point = ellipse.center + ellipse.major_axis * cos_t + minor_axis * sin_t;
        let Some(position) = project(placement, point, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        coordinates.push(position);
    }
    if closed {
        coordinates.pop();
    }

    push_z_warning(&mut warnings, max_abs_z);
    warnings.push(format!(
        "arc segments tessellated with chord tolerance {} drawing units",
        options.curve_tolerance
    ));
    finish_coordinates(coordinates, closed, true, options, placement, warnings)
}

/// Fixed sampling density for spline evaluation, per knot span.
const SPLINE_SEGMENTS_PER_SPAN: usize = 8;
const SPLINE_MIN_SEGMENTS: usize = 16;

/// Evaluate the NURBS control net with de Boor's algorithm; when the NURBS
/// data is invalid, fall back to a polyline through the fit points rather
/// than dropping the entity silently.
fn convert_spline(
    spline: &acadrust::entities::Spline,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    let degree = spline.degree;
    let control_count = spline.control_points.len();
    let nurbs_valid = degree >= 1
        && control_count > degree as usize
        && spline.knots.len() == control_count + degree as usize + 1
        && spline.knots.windows(2).all(|pair| pair[0] <= pair[1])
        && spline.knots.iter().all(|knot| knot.is_finite())
        && spline.control_points.iter().all(is_finite);

    if !nurbs_valid {
        if spline.fit_points.len() >= 2 && spline.fit_points.iter().all(is_finite) {
            let mut outcome =
                finish_wcs_path(&spline.fit_points, spline.flags.closed, options, placement);
            if let EntityOutcome::Converted {
                extra_properties,
                warnings,
                ..
            } = &mut outcome
            {
                extra_properties.push(("approximated", JsonValue::Bool(true)));
                warnings.push(format!(
                    "spline rendered as a polyline through its {} fit points (invalid or missing NURBS data)",
                    spline.fit_points.len()
                ));
            }
            return outcome;
        }
        return EntityOutcome::Skipped(
            "spline has invalid NURBS data and no fit points".to_string(),
        );
    }

    let degree = degree as usize;
    let uniform_weights =
        spline.weights.len() != control_count || spline.weights.iter().any(|w| !w.is_finite());
    let homogeneous: Vec<[f64; 4]> = spline
        .control_points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let weight = if uniform_weights {
                1.0
            } else {
                spline.weights[index]
            };
            [point.x * weight, point.y * weight, point.z * weight, weight]
        })
        .collect();

    let domain_start = spline.knots[degree];
    let domain_end = spline.knots[control_count];
    if domain_end <= domain_start {
        return EntityOutcome::Skipped("spline has an empty parameter domain".to_string());
    }

    let spans = control_count - degree;
    let segments = (spans * SPLINE_SEGMENTS_PER_SPAN).clamp(SPLINE_MIN_SEGMENTS, MAX_ARC_SEGMENTS);

    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(segments + 1);
    let mut max_abs_z: f64 = 0.0;
    for i in 0..=segments {
        let t = domain_start + (domain_end - domain_start) * (i as f64) / (segments as f64);
        let Some(point) = evaluate_nurbs(t, degree, &spline.knots, &homogeneous) else {
            return EntityOutcome::Failed(
                "spline evaluation produced a non-finite or zero-weight point".to_string(),
            );
        };
        let Some(position) = project(
            placement,
            Vector3::new(point.0, point.1, point.2),
            &mut max_abs_z,
        ) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        coordinates.push(position);
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);
    warnings.push(format!(
        "spline sampled at {} points with uniform parameter spacing; chord tolerance is not applied to splines yet",
        segments + 1
    ));
    finish_coordinates(
        coordinates,
        spline.flags.closed,
        true,
        options,
        placement,
        warnings,
    )
}

/// De Boor evaluation on homogeneous coordinates. Returns the Cartesian
/// point, or None on zero weight or non-finite arithmetic.
fn evaluate_nurbs(
    t: f64,
    degree: usize,
    knots: &[f64],
    control: &[[f64; 4]],
) -> Option<(f64, f64, f64)> {
    let count = control.len();
    let mut span = count - 1;
    for i in degree..count {
        if t < knots[i + 1] {
            span = i;
            break;
        }
    }

    let mut d: Vec<[f64; 4]> = (0..=degree).map(|j| control[j + span - degree]).collect();
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = j + span - degree;
            let denominator = knots[i + degree - r + 1] - knots[i];
            let alpha = if denominator.abs() <= f64::EPSILON {
                0.0
            } else {
                (t - knots[i]) / denominator
            };
            let previous = d[j - 1];
            for (c, cell) in d[j].iter_mut().enumerate() {
                *cell = (1.0 - alpha) * previous[c] + alpha * *cell;
            }
        }
    }

    let [x, y, z, w] = d[degree];
    if w.abs() <= f64::EPSILON || !(x / w).is_finite() || !(y / w).is_finite() {
        return None;
    }
    Some((x / w, y / w, z / w))
}

/// TEXT anchors live in OCS; the second alignment point, when present, is
/// the effective anchor for aligned text.
fn convert_text(text: &acadrust::entities::Text, placement: &Placement) -> EntityOutcome {
    let anchor = text.alignment_point.unwrap_or(text.insertion_point);
    if !is_finite(&anchor) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    let wcs = Matrix3::arbitrary_axis(text.normal).transform_point(anchor);
    let mut max_abs_z: f64 = 0.0;
    let Some(position) = project(placement, wcs, &mut max_abs_z) else {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    };

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    let rotation_deg = effective_rotation_degrees(
        text.rotation,
        Some(&Matrix3::arbitrary_axis(text.normal)),
        placement,
    );

    EntityOutcome::Converted {
        geometry: GeometryValue::new_point(position),
        extra_properties: vec![
            ("text", JsonValue::from(text.value.clone())),
            ("text_height", JsonValue::from(text.height)),
            ("text_rotation_deg", JsonValue::from(rotation_deg)),
            ("text_style", JsonValue::from(text.style.clone())),
        ],
        warnings,
    }
}

/// Rotation of a text baseline after the placement transform, in degrees.
/// Model-space text keeps its stored rotation verbatim; block content gets
/// the direction of the transformed baseline (well-defined even under
/// non-uniform scale), normalized to [0, 360).
fn effective_rotation_degrees(
    rotation: f64,
    ocs_to_wcs: Option<&Matrix3>,
    placement: &Placement,
) -> f64 {
    if placement.block_path.is_empty() {
        return rotation.to_degrees();
    }
    let (sin, cos) = rotation.sin_cos();
    let mut direction = Vector3::new(cos, sin, 0.0);
    if let Some(matrix) = ocs_to_wcs {
        direction = matrix.transform_point(direction);
    }
    let placed = placement.matrix.apply_linear(direction);
    if !placed.x.is_finite() || !placed.y.is_finite() || placed.x.hypot(placed.y) <= f64::EPSILON {
        return rotation.to_degrees();
    }
    placed.y.atan2(placed.x).to_degrees().rem_euclid(360.0)
}

/// MTEXT insertion points are WCS; the value may carry inline format codes,
/// which are stripped into a plain-text property (raw kept when different).
fn convert_mtext(mtext: &acadrust::entities::MText, placement: &Placement) -> EntityOutcome {
    let anchor = mtext.insertion_point;
    if !is_finite(&anchor) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    let mut max_abs_z: f64 = 0.0;
    let Some(position) = project(placement, anchor, &mut max_abs_z) else {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    };

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    let plain = strip_mtext_codes(&mtext.value);
    let rotation_deg = effective_rotation_degrees(mtext.rotation, None, placement);
    let mut extra_properties = vec![
        ("text", JsonValue::from(plain.clone())),
        ("text_height", JsonValue::from(mtext.height)),
        ("text_rotation_deg", JsonValue::from(rotation_deg)),
        ("text_style", JsonValue::from(mtext.style.clone())),
    ];
    if plain != mtext.value {
        extra_properties.push(("text_raw", JsonValue::from(mtext.value.clone())));
    }

    EntityOutcome::Converted {
        geometry: GeometryValue::new_point(position),
        extra_properties,
        warnings,
    }
}

/// Best-effort removal of MTEXT inline format codes: paragraph breaks become
/// newlines, format commands with `;` terminators are dropped, grouping
/// braces are removed, and stacked fractions keep their text.
fn strip_mtext_codes(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '{' | '}' => {}
            '\\' => match chars.next() {
                Some('P') => plain.push('\n'),
                Some('~') => plain.push(' '),
                Some('\\') => plain.push('\\'),
                Some('{') => plain.push('{'),
                Some('}') => plain.push('}'),
                Some('S') => {
                    for stacked in chars.by_ref() {
                        if stacked == ';' {
                            break;
                        }
                        plain.push(match stacked {
                            '^' | '#' => '/',
                            other => other,
                        });
                    }
                }
                Some('f') | Some('F') | Some('H') | Some('C') | Some('c') | Some('T')
                | Some('Q') | Some('W') | Some('A') | Some('p') => {
                    for skipped in chars.by_ref() {
                        if skipped == ';' {
                            break;
                        }
                    }
                }
                Some('L') | Some('l') | Some('O') | Some('o') | Some('K') | Some('k')
                | Some('X') => {}
                Some(other) => plain.push(other),
                None => {}
            },
            other => plain.push(other),
        }
    }
    plain
}

/// Lift tessellated OCS curve points to WCS and build the final geometry,
/// always marked as approximated.
fn finish_curve(
    ocs_points: Vec<(f64, f64)>,
    ocs_z: f64,
    normal: Vector3,
    closed: bool,
    options: &GeometryOptions,
    placement: &Placement,
    mut warnings: Vec<String>,
) -> EntityOutcome {
    let ocs_to_wcs = Matrix3::arbitrary_axis(normal);
    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(ocs_points.len() + 1);
    let mut max_abs_z: f64 = 0.0;
    for (x, y) in ocs_points {
        let wcs = ocs_to_wcs.transform_point(Vector3::new(x, y, ocs_z));
        let Some(position) = project(placement, wcs, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        coordinates.push(position);
    }

    push_z_warning(&mut warnings, max_abs_z);
    warnings.push(format!(
        "arc segments tessellated with chord tolerance {} drawing units",
        options.curve_tolerance
    ));
    finish_coordinates(coordinates, closed, true, options, placement, warnings)
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
        DEFAULT_CURVE_TOLERANCE, EntityOutcome, GeometryOptions, Placement, extract, signed_area,
        tessellate_bulge,
    };

    fn opts(polygonize_closed: bool) -> GeometryOptions {
        GeometryOptions {
            polygonize_closed,
            curve_tolerance: DEFAULT_CURVE_TOLERANCE,
            preserve_inserts: false,
        }
    }

    /// Test shorthand: convert with the identity model-space placement.
    fn convert_entity(entity: &EntityType, options: &GeometryOptions) -> EntityOutcome {
        super::convert_entity(entity, options, &Placement::model_space())
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
    fn circle_tessellates_to_closed_ring_on_the_circle() {
        let circle = EntityType::Circle(Circle::from_coords(5.0, -2.0, 0.0, 5.0));
        match convert_entity(&circle, &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                warnings,
            } => {
                match geometry {
                    GeometryValue::LineString { coordinates } => {
                        assert_eq!(coordinates.first(), coordinates.last());
                        assert!(coordinates.len() >= 25, "got {}", coordinates.len());
                        for position in &coordinates {
                            let radius =
                                ((position[0] - 5.0).powi(2) + (position[1] + 2.0).powi(2)).sqrt();
                            assert!((radius - 5.0).abs() < 1e-9);
                        }
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
            _ => panic!("circle must convert"),
        }
    }

    #[test]
    fn circle_polygonizes_to_ccw_ring() {
        let circle = EntityType::Circle(Circle::from_coords(0.0, 0.0, 0.0, 2.0));
        match convert_entity(&circle, &opts(true)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::Polygon { coordinates } => {
                    let ring = &coordinates[0];
                    assert_eq!(ring.first(), ring.last());
                    let tuples: Vec<(f64, f64)> = ring.iter().map(|p| (p[0], p[1])).collect();
                    assert!(signed_area(&tuples) > 0.0, "ring must be CCW");
                }
                other => panic!("expected Polygon, got {other:?}"),
            },
            _ => panic!("circle must polygonize"),
        }
    }

    #[test]
    fn arc_preserves_endpoints_and_crosses_zero_angle() {
        use acadrust::entities::Arc;

        // From 270 degrees to 90 degrees: sweeps CCW through 0 degrees.
        let arc = EntityType::Arc(Arc::from_coords(
            0.0,
            0.0,
            0.0,
            1.0,
            1.5 * std::f64::consts::PI,
            0.5 * std::f64::consts::PI,
        ));
        match convert_entity(&arc, &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    let first = coordinates.first().expect("start");
                    let last = coordinates.last().expect("end");
                    assert!((first[0] - 0.0).abs() < 1e-9 && (first[1] + 1.0).abs() < 1e-9);
                    assert!((last[0] - 0.0).abs() < 1e-9 && (last[1] - 1.0).abs() < 1e-9);
                    // Passes through (1, 0), never through (-1, 0).
                    assert!(coordinates.iter().all(|p| p[0] > -1e-9));
                    for position in &coordinates {
                        let radius = (position[0].powi(2) + position[1].powi(2)).sqrt();
                        assert!((radius - 1.0).abs() < 1e-9);
                    }
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("arc must convert"),
        }
    }

    #[test]
    fn full_ellipse_tessellates_on_the_ellipse_and_closes() {
        use acadrust::entities::Ellipse;
        use acadrust::types::Vector3;

        let mut ellipse = Ellipse::new();
        ellipse.center = Vector3::new(10.0, 5.0, 0.0);
        ellipse.major_axis = Vector3::new(4.0, 0.0, 0.0);
        ellipse.minor_axis_ratio = 0.5;
        ellipse.start_parameter = 0.0;
        ellipse.end_parameter = std::f64::consts::TAU;
        match convert_entity(&EntityType::Ellipse(ellipse), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    assert_eq!(coordinates.first(), coordinates.last());
                    assert!(coordinates.len() > 20);
                    for position in &coordinates {
                        let ellipse_eq = ((position[0] - 10.0) / 4.0).powi(2)
                            + ((position[1] - 5.0) / 2.0).powi(2);
                        assert!((ellipse_eq - 1.0).abs() < 1e-9, "off ellipse: {position:?}");
                    }
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("ellipse must convert"),
        }
    }

    #[test]
    fn quarter_ellipse_arc_preserves_parametric_endpoints() {
        use acadrust::entities::Ellipse;
        use acadrust::types::Vector3;

        let mut ellipse = Ellipse::new();
        ellipse.major_axis = Vector3::new(4.0, 0.0, 0.0);
        ellipse.minor_axis_ratio = 0.5;
        ellipse.start_parameter = 0.0;
        ellipse.end_parameter = std::f64::consts::PI / 2.0;
        match convert_entity(&EntityType::Ellipse(ellipse), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                GeometryValue::LineString { coordinates } => {
                    let first = coordinates.first().expect("start");
                    let last = coordinates.last().expect("end");
                    assert!((first[0] - 4.0).abs() < 1e-9 && first[1].abs() < 1e-9);
                    assert!(last[0].abs() < 1e-9 && (last[1] - 2.0).abs() < 1e-9);
                }
                other => panic!("expected LineString, got {other:?}"),
            },
            _ => panic!("ellipse arc must convert"),
        }
    }

    #[test]
    fn clamped_cubic_spline_through_collinear_controls_stays_on_the_line() {
        use acadrust::entities::Spline;
        use acadrust::types::Vector3;

        let mut spline = Spline::new();
        spline.degree = 3;
        spline.control_points = vec![
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(3.0, 3.0, 0.0),
        ];
        spline.knots = vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        match convert_entity(&EntityType::Spline(spline), &opts(false)) {
            EntityOutcome::Converted {
                geometry, warnings, ..
            } => {
                match geometry {
                    GeometryValue::LineString { coordinates } => {
                        let first = coordinates.first().expect("start");
                        let last = coordinates.last().expect("end");
                        assert!(first[0].abs() < 1e-9 && first[1].abs() < 1e-9);
                        assert!((last[0] - 3.0).abs() < 1e-9 && (last[1] - 3.0).abs() < 1e-9);
                        for position in &coordinates {
                            assert!(
                                (position[0] - position[1]).abs() < 1e-9,
                                "off line: {position:?}"
                            );
                        }
                    }
                    other => panic!("expected LineString, got {other:?}"),
                }
                assert!(warnings.iter().any(|w| w.contains("spline sampled")));
            }
            _ => panic!("spline must convert"),
        }
    }

    #[test]
    fn invalid_spline_falls_back_to_fit_points_or_skips() {
        use acadrust::entities::Spline;
        use acadrust::types::Vector3;

        let mut with_fit = Spline::new();
        with_fit.degree = 3;
        with_fit.fit_points = vec![Vector3::new(0.0, 0.0, 0.0), Vector3::new(5.0, 5.0, 0.0)];
        match convert_entity(&EntityType::Spline(with_fit), &opts(false)) {
            EntityOutcome::Converted { warnings, .. } => {
                assert!(warnings.iter().any(|w| w.contains("fit points")));
            }
            _ => panic!("spline with fit points must fall back, not vanish"),
        }

        let empty = Spline::new();
        match convert_entity(&EntityType::Spline(empty), &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("NURBS")),
            _ => panic!("empty spline must be skipped"),
        }
    }

    #[test]
    fn text_becomes_point_with_text_properties() {
        use acadrust::entities::Text;
        use acadrust::types::Vector3;

        let mut text = Text::new();
        text.value = "COTA 123".to_string();
        text.insertion_point = Vector3::new(7.0, 8.0, 0.0);
        text.height = 2.5;
        text.rotation = std::f64::consts::PI / 2.0;
        match convert_entity(&EntityType::Text(text), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                ..
            } => {
                assert_eq!(geometry, GeometryValue::new_point((7.0, 8.0)));
                let get = |key: &str| {
                    extra_properties
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.clone())
                };
                assert_eq!(get("text"), Some(serde_json::json!("COTA 123")));
                assert_eq!(get("text_height"), Some(serde_json::json!(2.5)));
                assert_eq!(get("text_rotation_deg"), Some(serde_json::json!(90.0)));
            }
            _ => panic!("text must convert"),
        }
    }

    #[test]
    fn mtext_strips_format_codes_and_keeps_raw() {
        use acadrust::entities::MText;
        use acadrust::types::Vector3;

        let mut mtext = MText::new();
        mtext.value = r"{\fArial|b0;CORREDOR} SUL\Ptrecho \S1^2; km".to_string();
        mtext.insertion_point = Vector3::new(1.0, 2.0, 0.0);
        match convert_entity(&EntityType::MText(mtext), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                ..
            } => {
                assert_eq!(geometry, GeometryValue::new_point((1.0, 2.0)));
                let text = extra_properties
                    .iter()
                    .find(|(k, _)| *k == "text")
                    .map(|(_, v)| v.as_str().expect("string").to_string())
                    .expect("text property");
                assert_eq!(text, "CORREDOR SUL\ntrecho 1/2 km");
                assert!(
                    extra_properties.iter().any(|(k, _)| *k == "text_raw"),
                    "raw value must be preserved when stripping changed it"
                );
            }
            _ => panic!("mtext must convert"),
        }
    }

    #[test]
    fn degenerate_circle_is_skipped() {
        let circle = EntityType::Circle(Circle::from_coords(0.0, 0.0, 0.0, 0.0));
        match convert_entity(&circle, &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("radius")),
            _ => panic!("zero-radius circle must be skipped"),
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
            .add_entity(EntityType::Solid(acadrust::entities::Solid::new(
                acadrust::types::Vector3::new(0.0, 0.0, 0.0),
                acadrust::types::Vector3::new(1.0, 0.0, 0.0),
                acadrust::types::Vector3::new(0.0, 1.0, 0.0),
                acadrust::types::Vector3::new(1.0, 1.0, 0.0),
            )))
            .expect("add solid");
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
        assert_eq!(skipped[0].0, "SOLID");
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

    /// Register a block definition and route its entities to the new record.
    fn add_block(
        document: &mut CadDocument,
        name: &str,
        base_point: acadrust::types::Vector3,
        entities: Vec<EntityType>,
    ) {
        let mut record = acadrust::BlockRecord::new(name);
        record.handle = document.allocate_handle();
        record.base_point = base_point;
        let owner = record.handle;
        document
            .block_records
            .add(record)
            .expect("add block record");
        for mut entity in entities {
            entity.common_mut().owner_handle = owner;
            document.add_entity(entity).expect("add block entity");
        }
    }

    fn string_id(feature: &geojson::Feature) -> String {
        match &feature.id {
            Some(geojson::feature::Id::String(id)) => id.clone(),
            other => panic!("expected string feature id, got {other:?}"),
        }
    }

    #[test]
    fn insert_expands_block_geometry_with_scale_rotation_translation() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "PART",
            Vector3::ZERO,
            vec![EntityType::Line(Line::from_coords(
                0.0, 0.0, 0.0, 1.0, 0.0, 0.0,
            ))],
        );

        let mut insert = Insert::new("PART", Vector3::new(10.0, 0.0, 0.0));
        insert.set_x_scale(2.0);
        insert.set_y_scale(2.0);
        insert.set_z_scale(2.0);
        insert.rotation = std::f64::consts::FRAC_PI_2;
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.inserts_expanded, 1);
        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.converted.get("LINE"), Some(&1));
        let feature = &extraction.features[0];
        let geometry = feature.geometry.as_ref().expect("geometry");
        let GeometryValue::LineString { coordinates } = &geometry.value else {
            panic!("expected LineString, got {:?}", geometry.value);
        };
        // (0,0) maps to the insertion point; (1,0) scales to (2,0), rotates
        // 90 degrees to (0,2), and shifts to (10,2).
        assert!((coordinates[0][0] - 10.0).abs() < 1e-9 && coordinates[0][1].abs() < 1e-9);
        assert!((coordinates[1][0] - 10.0).abs() < 1e-9 && (coordinates[1][1] - 2.0).abs() < 1e-9);
        let properties = feature.properties.as_ref().expect("properties");
        assert_eq!(properties.get("block_path"), Some(&JsonValue::from("PART")));
        let id = string_id(feature);
        assert!(
            id.contains('/'),
            "id must be prefixed by the insert chain: {id}"
        );
    }

    #[test]
    fn nested_inserts_compose_transforms_and_block_paths() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "INNER",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(1.0, 0.0, 0.0))],
        );
        add_block(
            &mut document,
            "OUTER",
            Vector3::ZERO,
            vec![EntityType::Insert(Insert::new(
                "INNER",
                Vector3::new(2.0, 0.0, 0.0),
            ))],
        );
        document
            .add_entity(EntityType::Insert(Insert::new(
                "OUTER",
                Vector3::new(10.0, 0.0, 0.0),
            )))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.inserts_expanded, 2);
        assert_eq!(extraction.features.len(), 1);
        let feature = &extraction.features[0];
        let geometry = feature.geometry.as_ref().expect("geometry");
        assert_eq!(geometry.value, GeometryValue::new_point((13.0, 0.0)));
        let properties = feature.properties.as_ref().expect("properties");
        assert_eq!(
            properties.get("block_path"),
            Some(&JsonValue::from("OUTER/INNER"))
        );
    }

    #[test]
    fn layer_zero_block_content_inherits_the_insert_layer() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "SYM",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(1.0, 1.0, 0.0))],
        );

        let mut insert = EntityType::Insert(Insert::new("SYM", Vector3::ZERO));
        insert.common_mut().layer = "PIPES".to_string();
        document.add_entity(insert).expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.features.len(), 1);
        let properties = extraction.features[0]
            .properties
            .as_ref()
            .expect("properties");
        assert_eq!(properties.get("layer"), Some(&JsonValue::from("PIPES")));
        assert_eq!(properties.get("source_layer"), Some(&JsonValue::from("0")));
    }

    #[test]
    fn preserve_inserts_emits_anchor_points_with_attributes() {
        use acadrust::entities::{AttributeEntity, Insert};
        use acadrust::types::Vector3;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "SYM",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(1.0, 1.0, 0.0))],
        );

        let mut insert = Insert::new("SYM", Vector3::new(5.0, 6.0, 0.0));
        insert
            .attributes
            .push(AttributeEntity::new("TAG".to_string(), "V1".to_string()));
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let mut options = opts(false);
        options.preserve_inserts = true;
        let extraction = extract(&document, &options).expect("extract");

        assert_eq!(extraction.inserts_expanded, 0);
        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.converted.get("INSERT"), Some(&1));
        let feature = &extraction.features[0];
        let geometry = feature.geometry.as_ref().expect("geometry");
        assert_eq!(geometry.value, GeometryValue::new_point((5.0, 6.0)));
        let properties = feature.properties.as_ref().expect("properties");
        assert_eq!(properties.get("block_name"), Some(&JsonValue::from("SYM")));
        let attributes = properties.get("attributes").expect("attributes");
        assert_eq!(attributes.get("TAG"), Some(&JsonValue::from("V1")));
    }

    #[test]
    fn recursive_block_references_are_failed_not_looped() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "LOOP",
            Vector3::ZERO,
            vec![EntityType::Insert(Insert::new("LOOP", Vector3::ZERO))],
        );
        document
            .add_entity(EntityType::Insert(Insert::new("LOOP", Vector3::ZERO)))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert!(extraction.features.is_empty());
        assert!(
            extraction.failed.keys().any(
                |(entity_type, reason)| entity_type == "INSERT" && reason.contains("recursive")
            ),
            "failed outcomes: {:?}",
            extraction.failed.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_block_definition_is_a_failed_insert() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        document
            .add_entity(EntityType::Insert(Insert::new("GHOST", Vector3::ZERO)))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert!(extraction.features.is_empty());
        assert!(
            extraction
                .failed
                .keys()
                .any(|(entity_type, reason)| entity_type == "INSERT"
                    && reason.contains("missing block definition")),
            "failed outcomes: {:?}",
            extraction.failed.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn minsert_grids_emit_one_feature_per_cell() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "CELL",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(0.0, 0.0, 0.0))],
        );

        let mut insert = Insert::new("CELL", Vector3::ZERO);
        insert.column_count = 2;
        insert.row_count = 1;
        insert.column_spacing = 5.0;
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.features.len(), 2);
        let points: Vec<_> = extraction
            .features
            .iter()
            .map(|feature| feature.geometry.as_ref().expect("geometry").value.clone())
            .collect();
        assert_eq!(points[0], GeometryValue::new_point((0.0, 0.0)));
        assert_eq!(points[1], GeometryValue::new_point((5.0, 0.0)));
        let ids: Vec<String> = extraction.features.iter().map(string_id).collect();
        assert!(
            ids[0].contains("[0,0]") && ids[1].contains("[0,1]"),
            "{ids:?}"
        );
        assert_ne!(ids[0], ids[1]);
    }

    #[test]
    fn bylayer_styles_resolve_from_the_layer_table() {
        use acadrust::Layer;
        use acadrust::types::Color;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut layer = Layer::new("PIPES");
        layer.color = Color::GREEN;
        layer.line_type = "CENTER".to_string();
        document.layers.add(layer).expect("add layer");

        let mut point = EntityType::Point(Point::from_coords(1.0, 1.0, 0.0));
        point.common_mut().layer = "PIPES".to_string();
        document.add_entity(point).expect("add point");

        let extraction = extract(&document, &opts(false)).expect("extract");

        let properties = extraction.features[0]
            .properties
            .as_ref()
            .expect("properties");
        assert_eq!(properties.get("color_index"), Some(&JsonValue::from(3)));
        assert_eq!(
            properties.get("color_rgb"),
            Some(&JsonValue::from("#00FF00"))
        );
        assert_eq!(properties.get("linetype"), Some(&JsonValue::from("CENTER")));
        assert!(properties.get("color").is_none());
    }

    #[test]
    fn byblock_styles_resolve_through_the_insert_chain() {
        use acadrust::entities::Insert;
        use acadrust::types::{Color, Vector3};
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut block_point = EntityType::Point(Point::from_coords(0.0, 0.0, 0.0));
        block_point.common_mut().color = Color::ByBlock;
        block_point.common_mut().linetype = "BYBLOCK".to_string();
        add_block(&mut document, "SYM", Vector3::ZERO, vec![block_point]);

        let mut insert = EntityType::Insert(Insert::new("SYM", Vector3::ZERO));
        insert.common_mut().color = Color::RED;
        insert.common_mut().linetype = "DASHED".to_string();
        document.add_entity(insert).expect("add insert");

        // A second ByBlock point directly in model space stays unresolved.
        let mut loose_point = EntityType::Point(Point::from_coords(9.0, 9.0, 0.0));
        loose_point.common_mut().color = Color::ByBlock;
        loose_point.common_mut().linetype = "ByBlock".to_string();
        document.add_entity(loose_point).expect("add point");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.features.len(), 2);
        let block_properties = extraction.features[0]
            .properties
            .as_ref()
            .expect("properties");
        assert_eq!(
            block_properties.get("color_index"),
            Some(&JsonValue::from(1))
        );
        assert_eq!(
            block_properties.get("linetype"),
            Some(&JsonValue::from("DASHED"))
        );
        let loose_properties = extraction.features[1]
            .properties
            .as_ref()
            .expect("properties");
        assert_eq!(
            loose_properties.get("color"),
            Some(&JsonValue::from("ByBlock"))
        );
        assert_eq!(
            loose_properties.get("linetype"),
            Some(&JsonValue::from("ByBlock"))
        );
    }

    #[test]
    fn text_rotation_composes_with_the_insert_rotation() {
        use acadrust::entities::{Insert, Text};
        use acadrust::types::Vector3;
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut text = Text::new();
        text.value = "LABEL".to_string();
        text.insertion_point = Vector3::ZERO;
        text.rotation = std::f64::consts::FRAC_PI_6; // 30 degrees
        add_block(
            &mut document,
            "LBL",
            Vector3::ZERO,
            vec![EntityType::Text(text)],
        );

        let mut insert = Insert::new("LBL", Vector3::ZERO);
        insert.rotation = std::f64::consts::FRAC_PI_2; // +90 degrees
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        let properties = extraction.features[0]
            .properties
            .as_ref()
            .expect("properties");
        let Some(JsonValue::Number(rotation)) = properties.get("text_rotation_deg") else {
            panic!("text_rotation_deg must be present");
        };
        let rotation = rotation.as_f64().expect("finite rotation");
        assert!(
            (rotation - 120.0).abs() < 1e-9,
            "expected 120 degrees, got {rotation}"
        );
    }
}
