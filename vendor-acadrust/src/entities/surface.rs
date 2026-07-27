//! Surface entities (ACAD_SURFACE family).
//!
//! Lofted / swept / extruded / revolved / plane / NURB surfaces share the
//! `AcDbSurface` base, which stores its geometry in ACIS format just like
//! [`Body`](super::solid3d::Body). They are kept as a distinct entity type so
//! the original surface kind survives a DWG round-trip; the raw object bytes
//! are preserved verbatim for write-back.

use crate::entities::solid3d::{AcisData, Silhouette, Wire};
use crate::entities::{Entity, EntityCommon};
use crate::types::{BoundingBox3D, Color, Handle, LineWeight, Transparency, Vector3};

/// Which ACAD_SURFACE subtype a [`Surface`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SurfaceKind {
    /// Generic `SURFACE` / `AcDbSurface`.
    #[default]
    Generic,
    /// `PLANESURFACE`.
    Plane,
    /// `EXTRUDEDSURFACE`.
    Extruded,
    /// `LOFTEDSURFACE`.
    Lofted,
    /// `REVOLVEDSURFACE`.
    Revolved,
    /// `SWEPTSURFACE`.
    Swept,
    /// `NURBSURFACE`.
    Nurb,
}

impl SurfaceKind {
    /// Map a DXF class name to a surface kind.
    pub fn from_dxf_name(name: &str) -> Self {
        match name.to_uppercase().as_str() {
            "PLANESURFACE" => SurfaceKind::Plane,
            "EXTRUDEDSURFACE" => SurfaceKind::Extruded,
            "LOFTEDSURFACE" => SurfaceKind::Lofted,
            "REVOLVEDSURFACE" => SurfaceKind::Revolved,
            "SWEPTSURFACE" => SurfaceKind::Swept,
            "NURBSURFACE" => SurfaceKind::Nurb,
            _ => SurfaceKind::Generic,
        }
    }

    /// The DXF class name for this kind.
    pub fn dxf_name(self) -> &'static str {
        match self {
            SurfaceKind::Generic => "SURFACE",
            SurfaceKind::Plane => "PLANESURFACE",
            SurfaceKind::Extruded => "EXTRUDEDSURFACE",
            SurfaceKind::Lofted => "LOFTEDSURFACE",
            SurfaceKind::Revolved => "REVOLVEDSURFACE",
            SurfaceKind::Swept => "SWEPTSURFACE",
            SurfaceKind::Nurb => "NURBSURFACE",
        }
    }
}

/// A 3D surface entity (ACAD_SURFACE family), backed by ACIS geometry.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Surface {
    /// Common entity data.
    pub common: EntityCommon,
    /// Which surface subtype this is.
    pub kind: SurfaceKind,
    /// ACIS/SAT surface geometry.
    pub acis_data: AcisData,
    /// Wireframe edges for visualization.
    pub wires: Vec<Wire>,
    /// Silhouette data for viewports.
    pub silhouettes: Vec<Silhouette>,
    /// Raw DWG object bytes, preserved verbatim for round-trip write-back.
    pub raw_dwg_data: Option<Vec<u8>>,
    /// Handle-stream bit length captured alongside `raw_dwg_data`.
    pub dwg_handle_bits: i64,
    /// DWG version `raw_dwg_data` was read from (drop on incompatible cross-version save).
    pub dwg_source_version: Option<crate::types::DxfVersion>,
}

impl Surface {
    /// Creates a new empty surface of the given kind.
    pub fn new(kind: SurfaceKind) -> Self {
        Self {
            common: EntityCommon::default(),
            kind,
            acis_data: AcisData::new(),
            wires: Vec::new(),
            silhouettes: Vec::new(),
            raw_dwg_data: None,
            dwg_handle_bits: 0,
            dwg_source_version: None,
        }
    }

    /// Returns true if this surface has valid ACIS data.
    pub fn has_acis_data(&self) -> bool {
        self.acis_data.has_data()
    }

    /// Parses the raw SAT text data into a structured [`SatDocument`].
    ///
    /// Returns `None` if the ACIS data is empty or binary (SAB).
    pub fn parse_sat(&self) -> Option<crate::entities::acis::SatDocument> {
        if self.acis_data.is_binary || self.acis_data.sat_data.is_empty() {
            return None;
        }
        crate::entities::acis::SatDocument::parse(&self.acis_data.sat_data).ok()
    }
}

impl Entity for Surface {
    fn handle(&self) -> Handle {
        self.common.handle
    }
    fn set_handle(&mut self, handle: Handle) {
        self.common.handle = handle;
    }
    fn layer(&self) -> &str {
        &self.common.layer
    }
    fn set_layer(&mut self, layer: String) {
        self.common.layer = layer;
    }
    fn color(&self) -> Color {
        self.common.color
    }
    fn set_color(&mut self, color: Color) {
        self.common.color = color;
    }
    fn line_weight(&self) -> LineWeight {
        self.common.line_weight
    }
    fn set_line_weight(&mut self, line_weight: LineWeight) {
        self.common.line_weight = line_weight;
    }
    fn transparency(&self) -> Transparency {
        self.common.transparency
    }
    fn set_transparency(&mut self, transparency: Transparency) {
        self.common.transparency = transparency;
    }
    fn is_invisible(&self) -> bool {
        self.common.invisible
    }
    fn set_invisible(&mut self, invisible: bool) {
        self.common.invisible = invisible;
    }
    fn bounding_box(&self) -> BoundingBox3D {
        if self.wires.is_empty() {
            return BoundingBox3D::default();
        }
        let mut min = Vector3::new(f64::MAX, f64::MAX, f64::MAX);
        let mut max = Vector3::new(f64::MIN, f64::MIN, f64::MIN);
        for wire in &self.wires {
            for pt in &wire.points {
                min.x = min.x.min(pt.x);
                min.y = min.y.min(pt.y);
                min.z = min.z.min(pt.z);
                max.x = max.x.max(pt.x);
                max.y = max.y.max(pt.y);
                max.z = max.z.max(pt.z);
            }
        }
        BoundingBox3D::new(min, max)
    }
    fn translate(&mut self, offset: Vector3) {
        super::translate::translate_surface(self, offset);
    }
    fn entity_type(&self) -> &'static str {
        "SURFACE"
    }
    fn apply_transform(&mut self, transform: &crate::types::Transform) {
        super::transform::transform_surface(self, transform);
    }
}
