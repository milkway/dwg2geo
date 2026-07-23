//! Native DWG -> GeoJSON conversion (Milestone 3).
//!
//! Converts model-space geometry (points, lines, polylines with bulge arcs,
//! arcs, circles, ellipses, splines, and text anchors) in raw drawing
//! coordinates; reprojection arrives with the `native-reproject` feature in
//! Milestone 5. Every entity that is not converted is counted in the report
//! with a reason — nothing is silently dropped. Output is deterministic:
//! features follow model-space document order, identifiers come from entity
//! handles, and all curve approximation uses pure arithmetic on the inputs.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufWriter, Write},
    time::Instant,
};

use acadrust::{
    CadDocument,
    entities::EntityType,
    types::{Color, LineWeight, Matrix3, Vector3},
};
use anyhow::{Context, Result, bail};
use geojson::{Feature, FeatureCollection, GeometryValue, JsonObject, JsonValue};

use super::model::{CadFeature, CadGeometry};
#[cfg(feature = "native-reproject")]
use super::reproject::Reprojector;
#[cfg(feature = "native-reproject")]
use super::units;
use super::{ReadMode, read_document, writer};
use crate::{
    backend::{
        ConvertRequest, OutputFormat, append_suffix, check_output_collision,
        ensure_nonempty_output, ensure_parent_directory, remove_stale, validate_input,
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

/// Angular tolerance distinguishing a zero-length sweep from a full turn.
const ANGLE_EPSILON: f64 = 1e-12;

/// CCW sweep for an arc-family span. `None` for a zero-length span; raw
/// spans at (multiples of) a full turn normalize to exactly 2 pi instead of
/// collapsing to zero, and sub-epsilon spans are never promoted to full
/// revolutions.
fn ccw_sweep(start_angle: f64, end_angle: f64) -> Option<f64> {
    let raw = end_angle - start_angle;
    if raw.abs() <= ANGLE_EPSILON {
        return None;
    }
    let mut sweep = raw.rem_euclid(std::f64::consts::TAU);
    if sweep <= ANGLE_EPSILON || sweep >= std::f64::consts::TAU - ANGLE_EPSILON {
        sweep = std::f64::consts::TAU;
    }
    Some(sweep)
}

/// Hard cap on nested INSERT expansion depth.
const MAX_BLOCK_DEPTH: usize = 16;

/// Geometry-mapping options resolved from the CLI.
struct GeometryOptions {
    polygonize_closed: bool,
    curve_tolerance: f64,
    preserve_inserts: bool,
    /// Lowercased layer names; when non-empty, only these layers convert.
    include_layers: Vec<String>,
    /// Lowercased layer names excluded from conversion.
    exclude_layers: Vec<String>,
}

impl GeometryOptions {
    /// Whether a top-level entity on `layer` passes the layer filters
    /// (case-insensitive, matching the GDAL route's semantics).
    fn layer_passes(&self, layer: &str) -> bool {
        let normalized = layer.to_ascii_lowercase();
        if !self.include_layers.is_empty() {
            return self.include_layers.contains(&normalized);
        }
        !self.exclude_layers.contains(&normalized)
    }

    fn has_layer_filters(&self) -> bool {
        !self.include_layers.is_empty() || !self.exclude_layers.is_empty()
    }
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
    /// Resolved line weight (mm) of the enclosing insert, substituted for
    /// ByBlock entity weights; None outside block content or when unresolvable.
    inherited_lineweight: Option<f64>,
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
            inherited_lineweight: None,
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

/// Resolve an entity's line weight to millimetres, following ByLayer to the
/// layer table and ByBlock to the enclosing insert (like color/linetype).
/// Default and unresolvable weights return None so the renderer falls back to
/// its own default width. Values are stored in 1/100 mm.
fn resolve_lineweight_mm(
    document: &CadDocument,
    line_weight: LineWeight,
    layer: &str,
    placement: &Placement,
) -> Option<f64> {
    let raw = match line_weight {
        LineWeight::Value(v) => v,
        LineWeight::ByLayer => match document.layers.get(layer).map(|entry| entry.line_weight) {
            Some(LineWeight::Value(v)) => v,
            _ => return None,
        },
        LineWeight::ByBlock => return placement.inherited_lineweight,
        LineWeight::Default => return None,
    };
    (raw >= 0).then(|| f64::from(raw) / 100.0)
}

/// Style properties for a feature: resolved color (ACI index and/or RGB),
/// linetype, and line weight in mm. Unresolvable policies are emitted verbatim
/// ("ByLayer" or "ByBlock") instead of being dropped.
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
    if let Some(mm) = resolve_lineweight_mm(document, common.line_weight, layer, placement) {
        properties.push(("lineweight_mm", JsonValue::from(mm)));
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
    // DXF stores the INSERT insertion point in OCS (group 10); lift it
    // through the arbitrary-axis transform before translating.
    let ocs_to_wcs = Matrix3::arbitrary_axis(insert.normal);
    let to_insertion = Affine::from_translation(ocs_to_wcs.transform_point(insert.insert_point));
    let orient = Affine::from_linear(ocs_to_wcs.m);
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

    #[cfg(not(feature = "native-reproject"))]
    if let Some(source_crs) = request.source_crs {
        bail!(
            "this binary was built without reprojection support and cannot transform from {source_crs}; rebuild with --features native-reproject, use --backend external, or pass --allow-local-coordinates for raw drawing coordinates"
        );
    }
    if !request.allow_local_coordinates
        && request.source_crs.is_none()
        && request.control_points.is_empty()
    {
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
    if request.allow_local_coordinates {
        warnings.push(
            "output uses raw drawing coordinates; no geographic CRS was established".to_string(),
        );
    }
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

    // Resolve units, PROJ transforms, and calibrations before extraction so
    // a bad CRS, ambiguous units, or bad control points fail fast.
    #[cfg(feature = "native-reproject")]
    let reprojection_plan = match request.source_crs {
        Some(source_crs) => Some(build_reprojection_plan(
            request,
            source_crs,
            &document,
            &mut warnings,
        )?),
        None => None,
    };
    let calibration_plan = if request.control_points.is_empty() {
        None
    } else {
        Some(build_calibration_plan(request)?)
    };

    let geometry_options = GeometryOptions {
        polygonize_closed: request.polygonize_closed,
        curve_tolerance: request.curve_tolerance.unwrap_or(DEFAULT_CURVE_TOLERANCE),
        preserve_inserts: request.preserve_inserts,
        include_layers: request
            .include_layers
            .iter()
            .map(|layer| layer.to_ascii_lowercase())
            .collect(),
        exclude_layers: request
            .exclude_layers
            .iter()
            .map(|layer| layer.to_ascii_lowercase())
            .collect(),
    };

    // Per-feature pipeline (model -> stats -> transform -> checks -> write).
    // The geojson-seq route streams every feature to disk immediately, so
    // memory stays bounded by the parsed document rather than the feature
    // count; the pretty FeatureCollection route must hold the features to
    // emit one JSON document.
    let partial = append_suffix(request.output, ".partial");
    remove_stale(&partial)?;

    let boundary_index = match request.validate_boundary {
        Some(path) => Some(BoundaryIndex::load(path)?),
        None => None,
    };
    let mut boundary_tally = BoundaryTally::default();

    let georeferenced = request.source_crs.is_some() || calibration_plan.is_some();
    let enforce_wgs84_extents =
        georeferenced && crs_is_wgs84(request.target_crs) && !request.allow_suspect_extents;

    let mut collected: Vec<Feature> = Vec::new();
    let mut seq_writer = match request.output_format {
        OutputFormat::GeoJsonSeq => Some(BufWriter::new(
            fs::File::create(&partial)
                .with_context(|| format!("cannot write output {}", partial.display()))?,
        )),
        OutputFormat::GeoJson => None,
    };

    let mut bbox_drawing: Option<[f64; 4]> = None;
    let mut bbox_output: Option<[f64; 4]> = None;
    let mut geometry_checks = empty_geometry_checks();
    // The spatial-outlier scan needs a global median, so a lightweight
    // per-feature center is retained (this is the only O(feature-count)
    // retention on the streaming path — the features themselves are written
    // and dropped). Ids are kept only to name up to ten outlier samples.
    let mut centers: Vec<(String, f64, f64)> = Vec::new();
    let mut features_written = 0usize;
    // Transforms are interleaved with extraction in the streaming pipeline;
    // accumulate their time so the report still shows them as distinct
    // audit-trail steps rather than folding them into extraction.
    // `reproject_elapsed` is only mutated on the native-reproject path.
    #[cfg_attr(not(feature = "native-reproject"), allow(unused_mut))]
    let mut reproject_elapsed = std::time::Duration::ZERO;
    let mut calibrate_elapsed = std::time::Duration::ZERO;

    let extract_started = Instant::now();
    let extraction = {
        let mut sink = |mut feature: CadFeature| -> Result<()> {
            accumulate_bbox(&mut bbox_drawing, &feature);
            if let Some(center) = feature_center(&feature) {
                centers.push((feature.id.clone(), center.0, center.1));
            }
            #[cfg(feature = "native-reproject")]
            if let Some(plan) = &reprojection_plan {
                let reprojector = &plan.reprojector;
                let started = Instant::now();
                feature
                    .geometry
                    .transform(&|x, y| reprojector.transform(x, y))
                    .with_context(|| format!("while reprojecting feature {}", feature.id))?;
                reproject_elapsed += started.elapsed();
            }
            if let Some((calibration, _)) = &calibration_plan {
                let started = Instant::now();
                feature
                    .geometry
                    .transform(&|x, y| Ok(calibration.apply((x, y))))
                    .with_context(|| format!("while calibrating feature {}", feature.id))?;
                calibrate_elapsed += started.elapsed();
            }
            if enforce_wgs84_extents {
                if let Some((x, y)) = wgs84_violation(&feature) {
                    bail!(
                        "output contains implausible WGS 84 coordinates (feature {}: {x}, {y}); the source CRS, --source-units, or control points are probably wrong for this drawing. Pass --allow-suspect-extents to deliver anyway",
                        feature.id
                    );
                }
            }
            accumulate_bbox(&mut bbox_output, &feature);
            update_geometry_checks(&mut geometry_checks, &feature);
            if let Some(index) = &boundary_index {
                index.classify(&feature, &mut boundary_tally);
            }
            match &mut seq_writer {
                Some(writer_handle) => {
                    serde_json::to_writer(&mut *writer_handle, &writer::to_geojson(&feature))
                        .context("cannot serialize GeoJSONSeq feature")?;
                    writer_handle
                        .write_all(b"\n")
                        .with_context(|| format!("cannot write output {}", partial.display()))?;
                }
                None => collected.push(writer::to_geojson(&feature)),
            }
            features_written += 1;
            Ok(())
        };
        match extract_with_sink(&document, &geometry_options, &mut sink) {
            Ok(extraction) => extraction,
            Err(error) => {
                let _ = fs::remove_file(&partial);
                return Err(error);
            }
        }
    };
    // The interleaved transform time is reported as its own step, so
    // subtract it from the extraction total to keep the audit trail honest.
    let total_elapsed = extract_started.elapsed();
    let extraction_only = total_elapsed
        .saturating_sub(reproject_elapsed)
        .saturating_sub(calibrate_elapsed);
    steps.push(Step {
        purpose: "entity extraction and GeoJSON mapping".to_string(),
        command: "(in-process converter)".to_string(),
        duration_ms: extraction_only.as_millis() as u64,
    });
    #[cfg(feature = "native-reproject")]
    if let Some(plan) = &reprojection_plan {
        steps.push(Step {
            purpose: "coordinate reprojection".to_string(),
            command: format!("(in-process PROJ {})", plan.info.proj_version),
            duration_ms: reproject_elapsed.as_millis() as u64,
        });
    }
    if calibration_plan.is_some() {
        steps.push(Step {
            purpose: "control-point calibration".to_string(),
            command: "(in-process similarity transform)".to_string(),
            duration_ms: calibrate_elapsed.as_millis() as u64,
        });
    }

    if features_written == 0 {
        warnings.push(
            "no features were converted; see the native section of the report for reasons"
                .to_string(),
        );
    }

    #[cfg(feature = "native-reproject")]
    let reprojection_info = reprojection_plan.map(|plan| plan.info);
    #[cfg(not(feature = "native-reproject"))]
    let reprojection_info: Option<report::ReprojectionInfo> = None;
    let calibration_info = calibration_plan.map(|(_, info)| info);

    let spatial_outliers = finalize_spatial_outliers(&centers);
    drop(centers);
    let accounting = report::AccountingInfo {
        model_space_entities: extraction.model_space_entities,
        top_level_accounted: extraction.top_level_accounted,
        unaccounted: extraction
            .model_space_entities
            .saturating_sub(extraction.top_level_accounted),
    };
    if accounting.unaccounted != 0 {
        warnings.push(format!(
            "{} model-space entities did not reach any conversion outcome; this is a converter bug — please report it",
            accounting.unaccounted
        ));
    }
    let invariant_violations = geometry_checks.empty_geometries
        + geometry_checks.non_finite_coordinates
        + geometry_checks.unclosed_rings
        + geometry_checks.misoriented_rings
        + geometry_checks.degenerate_rings;
    if invariant_violations != 0 {
        warnings.push(format!(
            "{invariant_violations} geometry validity violations in the output (see the report's geometry_checks); this is a converter bug — please report it"
        ));
    }
    if spatial_outliers.outlier_features != 0 {
        warnings.push(format!(
            "{} of {} features lie far from the main coordinate cluster (see the report's spatial_outliers); title blocks and legends drawn in model space are the typical cause",
            spatial_outliers.outlier_features, spatial_outliers.features_checked
        ));
    }
    let boundary_check = boundary_index.map(|index| index.into_report(boundary_tally));
    if let Some(check) = &boundary_check {
        let not_inside = check.features_partial + check.features_outside;
        if not_inside != 0 {
            warnings.push(format!(
                "{not_inside} of {features_written} features are not fully inside the reference boundary {} (see the report's boundary_check)",
                check.boundary_path
            ));
        }
    }

    let write_result = match request.output_format {
        OutputFormat::GeoJson => {
            let collection = FeatureCollection {
                bbox: None,
                features: collected,
                foreign_members: Some(foreign_members(
                    &source,
                    reprojection_info.as_ref(),
                    calibration_info.as_ref(),
                )),
            };
            serde_json::to_string_pretty(&collection)
                .context("cannot serialize GeoJSON feature collection")
                .and_then(|mut json| {
                    json.push('\n');
                    fs::write(&partial, json)
                        .with_context(|| format!("cannot write output {}", partial.display()))
                })
        }
        OutputFormat::GeoJsonSeq => seq_writer
            .take()
            .expect("seq writer exists in geojson-seq mode")
            .flush()
            .with_context(|| format!("cannot write output {}", partial.display())),
    };
    if let Err(error) = write_result.and_then(|()| ensure_nonempty_output(&partial)) {
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
            source_crs: request.source_crs.map(str::to_string),
            target_crs: (request.source_crs.is_some() || !request.control_points.is_empty())
                .then(|| request.target_crs.to_string()),
            allow_local_coordinates: request.allow_local_coordinates,
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
            output_format: Some(request.output_format.to_string()),
            source_units: request.source_units.map(str::to_string),
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
                by_layer_filter: extraction.excluded_by_layer_filter,
            },
            feature_warnings: extraction.feature_warnings,
            reprojection: reprojection_info,
            calibration: calibration_info,
            accounting,
            bbox_drawing,
            bbox_output,
            geometry_checks,
            spatial_outliers,
            boundary_check,
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

/// GeoJSON foreign members recording the coordinate status: either the
/// local-coordinates marker (RFC 7946 output is otherwise assumed WGS 84) or
/// the reprojection provenance.
fn foreign_members(
    source: &dwg::DwgInfo,
    reprojection: Option<&report::ReprojectionInfo>,
    calibration: Option<&report::CalibrationInfo>,
) -> JsonObject {
    let mut members = JsonObject::new();
    let payload = match (reprojection, calibration) {
        (None, Some(info)) => serde_json::json!({
            "coordinate_status": "calibrated",
            "target_crs": info.target_crs,
            "control_points": info.control_points,
            "rms_error": info.rms_error,
            "note": "coordinates were georeferenced by a local similarity calibration; accuracy is bounded by the control points",
            "source_sha256": source.sha256,
        }),
        (None, None) => serde_json::json!({
            "coordinate_status": "local-unreferenced",
            "note": "coordinates are raw drawing units; no geographic CRS was established",
            "source_sha256": source.sha256,
        }),
        (Some(info), _) => {
            let note = if crs_is_wgs84(&info.target_crs) {
                "coordinates are WGS 84 longitude/latitude per RFC 7946"
            } else {
                "coordinates are NOT WGS 84; RFC 7946 consumers must handle the recorded CRS explicitly"
            };
            serde_json::json!({
                "coordinate_status": "georeferenced",
                "source_crs": info.source_crs,
                "target_crs": info.target_crs,
                "axis_order": info.axis_order,
                "note": note,
                "source_sha256": source.sha256,
            })
        }
    };
    members.insert("dwg2geo".to_string(), payload);
    members
}

/// Whether a CRS string names WGS 84 longitude/latitude as GeoJSON uses it.
fn crs_is_wgs84(crs: &str) -> bool {
    let normalized = crs.trim().to_ascii_uppercase();
    normalized == "EPSG:4326" || normalized == "OGC:CRS84" || normalized == "CRS84"
}

/// First coordinate of one feature outside the plausible WGS 84
/// longitude/latitude range.
fn wgs84_violation(feature: &CadFeature) -> Option<(f64, f64)> {
    let mut offending = None;
    feature.geometry.visit_positions(&mut |x, y| {
        if offending.is_none() && (!(-180.0..=180.0).contains(&x) || !(-90.0..=90.0).contains(&y)) {
            offending = Some((x, y));
        }
    });
    offending
}

/// Deviation factor for the robust outlier scan: a feature is an outlier
/// when its center is more than this many median-absolute-deviations from
/// the median center on either axis.
const OUTLIER_MAD_FACTOR: f64 = 100.0;

/// Median of a list (average of the two middle values for even counts).
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).expect("finite values"));
    let count = values.len();
    if count % 2 == 1 {
        values[count / 2]
    } else {
        (values[count / 2 - 1] + values[count / 2]) / 2.0
    }
}

/// The feature's finite bbox center, for the outlier scan.
fn feature_center(feature: &CadFeature) -> Option<(f64, f64)> {
    let mut bbox: Option<[f64; 4]> = None;
    feature.geometry.visit_positions(&mut |x, y| {
        if x.is_finite() && y.is_finite() {
            bbox = Some(match bbox {
                None => [x, y, x, y],
                Some([min_x, min_y, max_x, max_y]) => {
                    [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
                }
            });
        }
    });
    bbox.map(|[min_x, min_y, max_x, max_y]| ((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
}

/// Robust scan over the collected (id, center) pairs; see
/// [`report::SpatialOutliers`]. Informational only.
fn finalize_spatial_outliers(centers: &[(String, f64, f64)]) -> report::SpatialOutliers {
    if centers.is_empty() {
        return report::SpatialOutliers {
            features_checked: 0,
            outlier_features: 0,
            center: [0.0, 0.0],
            axis_thresholds: [0.0, 0.0],
            sample_ids: Vec::new(),
        };
    }

    let mut xs: Vec<f64> = centers.iter().map(|(_, x, _)| *x).collect();
    let mut ys: Vec<f64> = centers.iter().map(|(_, _, y)| *y).collect();
    let median_x = median(&mut xs);
    let median_y = median(&mut ys);
    let mut deviations_x: Vec<f64> = centers
        .iter()
        .map(|(_, x, _)| (x - median_x).abs())
        .collect();
    let mut deviations_y: Vec<f64> = centers
        .iter()
        .map(|(_, _, y)| (y - median_y).abs())
        .collect();
    // Floor the thresholds so a zero-MAD (tightly clustered) drawing does
    // not flag float- or millimeter-scale neighbors: never call anything
    // closer than one drawing unit — or the coordinate magnitude's own
    // float-noise scale — an outlier.
    let magnitude = median_x.abs().max(median_y.abs());
    let floor = (1e-6 * magnitude).max(1.0);
    let threshold_x = (OUTLIER_MAD_FACTOR * median(&mut deviations_x)).max(floor);
    let threshold_y = (OUTLIER_MAD_FACTOR * median(&mut deviations_y)).max(floor);

    let mut outliers = 0usize;
    let mut sample_ids = Vec::new();
    for (id, x, y) in centers {
        if (x - median_x).abs() > threshold_x || (y - median_y).abs() > threshold_y {
            outliers += 1;
            if sample_ids.len() < MAX_HANDLE_SAMPLES {
                sample_ids.push(id.clone());
            }
        }
    }

    report::SpatialOutliers {
        features_checked: centers.len(),
        outlier_features: outliers,
        center: [median_x, median_y],
        axis_thresholds: [threshold_x, threshold_y],
        sample_ids,
    }
}

/// A reference boundary loaded from GeoJSON, validated once and reused for
/// every feature classification.
struct BoundaryIndex {
    path: String,
    polygons: Vec<Vec<Vec<(f64, f64)>>>,
}

/// Running containment tally over streamed features.
#[derive(Default)]
struct BoundaryTally {
    inside: usize,
    partial: usize,
    outside: usize,
    sample_not_inside_ids: Vec<String>,
}

impl BoundaryIndex {
    /// Load a boundary GeoJSON (Polygon/MultiPolygon in Feature,
    /// FeatureCollection, or bare Geometry form), rejecting malformed
    /// content loudly rather than panicking or classifying against garbage.
    fn load(path: &std::path::Path) -> Result<BoundaryIndex> {
        let text = fs::read_to_string(path)
            .with_context(|| format!("cannot read boundary file {}", path.display()))?;
        let boundary: geojson::GeoJson = text
            .parse()
            .with_context(|| format!("boundary file {} is not valid GeoJSON", path.display()))?;

        let convert_ring = |ring: &Vec<geojson::Position>| -> Result<Vec<(f64, f64)>> {
            let mut points = Vec::with_capacity(ring.len());
            for position in ring {
                if position.len() < 2 {
                    bail!("a boundary position has fewer than two coordinates");
                }
                let (x, y) = (position[0], position[1]);
                if !x.is_finite() || !y.is_finite() {
                    bail!("a boundary position is not finite");
                }
                points.push((x, y));
            }
            if points.len() < 4 {
                bail!("a boundary ring has fewer than four positions");
            }
            if points.first() != points.last() {
                bail!("a boundary ring is not closed (first and last positions differ)");
            }
            Ok(points)
        };

        let mut polygons: Vec<Vec<Vec<(f64, f64)>>> = Vec::new();
        let mut collect = |value: &GeometryValue| -> Result<()> {
            match value {
                GeometryValue::Polygon { coordinates } => {
                    if !coordinates.is_empty() {
                        polygons.push(
                            coordinates
                                .iter()
                                .map(&convert_ring)
                                .collect::<Result<Vec<_>>>()?,
                        );
                    }
                }
                GeometryValue::MultiPolygon { coordinates } => {
                    for polygon in coordinates {
                        if !polygon.is_empty() {
                            polygons.push(
                                polygon
                                    .iter()
                                    .map(&convert_ring)
                                    .collect::<Result<Vec<_>>>()?,
                            );
                        }
                    }
                }
                _ => {}
            }
            Ok(())
        };
        let collected: Result<()> = match &boundary {
            geojson::GeoJson::Geometry(geometry) => collect(&geometry.value),
            geojson::GeoJson::Feature(feature) => match &feature.geometry {
                Some(geometry) => collect(&geometry.value),
                None => Ok(()),
            },
            geojson::GeoJson::FeatureCollection(collection) => collection
                .features
                .iter()
                .filter_map(|feature| feature.geometry.as_ref())
                .try_for_each(|geometry| collect(&geometry.value)),
        };
        collected.with_context(|| format!("invalid boundary file {}", path.display()))?;
        if polygons.is_empty() {
            bail!(
                "boundary file {} contains no Polygon or MultiPolygon geometry",
                path.display()
            );
        }
        Ok(BoundaryIndex {
            path: path.display().to_string(),
            polygons,
        })
    }

    /// Even-odd containment of one point (holes honored, any polygon).
    fn contains(&self, x: f64, y: f64) -> bool {
        self.polygons.iter().any(|rings| {
            rings
                .iter()
                .filter(|ring| ring_contains_point(ring, (x, y)))
                .count()
                % 2
                == 1
        })
    }

    /// Classify one feature into the running tally.
    fn classify(&self, feature: &CadFeature, tally: &mut BoundaryTally) {
        let (mut in_count, mut out_count) = (0usize, 0usize);
        feature.geometry.visit_positions(&mut |x, y| {
            if self.contains(x, y) {
                in_count += 1;
            } else {
                out_count += 1;
            }
        });
        // A segment can leave and re-enter the boundary between two inside
        // vertices (concavities, holes); a proper crossing forces partial.
        let mut crossed = false;
        feature.geometry.visit_segments(&mut |a, b| {
            if !crossed {
                crossed = self.polygons.iter().any(|rings| {
                    rings.iter().any(|ring| {
                        ring.windows(2)
                            .any(|edge| segments_cross(a, b, edge[0], edge[1]))
                    })
                });
            }
        });
        let bucket = match (in_count, out_count, crossed) {
            (_, _, true) => &mut tally.partial,
            (_, 0, false) => &mut tally.inside,
            (0, _, false) => &mut tally.outside,
            _ => &mut tally.partial,
        };
        *bucket += 1;
        if (out_count > 0 || crossed) && tally.sample_not_inside_ids.len() < MAX_HANDLE_SAMPLES {
            tally.sample_not_inside_ids.push(feature.id.clone());
        }
    }

    fn into_report(self, tally: BoundaryTally) -> report::BoundaryCheck {
        report::BoundaryCheck {
            boundary_path: self.path,
            polygons: self.polygons.len(),
            features_inside: tally.inside,
            features_partial: tally.partial,
            features_outside: tally.outside,
            sample_not_inside_ids: tally.sample_not_inside_ids,
        }
    }
}

/// Fold one feature into a running bounding box.
fn accumulate_bbox(bbox: &mut Option<[f64; 4]>, feature: &CadFeature) {
    feature.geometry.visit_positions(&mut |x, y| {
        *bbox = Some(match *bbox {
            None => [x, y, x, y],
            Some([min_x, min_y, max_x, max_y]) => {
                [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
            }
        });
    });
}

/// Fold one feature into the output-side validity counters; see
/// [`report::GeometryChecks`].
fn update_geometry_checks(checks: &mut report::GeometryChecks, feature: &CadFeature) {
    checks.features_checked += 1;
    let mut positions = 0usize;
    feature.geometry.visit_positions(&mut |x, y| {
        positions += 1;
        if !x.is_finite() || !y.is_finite() {
            checks.non_finite_coordinates += 1;
        }
    });
    if positions == 0 {
        checks.empty_geometries += 1;
    }
    let mut duplicates = false;
    feature.geometry.visit_segments(&mut |a, b| {
        duplicates |= a == b;
    });
    if duplicates {
        checks.duplicate_vertex_features += 1;
    }
    for rings in feature.geometry.polygon_rings() {
        for (index, ring) in rings.iter().enumerate() {
            checks.rings_checked += 1;
            if ring.len() < 4 {
                checks.degenerate_rings += 1;
                continue;
            }
            if ring.first() != ring.last() {
                checks.unclosed_rings += 1;
            }
            let area = signed_area(ring);
            let first = ring[0];
            let extent = ring.iter().fold(0.0f64, |extent, point| {
                extent
                    .max((point.0 - first.0).abs())
                    .max((point.1 - first.1).abs())
            });
            // The threshold is on twice the area, matching the pre-model
            // batch check so degenerate-ring diagnostics stay stable.
            if !extent.is_finite() || 2.0 * area.abs() <= 1e-12 * extent * extent {
                checks.degenerate_rings += 1;
                continue;
            }
            let is_shell = index == 0;
            if is_shell != (area > 0.0) {
                checks.misoriented_rings += 1;
            }
        }
    }
}

fn empty_geometry_checks() -> report::GeometryChecks {
    report::GeometryChecks {
        features_checked: 0,
        empty_geometries: 0,
        non_finite_coordinates: 0,
        duplicate_vertex_features: 0,
        rings_checked: 0,
        unclosed_rings: 0,
        misoriented_rings: 0,
        degenerate_rings: 0,
    }
}

/// Strictly proper segment intersection (shared endpoints and collinear
/// touches do not count; boundary-inclusive vertex classification already
/// covers those).
fn segments_cross(a1: (f64, f64), a2: (f64, f64), b1: (f64, f64), b2: (f64, f64)) -> bool {
    let orient = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| -> f64 {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let d1 = orient(b1, b2, a1);
    let d2 = orient(b1, b2, a2);
    let d3 = orient(a1, a2, b1);
    let d4 = orient(a1, a2, b2);
    ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

#[cfg(feature = "native-reproject")]
struct ReprojectionPlan {
    reprojector: Reprojector,
    info: report::ReprojectionInfo,
}

#[cfg(feature = "native-reproject")]
fn build_reprojection_plan(
    request: &ConvertRequest<'_>,
    source_crs: &str,
    document: &CadDocument,
    warnings: &mut Vec<String>,
) -> Result<ReprojectionPlan> {
    let (unit, unit_source) = match request.source_units {
        Some(text) => {
            let unit = units::parse_override(text).map_err(|reason| anyhow::anyhow!(reason))?;
            (unit, "override")
        }
        None => {
            match units::from_header(document.header.insertion_units, document.header.measurement) {
                Ok(unit) => (unit, "header"),
                Err(reason) => bail!(
                    "cannot determine the drawing's linear unit: {reason}. Pass --source-units <UNIT> (m, mm, cm, dm, km, in, ft, usft) stating what one drawing unit means"
                ),
            }
        }
    };

    if unit_source == "header" {
        warnings.push(format!(
            "drawing unit {} taken from the DWG header; header unit hints are not authoritative for georeferencing — pass --source-units to override",
            unit.name
        ));
    }
    let reprojector = Reprojector::new(source_crs, request.target_crs, unit.meters_per_unit)?;
    if reprojector.crs_unit.is_angular {
        warnings.push(format!(
            "source CRS {source_crs} is geographic with angular unit {}; the drawing unit declaration {} is treated only as an explicit declaration to trust coordinates already in CRS units, so linear units are ignored and coordinate scale 1 is applied",
            reprojector.crs_unit.name, unit.name
        ));
    } else {
        warnings.push(format!(
            "source CRS {source_crs} horizontal unit resolved by PROJ as {}; applying coordinate scale {} CRS units per drawing unit ({})",
            reprojector.crs_unit.name, reprojector.coordinate_scale, unit.name
        ));
    }

    let info = report::ReprojectionInfo {
        source_crs: source_crs.to_string(),
        target_crs: request.target_crs.to_string(),
        drawing_unit: unit.name.to_string(),
        unit_source: unit_source.to_string(),
        meters_per_drawing_unit: unit.meters_per_unit,
        crs_unit: reprojector.crs_unit.name.clone(),
        coordinate_scale: reprojector.coordinate_scale,
        axis_order: super::reproject::AXIS_ORDER.to_string(),
        pipeline: reprojector.pipeline(),
        proj_version: reprojector.proj_version(),
    };
    Ok(ReprojectionPlan { reprojector, info })
}

/// Parse and solve the control-point calibration, producing the transform
/// and its report block.
fn build_calibration_plan(
    request: &ConvertRequest<'_>,
) -> Result<(super::calibrate::Calibration, report::CalibrationInfo)> {
    let points: Vec<super::calibrate::ControlPoint> = request
        .control_points
        .iter()
        .map(|text| super::calibrate::parse_control_point(text))
        .collect::<Result<_, String>>()
        .map_err(|reason| anyhow::anyhow!("invalid --control-point: {reason}"))?;
    let (calibration, quality) = super::calibrate::solve(&points)
        .map_err(|reason| anyhow::anyhow!("cannot calibrate from the control points: {reason}"))?;
    let info = report::CalibrationInfo {
        control_points: points.len(),
        scale: quality.scale,
        rotation_deg: quality.rotation_deg,
        translation: (calibration.tx, calibration.ty),
        residuals: quality.residuals,
        rms_error: quality.rms_error,
        max_error: quality.max_error,
        target_crs: request.target_crs.to_string(),
    };
    Ok((calibration, info))
}

#[derive(Default)]
struct HandleSamples {
    count: usize,
    samples: Vec<String>,
}

#[derive(Default)]
struct Extraction {
    /// Populated only by the collecting test wrapper [`extract`]; the
    /// streaming path hands features to a sink instead.
    #[cfg(test)]
    features: Vec<CadFeature>,
    converted: BTreeMap<String, usize>,
    skipped: BTreeMap<(String, String), HandleSamples>,
    failed: BTreeMap<(String, String), HandleSamples>,
    excluded_paper_space: usize,
    excluded_block_definitions: usize,
    excluded_unowned: usize,
    feature_warnings: usize,
    approximated_features: usize,
    inserts_expanded: usize,
    /// Top-level model-space entities dropped by --include/--exclude-layers.
    excluded_by_layer_filter: usize,
    /// Top-level model-space entities encountered.
    model_space_entities: usize,
    /// Top-level entities that reached an outcome; must equal
    /// `model_space_entities` (checked in the report accounting).
    top_level_accounted: usize,
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

#[derive(Debug)]
enum EntityOutcome {
    Converted {
        geometry: CadGeometry,
        extra_properties: Vec<(&'static str, JsonValue)>,
        warnings: Vec<String>,
    },
    Skipped(String),
    Failed(String),
}

/// Convert every model-space entity in document order; count paper-space,
/// block-definition, and unowned entities as excluded by the documented
/// model-space filter.
/// Collecting wrapper over [`extract_with_sink`], used by unit tests.
#[cfg(test)]
fn extract(document: &CadDocument, options: &GeometryOptions) -> Result<Extraction> {
    let mut features = Vec::new();
    let mut extraction = extract_with_sink(document, options, &mut |feature| {
        features.push(feature);
        Ok(())
    })?;
    extraction.features = features;
    Ok(extraction)
}

/// Convert every model-space entity in document order, handing each finished
/// [`CadFeature`] to `sink` immediately (no feature retention here).
fn extract_with_sink(
    document: &CadDocument,
    options: &GeometryOptions,
    sink: &mut dyn FnMut(CadFeature) -> Result<()>,
) -> Result<Extraction> {
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
                // Inspection reports unresolved handles; model-space ones
                // count as failed outcomes AND as source entities so the
                // accounting denominator still balances.
                if is_model {
                    extraction.model_space_entities += 1;
                    extraction.top_level_accounted += 1;
                    record_outcome(
                        &mut extraction.failed,
                        "UNRESOLVED".to_string(),
                        "entity handle does not resolve to an entity".to_string(),
                        &format!("{handle}"),
                    );
                }
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

            if options.has_layer_filters() && !options.layer_passes(&entity.common().layer) {
                extraction.excluded_by_layer_filter += 1;
                continue;
            }

            process_entity(
                document,
                entity,
                model_index,
                options,
                &mut extraction,
                sink,
                &model_placement,
                0,
            )?;
            model_index += 1;
            extraction.model_space_entities += 1;
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
    sink: &mut dyn FnMut(CadFeature) -> Result<()>,
    placement: &Placement,
    depth: usize,
) -> Result<()> {
    if let EntityType::Insert(insert) = entity {
        return process_insert(
            document, entity, insert, index, options, extraction, sink, placement, depth,
        );
    }

    // Every non-INSERT entity reaches exactly one outcome below.
    if placement.block_path.is_empty() {
        extraction.top_level_accounted += 1;
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
            sink(build_feature(
                entity,
                feature_id,
                &entity_type,
                geometry,
                extra_properties,
                warnings,
                placement,
            ))?;
        }
        EntityOutcome::Skipped(reason) => {
            record_outcome(&mut extraction.skipped, entity_type, reason, &feature_id);
        }
        EntityOutcome::Failed(reason) => {
            record_outcome(&mut extraction.failed, entity_type, reason, &feature_id);
        }
    }
    Ok(())
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
    index: usize,
    options: &GeometryOptions,
    extraction: &mut Extraction,
    sink: &mut dyn FnMut(CadFeature) -> Result<()>,
    placement: &Placement,
    depth: usize,
) -> Result<()> {
    let id = feature_id(entity, index, placement);

    // Every top-level INSERT reaches a terminal outcome (anchor, expansion,
    // or a failure) on all paths below; count it before any early return so
    // the accounting denominator stays balanced.
    if placement.block_path.is_empty() {
        extraction.top_level_accounted += 1;
    }

    if !valid_normal(&insert.normal) {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            "zero or non-finite extrusion normal".to_string(),
            &id,
        );
        return Ok(());
    }

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
    let insert_lineweight = resolve_lineweight_mm(
        document,
        entity.common().line_weight,
        &insert_layer,
        placement,
    );

    let attributes: BTreeMap<String, String> = insert
        .attributes
        .iter()
        .map(|attribute| (attribute.tag.clone(), attribute.value.clone()))
        .collect();

    if options.preserve_inserts {
        return emit_insert_anchor(
            document,
            entity,
            insert,
            index,
            &attributes,
            extraction,
            sink,
            placement,
        );
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
        return Ok(());
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
        return Ok(());
    }
    if depth >= MAX_BLOCK_DEPTH {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            format!("block nesting deeper than {MAX_BLOCK_DEPTH} levels"),
            &id,
        );
        return Ok(());
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
                inherited_lineweight: insert_lineweight,
                max_scale: placement.max_scale * instance_scale,
            };

            for (child_index, handle) in record.entity_handles.iter().enumerate() {
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
                    child_index,
                    options,
                    extraction,
                    sink,
                    &child_placement,
                    depth + 1,
                )?;
            }
        }
    }

    extraction.inserts_expanded += 1;
    if !attributes.is_empty() {
        emit_insert_anchor(
            document,
            entity,
            insert,
            index,
            &attributes,
            extraction,
            sink,
            placement,
        )?;
    }
    Ok(())
}

/// Point feature at the (transformed) insertion point carrying the block
/// name and attribute values.
#[allow(clippy::too_many_arguments)]
fn emit_insert_anchor(
    document: &CadDocument,
    entity: &EntityType,
    insert: &acadrust::entities::Insert,
    index: usize,
    attributes: &BTreeMap<String, String>,
    extraction: &mut Extraction,
    sink: &mut dyn FnMut(CadFeature) -> Result<()>,
    placement: &Placement,
) -> Result<()> {
    let id = feature_id(entity, index, placement);
    // The insertion point is OCS; lift it before the placement transform.
    let anchor = Matrix3::arbitrary_axis(insert.normal).transform_point(insert.insert_point);
    let mut max_abs_z: f64 = 0.0;
    let Some(position) = project(placement, anchor, &mut max_abs_z) else {
        record_outcome(
            &mut extraction.failed,
            "INSERT".to_string(),
            "non-finite coordinates".to_string(),
            &id,
        );
        return Ok(());
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
    sink(build_feature(
        entity,
        id,
        "INSERT",
        CadGeometry::Point(position),
        extra_properties,
        warnings,
        placement,
    ))
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
    geometry: CadGeometry,
    extra_properties: Vec<(&'static str, JsonValue)>,
    warnings: Vec<String>,
    placement: &Placement,
) -> CadFeature {
    let common = entity.common();

    let source_layer = common.layer.clone();
    let layer = effective_layer(&source_layer, placement);

    CadFeature {
        id: feature_id,
        entity_type: entity_type.to_string(),
        handle: format!("{}", common.handle),
        source_layer: (layer != source_layer).then_some(source_layer),
        layer,
        block_path: placement.block_path.clone(),
        extra_properties,
        warnings,
        geometry,
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
        EntityType::Face3D(face) => convert_face3d(face, placement),
        EntityType::Solid(solid) => convert_solid(solid, placement),
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
        EntityType::Hatch(hatch) => convert_hatch(hatch, options, placement),
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
        geometry: CadGeometry::Point(position),
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
        geometry: CadGeometry::Line(vec![start, end]),
        extra_properties: Vec::new(),
        warnings,
    }
}

fn convert_face3d(face: &acadrust::entities::Face3D, placement: &Placement) -> EntityOutcome {
    let corners = [
        face.first_corner,
        face.second_corner,
        face.third_corner,
        face.fourth_corner,
    ];
    if corners.iter().any(|corner| !is_finite(corner)) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }

    let mut ring: Vec<(f64, f64)> = Vec::with_capacity(5);
    let mut max_abs_z: f64 = 0.0;
    for corner in corners {
        let Some(position) = project(placement, corner, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        ring.push(position);
    }
    ring.dedup();

    if count_distinct(&ring) < 3 {
        return EntityOutcome::Skipped(
            "degenerate 3DFACE: fewer than three distinct corners".to_string(),
        );
    }
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    if signed_area(&ring) < 0.0 {
        ring.reverse();
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    EntityOutcome::Converted {
        geometry: CadGeometry::Polygon(vec![ring]),
        extra_properties: vec![("is_closed", JsonValue::from(true))],
        warnings,
    }
}

/// DXF SOLID corners live in OCS and are stored in bow-tie order: the
/// visual quad is first, second, fourth, third; triangles duplicate the
/// third corner into the fourth.
fn convert_solid(solid: &acadrust::entities::Solid, placement: &Placement) -> EntityOutcome {
    let corners = [
        solid.first_corner,
        solid.second_corner,
        solid.fourth_corner,
        solid.third_corner,
    ];
    if corners.iter().any(|corner| !is_finite(corner)) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if !valid_normal(&solid.normal) {
        return EntityOutcome::Failed("zero or non-finite extrusion normal".to_string());
    }

    let ocs_to_wcs = Matrix3::arbitrary_axis(solid.normal);
    let mut ring: Vec<(f64, f64)> = Vec::with_capacity(5);
    let mut max_abs_z: f64 = 0.0;
    for corner in corners {
        let wcs = ocs_to_wcs.transform_point(corner);
        let Some(position) = project(placement, wcs, &mut max_abs_z) else {
            return EntityOutcome::Failed("non-finite coordinates".to_string());
        };
        ring.push(position);
    }
    ring.dedup();

    if count_distinct(&ring) < 3 {
        return EntityOutcome::Skipped(
            "degenerate SOLID: fewer than three distinct corners".to_string(),
        );
    }
    if ring.first() != ring.last() {
        ring.push(ring[0]);
    }
    if signed_area(&ring) < 0.0 {
        ring.reverse();
    }

    let mut warnings = Vec::new();
    push_z_warning(&mut warnings, max_abs_z);

    EntityOutcome::Converted {
        geometry: CadGeometry::Polygon(vec![ring]),
        extra_properties: vec![("is_closed", JsonValue::from(true))],
        warnings,
    }
}

/// A hatch boundary loop tessellated in the OCS plane.
struct BoundaryLoop {
    ring: Vec<(f64, f64)>,
    approximated: bool,
}

/// HATCH boundaries live in the OCS plane of the hatch normal. Each boundary
/// path becomes a ring: polyline paths reuse the bulge tessellator; edge
/// paths chain line/arc/elliptic-arc/spline edges, reversing edges whose far
/// end is the better connection. Gaps within the curve tolerance snap
/// silently; larger gaps are bridged and closed with repair warnings, never
/// silently. Loops are nested by even-odd containment into Polygon /
/// MultiPolygon with CCW shells and CW holes.
fn convert_hatch(
    hatch: &acadrust::entities::Hatch,
    options: &GeometryOptions,
    placement: &Placement,
) -> EntityOutcome {
    if hatch.paths.is_empty() {
        return EntityOutcome::Skipped("hatch has no boundary paths".to_string());
    }
    if !hatch.elevation.is_finite() {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if !valid_normal(&hatch.normal) {
        return EntityOutcome::Failed("zero or non-finite extrusion normal".to_string());
    }

    let mut warnings = Vec::new();
    let mut loops: Vec<BoundaryLoop> = Vec::new();
    let mut any_curved = false;
    for (path_index, path) in hatch.paths.iter().enumerate() {
        match boundary_ring(path, options, &mut warnings, path_index) {
            Ok(boundary) => {
                any_curved |= boundary.approximated;
                loops.push(boundary);
            }
            Err(reason) => {
                warnings.push(format!("boundary loop {path_index} dropped: {reason}"));
            }
        }
    }
    if loops.is_empty() {
        return EntityOutcome::Skipped(
            "hatch has no valid boundary loops after repair".to_string(),
        );
    }
    let dropped = hatch.paths.len() - loops.len();

    // Lift every ring from the OCS plane to WCS, then through the placement.
    let ocs_to_wcs = Matrix3::arbitrary_axis(hatch.normal);
    let mut max_abs_z: f64 = 0.0;
    let mut rings: Vec<Vec<(f64, f64)>> = Vec::with_capacity(loops.len());
    for boundary in &loops {
        let mut ring = Vec::with_capacity(boundary.ring.len());
        for (x, y) in &boundary.ring {
            let wcs = ocs_to_wcs.transform_point(Vector3::new(*x, *y, hatch.elevation));
            let Some(position) = project(placement, wcs, &mut max_abs_z) else {
                return EntityOutcome::Failed("non-finite coordinates".to_string());
            };
            ring.push(position);
        }
        rings.push(ring);
    }

    let geometry = assemble_hatch_polygons(rings);
    push_z_warning(&mut warnings, max_abs_z);
    if any_curved {
        warnings.push(format!(
            "arc segments tessellated with chord tolerance {} drawing units",
            options.curve_tolerance
        ));
    }

    let repaired = warnings
        .iter()
        .any(|warning| warning.contains("bridged") || warning.contains("closed across"));
    let mut extra_properties = vec![
        ("is_closed", JsonValue::from(true)),
        ("hatch_pattern", JsonValue::from(hatch.pattern.name.clone())),
        ("hatch_solid", JsonValue::from(hatch.is_solid)),
    ];
    if dropped > 0 {
        extra_properties.push(("hatch_loops_dropped", JsonValue::from(dropped)));
    }
    if any_curved || repaired {
        extra_properties.push(("approximated", JsonValue::from(true)));
    }

    EntityOutcome::Converted {
        geometry,
        extra_properties,
        warnings,
    }
}

/// Build one closed OCS ring from a boundary path, or a reason it is
/// unusable. Repairs (bridged gaps, forced closure) warn but do not fail.
fn boundary_ring(
    path: &acadrust::entities::BoundaryPath,
    options: &GeometryOptions,
    warnings: &mut Vec<String>,
    path_index: usize,
) -> Result<BoundaryLoop, String> {
    let tolerance = options.curve_tolerance;
    let mut ring: Vec<(f64, f64)> = Vec::new();
    let mut approximated = false;

    for edge in &path.edges {
        let points = boundary_edge_points(edge, options, warnings, &mut approximated)?;
        append_connected(&mut ring, points, tolerance, warnings, path_index);
    }

    if ring.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Err("non-finite coordinates".to_string());
    }
    // Close the loop; a closure gap wider than the tolerance is a repair.
    if let (Some(first), Some(last)) = (ring.first().copied(), ring.last().copied()) {
        let gap = (last.0 - first.0).hypot(last.1 - first.1);
        if gap <= tolerance {
            if ring.len() > 1 {
                ring.pop();
            }
        } else {
            warnings.push(format!(
                "boundary loop {path_index} closed across a gap of {gap} drawing units"
            ));
        }
        ring.push(first);
    }
    if count_distinct(&ring) < 3 {
        return Err("fewer than three distinct points".to_string());
    }
    if ring_is_zero_area(&ring) {
        return Err("zero-area (collinear) loop".to_string());
    }
    Ok(BoundaryLoop { ring, approximated })
}

/// Append an edge's points to the ring under construction, reversing the
/// edge when its far end is the better connection and warning when neither
/// end meets the ring within the tolerance.
fn append_connected(
    ring: &mut Vec<(f64, f64)>,
    mut points: Vec<(f64, f64)>,
    tolerance: f64,
    warnings: &mut Vec<String>,
    path_index: usize,
) {
    if points.is_empty() {
        return;
    }
    let Some(&last) = ring.last() else {
        ring.extend(points);
        return;
    };
    let distance = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).hypot(a.1 - b.1);
    let first_point = points[0];
    let last_point = points[points.len() - 1];
    if distance(last, last_point) < distance(last, first_point) {
        points.reverse();
    }
    let gap = distance(last, points[0]);
    if gap <= tolerance {
        ring.extend(points.into_iter().skip(1));
    } else {
        warnings.push(format!(
            "boundary loop {path_index}: edge gap of {gap} drawing units bridged"
        ));
        ring.extend(points);
    }
}

/// Tessellated points of one boundary edge in OCS, endpoints included.
fn boundary_edge_points(
    edge: &acadrust::entities::BoundaryEdge,
    options: &GeometryOptions,
    warnings: &mut Vec<String>,
    approximated: &mut bool,
) -> Result<Vec<(f64, f64)>, String> {
    use acadrust::entities::BoundaryEdge;

    match edge {
        BoundaryEdge::Line(line) => {
            Ok(vec![(line.start.x, line.start.y), (line.end.x, line.end.y)])
        }
        BoundaryEdge::Polyline(polyline) => {
            let mut points: Vec<(f64, f64)> = Vec::new();
            let vertices = &polyline.vertices;
            if vertices.is_empty() {
                return Err("empty polyline boundary edge".to_string());
            }
            *approximated |= polyline.has_bulge();
            let segments = if polyline.is_closed {
                vertices.len()
            } else {
                vertices.len() - 1
            };
            points.push((vertices[0].x, vertices[0].y));
            for i in 0..segments {
                let start = vertices[i];
                let end = vertices[(i + 1) % vertices.len()];
                if start.z.abs() > f64::EPSILON {
                    points.extend(tessellate_bulge(
                        (start.x, start.y),
                        (end.x, end.y),
                        start.z,
                        options.curve_tolerance,
                        warnings,
                    ));
                }
                points.push((end.x, end.y));
            }
            Ok(points)
        }
        BoundaryEdge::CircularArc(arc) => {
            if !arc.radius.is_finite() || arc.radius <= 0.0 {
                return Err("circular arc edge with non-positive radius".to_string());
            }
            *approximated = true;
            let Some((start, sweep)) =
                edge_sweep(arc.start_angle, arc.end_angle, arc.counter_clockwise)?
            else {
                warnings.push("zero-sweep circular arc edge ignored".to_string());
                return Ok(Vec::new());
            };
            Ok(arc_points(
                (arc.center.x, arc.center.y),
                arc.radius,
                start,
                sweep,
                options.curve_tolerance,
                warnings,
            ))
        }
        BoundaryEdge::EllipticArc(ellipse) => {
            let major = (ellipse.major_axis_endpoint.x, ellipse.major_axis_endpoint.y);
            let major_length = major.0.hypot(major.1);
            if !major_length.is_finite()
                || major_length <= 0.0
                || !ellipse.minor_axis_ratio.is_finite()
                || ellipse.minor_axis_ratio <= 0.0
            {
                return Err("degenerate elliptic arc edge".to_string());
            }
            *approximated = true;
            let Some((start, sweep)) = edge_sweep(
                ellipse.start_angle,
                ellipse.end_angle,
                ellipse.counter_clockwise,
            )?
            else {
                warnings.push("zero-sweep elliptic arc edge ignored".to_string());
                return Ok(Vec::new());
            };
            let minor = (
                -major.1 * ellipse.minor_axis_ratio,
                major.0 * ellipse.minor_axis_ratio,
            );
            let parameters = arc_points(
                (0.0, 0.0),
                1.0,
                start,
                sweep,
                options.curve_tolerance / major_length,
                warnings,
            );
            Ok(parameters
                .into_iter()
                .map(|(cos_t, sin_t)| {
                    (
                        ellipse.center.x + major.0 * cos_t + minor.0 * sin_t,
                        ellipse.center.y + major.1 * cos_t + minor.1 * sin_t,
                    )
                })
                .collect())
        }
        BoundaryEdge::Spline(spline) => {
            let degree = spline.degree.max(0) as usize;
            let control_count = spline.control_points.len();
            let nurbs_valid = degree >= 1
                && control_count > degree
                && spline.knots.len() == control_count + degree + 1
                && spline.knots.windows(2).all(|pair| pair[0] <= pair[1])
                && spline.knots.iter().all(|knot| knot.is_finite())
                && spline
                    .control_points
                    .iter()
                    .all(|point| point.x.is_finite() && point.y.is_finite());
            if !nurbs_valid {
                if spline.fit_points.len() >= 2
                    && spline
                        .fit_points
                        .iter()
                        .all(|point| point.x.is_finite() && point.y.is_finite())
                {
                    *approximated = true;
                    warnings.push(
                        "spline boundary edge rendered as a polyline through its fit points"
                            .to_string(),
                    );
                    return Ok(spline
                        .fit_points
                        .iter()
                        .map(|point| (point.x, point.y))
                        .collect());
                }
                return Err("invalid spline boundary edge".to_string());
            }
            // Control points pack (x, y, weight); weights only count when
            // the edge is rational.
            let homogeneous: Vec<[f64; 4]> = spline
                .control_points
                .iter()
                .map(|point| {
                    let weight =
                        if spline.rational && point.z.is_finite() && point.z.abs() > f64::EPSILON {
                            point.z
                        } else {
                            1.0
                        };
                    [point.x * weight, point.y * weight, 0.0, weight]
                })
                .collect();
            let domain_start = spline.knots[degree];
            let domain_end = spline.knots[control_count];
            if domain_end <= domain_start {
                return Err("spline boundary edge with an empty parameter domain".to_string());
            }
            let spans = control_count - degree;
            *approximated = true;
            let Some(NurbsSampling {
                points,
                tolerance_met,
                ..
            }) = sample_nurbs_with_tolerance(
                degree,
                &spline.knots,
                &homogeneous,
                domain_start,
                domain_end,
                spans,
                options.curve_tolerance,
            )
            else {
                return Err("spline boundary edge evaluated to a non-finite point".to_string());
            };
            if !tolerance_met {
                warnings.push(format!(
                    "spline boundary edge capped at {MAX_ARC_SEGMENTS} segments; chord tolerance not met"
                ));
            }
            Ok(points.into_iter().map(|point| (point.0, point.1)).collect())
        }
    }
}

/// Start angle and signed sweep for an arc-like boundary edge. Clockwise
/// edges store their angles mirrored, so both the start angle and the sweep
/// are negated. A zero sweep means a full revolution.
fn edge_sweep(
    start_angle: f64,
    end_angle: f64,
    counter_clockwise: bool,
) -> Result<Option<(f64, f64)>, String> {
    if !start_angle.is_finite() || !end_angle.is_finite() {
        return Err("non-finite arc edge angles".to_string());
    }
    // A zero-sweep edge contributes nothing; the caller skips it with a
    // warning instead of dropping the whole loop.
    let Some(sweep) = ccw_sweep(start_angle, end_angle) else {
        return Ok(None);
    };
    if counter_clockwise {
        Ok(Some((start_angle, sweep)))
    } else {
        Ok(Some((-start_angle, -sweep)))
    }
}

/// Nest closed rings by even-odd containment: even depth is a shell, odd
/// depth is a hole assigned to its innermost containing shell. Shells are
/// oriented CCW and holes CW. One shell yields a Polygon, several a
/// MultiPolygon. Deterministic: input order is preserved.
type ShellRings = (usize, Vec<Vec<(f64, f64)>>);

fn assemble_hatch_polygons(mut rings: Vec<Vec<(f64, f64)>>) -> CadGeometry {
    for ring in &mut rings {
        if signed_area(ring) < 0.0 {
            ring.reverse();
        }
    }
    // Containment is pairwise, so hatches with many loops are quadratic in
    // loop count; the bbox pre-check makes the common disjoint-loop pair an
    // O(1) reject instead of a full ray cast (audit finding C2).
    let bboxes: Vec<[f64; 4]> = rings
        .iter()
        .map(|ring| {
            let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
            for (x, y) in ring {
                bbox = [
                    bbox[0].min(*x),
                    bbox[1].min(*y),
                    bbox[2].max(*x),
                    bbox[3].max(*y),
                ];
            }
            bbox
        })
        .collect();
    let contains = |ring_index: usize, point: (f64, f64)| -> bool {
        let bbox = &bboxes[ring_index];
        point.0 >= bbox[0]
            && point.0 <= bbox[2]
            && point.1 >= bbox[1]
            && point.1 <= bbox[3]
            && ring_contains_point(&rings[ring_index], point)
    };
    // Uniform x-grid over the ring bboxes: candidate rings for a probe are
    // those sharing its x-cell, which makes the disjoint-loop case (the
    // common one) near-linear instead of all-pairs.
    let (grid_min_x, grid_cell) = {
        let min_x = bboxes.iter().fold(f64::MAX, |m, b| m.min(b[0]));
        let max_x = bboxes.iter().fold(f64::MIN, |m, b| m.max(b[2]));
        let width = (max_x - min_x).max(f64::MIN_POSITIVE);
        (min_x, width / (rings.len().max(1) as f64))
    };
    let cell_of = |x: f64| -> usize { ((x - grid_min_x) / grid_cell) as usize };
    let mut grid: Vec<Vec<usize>> = vec![Vec::new(); rings.len() + 2];
    for (index, bbox) in bboxes.iter().enumerate() {
        let last = grid.len() - 1;
        let (first_cell, last_cell) = (cell_of(bbox[0]).min(last), cell_of(bbox[2]).min(last));
        for cell in grid[first_cell..=last_cell].iter_mut() {
            cell.push(index);
        }
    }
    let depths: Vec<usize> = (0..rings.len())
        .map(|i| {
            let probe = rings[i][0];
            grid[cell_of(probe.0).min(grid.len() - 1)]
                .iter()
                .filter(|&&j| j != i && contains(j, probe))
                .count()
        })
        .collect();

    let mut polygons: Vec<ShellRings> = Vec::new();
    for (index, ring) in rings.iter().enumerate() {
        if depths[index] % 2 == 0 {
            polygons.push((index, vec![ring.clone()]));
        }
    }
    for (index, ring) in rings.iter().enumerate() {
        if depths[index] % 2 == 0 {
            continue;
        }
        // Innermost containing shell: the containing shell of greatest depth.
        let probe = ring[0];
        let shell = polygons
            .iter_mut()
            .filter(|(shell_index, _)| contains(*shell_index, probe))
            .max_by_key(|(shell_index, _)| depths[*shell_index]);
        let mut hole = ring.clone();
        hole.reverse();
        match shell {
            Some((_, rings_of_shell)) => rings_of_shell.push(hole),
            // A hole with no containing shell is promoted to its own shell.
            None => {
                let mut promoted = hole;
                promoted.reverse();
                polygons.push((index, vec![promoted]));
            }
        }
    }

    if polygons.len() == 1 {
        CadGeometry::Polygon(polygons.remove(0).1)
    } else {
        let coordinates: Vec<Vec<Vec<(f64, f64)>>> =
            polygons.into_iter().map(|(_, rings)| rings).collect();
        CadGeometry::MultiPolygon(coordinates)
    }
}

/// Even-odd ray casting; boundary cases are treated as outside.
fn ring_contains_point(ring: &[(f64, f64)], point: (f64, f64)) -> bool {
    let mut inside = false;
    for pair in ring.windows(2) {
        let (x1, y1) = pair[0];
        let (x2, y2) = pair[1];
        if (y1 > point.1) != (y2 > point.1) {
            let x_cross = x1 + (point.1 - y1) / (y2 - y1) * (x2 - x1);
            if point.0 < x_cross {
                inside = !inside;
            }
        }
    }
    inside
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
    if !valid_normal(&normal) {
        return EntityOutcome::Failed("zero or non-finite extrusion normal".to_string());
    }
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
            geometry: CadGeometry::Polygon(vec![ring]),
            extra_properties,
            warnings,
        };
    }

    if closed && coordinates.first() != coordinates.last() {
        coordinates.push(coordinates[0]);
    }

    EntityOutcome::Converted {
        geometry: CadGeometry::Line(coordinates),
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

    let Some(sweep) = ccw_sweep(arc.start_angle, arc.end_angle) else {
        return EntityOutcome::Skipped("degenerate arc: zero angular sweep".to_string());
    };

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

    let Some(sweep) = ccw_sweep(ellipse.start_parameter, ellipse.end_parameter) else {
        return EntityOutcome::Skipped("degenerate ellipse: zero parameter sweep".to_string());
    };
    let closed = sweep >= std::f64::consts::TAU - ANGLE_EPSILON;

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
const SPLINE_SEGMENTS_PER_SPAN: usize = 2;
const SPLINE_MIN_SEGMENTS: usize = 8;

/// Uniform NURBS sampling with the smallest segment count (doubling from a
/// span-based floor) whose estimated chord error meets `tolerance`. The
/// error estimate is the deviation of the curve at each segment's parameter
/// midpoint from the segment chord's midpoint — the standard subdivision
/// bound for smooth curves. Returns the 3D samples, the segment count used,
/// and whether the tolerance was met before the segment cap; None when
/// evaluation produces a non-finite or zero-weight point.
#[allow(clippy::too_many_arguments)]
fn sample_nurbs_with_tolerance(
    degree: usize,
    knots: &[f64],
    homogeneous: &[[f64; 4]],
    domain_start: f64,
    domain_end: f64,
    spans: usize,
    tolerance: f64,
) -> Option<NurbsSampling> {
    let mut segments =
        (spans * SPLINE_SEGMENTS_PER_SPAN).clamp(SPLINE_MIN_SEGMENTS, MAX_ARC_SEGMENTS);
    loop {
        let mut points = Vec::with_capacity(segments + 1);
        for i in 0..=segments {
            let t = domain_start + (domain_end - domain_start) * (i as f64) / (segments as f64);
            points.push(evaluate_nurbs(t, degree, knots, homogeneous)?);
        }

        let mut max_error: f64 = 0.0;
        for i in 0..segments {
            let t_mid =
                domain_start + (domain_end - domain_start) * (i as f64 + 0.5) / (segments as f64);
            let on_curve = evaluate_nurbs(t_mid, degree, knots, homogeneous)?;
            let (a, b) = (points[i], points[i + 1]);
            let chord_mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, (a.2 + b.2) / 2.0);
            let error = ((on_curve.0 - chord_mid.0).powi(2)
                + (on_curve.1 - chord_mid.1).powi(2)
                + (on_curve.2 - chord_mid.2).powi(2))
            .sqrt();
            max_error = max_error.max(error);
        }

        if max_error <= tolerance {
            return Some(NurbsSampling {
                points,
                segments,
                tolerance_met: true,
            });
        }
        if segments >= MAX_ARC_SEGMENTS {
            return Some(NurbsSampling {
                points,
                segments,
                tolerance_met: false,
            });
        }
        segments = (segments * 2).min(MAX_ARC_SEGMENTS);
    }
}

/// Result of [`sample_nurbs_with_tolerance`].
struct NurbsSampling {
    points: Vec<(f64, f64, f64)>,
    segments: usize,
    tolerance_met: bool,
}

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
    let Some(NurbsSampling {
        points,
        segments,
        tolerance_met,
    }) = sample_nurbs_with_tolerance(
        degree,
        &spline.knots,
        &homogeneous,
        domain_start,
        domain_end,
        spans,
        options.curve_tolerance,
    )
    else {
        return EntityOutcome::Failed(
            "spline evaluation produced a non-finite or zero-weight point".to_string(),
        );
    };

    let mut coordinates: Vec<(f64, f64)> = Vec::with_capacity(points.len());
    let mut max_abs_z: f64 = 0.0;
    for point in points {
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
    if tolerance_met {
        warnings.push(format!(
            "spline tessellated at {} segments to meet chord tolerance {} drawing units",
            segments, options.curve_tolerance
        ));
    } else {
        warnings.push(format!(
            "spline tessellation capped at {MAX_ARC_SEGMENTS} segments; chord tolerance not met"
        ));
    }
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

    // Zero-denominator detection must scale with the knot domain: an
    // absolute epsilon misreads tiny-but-valid knot spans as repeated knots.
    let knot_scale = knots.iter().fold(0.0f64, |max, knot| max.max(knot.abs()));
    let knot_epsilon = knot_scale * f64::EPSILON;

    let mut d: Vec<[f64; 4]> = (0..=degree).map(|j| control[j + span - degree]).collect();
    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = j + span - degree;
            let denominator = knots[i + degree - r + 1] - knots[i];
            let alpha = if denominator.abs() <= knot_epsilon {
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

/// TEXT anchors live in OCS. DXF uses the insertion point only for the
/// default left/baseline alignment; other alignments use the second point
/// when it is present.
fn convert_text(text: &acadrust::entities::Text, placement: &Placement) -> EntityOutcome {
    let default_alignment = matches!(
        text.horizontal_alignment,
        acadrust::entities::TextHorizontalAlignment::Left
    ) && matches!(
        text.vertical_alignment,
        acadrust::entities::TextVerticalAlignment::Baseline
    );
    let (anchor, anchor_name) = if default_alignment {
        (text.insertion_point, "insertion")
    } else if let Some(alignment_point) = text.alignment_point {
        (alignment_point, "alignment")
    } else {
        (text.insertion_point, "insertion")
    };
    if !is_finite(&anchor) {
        return EntityOutcome::Failed("non-finite coordinates".to_string());
    }
    if !valid_normal(&text.normal) {
        return EntityOutcome::Failed("zero or non-finite extrusion normal".to_string());
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

    let mut extra_properties = vec![
        ("text", JsonValue::from(text.value.clone())),
        ("text_height", JsonValue::from(text.height)),
        ("text_rotation_deg", JsonValue::from(rotation_deg)),
        ("text_style", JsonValue::from(text.style.clone())),
    ];
    if !matches!(
        text.horizontal_alignment,
        acadrust::entities::TextHorizontalAlignment::Left
    ) {
        extra_properties.push((
            "text_h_align",
            JsonValue::from(text_horizontal_alignment_name(text.horizontal_alignment)),
        ));
    }
    if !matches!(
        text.vertical_alignment,
        acadrust::entities::TextVerticalAlignment::Baseline
    ) {
        extra_properties.push((
            "text_v_align",
            JsonValue::from(text_vertical_alignment_name(text.vertical_alignment)),
        ));
    }
    if !default_alignment || text.alignment_point.is_some() {
        extra_properties.push(("text_anchor", JsonValue::from(anchor_name)));
    }
    if text.width_factor != 1.0 {
        extra_properties.push(("text_width_factor", JsonValue::from(text.width_factor)));
    }
    if text.oblique_angle != 0.0 {
        extra_properties.push((
            "text_oblique_deg",
            JsonValue::from(text.oblique_angle.to_degrees()),
        ));
    }
    if text.generation_flags & 2 != 0 {
        extra_properties.push(("text_mirrored_x", JsonValue::from(true)));
    }
    if text.generation_flags & 4 != 0 {
        extra_properties.push(("text_mirrored_y", JsonValue::from(true)));
    }

    EntityOutcome::Converted {
        geometry: CadGeometry::Point(position),
        extra_properties,
        warnings,
    }
}

fn text_horizontal_alignment_name(
    alignment: acadrust::entities::TextHorizontalAlignment,
) -> &'static str {
    use acadrust::entities::TextHorizontalAlignment;

    match alignment {
        TextHorizontalAlignment::Left => "left",
        TextHorizontalAlignment::Center => "center",
        TextHorizontalAlignment::Right => "right",
        TextHorizontalAlignment::Aligned => "aligned",
        TextHorizontalAlignment::Middle => "middle",
        TextHorizontalAlignment::Fit => "fit",
    }
}

fn text_vertical_alignment_name(
    alignment: acadrust::entities::TextVerticalAlignment,
) -> &'static str {
    use acadrust::entities::TextVerticalAlignment;

    match alignment {
        TextVerticalAlignment::Baseline => "baseline",
        TextVerticalAlignment::Bottom => "bottom",
        TextVerticalAlignment::Middle => "middle",
        TextVerticalAlignment::Top => "top",
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
    if !matches!(
        mtext.attachment_point,
        acadrust::entities::AttachmentPoint::TopLeft
    ) {
        extra_properties.push((
            "text_attachment",
            JsonValue::from(mtext_attachment_name(mtext.attachment_point)),
        ));
    }
    if !matches!(
        mtext.drawing_direction,
        acadrust::entities::DrawingDirection::LeftToRight
    ) {
        extra_properties.push((
            "text_direction",
            JsonValue::from(mtext_direction_name(mtext.drawing_direction)),
        ));
    }
    if mtext.rectangle_width != 10.0 {
        extra_properties.push(("text_width", JsonValue::from(mtext.rectangle_width)));
    }
    if mtext.line_spacing_factor != 1.0 {
        extra_properties.push((
            "text_line_spacing_factor",
            JsonValue::from(mtext.line_spacing_factor),
        ));
    }
    if matches!(
        mtext.line_spacing_style,
        acadrust::entities::LineSpacingStyle::Exactly
    ) {
        extra_properties.push(("text_line_spacing_style", JsonValue::from("exactly")));
    }
    if mtext.column_data.column_type != 0 {
        let columns = &mtext.column_data;
        let column_type = match columns.column_type {
            1 => JsonValue::from("static"),
            2 => JsonValue::from("dynamic"),
            other => JsonValue::from(other),
        };
        let mut properties = JsonObject::new();
        properties.insert("type".to_string(), column_type);
        properties.insert("count".to_string(), JsonValue::from(columns.column_count));
        properties.insert("width".to_string(), JsonValue::from(columns.width));
        properties.insert("gutter".to_string(), JsonValue::from(columns.gutter));
        if columns.flow_reversed {
            properties.insert("flow_reversed".to_string(), JsonValue::from(true));
        }
        if columns.auto_height {
            properties.insert("auto_height".to_string(), JsonValue::from(true));
        }
        if !columns.heights.is_empty() {
            properties.insert(
                "heights".to_string(),
                JsonValue::from(columns.heights.clone()),
            );
        }
        extra_properties.push(("text_columns", JsonValue::Object(properties)));
    }

    EntityOutcome::Converted {
        geometry: CadGeometry::Point(position),
        extra_properties,
        warnings,
    }
}

fn mtext_attachment_name(attachment: acadrust::entities::AttachmentPoint) -> &'static str {
    use acadrust::entities::AttachmentPoint;

    match attachment {
        AttachmentPoint::TopLeft => "top-left",
        AttachmentPoint::TopCenter => "top-center",
        AttachmentPoint::TopRight => "top-right",
        AttachmentPoint::MiddleLeft => "middle-left",
        AttachmentPoint::MiddleCenter => "middle-center",
        AttachmentPoint::MiddleRight => "middle-right",
        AttachmentPoint::BottomLeft => "bottom-left",
        AttachmentPoint::BottomCenter => "bottom-center",
        AttachmentPoint::BottomRight => "bottom-right",
    }
}

fn mtext_direction_name(direction: acadrust::entities::DrawingDirection) -> &'static str {
    use acadrust::entities::DrawingDirection;

    match direction {
        DrawingDirection::LeftToRight => "left-to-right",
        DrawingDirection::TopToBottom => "top-to-bottom",
        DrawingDirection::ByStyle => "by-style",
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
    if !valid_normal(&normal) {
        return EntityOutcome::Failed("zero or non-finite extrusion normal".to_string());
    }
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

/// An OCS extrusion normal must be finite and robustly nonzero; feeding a
/// zero vector to the arbitrary-axis transform silently collapses geometry.
fn valid_normal(normal: &Vector3) -> bool {
    is_finite(normal) && normal.length() > 1e-12
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

/// A ring whose area is negligible relative to its own extent encloses
/// nothing (collinear or duplicated points).
fn ring_is_zero_area(ring: &[(f64, f64)]) -> bool {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (x, y) in ring {
        min_x = min_x.min(*x);
        min_y = min_y.min(*y);
        max_x = max_x.max(*x);
        max_y = max_y.max(*y);
    }
    let extent = (max_x - min_x).max(max_y - min_y);
    if !extent.is_finite() || extent <= 0.0 {
        return true;
    }
    signed_area(ring).abs() <= 1e-12 * extent * extent
}

/// Shoelace formula; positive for counter-clockwise rings.
fn signed_area(ring: &[(f64, f64)]) -> f64 {
    let mut sum = 0.0;
    for pair in ring.windows(2) {
        sum += (pair[1].0 - pair[0].0) * (pair[1].1 + pair[0].1);
    }
    -sum / 2.0
}

/// Result of the in-memory embedding conversion ([`convert_bytes`]).
#[derive(Debug, serde::Serialize)]
pub struct EmbedResult {
    /// GeoJSON FeatureCollection (local drawing coordinates) as a string.
    pub geojson: String,
    pub feature_count: usize,
    pub model_space_entities: usize,
    pub converted: Vec<report::ConvertedCount>,
    pub skipped: Vec<report::OutcomeCount>,
    pub failed: Vec<report::OutcomeCount>,
    pub warnings: Vec<String>,
    /// Drawing-coordinate bounding box [min_x, min_y, max_x, max_y].
    pub bbox: Option<[f64; 4]>,
    /// SHA-256 of the input bytes.
    pub source_sha256: String,
}

/// Embedding / WebAssembly entry point: convert model-space geometry from an
/// in-memory DWG to a GeoJSON FeatureCollection in raw drawing coordinates —
/// no CRS handling, no file I/O. Reprojection is the embedder's concern (the
/// browser app reprojects with proj4js). Reuses the same audited extraction
/// and writer as the CLI native backend.
pub fn convert_bytes(
    bytes: &[u8],
    polygonize_closed: bool,
    curve_tolerance: Option<f64>,
) -> Result<EmbedResult> {
    use sha2::{Digest, Sha256};

    let (document, _read_mode, mut warnings) = super::read_stream(bytes)?;

    let options = GeometryOptions {
        polygonize_closed,
        curve_tolerance: curve_tolerance.unwrap_or(DEFAULT_CURVE_TOLERANCE),
        preserve_inserts: false,
        include_layers: Vec::new(),
        exclude_layers: Vec::new(),
    };

    let mut features: Vec<Feature> = Vec::new();
    let mut bbox: Option<[f64; 4]> = None;
    let extraction = extract_with_sink(&document, &options, &mut |feature| {
        accumulate_bbox(&mut bbox, &feature);
        features.push(writer::to_geojson(&feature));
        Ok(())
    })?;

    let source_sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };

    let mut members = JsonObject::new();
    members.insert(
        "dwg2geo".to_string(),
        serde_json::json!({
            "coordinate_status": "local-unreferenced",
            "note": "coordinates are raw drawing units; no geographic CRS was established",
            "source_sha256": source_sha256,
        }),
    );
    let collection = FeatureCollection {
        bbox: None,
        features,
        foreign_members: Some(members),
    };
    let geojson = serde_json::to_string(&collection)
        .context("cannot serialize GeoJSON feature collection")?;

    if extraction.model_space_entities != extraction.top_level_accounted {
        warnings.push(format!(
            "{} model-space entities were not accounted",
            extraction
                .model_space_entities
                .saturating_sub(extraction.top_level_accounted)
        ));
    }

    let feature_count = collection_len(&geojson);
    Ok(EmbedResult {
        feature_count,
        model_space_entities: extraction.model_space_entities,
        converted: extraction
            .converted
            .into_iter()
            .map(|(entity_type, count)| report::ConvertedCount { entity_type, count })
            .collect(),
        skipped: outcome_counts(extraction.skipped),
        failed: outcome_counts(extraction.failed),
        warnings,
        bbox,
        source_sha256,
        geojson,
    })
}

/// Count the Feature objects in a serialized collection without reparsing the
/// geometry (the string was just produced from a known-shaped collection).
fn collection_len(geojson: &str) -> usize {
    serde_json::from_str::<serde_json::Value>(geojson)
        .ok()
        .and_then(|v| {
            v.get("features")
                .and_then(|f| f.as_array().map(|a| a.len()))
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::CadGeometry;
    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, Line, LwPolyline, Point},
        types::Vector2,
    };

    use super::{
        DEFAULT_CURVE_TOLERANCE, EntityOutcome, GeometryOptions, Placement, extract, signed_area,
        tessellate_bulge,
    };

    fn opts(polygonize_closed: bool) -> GeometryOptions {
        GeometryOptions {
            polygonize_closed,
            curve_tolerance: DEFAULT_CURVE_TOLERANCE,
            preserve_inserts: false,
            include_layers: Vec::new(),
            exclude_layers: Vec::new(),
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
                assert_eq!(geometry, CadGeometry::Point((1.0, 2.0)));
            }
            _ => panic!("point must convert"),
        }

        let line = EntityType::Line(Line::from_coords(0.0, 0.0, 3.0, 4.0, 5.0, 3.0));
        match convert_entity(&line, &opts(false)) {
            EntityOutcome::Converted {
                geometry, warnings, ..
            } => {
                assert_eq!(geometry, CadGeometry::Line(vec![(0.0, 0.0), (4.0, 5.0)]));
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
                CadGeometry::Line(coordinates) => {
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
                CadGeometry::Polygon(coordinates) => {
                    assert_eq!(coordinates.len(), 1);
                    let ring = &coordinates[0];
                    assert_eq!(ring.first(), ring.last());
                    let tuples: Vec<(f64, f64)> = ring
                        .iter()
                        .map(|position| (position.0, position.1))
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
                    CadGeometry::Line(coordinates) => {
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
                CadGeometry::Line(coordinates) => {
                    assert_eq!(coordinates.first(), coordinates.last(), "ring must close");
                    assert!(coordinates.len() > 12);
                    for position in &coordinates {
                        let radius = ((position.0 - 5.0).powi(2) + position.1.powi(2)).sqrt();
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
                    CadGeometry::Line(coordinates) => {
                        assert_eq!(coordinates.first(), coordinates.last());
                        assert!(coordinates.len() >= 25, "got {}", coordinates.len());
                        for position in &coordinates {
                            let radius =
                                ((position.0 - 5.0).powi(2) + (position.1 + 2.0).powi(2)).sqrt();
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
                CadGeometry::Polygon(coordinates) => {
                    let ring = &coordinates[0];
                    assert_eq!(ring.first(), ring.last());
                    let tuples: Vec<(f64, f64)> = ring.iter().map(|p| (p.0, p.1)).collect();
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
                CadGeometry::Line(coordinates) => {
                    let first = coordinates.first().expect("start");
                    let last = coordinates.last().expect("end");
                    assert!((first.0 - 0.0).abs() < 1e-9 && (first.1 + 1.0).abs() < 1e-9);
                    assert!((last.0 - 0.0).abs() < 1e-9 && (last.1 - 1.0).abs() < 1e-9);
                    // Passes through (1, 0), never through (-1, 0).
                    assert!(coordinates.iter().all(|p| p.0 > -1e-9));
                    for position in &coordinates {
                        let radius = (position.0.powi(2) + position.1.powi(2)).sqrt();
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
                CadGeometry::Line(coordinates) => {
                    assert_eq!(coordinates.first(), coordinates.last());
                    assert!(coordinates.len() > 20);
                    for position in &coordinates {
                        let ellipse_eq = ((position.0 - 10.0) / 4.0).powi(2)
                            + ((position.1 - 5.0) / 2.0).powi(2);
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
                CadGeometry::Line(coordinates) => {
                    let first = coordinates.first().expect("start");
                    let last = coordinates.last().expect("end");
                    assert!((first.0 - 4.0).abs() < 1e-9 && first.1.abs() < 1e-9);
                    assert!(last.0.abs() < 1e-9 && (last.1 - 2.0).abs() < 1e-9);
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
                    CadGeometry::Line(coordinates) => {
                        let first = coordinates.first().expect("start");
                        let last = coordinates.last().expect("end");
                        assert!(first.0.abs() < 1e-9 && first.1.abs() < 1e-9);
                        assert!((last.0 - 3.0).abs() < 1e-9 && (last.1 - 3.0).abs() < 1e-9);
                        for position in &coordinates {
                            assert!(
                                (position.0 - position.1).abs() < 1e-9,
                                "off line: {position:?}"
                            );
                        }
                    }
                    other => panic!("expected LineString, got {other:?}"),
                }
                assert!(
                    warnings
                        .iter()
                        .any(|w| w.contains("spline tessellated") && w.contains("chord tolerance")),
                    "{warnings:?}"
                );
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
                assert_eq!(geometry, CadGeometry::Point((7.0, 8.0)));
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
                assert_eq!(geometry, CadGeometry::Point((1.0, 2.0)));
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
                CadGeometry::Line(coordinates) => {
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
                    CadGeometry::Line(coordinates) => {
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
    fn face3d_quad_becomes_closed_ccw_polygon_and_drops_z() {
        use acadrust::entities::Face3D;
        use acadrust::types::Vector3;

        // Clockwise input verifies that the exterior ring is reoriented.
        let face = EntityType::Face3D(Face3D::new(
            Vector3::new(0.0, 0.0, 2.0),
            Vector3::new(0.0, 10.0, 2.0),
            Vector3::new(10.0, 10.0, 2.0),
            Vector3::new(10.0, 0.0, 2.0),
        ));
        match convert_entity(&face, &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                warnings,
            } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon, got {geometry:?}");
                };
                assert_eq!(coordinates.len(), 1);
                let ring = &coordinates[0];
                assert_eq!(ring.len(), 5);
                assert_eq!(ring.first(), ring.last());
                let tuples: Vec<(f64, f64)> = ring
                    .iter()
                    .map(|position| (position.0, position.1))
                    .collect();
                assert!(signed_area(&tuples) > 0.0, "ring must be CCW");
                assert_eq!(
                    extra_properties
                        .iter()
                        .find(|(key, _)| *key == "is_closed")
                        .map(|(_, value)| value),
                    Some(&serde_json::json!(true))
                );
                assert!(warnings.iter().any(|w| w.contains("z coordinates dropped")));
            }
            _ => panic!("3DFACE quad must convert"),
        }
    }

    #[test]
    fn face3d_triangle_collapses_duplicated_fourth_corner() {
        use acadrust::entities::Face3D;
        use acadrust::types::Vector3;

        let face = EntityType::Face3D(Face3D::triangle(
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(5.0, 0.0, 0.0),
            Vector3::new(0.0, 5.0, 0.0),
        ));
        match convert_entity(&face, &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => match geometry {
                CadGeometry::Polygon(coordinates) => {
                    assert_eq!(coordinates.len(), 1);
                    assert_eq!(coordinates[0].len(), 4);
                    assert_eq!(coordinates[0].first(), coordinates[0].last());
                }
                other => panic!("expected Polygon, got {other:?}"),
            },
            _ => panic!("triangular 3DFACE must convert"),
        }
    }

    #[test]
    fn degenerate_face3d_is_skipped() {
        use acadrust::entities::Face3D;
        use acadrust::types::Vector3;

        let face = EntityType::Face3D(Face3D::new(
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(2.0, 2.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(2.0, 2.0, 0.0),
        ));
        match convert_entity(&face, &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("degenerate")),
            _ => panic!("degenerate 3DFACE must be skipped"),
        }
    }

    #[test]
    fn face3d_with_non_finite_corner_fails() {
        use acadrust::entities::Face3D;
        use acadrust::types::Vector3;

        let face = EntityType::Face3D(Face3D::triangle(
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(f64::NAN, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
        ));
        match convert_entity(&face, &opts(false)) {
            EntityOutcome::Failed(reason) => assert_eq!(reason, "non-finite coordinates"),
            _ => panic!("non-finite 3DFACE must fail"),
        }
    }

    #[test]
    fn unsupported_types_are_counted_and_paper_space_is_excluded() {
        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        document
            .add_entity(EntityType::Ray(acadrust::entities::Ray::new(
                acadrust::types::Vector3::new(0.0, 0.0, 0.0),
                acadrust::types::Vector3::new(1.0, 0.0, 0.0),
            )))
            .expect("add ray");
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
        assert_eq!(skipped[0].0, "RAY");
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

    fn string_id(feature: &super::CadFeature) -> String {
        feature.id.clone()
    }

    /// Rendered GeoJSON properties of a CAD feature (via the writer), for
    /// property-assertion tests.
    fn props(feature: &super::CadFeature) -> geojson::JsonObject {
        super::writer::to_geojson(feature)
            .properties
            .expect("properties")
    }

    /// Minimal CAD feature for pipeline-helper unit tests.
    fn cad_feature(id: &str, geometry: CadGeometry) -> super::CadFeature {
        super::CadFeature {
            id: id.to_string(),
            entity_type: "TEST".to_string(),
            handle: "0x0".to_string(),
            layer: "0".to_string(),
            source_layer: None,
            block_path: Vec::new(),
            extra_properties: Vec::new(),
            warnings: Vec::new(),
            geometry,
        }
    }

    /// Fold features through the incremental geometry-check helper.
    fn check_all(features: &[super::CadFeature]) -> crate::report::GeometryChecks {
        let mut checks = super::empty_geometry_checks();
        for feature in features {
            super::update_geometry_checks(&mut checks, feature);
        }
        checks
    }

    /// Bounding box over features via the incremental accumulator.
    fn bbox_all(features: &[super::CadFeature]) -> Option<[f64; 4]> {
        let mut bbox = None;
        for feature in features {
            super::accumulate_bbox(&mut bbox, feature);
        }
        bbox
    }

    /// Outlier scan over features via the collect-then-finalize helpers.
    fn outliers_all(features: &[super::CadFeature]) -> crate::report::SpatialOutliers {
        let mut centers = Vec::new();
        for feature in features {
            if let Some((x, y)) = super::feature_center(feature) {
                centers.push((feature.id.clone(), x, y));
            }
        }
        super::finalize_spatial_outliers(&centers)
    }

    /// Classify features against a boundary file via the streaming index.
    fn boundary_all(
        features: &[super::CadFeature],
        path: &std::path::Path,
    ) -> anyhow::Result<crate::report::BoundaryCheck> {
        let index = super::BoundaryIndex::load(path)?;
        let mut tally = super::BoundaryTally::default();
        for feature in features {
            index.classify(feature, &mut tally);
        }
        Ok(index.into_report(tally))
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
        let geometry = &feature.geometry;
        let CadGeometry::Line(coordinates) = geometry else {
            panic!("expected LineString, got {geometry:?}");
        };
        // (0,0) maps to the insertion point; (1,0) scales to (2,0), rotates
        // 90 degrees to (0,2), and shifts to (10,2).
        assert!((coordinates[0].0 - 10.0).abs() < 1e-9 && coordinates[0].1.abs() < 1e-9);
        assert!((coordinates[1].0 - 10.0).abs() < 1e-9 && (coordinates[1].1 - 2.0).abs() < 1e-9);
        let properties = props(feature);
        assert_eq!(properties.get("block_path"), Some(&JsonValue::from("PART")));
        let id = string_id(feature);
        assert!(
            id.contains('/'),
            "id must be prefixed by the insert chain: {id}"
        );
    }

    #[test]
    fn insert_translates_face3d_block_geometry() {
        use acadrust::entities::{Face3D, Insert};
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "FACE",
            Vector3::ZERO,
            vec![EntityType::Face3D(Face3D::new(
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(2.0, 0.0, 0.0),
                Vector3::new(2.0, 1.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ))],
        );
        document
            .add_entity(EntityType::Insert(Insert::new(
                "FACE",
                Vector3::new(10.0, 20.0, 0.0),
            )))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.converted.get("3DFACE"), Some(&1));
        let geometry = &extraction.features[0].geometry;
        let CadGeometry::Polygon(coordinates) = geometry else {
            panic!("expected Polygon, got {geometry:?}");
        };
        let ring: Vec<(f64, f64)> = coordinates[0]
            .iter()
            .map(|position| (position.0, position.1))
            .collect();
        assert_eq!(
            ring,
            vec![
                (10.0, 20.0),
                (12.0, 20.0),
                (12.0, 21.0),
                (10.0, 21.0),
                (10.0, 20.0),
            ]
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
        let geometry = &feature.geometry;
        assert_eq!(*geometry, CadGeometry::Point((13.0, 0.0)));
        let properties = props(feature);
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
        let properties = props(&extraction.features[0]);
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
        let geometry = &feature.geometry;
        assert_eq!(*geometry, CadGeometry::Point((5.0, 6.0)));
        let properties = props(feature);
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
            .map(|feature| feature.geometry.clone())
            .collect();
        assert_eq!(points[0], CadGeometry::Point((0.0, 0.0)));
        assert_eq!(points[1], CadGeometry::Point((5.0, 0.0)));
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
        use acadrust::types::{Color, LineWeight};
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut layer = Layer::new("PIPES");
        layer.color = Color::GREEN;
        layer.line_type = "CENTER".to_string();
        layer.line_weight = LineWeight::Value(50); // 0.50 mm
        document.layers.add(layer).expect("add layer");

        let mut point = EntityType::Point(Point::from_coords(1.0, 1.0, 0.0));
        point.common_mut().layer = "PIPES".to_string();
        document.add_entity(point).expect("add point");

        let extraction = extract(&document, &opts(false)).expect("extract");

        let properties = props(&extraction.features[0]);
        assert_eq!(properties.get("color_index"), Some(&JsonValue::from(3)));
        assert_eq!(
            properties.get("color_rgb"),
            Some(&JsonValue::from("#00FF00"))
        );
        assert_eq!(properties.get("linetype"), Some(&JsonValue::from("CENTER")));
        assert_eq!(
            properties.get("lineweight_mm"),
            Some(&JsonValue::from(0.5)),
            "ByLayer line weight should resolve from the layer table"
        );
        assert!(properties.get("color").is_none());
    }

    #[test]
    fn byblock_styles_resolve_through_the_insert_chain() {
        use acadrust::entities::Insert;
        use acadrust::types::{Color, LineWeight, Vector3};
        use geojson::JsonValue;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut block_point = EntityType::Point(Point::from_coords(0.0, 0.0, 0.0));
        block_point.common_mut().color = Color::ByBlock;
        block_point.common_mut().linetype = "BYBLOCK".to_string();
        block_point.common_mut().line_weight = LineWeight::ByBlock;
        add_block(&mut document, "SYM", Vector3::ZERO, vec![block_point]);

        let mut insert = EntityType::Insert(Insert::new("SYM", Vector3::ZERO));
        insert.common_mut().color = Color::RED;
        insert.common_mut().linetype = "DASHED".to_string();
        insert.common_mut().line_weight = LineWeight::Value(50); // 0.50 mm
        document.add_entity(insert).expect("add insert");

        // A second ByBlock point directly in model space stays unresolved.
        let mut loose_point = EntityType::Point(Point::from_coords(9.0, 9.0, 0.0));
        loose_point.common_mut().color = Color::ByBlock;
        loose_point.common_mut().linetype = "ByBlock".to_string();
        document.add_entity(loose_point).expect("add point");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.features.len(), 2);
        let block_properties = props(&extraction.features[0]);
        assert_eq!(
            block_properties.get("color_index"),
            Some(&JsonValue::from(1))
        );
        assert_eq!(
            block_properties.get("linetype"),
            Some(&JsonValue::from("DASHED"))
        );
        assert_eq!(
            block_properties.get("lineweight_mm"),
            Some(&JsonValue::from(0.5)),
            "ByBlock line weight should inherit the INSERT's weight"
        );
        let loose_properties = props(&extraction.features[1]);
        assert_eq!(
            loose_properties.get("color"),
            Some(&JsonValue::from("ByBlock"))
        );
        assert_eq!(
            loose_properties.get("linetype"),
            Some(&JsonValue::from("ByBlock"))
        );
        assert!(
            loose_properties.get("lineweight_mm").is_none(),
            "ByBlock outside a block stays unresolved"
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

        let properties = props(&extraction.features[0]);
        let Some(JsonValue::Number(rotation)) = properties.get("text_rotation_deg") else {
            panic!("text_rotation_deg must be present");
        };
        let rotation = rotation.as_f64().expect("finite rotation");
        assert!(
            (rotation - 120.0).abs() < 1e-9,
            "expected 120 degrees, got {rotation}"
        );
    }

    fn square_path(min: (f64, f64), max: (f64, f64)) -> acadrust::entities::BoundaryPath {
        use acadrust::entities::{BoundaryEdge, BoundaryPath, PolylineEdge};
        use acadrust::types::Vector2;

        let mut path = BoundaryPath::new();
        path.add_edge(BoundaryEdge::Polyline(PolylineEdge::new(
            vec![
                Vector2::new(min.0, min.1),
                Vector2::new(max.0, min.1),
                Vector2::new(max.0, max.1),
                Vector2::new(min.0, max.1),
            ],
            true,
        )));
        path
    }

    fn ring_tuples(ring: &[(f64, f64)]) -> Vec<(f64, f64)> {
        ring.to_vec()
    }

    #[test]
    fn hatch_island_becomes_polygon_with_ccw_shell_and_cw_hole() {
        use acadrust::entities::Hatch;
        use geojson::JsonValue;

        let mut hatch = Hatch::new();
        hatch.paths.push(square_path((0.0, 0.0), (10.0, 10.0)));
        hatch.paths.push(square_path((2.0, 2.0), (4.0, 4.0)));

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                ..
            } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon");
                };
                assert_eq!(coordinates.len(), 2, "one shell and one hole");
                let shell = ring_tuples(&coordinates[0]);
                let hole = ring_tuples(&coordinates[1]);
                assert_eq!(shell.first(), shell.last());
                assert!(signed_area(&shell) > 0.0, "shell must be CCW");
                assert!(signed_area(&hole) < 0.0, "hole must be CW");
                let get = |key: &str| {
                    extra_properties
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| v.clone())
                };
                assert_eq!(get("hatch_solid"), Some(JsonValue::Bool(true)));
                assert_eq!(get("is_closed"), Some(JsonValue::Bool(true)));
            }
            other => panic!("hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn hatch_disjoint_loops_become_a_multipolygon() {
        use acadrust::entities::Hatch;

        let mut hatch = Hatch::new();
        hatch.paths.push(square_path((0.0, 0.0), (10.0, 10.0)));
        hatch.paths.push(square_path((20.0, 0.0), (30.0, 10.0)));

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => {
                let CadGeometry::MultiPolygon(coordinates) = geometry else {
                    panic!("expected MultiPolygon");
                };
                assert_eq!(coordinates.len(), 2);
                assert_eq!(coordinates[0].len(), 1);
                assert_eq!(coordinates[1].len(), 1);
            }
            other => panic!("hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn hatch_arc_edges_connect_and_mark_approximated() {
        use acadrust::entities::{BoundaryEdge, BoundaryPath, CircularArcEdge, Hatch, LineEdge};
        use acadrust::types::Vector2;
        use geojson::JsonValue;

        let mut path = BoundaryPath::new();
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(0.0, 0.0),
            end: Vector2::new(10.0, 0.0),
        }));
        path.add_edge(BoundaryEdge::CircularArc(CircularArcEdge {
            center: Vector2::new(5.0, 0.0),
            radius: 5.0,
            start_angle: 0.0,
            end_angle: std::f64::consts::PI,
            counter_clockwise: true,
        }));
        let mut hatch = Hatch::new();
        hatch.paths.push(path);

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                warnings,
            } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon");
                };
                let shell = ring_tuples(&coordinates[0]);
                assert!(shell.len() > 4, "arc must be tessellated");
                assert_eq!(shell.first(), shell.last());
                assert!(
                    !warnings.iter().any(|w| w.contains("bridged")),
                    "edges connect without repair: {warnings:?}"
                );
                assert!(
                    extra_properties
                        .iter()
                        .any(|(k, v)| *k == "approximated" && *v == JsonValue::Bool(true))
                );
            }
            other => panic!("hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn hatch_reversed_edges_connect_without_repair_warnings() {
        use acadrust::entities::{BoundaryEdge, BoundaryPath, Hatch, LineEdge};
        use acadrust::types::Vector2;

        // Second and third edges are stored backwards; connection must flip
        // them instead of bridging gaps.
        let mut path = BoundaryPath::new();
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(0.0, 0.0),
            end: Vector2::new(10.0, 0.0),
        }));
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(5.0, 8.0),
            end: Vector2::new(10.0, 0.0),
        }));
        let mut hatch = Hatch::new();
        hatch.paths.push(path);

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Converted { warnings, .. } => {
                assert!(
                    !warnings.iter().any(|w| w.contains("bridged")),
                    "reversed edge must connect: {warnings:?}"
                );
                // The triangle is open between (5,8) and (0,0): closure is a
                // repair and must be reported.
                assert!(
                    warnings.iter().any(|w| w.contains("closed across")),
                    "open loop closure must warn: {warnings:?}"
                );
            }
            other => panic!("hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn hatch_with_only_degenerate_loops_is_skipped() {
        use acadrust::entities::{BoundaryEdge, BoundaryPath, Hatch, LineEdge};
        use acadrust::types::Vector2;

        let mut path = BoundaryPath::new();
        path.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(0.0, 0.0),
            end: Vector2::new(1.0, 0.0),
        }));
        let mut hatch = Hatch::new();
        hatch.paths.push(path);

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Skipped(reason) => {
                assert!(reason.contains("no valid boundary loops"), "{reason}");
            }
            other => panic!("degenerate hatch must be skipped, got {other:?}"),
        }

        let mut mixed = Hatch::new();
        let mut degenerate = BoundaryPath::new();
        degenerate.add_edge(BoundaryEdge::Line(LineEdge {
            start: Vector2::new(0.0, 0.0),
            end: Vector2::new(1.0, 0.0),
        }));
        mixed.paths.push(square_path((0.0, 0.0), (10.0, 10.0)));
        mixed.paths.push(degenerate);
        match convert_entity(&EntityType::Hatch(mixed), &opts(false)) {
            EntityOutcome::Converted {
                extra_properties,
                warnings,
                ..
            } => {
                assert!(
                    extra_properties
                        .iter()
                        .any(|(k, v)| *k == "hatch_loops_dropped" && *v == serde_json::json!(1)),
                    "dropped loop must be counted: {extra_properties:?}"
                );
                assert!(warnings.iter().any(|w| w.contains("dropped")));
            }
            other => panic!("mixed hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn solid_bowtie_corner_order_becomes_a_proper_quad() {
        use acadrust::entities::Solid;
        use acadrust::types::Vector3;

        // DXF stores the quad as 1,2,3,4 with 3/4 swapped visually: this
        // input is a unit square only when read as first-second-fourth-third.
        let solid = Solid::new(
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(1.0, 0.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
        );
        match convert_entity(&EntityType::Solid(solid), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon");
                };
                let ring = ring_tuples(&coordinates[0]);
                assert_eq!(ring.len(), 5);
                assert_eq!(ring.first(), ring.last());
                assert!(signed_area(&ring) > 0.0, "ring must be CCW");
                // A proper square has area 1; a bow-tie would cancel to ~0.
                assert!(
                    (signed_area(&ring) - 1.0).abs() < 1e-9,
                    "corner order must untwist the bow-tie: area {}",
                    signed_area(&ring)
                );
            }
            other => panic!("solid must convert, got {other:?}"),
        }
    }

    #[test]
    fn triangular_and_degenerate_solids() {
        use acadrust::entities::Solid;
        use acadrust::types::Vector3;

        let triangle = Solid::new(
            Vector3::new(0.0, 0.0, 0.0),
            Vector3::new(2.0, 0.0, 0.0),
            Vector3::new(1.0, 2.0, 0.0),
            Vector3::new(1.0, 2.0, 0.0),
        );
        match convert_entity(&EntityType::Solid(triangle), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon");
                };
                assert_eq!(coordinates[0].len(), 4, "triangle ring has 4 positions");
            }
            other => panic!("triangular solid must convert, got {other:?}"),
        }

        let degenerate = Solid::new(
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
            Vector3::new(1.0, 1.0, 0.0),
        );
        match convert_entity(&EntityType::Solid(degenerate), &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("degenerate"), "{reason}"),
            other => panic!("degenerate solid must be skipped, got {other:?}"),
        }
    }

    #[test]
    fn wgs84_extent_check_flags_out_of_range_coordinates() {
        use super::{crs_is_wgs84, wgs84_violation};

        assert!(wgs84_violation(&cad_feature("A", CadGeometry::Point((-51.2, -23.4)))).is_none());
        assert!(wgs84_violation(&cad_feature("B", CadGeometry::Point((180.0, 90.0)))).is_none());

        // UTM-magnitude coordinates delivered as "WGS 84" must be flagged.
        let bad = cad_feature("B", CadGeometry::Point((248_000.0, 7_396_000.0)));
        let (x, _y) = wgs84_violation(&bad).expect("must flag");
        assert_eq!(x, 248_000.0);

        assert!(crs_is_wgs84("epsg:4326"));
        assert!(crs_is_wgs84(" OGC:CRS84 "));
        assert!(!crs_is_wgs84("EPSG:31982"));
    }

    #[test]
    fn geometry_checks_count_violations_and_pass_valid_output() {
        // A valid CCW closed square and a line: no violations.
        let valid = vec![
            cad_feature(
                "sq",
                CadGeometry::Polygon(vec![vec![
                    (0.0, 0.0),
                    (10.0, 0.0),
                    (10.0, 10.0),
                    (0.0, 10.0),
                    (0.0, 0.0),
                ]]),
            ),
            cad_feature("ln", CadGeometry::Line(vec![(0.0, 0.0), (5.0, 5.0)])),
        ];
        let checks = check_all(&valid);
        assert_eq!(checks.features_checked, 2);
        assert_eq!(checks.rings_checked, 1);
        assert_eq!(
            checks.empty_geometries
                + checks.non_finite_coordinates
                + checks.duplicate_vertex_features
                + checks.unclosed_rings
                + checks.misoriented_rings
                + checks.degenerate_rings,
            0
        );
        assert_eq!(bbox_all(&valid), Some([0.0, 0.0, 10.0, 10.0]));

        // A CW shell that is also unclosed, a degenerate ring, a NaN line
        // with a duplicate vertex, and an empty line.
        let invalid = vec![
            cad_feature(
                "cw",
                CadGeometry::Polygon(vec![
                    vec![(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (10.0, 0.0)],
                    vec![(1.0, 1.0), (2.0, 2.0)],
                ]),
            ),
            cad_feature(
                "nan",
                CadGeometry::Line(vec![(f64::NAN, 0.0), (1.0, 1.0), (1.0, 1.0)]),
            ),
            cad_feature("empty", CadGeometry::Line(Vec::new())),
        ];
        let checks = check_all(&invalid);
        assert_eq!(checks.rings_checked, 2);
        assert_eq!(checks.unclosed_rings, 1);
        assert_eq!(checks.misoriented_rings, 1);
        assert_eq!(checks.degenerate_rings, 1);
        assert_eq!(checks.non_finite_coordinates, 1);
        assert_eq!(checks.duplicate_vertex_features, 1);
        assert_eq!(checks.empty_geometries, 1);
    }

    #[test]
    fn spatial_outliers_flag_far_features_only() {
        // A tight cluster around (248000, 7396000) plus one title block at
        // the drawing origin.
        let mut features: Vec<super::CadFeature> = (0..20)
            .map(|i| {
                cad_feature(
                    &format!("C{i}"),
                    CadGeometry::Point((
                        248_000.0 + f64::from(i) * 10.0,
                        7_396_000.0 + f64::from(i) * 5.0,
                    )),
                )
            })
            .collect();
        features.push(cad_feature("SHEET", CadGeometry::Point((0.0, 0.0))));

        let scan = outliers_all(&features);
        assert_eq!(scan.features_checked, 21);
        assert_eq!(scan.outlier_features, 1);
        assert_eq!(scan.sample_ids, vec!["SHEET".to_string()]);
        assert!(
            (scan.center[0] - 248_090.0).abs() < 1e-9,
            "{:?}",
            scan.center
        );

        // Without the outlier nothing is flagged.
        features.pop();
        let scan = outliers_all(&features);
        assert_eq!(scan.outlier_features, 0);
    }

    #[test]
    fn boundary_check_classifies_containment_with_holes() {
        use std::io::Write;

        // Boundary: 0..10 square with a 4..6 hole.
        let boundary = serde_json::json!({
            "type": "Feature",
            "properties": {},
            "geometry": {
                "type": "Polygon",
                "coordinates": [
                    [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [0.0, 0.0]],
                    [[4.0, 4.0], [6.0, 4.0], [6.0, 6.0], [4.0, 6.0], [4.0, 4.0]]
                ]
            }
        });
        let mut file = tempfile::NamedTempFile::new().expect("temp boundary");
        write!(file, "{boundary}").expect("write boundary");

        let features = vec![
            cad_feature("IN", CadGeometry::Point((2.0, 2.0))),
            cad_feature("IN-HOLE", CadGeometry::Point((5.0, 5.0))),
            cad_feature("OUT", CadGeometry::Point((20.0, 20.0))),
            cad_feature("PARTIAL", CadGeometry::Line(vec![(2.0, 2.0), (20.0, 2.0)])),
        ];

        let check = boundary_all(&features, file.path()).expect("boundary check");
        assert_eq!(check.polygons, 1);
        assert_eq!(check.features_inside, 1);
        assert_eq!(check.features_partial, 1);
        assert_eq!(check.features_outside, 2, "the hole counts as outside");
        assert!(check.sample_not_inside_ids.contains(&"OUT".to_string()));
        assert!(check.sample_not_inside_ids.contains(&"IN-HOLE".to_string()));

        // A boundary without polygons is an actionable error.
        let mut bad = tempfile::NamedTempFile::new().expect("temp boundary");
        write!(bad, r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[0,0]}}}}"#)
            .expect("write");
        let Err(error) = boundary_all(&features, bad.path()) else {
            panic!("polygon-free boundary must fail");
        };
        assert!(format!("{error:#}").contains("no Polygon"));
    }

    #[test]
    fn accounting_balances_across_outcomes_and_insert_expansion() {
        use acadrust::entities::{Insert, Ray};
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "SYM",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(1.0, 1.0, 0.0))],
        );
        document
            .add_entity(EntityType::Point(Point::from_coords(0.0, 0.0, 0.0)))
            .expect("add point");
        document
            .add_entity(EntityType::Ray(Ray::new(
                Vector3::ZERO,
                Vector3::new(1.0, 0.0, 0.0),
            )))
            .expect("add unsupported ray");
        document
            .add_entity(EntityType::Insert(Insert::new("SYM", Vector3::ZERO)))
            .expect("add insert");
        document
            .add_entity(EntityType::Insert(Insert::new("MISSING", Vector3::ZERO)))
            .expect("add broken insert");

        let extraction = extract(&document, &opts(false)).expect("extract");

        assert_eq!(extraction.model_space_entities, 4);
        assert_eq!(
            extraction.top_level_accounted, extraction.model_space_entities,
            "every top-level entity must reach exactly one outcome"
        );
    }

    #[test]
    fn insert_with_non_default_normal_lifts_insertion_point_from_ocs() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;

        // Audit case A1: normal (0,1,0), OCS insertion (10,20,30), block
        // point at the origin. Arbitrary axis maps it to WCS (-10,30,20),
        // so the 2D feature must land at (-10, 30).
        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "P",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(0.0, 0.0, 0.0))],
        );
        let mut insert = Insert::new("P", Vector3::new(10.0, 20.0, 30.0));
        insert.normal = Vector3::new(0.0, 1.0, 0.0);
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");
        assert_eq!(extraction.features.len(), 1);
        let geometry = &extraction.features[0].geometry;
        let CadGeometry::Point((x, y)) = geometry else {
            panic!("expected Point");
        };
        assert!(
            (x - -10.0).abs() < 1e-9 && (y - 30.0).abs() < 1e-9,
            "got ({x}, {y})"
        );
    }

    #[test]
    fn zero_sweep_arcs_are_skipped_and_full_turns_stay_full() {
        use acadrust::entities::Arc;
        use acadrust::types::Vector3;

        let mut zero = Arc::new();
        zero.center = Vector3::new(0.0, 0.0, 0.0);
        zero.radius = 5.0;
        zero.start_angle = 0.75;
        zero.end_angle = 0.75;
        match convert_entity(&EntityType::Arc(zero), &opts(false)) {
            EntityOutcome::Skipped(reason) => assert!(reason.contains("zero"), "{reason}"),
            other => panic!("zero-sweep arc must be skipped, got {other:?}"),
        }

        let mut full = Arc::new();
        full.center = Vector3::new(0.0, 0.0, 0.0);
        full.radius = 5.0;
        full.start_angle = 0.0;
        full.end_angle = std::f64::consts::TAU;
        match convert_entity(&EntityType::Arc(full), &opts(false)) {
            EntityOutcome::Converted { geometry, .. } => {
                let CadGeometry::Line(coordinates) = geometry else {
                    panic!("expected LineString");
                };
                assert!(coordinates.len() > 8, "full turn must tessellate");
            }
            other => panic!("full-turn arc must convert, got {other:?}"),
        }
    }

    #[test]
    fn zero_normals_fail_instead_of_collapsing_geometry() {
        let mut circle = Circle::from_coords(10.0, 20.0, 3.0, 5.0);
        circle.normal = acadrust::types::Vector3::new(0.0, 0.0, 0.0);
        match convert_entity(&EntityType::Circle(circle), &opts(false)) {
            EntityOutcome::Failed(reason) => {
                assert!(reason.contains("extrusion normal"), "{reason}");
            }
            other => panic!("zero-normal circle must fail, got {other:?}"),
        }
    }

    #[test]
    fn invalid_normal_insert_stays_in_the_accounting() {
        use acadrust::entities::Insert;
        use acadrust::types::Vector3;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        add_block(
            &mut document,
            "P",
            Vector3::ZERO,
            vec![EntityType::Point(Point::from_coords(0.0, 0.0, 0.0))],
        );
        let mut insert = Insert::new("P", Vector3::ZERO);
        insert.normal = Vector3::new(0.0, 0.0, 0.0);
        document
            .add_entity(EntityType::Insert(insert))
            .expect("add insert");

        let extraction = extract(&document, &opts(false)).expect("extract");
        // The failed INSERT must count toward the denominator so accounting
        // still balances (an early return must not skip the increment).
        assert_eq!(extraction.model_space_entities, 1);
        assert_eq!(extraction.top_level_accounted, 1);
        assert!(
            extraction
                .failed
                .keys()
                .any(|(entity_type, reason)| entity_type == "INSERT"
                    && reason.contains("extrusion normal")),
            "{:?}",
            extraction.failed.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn collinear_hatch_loops_are_dropped_as_zero_area() {
        use acadrust::entities::{BoundaryEdge, Hatch, PolylineEdge};
        use acadrust::types::Vector2;

        let mut hatch = Hatch::new();
        hatch.paths.push(square_path((0.0, 0.0), (10.0, 10.0)));
        let mut collinear = acadrust::entities::BoundaryPath::new();
        collinear.add_edge(BoundaryEdge::Polyline(PolylineEdge::new(
            vec![
                Vector2::new(2.0, 2.0),
                Vector2::new(3.0, 2.0),
                Vector2::new(4.0, 2.0),
            ],
            true,
        )));
        hatch.paths.push(collinear);

        match convert_entity(&EntityType::Hatch(hatch), &opts(false)) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                warnings,
            } => {
                let CadGeometry::Polygon(coordinates) = geometry else {
                    panic!("expected Polygon");
                };
                assert_eq!(coordinates.len(), 1, "zero-area hole must be dropped");
                assert!(
                    extra_properties
                        .iter()
                        .any(|(k, v)| *k == "hatch_loops_dropped" && *v == serde_json::json!(1)),
                    "{extra_properties:?}"
                );
                assert!(
                    warnings.iter().any(|w| w.contains("zero-area")),
                    "{warnings:?}"
                );
            }
            other => panic!("hatch must convert, got {other:?}"),
        }
    }

    #[test]
    fn nurbs_evaluation_is_invariant_to_knot_domain_scale() {
        use super::evaluate_nurbs;

        let control = [
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
            [2.0, 0.0, 0.0, 1.0],
        ];
        let knots_unit = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        let knots_tiny: Vec<f64> = knots_unit.iter().map(|k| k * 1e-20).collect();
        let at_unit = evaluate_nurbs(0.5, 2, &knots_unit, &control).expect("evaluate");
        let at_tiny = evaluate_nurbs(0.5e-20, 2, &knots_tiny, &control).expect("evaluate");
        assert!(
            (at_unit.0 - at_tiny.0).abs() < 1e-9 && (at_unit.1 - at_tiny.1).abs() < 1e-9,
            "{at_unit:?} vs {at_tiny:?}"
        );
        assert!((at_unit.0 - 1.0).abs() < 1e-12 && (at_unit.1 - 0.5).abs() < 1e-12);
    }

    #[test]
    fn unresolved_model_space_handles_stay_in_the_accounting() {
        use acadrust::types::Handle;

        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        document
            .add_entity(EntityType::Point(Point::from_coords(1.0, 1.0, 0.0)))
            .expect("add point");
        document
            .block_records
            .get_mut("*Model_Space")
            .expect("model space record")
            .entity_handles
            .push(Handle::new(0xDEAD));

        let extraction = extract(&document, &opts(false)).expect("extract");
        assert_eq!(extraction.model_space_entities, 2);
        assert_eq!(extraction.top_level_accounted, 2);
        assert!(
            extraction
                .failed
                .keys()
                .any(|(entity_type, _)| entity_type == "UNRESOLVED"),
            "{:?}",
            extraction.failed.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn boundary_segments_crossing_the_boundary_force_partial() {
        use std::io::Write;

        // U-shaped boundary: a 4x4 square with a notch cut from the top
        // between x=1..3 down to y=1.
        let boundary = serde_json::json!({
            "type": "Polygon",
            "coordinates": [[
                [0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [3.0, 4.0], [3.0, 1.0],
                [1.0, 1.0], [1.0, 4.0], [0.0, 4.0], [0.0, 0.0]
            ]]
        });
        let mut file = tempfile::NamedTempFile::new().expect("temp boundary");
        write!(file, "{boundary}").expect("write boundary");

        // Both endpoints are inside the arms of the U, but the segment
        // crosses the notch.
        let feature = cad_feature("BRIDGE", CadGeometry::Line(vec![(0.5, 3.0), (3.5, 3.0)]));
        let check = boundary_all(&[feature], file.path()).expect("boundary check");
        assert_eq!(check.features_partial, 1, "{check:?}");
        assert_eq!(check.features_inside, 0);
    }

    #[test]
    fn malformed_boundaries_error_instead_of_panicking() {
        use std::io::Write;

        let cases = [
            // Position with a single number (previously a panic).
            r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[1],[2],[3],[1]]]}}"#,
            // Open ring.
            r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[10,10],[0,10]]]}"#,
            // Ring with too few positions.
            r#"{"type":"Polygon","coordinates":[[[0,0],[10,0],[0,0]]]}"#,
            // Empty polygon carries no boundary at all.
            r#"{"type":"Polygon","coordinates":[]}"#,
        ];
        for case in cases {
            let mut file = tempfile::NamedTempFile::new().expect("temp boundary");
            write!(file, "{case}").expect("write");
            assert!(
                boundary_all(&[], file.path()).is_err(),
                "must reject: {case}"
            );
        }
    }

    #[test]
    fn millimeter_neighbors_in_tight_clusters_are_not_outliers() {
        let feature = |id: &str, x: f64, y: f64| cad_feature(id, CadGeometry::Point((x, y)));
        let mut features: Vec<super::CadFeature> = (0..20)
            .map(|i| feature(&format!("C{i}"), 248_000.0, 7_396_000.0))
            .collect();
        features.push(feature("NEAR", 248_000.001, 7_396_000.0));

        let scan = outliers_all(&features);
        assert_eq!(scan.outlier_features, 0, "{scan:?}");
    }

    #[test]
    fn native_layer_filters_exclude_top_level_entities() {
        let mut document = CadDocument::with_version(DxfVersion::AC1027);
        let mut on_eixo = EntityType::Point(Point::from_coords(1.0, 1.0, 0.0));
        on_eixo.common_mut().layer = "EIXO".to_string();
        document.add_entity(on_eixo).expect("add point");
        let mut on_pista = EntityType::Point(Point::from_coords(2.0, 2.0, 0.0));
        on_pista.common_mut().layer = "PISTA".to_string();
        document.add_entity(on_pista).expect("add point");

        let mut options = opts(false);
        options.exclude_layers = vec!["eixo".to_string()];
        let extraction = extract(&document, &options).expect("extract");
        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.excluded_by_layer_filter, 1);

        let mut options = opts(false);
        options.include_layers = vec!["eixo".to_string()];
        let extraction = extract(&document, &options).expect("extract");
        assert_eq!(extraction.features.len(), 1);
        assert_eq!(extraction.excluded_by_layer_filter, 1);
        let properties = props(&extraction.features[0]);
        assert_eq!(
            properties.get("layer"),
            Some(&geojson::JsonValue::from("EIXO"))
        );
    }

    #[test]
    fn spline_sampling_is_tolerance_driven() {
        use acadrust::entities::Spline;
        use acadrust::types::Vector3;

        let make = || {
            let mut spline = Spline::new();
            spline.degree = 2;
            spline.control_points = vec![
                Vector3::new(0.0, 0.0, 0.0),
                Vector3::new(50.0, 100.0, 0.0),
                Vector3::new(100.0, 0.0, 0.0),
            ];
            spline.knots = vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
            spline
        };
        let count = |tolerance: f64| -> usize {
            let mut options = opts(false);
            options.curve_tolerance = tolerance;
            match convert_entity(&EntityType::Spline(make()), &options) {
                EntityOutcome::Converted { geometry, .. } => match geometry {
                    CadGeometry::Line(coordinates) => coordinates.len(),
                    other => panic!("expected LineString, got {other:?}"),
                },
                other => panic!("spline must convert, got {other:?}"),
            }
        };

        let coarse = count(5.0);
        let fine = count(0.01);
        assert!(
            fine > coarse,
            "finer tolerance must sample more: coarse {coarse}, fine {fine}"
        );
        assert!(coarse >= 9, "span floor applies: {coarse}");
    }

    fn extra_property<'a>(
        properties: &'a [(&'static str, geojson::JsonValue)],
        key: &str,
    ) -> Option<&'a geojson::JsonValue> {
        properties
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn text_alignment_mode_names_are_stable() {
        use acadrust::entities::{TextHorizontalAlignment as H, TextVerticalAlignment as V};

        for (alignment, expected) in [
            (H::Left, "left"),
            (H::Center, "center"),
            (H::Right, "right"),
            (H::Aligned, "aligned"),
            (H::Middle, "middle"),
            (H::Fit, "fit"),
        ] {
            assert_eq!(super::text_horizontal_alignment_name(alignment), expected);
        }
        for (alignment, expected) in [
            (V::Baseline, "baseline"),
            (V::Bottom, "bottom"),
            (V::Middle, "middle"),
            (V::Top, "top"),
        ] {
            assert_eq!(super::text_vertical_alignment_name(alignment), expected);
        }
    }

    #[test]
    fn text_anchor_follows_dxf_alignment_rules() {
        use acadrust::{
            entities::{Text, TextHorizontalAlignment},
            types::Vector3,
        };

        let mut fit = Text::new();
        fit.insertion_point = Vector3::new(1.0, 2.0, 0.0);
        fit.alignment_point = Some(Vector3::new(7.0, 8.0, 0.0));
        fit.horizontal_alignment = TextHorizontalAlignment::Fit;
        match super::convert_text(&fit, &Placement::model_space()) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                ..
            } => {
                assert_eq!(geometry, CadGeometry::Point((7.0, 8.0)));
                assert_eq!(
                    extra_property(&extra_properties, "text_anchor"),
                    Some(&geojson::JsonValue::from("alignment"))
                );
            }
            _ => panic!("fit TEXT must convert"),
        }

        let mut left_baseline = Text::new();
        left_baseline.insertion_point = Vector3::new(3.0, 4.0, 0.0);
        left_baseline.alignment_point = Some(Vector3::new(30.0, 40.0, 0.0));
        match super::convert_text(&left_baseline, &Placement::model_space()) {
            EntityOutcome::Converted {
                geometry,
                extra_properties,
                ..
            } => {
                assert_eq!(geometry, CadGeometry::Point((3.0, 4.0)));
                assert_eq!(
                    extra_property(&extra_properties, "text_anchor"),
                    Some(&geojson::JsonValue::from("insertion"))
                );
            }
            _ => panic!("left/baseline TEXT must convert"),
        }
    }

    #[test]
    fn text_width_oblique_and_generation_flags_emit_only_when_non_default() {
        use acadrust::entities::Text;

        let mut text = Text::new();
        text.width_factor = 1.25;
        text.oblique_angle = std::f64::consts::FRAC_PI_6;
        text.generation_flags = 2 | 4;
        let EntityOutcome::Converted {
            extra_properties, ..
        } = super::convert_text(&text, &Placement::model_space())
        else {
            panic!("TEXT must convert");
        };

        assert_eq!(
            extra_property(&extra_properties, "text_width_factor"),
            Some(&geojson::JsonValue::from(1.25))
        );
        let oblique = extra_property(&extra_properties, "text_oblique_deg")
            .and_then(geojson::JsonValue::as_f64)
            .expect("numeric oblique angle");
        assert!((oblique - 30.0).abs() < 1e-12, "got {oblique}");
        assert_eq!(
            extra_property(&extra_properties, "text_mirrored_x"),
            Some(&geojson::JsonValue::from(true))
        );
        assert_eq!(
            extra_property(&extra_properties, "text_mirrored_y"),
            Some(&geojson::JsonValue::from(true))
        );
    }

    #[test]
    fn mtext_attachment_names_and_reference_width_are_preserved() {
        use acadrust::entities::{AttachmentPoint as A, MText};

        for (attachment, expected) in [
            (A::TopLeft, "top-left"),
            (A::TopCenter, "top-center"),
            (A::TopRight, "top-right"),
            (A::MiddleLeft, "middle-left"),
            (A::MiddleCenter, "middle-center"),
            (A::MiddleRight, "middle-right"),
            (A::BottomLeft, "bottom-left"),
            (A::BottomCenter, "bottom-center"),
            (A::BottomRight, "bottom-right"),
        ] {
            assert_eq!(super::mtext_attachment_name(attachment), expected);
        }

        let mut mtext = MText::new();
        mtext.attachment_point = A::BottomRight;
        mtext.rectangle_width = 42.0;
        let EntityOutcome::Converted {
            extra_properties, ..
        } = super::convert_mtext(&mtext, &Placement::model_space())
        else {
            panic!("MTEXT must convert");
        };
        assert_eq!(
            extra_property(&extra_properties, "text_attachment"),
            Some(&geojson::JsonValue::from("bottom-right"))
        );
        assert_eq!(
            extra_property(&extra_properties, "text_width"),
            Some(&geojson::JsonValue::from(42.0))
        );
    }

    #[test]
    fn mtext_direction_spacing_and_columns_are_preserved() {
        use acadrust::entities::{DrawingDirection, LineSpacingStyle, MText};

        for (direction, expected) in [
            (DrawingDirection::LeftToRight, "left-to-right"),
            (DrawingDirection::TopToBottom, "top-to-bottom"),
            (DrawingDirection::ByStyle, "by-style"),
        ] {
            assert_eq!(super::mtext_direction_name(direction), expected);
        }

        let mut mtext = MText::new();
        mtext.drawing_direction = DrawingDirection::TopToBottom;
        mtext.line_spacing_factor = 1.5;
        mtext.line_spacing_style = LineSpacingStyle::Exactly;
        mtext.column_data.column_type = 1;
        mtext.column_data.column_count = 2;
        mtext.column_data.width = 12.0;
        mtext.column_data.gutter = 1.25;
        let EntityOutcome::Converted {
            extra_properties, ..
        } = super::convert_mtext(&mtext, &Placement::model_space())
        else {
            panic!("MTEXT must convert");
        };

        assert_eq!(
            extra_property(&extra_properties, "text_direction"),
            Some(&geojson::JsonValue::from("top-to-bottom"))
        );
        assert_eq!(
            extra_property(&extra_properties, "text_line_spacing_factor"),
            Some(&geojson::JsonValue::from(1.5))
        );
        assert_eq!(
            extra_property(&extra_properties, "text_line_spacing_style"),
            Some(&geojson::JsonValue::from("exactly"))
        );
        assert_eq!(
            extra_property(&extra_properties, "text_columns"),
            Some(&serde_json::json!({
                "count": 2,
                "gutter": 1.25,
                "type": "static",
                "width": 12.0
            }))
        );
    }

    #[test]
    fn default_text_and_mtext_emit_no_layout_properties() {
        use acadrust::entities::{MText, Text};

        let EntityOutcome::Converted {
            extra_properties: text_properties,
            ..
        } = super::convert_text(&Text::new(), &Placement::model_space())
        else {
            panic!("default TEXT must convert");
        };
        for key in [
            "text_h_align",
            "text_v_align",
            "text_anchor",
            "text_width_factor",
            "text_oblique_deg",
            "text_mirrored_x",
            "text_mirrored_y",
        ] {
            assert_eq!(extra_property(&text_properties, key), None, "{key}");
        }

        let EntityOutcome::Converted {
            extra_properties: mtext_properties,
            ..
        } = super::convert_mtext(&MText::new(), &Placement::model_space())
        else {
            panic!("default MTEXT must convert");
        };
        for key in [
            "text_attachment",
            "text_direction",
            "text_width",
            "text_line_spacing_factor",
            "text_line_spacing_style",
            "text_columns",
        ] {
            assert_eq!(extra_property(&mtext_properties, key), None, "{key}");
        }
    }

    mod properties {
        use proptest::prelude::*;

        use super::super::{Affine, arc_points, tessellate_bulge};
        use crate::backend::native::calibrate::{Calibration, ControlPoint, solve};

        proptest! {
            /// Every tessellated arc point lies on the circle, the endpoints
            /// are analytically exact, and (when no segment cap fired) each
            /// chord's sagitta respects the requested tolerance.
            #[test]
            fn arc_points_stay_on_the_circle_within_tolerance(
                cx in -1.0e6f64..1.0e6,
                cy in -1.0e6f64..1.0e6,
                radius in 1.0e-3f64..1.0e5,
                start in -std::f64::consts::TAU..std::f64::consts::TAU,
                sweep in 0.01f64..std::f64::consts::TAU,
                negate in proptest::bool::ANY,
                tolerance in 1.0e-4f64..10.0,
            ) {
                let sweep = if negate { -sweep } else { sweep };
                let mut warnings = Vec::new();
                let points = arc_points((cx, cy), radius, start, sweep, tolerance, &mut warnings);

                prop_assert!(points.len() >= 2);
                // Scale-aware equality: coordinates are center +- radius.
                let scale = cx.abs().max(cy.abs()).max(radius).max(1.0);
                let epsilon = 1e-9 * scale;

                let expected_first = (cx + radius * start.cos(), cy + radius * start.sin());
                let end_angle = start + sweep;
                let expected_last = (cx + radius * end_angle.cos(), cy + radius * end_angle.sin());
                prop_assert!((points[0].0 - expected_first.0).abs() <= epsilon);
                prop_assert!((points[0].1 - expected_first.1).abs() <= epsilon);
                let last = points[points.len() - 1];
                prop_assert!((last.0 - expected_last.0).abs() <= epsilon);
                prop_assert!((last.1 - expected_last.1).abs() <= epsilon);

                for (x, y) in &points {
                    let distance = (x - cx).hypot(y - cy);
                    prop_assert!(
                        (distance - radius).abs() <= epsilon,
                        "point off circle by {}",
                        (distance - radius).abs()
                    );
                }

                if warnings.is_empty() {
                    let step = sweep.abs() / (points.len() - 1) as f64;
                    let sagitta = radius * (1.0 - (step / 2.0).cos());
                    prop_assert!(
                        sagitta <= tolerance * (1.0 + 1e-9) + epsilon,
                        "sagitta {sagitta} exceeds tolerance {tolerance}"
                    );
                }
            }

            /// Bulge tessellation returns interior points on the arc through
            /// the two endpoints with the given bulge.
            #[test]
            fn bulge_points_lie_on_the_defined_arc(
                sx in -1.0e4f64..1.0e4,
                sy in -1.0e4f64..1.0e4,
                dx in 0.1f64..1.0e3,
                dy in -1.0e3f64..1.0e3,
                bulge in 0.05f64..1.5,
                negate in proptest::bool::ANY,
                tolerance in 1.0e-3f64..1.0,
            ) {
                let bulge = if negate { -bulge } else { bulge };
                let start = (sx, sy);
                let end = (sx + dx, sy + dy);
                let mut warnings = Vec::new();
                let points = tessellate_bulge(start, end, bulge, tolerance, &mut warnings);

                // Reconstruct the arc's circle analytically.
                let chord = dx.hypot(dy);
                let sagitta = bulge.abs() * chord / 2.0;
                let radius = (chord * chord / 4.0 + sagitta * sagitta) / (2.0 * sagitta);
                let side = if bulge > 0.0 { 1.0 } else { -1.0 };
                let apothem = radius - sagitta;
                let center = (
                    (start.0 + end.0) / 2.0 + (-dy / chord) * apothem * side,
                    (start.1 + end.1) / 2.0 + (dx / chord) * apothem * side,
                );

                let scale = sx.abs().max(sy.abs()).max(radius).max(1.0);
                let epsilon = 1e-9 * scale;
                for (x, y) in &points {
                    let distance = (x - center.0).hypot(y - center.1);
                    prop_assert!(
                        (distance - radius).abs() <= epsilon,
                        "interior point off the bulge circle by {}",
                        (distance - radius).abs()
                    );
                }
            }

            /// Affine composition is exactly application order:
            /// (a . b)(p) == a(b(p)).
            #[test]
            fn affine_composition_matches_sequential_application(
                translate_x in -1.0e5f64..1.0e5,
                translate_y in -1.0e5f64..1.0e5,
                angle in -std::f64::consts::TAU..std::f64::consts::TAU,
                scale_x in 0.01f64..100.0,
                scale_y in 0.01f64..100.0,
                px in -1.0e4f64..1.0e4,
                py in -1.0e4f64..1.0e4,
            ) {
                use acadrust::types::Vector3;

                let a = Affine::from_translation(Vector3::new(translate_x, translate_y, 0.0))
                    .compose(&Affine::rotation_z(angle));
                let b = Affine::scale(scale_x, scale_y, 1.0)
                    .compose(&Affine::from_translation(Vector3::new(-translate_y, px, 0.0)));
                let point = Vector3::new(px, py, 0.0);

                let composed = a.compose(&b).apply(point);
                let sequential = a.apply(b.apply(point));
                let magnitude = sequential.x.abs().max(sequential.y.abs()).max(1.0);
                prop_assert!((composed.x - sequential.x).abs() <= 1e-9 * magnitude);
                prop_assert!((composed.y - sequential.y).abs() <= 1e-9 * magnitude);
            }

            /// Calibration recovers a known similarity transform from any
            /// non-degenerate control-point set.
            #[test]
            fn calibration_recovers_known_similarity(
                a in -5.0f64..5.0,
                b in -5.0f64..5.0,
                tx in -1.0e5f64..1.0e5,
                ty in -1.0e5f64..1.0e5,
                base_x in -1.0e4f64..1.0e4,
                base_y in -1.0e4f64..1.0e4,
                spread in 1.0f64..1.0e3,
            ) {
                prop_assume!(a.hypot(b) > 1e-3);
                let truth = Calibration { a, b, tx, ty };
                let sources = [
                    (base_x, base_y),
                    (base_x + spread, base_y),
                    (base_x, base_y + spread),
                ];
                let points: Vec<ControlPoint> = sources
                    .iter()
                    .map(|&source| ControlPoint {
                        source,
                        target: truth.apply(source),
                    })
                    .collect();

                let (fitted, quality) = solve(&points).expect("solvable");
                let magnitude = tx.abs().max(ty.abs()).max(1.0);
                prop_assert!((fitted.a - a).abs() <= 1e-6);
                prop_assert!((fitted.b - b).abs() <= 1e-6);
                prop_assert!((fitted.tx - tx).abs() <= 1e-6 * magnitude);
                prop_assert!((fitted.ty - ty).abs() <= 1e-6 * magnitude);
                prop_assert!(quality.rms_error <= 1e-6 * magnitude.max(spread));
            }
        }
    }
}
