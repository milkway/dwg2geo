//! Stub object types for DXF objects that need basic round-trip support.
//!
//! These are minimal representations of DXF objects that ACadSharp supports
//! but that don't require full rich data models for typical usage.

use crate::types::{Handle, Matrix4, Vector2, Vector3};

/// Trait for minimal stub objects that only need handle + owner fields.
/// Used by the generic `read_stub_object` reader.
pub trait StubObject {
    /// Create a new default instance
    fn new_stub() -> Self;
    /// Set the object handle
    fn set_handle(&mut self, handle: Handle);
    /// Set the owner handle
    fn set_owner(&mut self, owner: Handle);
    /// Get the object handle
    fn handle(&self) -> Handle;
}

/// VisualStyle object — named visual rendering style
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VisualStyle {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Description / name
    pub description: String,
    /// Style type (code 70)
    pub style_type: i16,
    /// Face lighting model (code 71)
    pub face_lighting_model: i16,
    /// Face lighting quality (code 72)
    pub face_lighting_quality: i16,
    /// Face color mode (code 73)
    pub face_color_mode: i16,
    /// Face modifier (code 90)
    pub face_modifier: i32,
    /// Edge model (code 91)
    pub edge_model: i32,
    /// Edge style (code 92)
    pub edge_style: i32,
    /// Internal use only flag (code 291)
    pub internal_use_only: bool,
}

impl VisualStyle {
    /// Create a new VisualStyle with defaults
    pub fn new() -> Self {
        VisualStyle {
            handle: Handle::NULL,
            owner: Handle::NULL,
            description: String::new(),
            style_type: 0,
            face_lighting_model: 0,
            face_lighting_quality: 0,
            face_color_mode: 0,
            face_modifier: 0,
            edge_model: 0,
            edge_style: 0,
            internal_use_only: false,
        }
    }
}

impl Default for VisualStyle {
    fn default() -> Self { Self::new() }
}

/// Material object — named material for 3D rendering
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Material {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Material name
    pub name: String,
    /// Description
    pub description: String,
}

impl Material {
    /// Create a new Material with defaults
    pub fn new() -> Self {
        Material {
            handle: Handle::NULL,
            owner: Handle::NULL,
            name: String::new(),
            description: String::new(),
        }
    }
}

impl Default for Material {
    fn default() -> Self { Self::new() }
}

/// GeoData — geographic location data for a drawing (AcDbGeoData).
///
/// Carries the drawing's georeference, most importantly the coordinate-system
/// definition (a MapGuide coordinate-system XML string on R2010+; a WKT PROJCS
/// string on R2009).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GeoData {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Object version (code 90): 1 = R2009, 2 = R2010, 3 = R2013
    pub version: i32,
    /// Soft pointer to the host block record
    pub host_block: Handle,
    /// Coordinate type (code 70): 0 = unknown, 1 = local grid, 2 = projected grid, 3 = geographic
    pub coordinate_type: i16,
    /// Design point (WCS) (codes 10/20/30)
    pub design_point: Vector3,
    /// Reference point (geographic/projected) (codes 11/21/31)
    pub reference_point: Vector3,
    /// North direction vector (codes 12/22)
    pub north_direction: Vector2,
    /// Up direction (codes 210/220/230)
    pub up_direction: Vector3,
    /// Horizontal unit scale (code 41)
    pub horizontal_unit_scale: f64,
    /// Vertical unit scale (code 40)
    pub vertical_unit_scale: f64,
    /// Horizontal units (code 91)
    pub horizontal_units: i32,
    /// Vertical units (code 92)
    pub vertical_units: i32,
    /// Scale estimation method (code 95)
    pub scale_estimation_method: i32,
    /// User-specified scale factor (code 141)
    pub user_scale_factor: f64,
    /// Enable sea-level correction (code 294)
    pub sea_level_correction: bool,
    /// Sea-level elevation (code 142)
    pub sea_level_elevation: f64,
    /// Coordinate projection radius (code 143)
    pub coordinate_projection_radius: f64,
    /// Coordinate system definition (code 301): MapGuide XML (R2010+) or WKT (R2009)
    pub coordinate_system_definition: String,
    /// Geo RSS tag (code 302)
    pub geo_rss_tag: String,
    /// Observation-from tag (code 305)
    pub observation_from_tag: String,
    /// Observation-to tag (code 306)
    pub observation_to_tag: String,
    /// Observation-coverage tag (code 307)
    pub observation_coverage_tag: String,
}

impl GeoData {
    /// Create a new GeoData
    pub fn new() -> Self {
        GeoData {
            handle: Handle::NULL,
            owner: Handle::NULL,
            version: 2,
            host_block: Handle::NULL,
            coordinate_type: 0,
            design_point: Vector3::default(),
            reference_point: Vector3::default(),
            north_direction: Vector2::default(),
            up_direction: Vector3::default(),
            horizontal_unit_scale: 1.0,
            vertical_unit_scale: 1.0,
            horizontal_units: 0,
            vertical_units: 0,
            scale_estimation_method: 0,
            user_scale_factor: 1.0,
            sea_level_correction: false,
            sea_level_elevation: 0.0,
            coordinate_projection_radius: 0.0,
            coordinate_system_definition: String::new(),
            geo_rss_tag: String::new(),
            observation_from_tag: String::new(),
            observation_to_tag: String::new(),
            observation_coverage_tag: String::new(),
        }
    }
}

impl Default for GeoData {
    fn default() -> Self { Self::new() }
}

/// SpatialFilter — the clip boundary (XCLIP) attached to a block reference.
///
/// Stored under the INSERT's extension dictionary as the `SPATIAL` entry of
/// the `ACAD_FILTER` sub-dictionary. The boundary points are 2D coordinates in
/// the clip boundary's local coordinate system; two transforms relate that
/// system to the block reference and to the world.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SpatialFilter {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Clip boundary definition points (code 10/20), in the boundary's local
    /// 2D coordinate system. Two points = rectangular clip (min/max corners);
    /// three or more = polygonal clip.
    pub boundary_points: Vec<Vector2>,
    /// Normal to the plane of the clip boundary (code 210/220/230).
    pub normal: Vector3,
    /// Origin of the clip boundary local coordinate system (code 11/21/31).
    pub origin: Vector3,
    /// Clip boundary display enabled flag (code 71).
    pub display_enabled: bool,
    /// Front clipping plane distance (code 40), `Some` when the front clip
    /// flag (code 72) is set.
    pub front_clip: Option<f64>,
    /// Back clipping plane distance (code 41), `Some` when the back clip
    /// flag (code 73) is set.
    pub back_clip: Option<f64>,
    /// 4×3 matrix (column-major in DXF) transforming WCS points into the
    /// block-definition coordinate system — the inverse block transform.
    pub inverse_block_transform: Matrix4,
    /// 4×3 matrix transforming clip boundary points into the block reference
    /// coordinate system.
    pub clip_bound_transform: Matrix4,
}

impl SpatialFilter {
    /// Create a new SpatialFilter
    pub fn new() -> Self {
        SpatialFilter {
            handle: Handle::NULL,
            owner: Handle::NULL,
            boundary_points: Vec::new(),
            normal: Vector3::new(0.0, 0.0, 1.0),
            origin: Vector3::new(0.0, 0.0, 0.0),
            display_enabled: true,
            front_clip: None,
            back_clip: None,
            inverse_block_transform: Matrix4::identity(),
            clip_bound_transform: Matrix4::identity(),
        }
    }
}

impl Default for SpatialFilter {
    fn default() -> Self { Self::new() }
}

/// RasterVariables — global raster image settings
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct RasterVariables {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Class version (code 90)
    pub class_version: i32,
    /// Image frame display (code 70): 0 = no frame, 1 = display frame
    pub display_image_frame: i16,
    /// Image quality (code 71): 0 = draft, 1 = high
    pub image_quality: i16,
    /// Units (code 72): 0 = none, 1 = mm, 2 = cm, 3 = m, 4 = km, 5 = in, 6 = ft, 7 = yd, 8 = mi
    pub units: i16,
}

impl RasterVariables {
    /// Create new RasterVariables
    pub fn new() -> Self {
        RasterVariables {
            handle: Handle::NULL,
            owner: Handle::NULL,
            class_version: 0,
            display_image_frame: 1,
            image_quality: 1,
            units: 0,
        }
    }
}

impl Default for RasterVariables {
    fn default() -> Self { Self::new() }
}

/// BookColor (DBCOLOR) — named color definition
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct BookColor {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Color name (code 1)
    pub color_name: String,
    /// Book name (code 2)
    pub book_name: String,
}

impl BookColor {
    /// Create a new BookColor
    pub fn new() -> Self {
        BookColor {
            handle: Handle::NULL,
            owner: Handle::NULL,
            color_name: String::new(),
            book_name: String::new(),
        }
    }
}

impl Default for BookColor {
    fn default() -> Self { Self::new() }
}

/// AcDbPlaceHolder — placeholder object (no data beyond handle)
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PlaceHolder {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
}

impl PlaceHolder {
    /// Create a new PlaceHolder
    pub fn new() -> Self {
        PlaceHolder {
            handle: Handle::NULL,
            owner: Handle::NULL,
        }
    }
}

impl Default for PlaceHolder {
    fn default() -> Self { Self::new() }
}

/// DictionaryWithDefault — dictionary with a default entry handle
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DictionaryWithDefault {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Dictionary entries (key -> handle)
    pub entries: Vec<(String, Handle)>,
    /// Default entry handle (code 340)
    pub default_handle: Handle,
    /// Duplicate record cloning flag (code 281)
    pub duplicate_cloning: i16,
    /// Hard owner flag (code 280)
    pub hard_owner: bool,
}

impl DictionaryWithDefault {
    /// Create a new DictionaryWithDefault
    pub fn new() -> Self {
        DictionaryWithDefault {
            handle: Handle::NULL,
            owner: Handle::NULL,
            entries: Vec::new(),
            default_handle: Handle::NULL,
            duplicate_cloning: 1,
            hard_owner: false,
        }
    }
}

impl Default for DictionaryWithDefault {
    fn default() -> Self { Self::new() }
}

/// WipeoutVariables — global wipeout display settings
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WipeoutVariables {
    /// Unique handle
    pub handle: Handle,
    /// Owner handle
    pub owner: Handle,
    /// Display image frame (code 70): 0 = no, 1 = yes
    pub display_frame: i16,
}

impl WipeoutVariables {
    /// Create new WipeoutVariables
    pub fn new() -> Self {
        WipeoutVariables {
            handle: Handle::NULL,
            owner: Handle::NULL,
            display_frame: 0,
        }
    }
}

impl Default for WipeoutVariables {
    fn default() -> Self { Self::new() }
}

// StubObject implementations for types that only need handle + owner parsing

macro_rules! impl_stub_object {
    ($ty:ident) => {
        impl StubObject for $ty {
            fn new_stub() -> Self { Self::new() }
            fn set_handle(&mut self, handle: Handle) { self.handle = handle; }
            fn set_owner(&mut self, owner: Handle) { self.owner = owner; }
            fn handle(&self) -> Handle { self.handle }
        }
    };
}

impl_stub_object!(GeoData);
impl_stub_object!(PlaceHolder);
