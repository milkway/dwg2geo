//! CAD-neutral internal feature model (ADR-004, AGENTS.md native-backend
//! boundary).
//!
//! Converters consume `acadrust` entities and produce [`CadFeature`]s; every
//! transform, validity check, and statistic in the pipeline operates on this
//! model; only the writer (`super::writer`) turns it into GeoJSON. Neither
//! `acadrust` nor `geojson` types appear here.

/// 2D geometry in drawing or world coordinates, always as plain (x, y)
/// tuples.
#[derive(Clone, Debug, PartialEq)]
pub enum CadGeometry {
    Point((f64, f64)),
    /// An open or closed polyline path (closure is a property, not a type).
    Line(Vec<(f64, f64)>),
    /// One shell ring followed by hole rings, shells CCW and holes CW.
    Polygon(Vec<Vec<(f64, f64)>>),
    MultiPolygon(Vec<Vec<Vec<(f64, f64)>>>),
}

impl CadGeometry {
    /// Visit every (x, y) position.
    pub fn visit_positions(&self, visit: &mut impl FnMut(f64, f64)) {
        match self {
            CadGeometry::Point((x, y)) => visit(*x, *y),
            CadGeometry::Line(line) => {
                for (x, y) in line {
                    visit(*x, *y);
                }
            }
            CadGeometry::Polygon(rings) => {
                for ring in rings {
                    for (x, y) in ring {
                        visit(*x, *y);
                    }
                }
            }
            CadGeometry::MultiPolygon(polygons) => {
                for polygon in polygons {
                    for ring in polygon {
                        for (x, y) in ring {
                            visit(*x, *y);
                        }
                    }
                }
            }
        }
    }

    /// Visit every consecutive-position segment of lines and rings.
    pub fn visit_segments(&self, visit: &mut impl FnMut((f64, f64), (f64, f64))) {
        let mut walk = |line: &[(f64, f64)]| {
            for pair in line.windows(2) {
                visit(pair[0], pair[1]);
            }
        };
        match self {
            CadGeometry::Point(_) => {}
            CadGeometry::Line(line) => walk(line),
            CadGeometry::Polygon(rings) => {
                for ring in rings {
                    walk(ring);
                }
            }
            CadGeometry::MultiPolygon(polygons) => {
                for polygon in polygons {
                    for ring in polygon {
                        walk(ring);
                    }
                }
            }
        }
    }

    /// Transform every position in place; the first failing position aborts.
    pub fn transform(
        &mut self,
        transform: &impl Fn(f64, f64) -> anyhow::Result<(f64, f64)>,
    ) -> anyhow::Result<()> {
        let apply_line = |line: &mut Vec<(f64, f64)>| -> anyhow::Result<()> {
            for point in line {
                *point = transform(point.0, point.1)?;
            }
            Ok(())
        };
        match self {
            CadGeometry::Point(point) => {
                *point = transform(point.0, point.1)?;
            }
            CadGeometry::Line(line) => apply_line(line)?,
            CadGeometry::Polygon(rings) => {
                for ring in rings {
                    apply_line(ring)?;
                }
            }
            CadGeometry::MultiPolygon(polygons) => {
                for polygon in polygons {
                    for ring in polygon {
                        apply_line(ring)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Polygon rings of this geometry (shell first per polygon), if any.
    pub fn polygon_rings(&self) -> Vec<&Vec<Vec<(f64, f64)>>> {
        match self {
            CadGeometry::Polygon(rings) => vec![rings],
            CadGeometry::MultiPolygon(polygons) => polygons.iter().collect(),
            _ => Vec::new(),
        }
    }
}

/// One converted entity, carrying identity, provenance, style, warnings, and
/// geometry — everything the writer needs and nothing tied to a CAD or
/// output library.
#[derive(Clone, Debug)]
pub struct CadFeature {
    /// Stable feature id: the insert-handle chain plus the entity handle (or
    /// a document-order fallback).
    pub id: String,
    /// DXF entity type name, e.g. "LWPOLYLINE".
    pub entity_type: String,
    /// The entity's own handle, hex-formatted.
    pub handle: String,
    /// Effective layer after block layer-0 inheritance.
    pub layer: String,
    /// Original layer when it differs from `layer`.
    pub source_layer: Option<String>,
    /// Block-name chain for expanded block content, outermost first.
    pub block_path: Vec<String>,
    /// Entity-specific properties (text, style, hatch metadata, ...).
    pub extra_properties: Vec<(&'static str, serde_json::Value)>,
    /// Human-readable conversion warnings.
    pub warnings: Vec<String>,
    pub geometry: CadGeometry,
}
