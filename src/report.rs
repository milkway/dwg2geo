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
    pub converted: Vec<ConvertedCount>,
    pub skipped: Vec<OutcomeCount>,
    pub failed: Vec<OutcomeCount>,
    pub excluded: ExcludedCounts,
    pub feature_warnings: usize,
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
