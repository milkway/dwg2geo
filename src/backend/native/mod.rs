//! Native inspection backend built on `acadrust`.
//!
//! `acadrust` types must not leak out of this module: everything returned to
//! the CLI is a CAD-neutral, serializable summary. Conversion to GeoJSON is
//! Milestone 3; this module only reads and reports.

pub mod convert;

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};

use acadrust::{
    CadDocument,
    entities::EntityType,
    io::dwg::{DwgReadOptions, DwgReader},
    notification::NotificationType,
};
use anyhow::{Context, Result, anyhow};
use serde::Serialize;

/// Notifications quoted verbatim in reports are capped at this count; the
/// remainder is summarized numerically so output stays bounded but no class
/// of problem is hidden.
const NOTIFICATION_SAMPLE_LIMIT: usize = 20;

/// AutoCAD stores "no extents" as +/-1e20 sentinels; treat anything in that
/// magnitude range as unset.
const EXTENTS_SENTINEL: f64 = 1e19;

#[derive(Debug, Serialize)]
pub struct NativeInspection {
    pub reader: &'static str,
    pub dwg_version: String,
    pub measurement: String,
    pub insertion_units_code: i16,
    pub insertion_units: String,
    pub model_extents: Option<Extents>,
    pub layer_count: usize,
    pub block_definition_count: usize,
    pub entity_counts: SpaceCounts,
    pub entity_histogram: Vec<HistogramEntry>,
    pub unknown_entity_count: usize,
    pub unresolved_entity_handles: usize,
    pub read_mode: ReadMode,
    pub read_errors: Vec<String>,
    pub notifications: NotificationSummary,
}

#[derive(Debug, Serialize)]
pub struct Extents {
    pub min: [f64; 3],
    pub max: [f64; 3],
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SpaceCounts {
    pub model_space: usize,
    pub paper_space: usize,
    pub block_definitions: usize,
    pub unowned: usize,
    pub total: usize,
}

#[derive(Debug, Serialize)]
pub struct HistogramEntry {
    pub entity_type: String,
    pub model_space: usize,
    pub paper_space: usize,
    pub block_definitions: usize,
    pub unowned: usize,
    pub total: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadMode {
    /// The strict parse succeeded.
    Strict,
    /// The strict parse failed and the report comes from a failsafe re-read;
    /// `read_errors` holds the strict failure.
    FailsafeRecovery,
}

#[derive(Debug, Serialize)]
pub struct NotificationSummary {
    pub errors: usize,
    pub warnings: usize,
    pub not_supported: usize,
    pub not_implemented: usize,
    pub sample_limit: usize,
    pub samples: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LayersReport {
    pub path: String,
    pub dwg_version: String,
    pub layer_count: usize,
    pub layers: Vec<LayerSummary>,
    /// Layer names referenced by entities but missing from the layer table.
    pub undefined_layers_referenced: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LayerSummary {
    pub name: String,
    pub frozen: bool,
    pub off: bool,
    pub locked: bool,
    pub plottable: bool,
    pub entity_counts: SpaceCounts,
    pub entity_types: Vec<TypeCount>,
}

#[derive(Debug, Serialize)]
pub struct TypeCount {
    pub entity_type: String,
    pub count: usize,
}

pub fn inspect(path: &Path) -> Result<NativeInspection> {
    let (document, read_mode, read_errors) = read_document(path)?;
    let survey = survey_entities(&document);

    let block_definition_count = document
        .block_records
        .iter()
        .filter(|record| !record.is_model_space() && !record.is_paper_space())
        .count();

    Ok(NativeInspection {
        reader: "acadrust",
        dwg_version: document.version.to_string(),
        measurement: measurement_name(document.header.measurement),
        insertion_units_code: document.header.insertion_units,
        insertion_units: units_name(document.header.insertion_units).to_string(),
        model_extents: extents(
            &document.header.model_space_extents_min,
            &document.header.model_space_extents_max,
        ),
        layer_count: document.layers.len(),
        block_definition_count,
        entity_counts: survey.totals,
        entity_histogram: survey.histogram(),
        unknown_entity_count: survey.unknown_entities,
        unresolved_entity_handles: survey.unresolved_handles,
        read_mode,
        read_errors,
        notifications: summarize_notifications(&document),
    })
}

pub fn layers(path: &Path) -> Result<LayersReport> {
    let (document, _read_mode, _read_errors) = read_document(path)?;
    let survey = survey_entities(&document);

    let mut summaries: Vec<LayerSummary> = document
        .layers
        .iter()
        .map(|layer| {
            let usage = survey.by_layer.get(layer.name.as_str());
            LayerSummary {
                name: layer.name.clone(),
                frozen: layer.flags.frozen,
                off: layer.flags.off,
                locked: layer.flags.locked,
                plottable: layer.is_plottable,
                entity_counts: usage.map(|u| u.counts).unwrap_or_default(),
                entity_types: usage.map(LayerUsage::type_counts).unwrap_or_default(),
            }
        })
        .collect();
    summaries.sort_by(|a, b| a.name.cmp(&b.name));

    let table_names: BTreeSet<&str> = document
        .layers
        .iter()
        .map(|layer| layer.name.as_str())
        .collect();
    let undefined_layers_referenced: Vec<String> = survey
        .by_layer
        .keys()
        .filter(|name| !table_names.contains(name.as_str()))
        .cloned()
        .collect();

    Ok(LayersReport {
        path: path.display().to_string(),
        dwg_version: document.version.to_string(),
        layer_count: document.layers.len(),
        layers: summaries,
        undefined_layers_referenced,
    })
}

/// Strict read first; on failure retry in failsafe mode so recoverable
/// corruption still yields a report instead of nothing. Both failures are
/// surfaced, never swallowed.
pub(crate) fn read_document(path: &Path) -> Result<(CadDocument, ReadMode, Vec<String>)> {
    let mut reader = DwgReader::from_file(path)
        .with_context(|| format!("cannot open {} for native reading", path.display()))?;

    match reader.read() {
        Ok(document) => Ok((document, ReadMode::Strict, Vec::new())),
        Err(strict_error) => {
            let mut failsafe_reader =
                DwgReader::from_file_with_options(path, DwgReadOptions::failsafe()).with_context(
                    || format!("cannot reopen {} for failsafe reading", path.display()),
                )?;
            match failsafe_reader.read() {
                // Failsafe mode returns an empty default document even for
                // garbage input; only accept it as a recovery if it actually
                // recovered drawing content.
                Ok(document) if document.entity_count() > 0 || document.layers.len() > 1 => Ok((
                    document,
                    ReadMode::FailsafeRecovery,
                    vec![format!(
                        "strict parse failed ({strict_error}); results come from a failsafe re-read and may be incomplete"
                    )],
                )),
                Ok(_) => Err(anyhow!(
                    "native backend cannot parse {}: strict error: {strict_error}; a failsafe re-read recovered no drawing content",
                    path.display()
                )),
                Err(failsafe_error) => Err(anyhow!(
                    "native backend cannot parse {}: strict error: {strict_error}; failsafe error: {failsafe_error}",
                    path.display()
                )),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Space {
    Model,
    Paper,
    Block,
    Unowned,
}

#[derive(Default)]
struct LayerUsage {
    counts: SpaceCounts,
    types: BTreeMap<String, usize>,
}

impl LayerUsage {
    fn type_counts(&self) -> Vec<TypeCount> {
        self.types
            .iter()
            .map(|(entity_type, count)| TypeCount {
                entity_type: entity_type.clone(),
                count: *count,
            })
            .collect()
    }
}

#[derive(Default)]
struct Survey {
    totals: SpaceCounts,
    by_type: BTreeMap<String, SpaceCounts>,
    by_layer: BTreeMap<String, LayerUsage>,
    unknown_entities: usize,
    unresolved_handles: usize,
}

impl Survey {
    fn record(&mut self, entity: &EntityType, space: Space) {
        let type_name = entity.as_entity().entity_type().to_string();
        let layer_name = entity.common().layer.clone();

        if matches!(entity, EntityType::Unknown(_)) {
            self.unknown_entities += 1;
        }

        bump(&mut self.totals, space);
        bump(self.by_type.entry(type_name.clone()).or_default(), space);

        let usage = self.by_layer.entry(layer_name).or_default();
        bump(&mut usage.counts, space);
        *usage.types.entry(type_name).or_default() += 1;
    }

    fn histogram(&self) -> Vec<HistogramEntry> {
        self.by_type
            .iter()
            .map(|(entity_type, counts)| HistogramEntry {
                entity_type: entity_type.clone(),
                model_space: counts.model_space,
                paper_space: counts.paper_space,
                block_definitions: counts.block_definitions,
                unowned: counts.unowned,
                total: counts.total,
            })
            .collect()
    }
}

fn bump(counts: &mut SpaceCounts, space: Space) {
    counts.total += 1;
    match space {
        Space::Model => counts.model_space += 1,
        Space::Paper => counts.paper_space += 1,
        Space::Block => counts.block_definitions += 1,
        Space::Unowned => counts.unowned += 1,
    }
}

/// Walk every entity exactly once, classified by the block record that owns
/// it. Entities not reachable from any block record are counted as unowned,
/// and handles that resolve to nothing are counted, so nothing is silently
/// dropped.
fn survey_entities(document: &CadDocument) -> Survey {
    let mut survey = Survey::default();
    let mut visited: HashSet<u64> = HashSet::new();

    for record in document.block_records.iter() {
        let space = if record.is_model_space() {
            Space::Model
        } else if record.is_paper_space() {
            Space::Paper
        } else {
            Space::Block
        };

        for handle in &record.entity_handles {
            match document.get_entity(*handle) {
                Some(entity) => {
                    if matches!(entity, EntityType::Block(_) | EntityType::BlockEnd(_)) {
                        continue;
                    }
                    if visited.insert(handle.value()) {
                        survey.record(entity, space);
                    }
                }
                None => survey.unresolved_handles += 1,
            }
        }
    }

    for entity in document.entities() {
        if !visited.contains(&entity.common().handle.value()) {
            survey.record(entity, Space::Unowned);
        }
    }

    survey
}

fn summarize_notifications(document: &CadDocument) -> NotificationSummary {
    let mut summary = NotificationSummary {
        errors: 0,
        warnings: 0,
        not_supported: 0,
        not_implemented: 0,
        sample_limit: NOTIFICATION_SAMPLE_LIMIT,
        samples: Vec::new(),
    };

    for notification in document.notifications.iter() {
        let label = match notification.notification_type {
            NotificationType::Error => {
                summary.errors += 1;
                "error"
            }
            NotificationType::Warning => {
                summary.warnings += 1;
                "warning"
            }
            NotificationType::NotSupported => {
                summary.not_supported += 1;
                "not-supported"
            }
            NotificationType::NotImplemented => {
                summary.not_implemented += 1;
                "not-implemented"
            }
        };
        if summary.samples.len() < NOTIFICATION_SAMPLE_LIMIT {
            summary
                .samples
                .push(format!("{label}: {}", notification.message));
        }
    }

    summary
}

fn extents(min: &acadrust::types::Vector3, max: &acadrust::types::Vector3) -> Option<Extents> {
    let coords = [min.x, min.y, min.z, max.x, max.y, max.z];
    if coords
        .iter()
        .any(|value| !value.is_finite() || value.abs() >= EXTENTS_SENTINEL)
    {
        return None;
    }
    if min.x > max.x || min.y > max.y || min.z > max.z {
        return None;
    }
    Some(Extents {
        min: [min.x, min.y, min.z],
        max: [max.x, max.y, max.z],
    })
}

fn measurement_name(code: i16) -> String {
    match code {
        0 => "english".to_string(),
        1 => "metric".to_string(),
        other => format!("unknown (code {other})"),
    }
}

fn units_name(code: i16) -> &'static str {
    match code {
        0 => "unitless",
        1 => "inches",
        2 => "feet",
        3 => "miles",
        4 => "millimeters",
        5 => "centimeters",
        6 => "meters",
        7 => "kilometers",
        8 => "microinches",
        9 => "mils",
        10 => "yards",
        11 => "angstroms",
        12 => "nanometers",
        13 => "microns",
        14 => "decimeters",
        15 => "decameters",
        16 => "hectometers",
        17 => "gigameters",
        18 => "astronomical units",
        19 => "light years",
        20 => "parsecs",
        21 => "US survey feet",
        22 => "US survey inches",
        23 => "US survey yards",
        24 => "US survey miles",
        _ => "unknown",
    }
}

impl NativeInspection {
    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Native reader: {}", self.reader),
            format!("DWG version: {}", self.dwg_version),
            format!("Measurement: {}", self.measurement),
            format!(
                "Insertion units: {} (code {})",
                self.insertion_units, self.insertion_units_code
            ),
            match &self.model_extents {
                Some(extents) => format!(
                    "Model extents: ({}, {}, {}) .. ({}, {}, {})",
                    extents.min[0],
                    extents.min[1],
                    extents.min[2],
                    extents.max[0],
                    extents.max[1],
                    extents.max[2]
                ),
                None => "Model extents: not set".to_string(),
            },
            format!("Layers: {}", self.layer_count),
            format!("Block definitions: {}", self.block_definition_count),
            format!(
                "Entities: {} (model {}, paper {}, in blocks {}, unowned {})",
                self.entity_counts.total,
                self.entity_counts.model_space,
                self.entity_counts.paper_space,
                self.entity_counts.block_definitions,
                self.entity_counts.unowned
            ),
        ];

        if !self.entity_histogram.is_empty() {
            lines.push("Entity histogram:".to_string());
            for entry in &self.entity_histogram {
                lines.push(format!(
                    "  {}: {} (model {}, paper {}, blocks {}, unowned {})",
                    entry.entity_type,
                    entry.total,
                    entry.model_space,
                    entry.paper_space,
                    entry.block_definitions,
                    entry.unowned
                ));
            }
        }
        if self.unknown_entity_count > 0 {
            lines.push(format!("Unknown entities: {}", self.unknown_entity_count));
        }
        if self.unresolved_entity_handles > 0 {
            lines.push(format!(
                "Unresolved entity handles: {}",
                self.unresolved_entity_handles
            ));
        }
        if self.read_mode == ReadMode::FailsafeRecovery {
            lines.push("Read mode: failsafe recovery".to_string());
        }
        for error in &self.read_errors {
            lines.push(format!("Read error: {error}"));
        }
        lines.push(format!(
            "Notifications: {} errors, {} warnings, {} not supported, {} not implemented",
            self.notifications.errors,
            self.notifications.warnings,
            self.notifications.not_supported,
            self.notifications.not_implemented
        ));
        lines
    }
}

impl LayersReport {
    pub fn human_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Path: {}", self.path),
            format!("DWG version: {}", self.dwg_version),
            format!("Layers: {}", self.layer_count),
        ];

        for layer in &self.layers {
            let mut flags = Vec::new();
            if layer.frozen {
                flags.push("frozen");
            }
            if layer.off {
                flags.push("off");
            }
            if layer.locked {
                flags.push("locked");
            }
            if !layer.plottable {
                flags.push("non-plottable");
            }
            let flag_text = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags.join(", "))
            };

            let types = layer
                .entity_types
                .iter()
                .map(|entry| format!("{} {}", entry.entity_type, entry.count))
                .collect::<Vec<_>>()
                .join(", ");
            let types_text = if types.is_empty() {
                "no entities".to_string()
            } else {
                types
            };

            lines.push(format!(
                "  {}{}: {} entities (model {}, paper {}, blocks {}, unowned {}) — {}",
                layer.name,
                flag_text,
                layer.entity_counts.total,
                layer.entity_counts.model_space,
                layer.entity_counts.paper_space,
                layer.entity_counts.block_definitions,
                layer.entity_counts.unowned,
                types_text
            ));
        }

        if !self.undefined_layers_referenced.is_empty() {
            lines.push(format!(
                "Layers referenced by entities but missing from the layer table: {}",
                self.undefined_layers_referenced.join(", ")
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use acadrust::{
        CadDocument, DxfVersion,
        entities::{Circle, EntityType, Line, Point},
        io::dwg::DwgWriter,
        tables::Layer,
    };
    use tempfile::TempDir;

    use super::{ReadMode, inspect, layers};

    fn fixture_document() -> CadDocument {
        let mut document = CadDocument::with_version(DxfVersion::AC1027);

        let mut eixo = Layer::new("EIXO");
        eixo.handle = document.allocate_handle();
        document.layers.add(eixo).expect("add EIXO layer");

        let mut apoio = Layer::new("APOIO");
        apoio.handle = document.allocate_handle();
        apoio.flags.frozen = true;
        document.layers.add(apoio).expect("add APOIO layer");

        let mut line = EntityType::Line(Line::from_coords(0.0, 0.0, 0.0, 100.0, 50.0, 0.0));
        line.common_mut().layer = "EIXO".to_string();
        document.add_entity(line).expect("add line");

        let mut second = EntityType::Line(Line::from_coords(10.0, 0.0, 0.0, 10.0, 90.0, 0.0));
        second.common_mut().layer = "EIXO".to_string();
        document.add_entity(second).expect("add second line");

        let mut point = EntityType::Point(Point::from_coords(5.0, 5.0, 0.0));
        point.common_mut().layer = "APOIO".to_string();
        document.add_entity(point).expect("add point");

        let mut circle = EntityType::Circle(Circle::from_coords(50.0, 25.0, 0.0, 12.5));
        circle.common_mut().layer = "EIXO".to_string();
        document
            .add_paper_space_entity(circle)
            .expect("add paper-space circle");

        document
    }

    fn write_fixture(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("fixture peça.dwg");
        DwgWriter::write_to_file(&path, &fixture_document()).expect("write DWG fixture");
        path
    }

    #[test]
    fn inspects_synthetic_ac1027_drawing() {
        let dir = TempDir::new().expect("temporary directory");
        let path = write_fixture(dir.path());

        let inspection = inspect(&path).expect("native inspection");

        assert_eq!(inspection.dwg_version, "AC1027");
        assert_eq!(inspection.read_mode, ReadMode::Strict);
        assert!(inspection.read_errors.is_empty());
        assert_eq!(inspection.entity_counts.model_space, 3);
        assert_eq!(inspection.entity_counts.paper_space, 1);
        assert_eq!(inspection.entity_counts.total, 4);
        assert_eq!(inspection.entity_counts.unowned, 0);
        // Layer table: "0" plus the two fixture layers.
        assert_eq!(inspection.layer_count, 3);

        let types: Vec<(&str, usize, usize)> = inspection
            .entity_histogram
            .iter()
            .map(|entry| {
                (
                    entry.entity_type.as_str(),
                    entry.model_space,
                    entry.paper_space,
                )
            })
            .collect();
        assert_eq!(
            types,
            vec![("CIRCLE", 0, 1), ("LINE", 2, 0), ("POINT", 1, 0)]
        );
    }

    #[test]
    fn layers_report_counts_by_layer_and_space() {
        let dir = TempDir::new().expect("temporary directory");
        let path = write_fixture(dir.path());

        let report = layers(&path).expect("layers report");

        assert_eq!(report.dwg_version, "AC1027");
        let names: Vec<&str> = report
            .layers
            .iter()
            .map(|layer| layer.name.as_str())
            .collect();
        assert_eq!(names, vec!["0", "APOIO", "EIXO"]);

        let eixo = report
            .layers
            .iter()
            .find(|layer| layer.name == "EIXO")
            .expect("EIXO layer");
        assert_eq!(eixo.entity_counts.model_space, 2);
        assert_eq!(eixo.entity_counts.paper_space, 1);
        assert_eq!(eixo.entity_counts.total, 3);
        assert!(!eixo.frozen);

        let apoio = report
            .layers
            .iter()
            .find(|layer| layer.name == "APOIO")
            .expect("APOIO layer");
        assert!(apoio.frozen);
        assert_eq!(apoio.entity_counts.total, 1);
        assert_eq!(apoio.entity_types.len(), 1);
        assert_eq!(apoio.entity_types[0].entity_type, "POINT");

        assert!(report.undefined_layers_referenced.is_empty());
    }

    #[test]
    fn garbage_input_fails_with_both_parse_errors() {
        let dir = TempDir::new().expect("temporary directory");
        let path = dir.path().join("garbage.dwg");
        std::fs::write(&path, b"AC1027 but not really a drawing").expect("write garbage");

        let error = inspect(&path).expect_err("garbage must not inspect");
        let text = format!("{error:#}");
        assert!(text.contains("strict error"), "error: {text}");
    }
}
