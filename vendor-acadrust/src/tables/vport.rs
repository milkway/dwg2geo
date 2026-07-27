//! Viewport table entry

use super::TableEntry;
use crate::entities::ViewportRenderMode;
use crate::types::{Handle, Vector2, Vector3};

/// A viewport table entry
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VPort {
    /// Unique handle
    pub handle: Handle,
    /// Viewport name
    pub name: String,
    /// Lower-left corner
    pub lower_left: Vector2,
    /// Upper-right corner
    pub upper_right: Vector2,
    /// View center point
    pub view_center: Vector2,
    /// Snap base point
    pub snap_base: Vector2,
    /// Snap spacing
    pub snap_spacing: Vector2,
    /// Grid spacing
    pub grid_spacing: Vector2,
    /// View direction
    pub view_direction: Vector3,
    /// View target
    pub view_target: Vector3,
    /// View height
    pub view_height: f64,
    /// Aspect ratio
    pub aspect_ratio: f64,
    /// Lens length
    pub lens_length: f64,
    /// View twist angle
    pub view_twist: f64,
    /// Front clipping plane distance
    pub front_clip: f64,
    /// Back clipping plane distance
    pub back_clip: f64,
    /// UCS follow mode
    pub ucsfollow: bool,
    /// Circle zoom percent
    pub circle_zoom: i16,
    /// Fast zoom enabled
    pub fast_zoom: bool,
    /// Grid on/off
    pub grid_on: bool,
    /// Snap on/off
    pub snap_on: bool,
    /// Snap style (isometric)
    pub snap_style: bool,
    /// Snap isometric pair
    pub snap_isopair: i16,
    /// Snap rotation angle
    pub snap_rotation: f64,
    /// Visual style / render mode (DXF code 281)
    pub render_mode: ViewportRenderMode,
}

impl VPort {
    /// Create a new viewport
    pub fn new(name: impl Into<String>) -> Self {
        VPort {
            handle: Handle::NULL,
            name: name.into(),
            lower_left: Vector2::ZERO,
            upper_right: Vector2::new(1.0, 1.0),
            view_center: Vector2::ZERO,
            snap_base: Vector2::ZERO,
            snap_spacing: Vector2::new(0.5, 0.5),
            grid_spacing: Vector2::new(10.0, 10.0),
            view_direction: Vector3::UNIT_Z,
            view_target: Vector3::ZERO,
            view_height: 10.0,
            aspect_ratio: 1.0,
            lens_length: 50.0,
            view_twist: 0.0,
            front_clip: 0.0,
            back_clip: 0.0,
            ucsfollow: false,
            circle_zoom: 100,
            fast_zoom: true,
            grid_on: false,
            snap_on: false,
            snap_style: false,
            snap_isopair: 0,
            snap_rotation: 0.0,
            render_mode: ViewportRenderMode::Wireframe2D,
        }
    }

    /// Create the standard "*Active" viewport
    pub fn active() -> Self {
        Self::new("*Active")
    }
}

impl TableEntry for VPort {
    fn handle(&self) -> Handle {
        self.handle
    }

    fn set_handle(&mut self, handle: Handle) {
        self.handle = handle;
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn set_name(&mut self, name: String) {
        self.name = name;
    }

    fn is_standard(&self) -> bool {
        self.name == "*Active"
    }
}


