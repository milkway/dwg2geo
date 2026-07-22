use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{backend::tools::ToolInfo, dwg::DwgInfo};

pub const REPORT_VERSION: u32 = 1;

/// Sidecar conversion report written next to the GeoJSON output.
///
/// Field order is the serialization order and must stay stable: given the same
/// input file and options, everything except the `duration_ms` values is
/// byte-for-byte reproducible.
#[derive(Debug, Serialize)]
pub struct ConversionReport {
    pub report_version: u32,
    pub generator: Generator,
    pub source: DwgInfo,
    pub options: ConversionOptions,
    pub external_tools: Vec<ToolInfo>,
    pub steps: Vec<Step>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeConversionSummary>,
    pub output: OutputInfo,
    pub total_duration_ms: u64,
}

/// Entity-level accounting for the native backend: everything read is either
/// converted, skipped with a reason, failed with a reason, or excluded by the
/// documented model-space filter — the counts must add up.
#[derive(Debug, Serialize)]
pub struct NativeConversionSummary {
    pub read_mode: String,
    pub read_errors: Vec<String>,
    pub features_written: usize,
    /// Features whose geometry required curve approximation.
    pub approximated_features: usize,
    /// INSERT references expanded into block geometry.
    pub inserts_expanded: usize,
    pub converted: Vec<ConvertedCount>,
    pub skipped: Vec<OutcomeCount>,
    pub failed: Vec<OutcomeCount>,
    pub excluded: ExcludedCounts,
    pub feature_warnings: usize,
    /// Present when the native backend reprojected the output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reprojection: Option<ReprojectionInfo>,
    /// Present when the output was georeferenced by control-point
    /// calibration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calibration: Option<CalibrationInfo>,
    /// Entity accounting: every top-level model-space entity must land in
    /// exactly one outcome bucket.
    pub accounting: AccountingInfo,
    /// Feature bounding box in drawing coordinates, before any transform:
    /// [min_x, min_y, max_x, max_y]. None when no feature has geometry.
    pub bbox_drawing: Option<[f64; 4]>,
    /// Feature bounding box in output coordinates, after reprojection or
    /// calibration; equals `bbox_drawing` for local-coordinate output.
    pub bbox_output: Option<[f64; 4]>,
    /// Output-side geometry validity counters.
    pub geometry_checks: GeometryChecks,
    /// Features whose location is far from the main coordinate cluster
    /// (computed on drawing coordinates, before any transform).
    pub spatial_outliers: SpatialOutliers,
    /// Present when --validate-boundary was passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary_check: Option<BoundaryCheck>,
}

/// Robust-statistics outlier scan: a feature is an outlier when its bbox
/// center deviates from the median center by more than 100x the median
/// absolute deviation on either axis. Informational — title blocks and
/// legends drawn near the drawing origin are the typical hit.
#[derive(Debug, Serialize)]
pub struct SpatialOutliers {
    pub features_checked: usize,
    pub outlier_features: usize,
    /// Median feature center in drawing coordinates.
    pub center: [f64; 2],
    /// Per-axis deviation thresholds (100 x MAD).
    pub axis_thresholds: [f64; 2],
    /// Up to ten outlier feature ids.
    pub sample_ids: Vec<String>,
}

/// Containment of the output features in a reference boundary polygon
/// (e.g. an IBGE municipal boundary), evaluated on output coordinates.
#[derive(Debug, Serialize)]
pub struct BoundaryCheck {
    pub boundary_path: String,
    /// Polygons read from the boundary file.
    pub polygons: usize,
    /// Features with every vertex inside the boundary.
    pub features_inside: usize,
    /// Features with some vertices inside and some outside.
    pub features_partial: usize,
    /// Features entirely outside the boundary.
    pub features_outside: usize,
    /// Up to ten ids of features not fully inside.
    pub sample_not_inside_ids: Vec<String>,
}

/// Histogram accounting between what model space contains and what the
/// conversion did with it. `unaccounted` must be zero; a nonzero value is a
/// converter bug surfaced as a warning, never hidden.
#[derive(Debug, Serialize)]
pub struct AccountingInfo {
    /// Top-level model-space entities encountered.
    pub model_space_entities: usize,
    /// Top-level entities that reached an outcome (converted, skipped,
    /// failed, or expanded as an INSERT).
    pub top_level_accounted: usize,
    pub unaccounted: usize,
}

/// Validity counters computed on the final output features. Construction
/// invariants make most of these zero; nonzero values of anything except
/// `duplicate_vertex_features` are converter bugs and raise warnings.
#[derive(Debug, Serialize)]
pub struct GeometryChecks {
    pub features_checked: usize,
    pub empty_geometries: usize,
    pub non_finite_coordinates: usize,
    /// Features containing at least one pair of identical consecutive
    /// vertices (informational; can occur in legitimate linework).
    pub duplicate_vertex_features: usize,
    pub rings_checked: usize,
    pub unclosed_rings: usize,
    /// Shells that are not CCW or holes that are not CW.
    pub misoriented_rings: usize,
    /// Rings with fewer than four positions.
    pub degenerate_rings: usize,
}

/// How a control-point calibration fit the drawing to the target CRS.
#[derive(Debug, Serialize)]
pub struct CalibrationInfo {
    pub control_points: usize,
    /// Uniform scale of the similarity transform.
    pub scale: f64,
    pub rotation_deg: f64,
    /// Translation component, in target-CRS units.
    pub translation: (f64, f64),
    /// Per-point Euclidean error in target-CRS units, in input order.
    pub residuals: Vec<f64>,
    pub rms_error: f64,
    pub max_error: f64,
    pub target_crs: String,
}

/// How coordinates were georeferenced by the native backend.
#[derive(Debug, Serialize)]
pub struct ReprojectionInfo {
    pub source_crs: String,
    pub target_crs: String,
    /// Resolved linear drawing unit name.
    pub drawing_unit: String,
    /// Where the unit came from: "header" or "override".
    pub unit_source: String,
    pub meters_per_drawing_unit: f64,
    /// Axis-order convention of both input and output coordinates.
    pub axis_order: String,
    /// PROJ transformation pipeline definition, when available.
    pub pipeline: Option<String>,
    pub proj_version: String,
}

#[derive(Debug, Serialize)]
pub struct ConvertedCount {
    pub entity_type: String,
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct OutcomeCount {
    pub entity_type: String,
    pub reason: String,
    pub count: usize,
    /// Bounded sample of entity handles (hex) affected by this outcome.
    pub sample_handles: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ExcludedCounts {
    pub paper_space: usize,
    pub block_definitions: usize,
    pub unowned: usize,
    /// Top-level model-space entities dropped by layer filters.
    pub by_layer_filter: usize,
}

#[derive(Debug, Serialize)]
pub struct Generator {
    pub name: &'static str,
    pub version: &'static str,
}

impl Generator {
    pub fn current() -> Self {
        Self {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ConversionOptions {
    pub backend: &'static str,
    pub source_crs: Option<String>,
    pub target_crs: Option<String>,
    pub allow_local_coordinates: bool,
    pub force: bool,
    pub keep_intermediate: bool,
    pub include_layers: Vec<String>,
    pub exclude_layers: Vec<String>,
    pub polygonize_closed: bool,
    /// Effective chord-error tolerance for arc tessellation, in drawing
    /// units. `None` on routes that do not tessellate.
    pub curve_tolerance: Option<f64>,
    /// How INSERT references were handled: "explode" or "preserve-inserts".
    /// `None` on routes that do not resolve blocks themselves.
    pub block_mode: Option<String>,
    /// Native output format: "geojson" or "geojson-seq". `None` on routes
    /// that do not choose the output representation themselves.
    pub output_format: Option<String>,
    /// The --source-units override as given, when one was passed.
    pub source_units: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Step {
    pub purpose: String,
    pub command: String,
    pub duration_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct OutputInfo {
    pub path: String,
    pub size_bytes: u64,
}

/// `<output>.report.json`, appended to the full output file name.
pub fn report_path(output: &Path) -> PathBuf {
    let mut name = output.as_os_str().to_owned();
    name.push(".report.json");
    PathBuf::from(name)
}

pub fn write(report: &ConversionReport, path: &Path) -> Result<()> {
    let mut json =
        serde_json::to_string_pretty(report).context("cannot serialize conversion report")?;
    json.push('\n');
    fs::write(path, json)
        .with_context(|| format!("cannot write conversion report {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        ConversionOptions, ConversionReport, Generator, OutputInfo, REPORT_VERSION, Step,
        report_path,
    };
    use crate::dwg::DwgInfo;

    fn sample() -> ConversionReport {
        ConversionReport {
            report_version: REPORT_VERSION,
            generator: Generator::current(),
            source: DwgInfo {
                path: "samples/fixture.dwg".to_string(),
                signature: "AC1027".to_string(),
                autocad_generation: "AutoCAD 2013/2014/2015/2016/2017".to_string(),
                size_bytes: 28,
                sha256: "00".repeat(32),
            },
            options: ConversionOptions {
                backend: "external",
                source_crs: Some("EPSG:31985".to_string()),
                target_crs: Some("EPSG:4326".to_string()),
                allow_local_coordinates: false,
                force: false,
                keep_intermediate: false,
                include_layers: vec!["EIXO".to_string()],
                exclude_layers: Vec::new(),
                polygonize_closed: false,
                curve_tolerance: None,
                block_mode: None,
                output_format: None,
                source_units: None,
            },
            external_tools: Vec::new(),
            steps: vec![Step {
                purpose: "LibreDWG conversion to DXF".to_string(),
                command: "dwgread -O DXF -o intermediate.dxf samples/fixture.dwg".to_string(),
                duration_ms: 0,
            }],
            warnings: Vec::new(),
            native: None,
            output: OutputInfo {
                path: "out.geojson".to_string(),
                size_bytes: 42,
            },
            total_duration_ms: 0,
        }
    }

    #[test]
    fn serialization_is_deterministic_and_ordered() {
        let first = serde_json::to_string_pretty(&sample()).expect("serialize");
        let second = serde_json::to_string_pretty(&sample()).expect("serialize");
        assert_eq!(first, second);
        let serialized: serde_json::Value = serde_json::from_str(&first).expect("valid JSON");
        assert!(serialized["options"]["output_format"].is_null());

        let order = [
            "\"report_version\"",
            "\"generator\"",
            "\"source\"",
            "\"options\"",
            "\"external_tools\"",
            "\"steps\"",
            "\"warnings\"",
            "\"output\"",
            "\"total_duration_ms\"",
        ];
        let positions: Vec<usize> = order
            .iter()
            .map(|key| first.find(key).unwrap_or_else(|| panic!("{key} missing")))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn report_path_appends_full_suffix() {
        assert_eq!(
            report_path(Path::new("out/corredor sul.geojson")),
            Path::new("out/corredor sul.geojson.report.json")
        );
    }
}
