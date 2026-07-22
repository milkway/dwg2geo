//! PROJ-backed reprojection for the native backend (Milestone 5, behind the
//! `native-reproject` feature).
//!
//! The transformer is created with PROJ's normalize-for-visualization mode,
//! so input and output are always x=east/longitude, y=north/latitude
//! regardless of the authority's axis-order definition; that choice is
//! recorded in the report. Drawing coordinates are scaled to meters before
//! the CRS transform under the documented assumption that the source CRS is
//! a meter-based projected system.

use anyhow::{Context, Result, anyhow};
use proj::Proj;

/// A ready-to-use drawing-to-target transform.
pub struct Reprojector {
    proj: Proj,
    /// Applied to drawing coordinates before the CRS transform.
    pub meters_per_drawing_unit: f64,
}

/// Human-readable axis-order statement recorded in reports.
pub const AXIS_ORDER: &str = "x=east/longitude, y=north/latitude (normalized for visualization)";

impl Reprojector {
    pub fn new(source_crs: &str, target_crs: &str, meters_per_drawing_unit: f64) -> Result<Self> {
        let proj = Proj::new_known_crs(source_crs, target_crs, None).with_context(|| {
            format!(
                "PROJ cannot build a transformation from {source_crs:?} to {target_crs:?}; \
                 use authority:code form (e.g. EPSG:31982)"
            )
        })?;
        Ok(Reprojector {
            proj,
            meters_per_drawing_unit,
        })
    }

    /// Transform one drawing coordinate to the target CRS.
    pub fn transform(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let scaled = (
            x * self.meters_per_drawing_unit,
            y * self.meters_per_drawing_unit,
        );
        let (tx, ty): (f64, f64) = self
            .proj
            .convert(scaled)
            .map_err(|error| anyhow!("PROJ transform failed at ({x}, {y}): {error}"))?;
        if !tx.is_finite() || !ty.is_finite() {
            return Err(anyhow!(
                "PROJ transform produced non-finite coordinates at ({x}, {y}); \
                 the source CRS or units are probably wrong for this drawing"
            ));
        }
        Ok((tx, ty))
    }

    /// The PROJ library version string.
    pub fn proj_version(&self) -> String {
        self.proj
            .lib_info()
            .map(|info| info.version)
            .unwrap_or_else(|_| "unknown".to_string())
    }

    /// The transformation pipeline definition, when PROJ exposes one.
    pub fn pipeline(&self) -> Option<String> {
        self.proj.proj_info().definition
    }
}

#[cfg(test)]
mod tests {
    use super::Reprojector;

    #[test]
    fn utm_22s_to_wgs84_lands_in_brazil() {
        let reprojector = Reprojector::new("EPSG:31982", "EPSG:4326", 1.0).expect("build proj");
        // Coordinates from the reference drawing's extents.
        let (lon, lat) = reprojector
            .transform(248_000.0, 7_396_000.0)
            .expect("transform");
        assert!((-56.0..=-48.0).contains(&lon), "lon {lon}");
        assert!((-26.0..=-21.0).contains(&lat), "lat {lat}");
    }

    #[test]
    fn unit_scale_applies_before_the_crs_transform() {
        let meters = Reprojector::new("EPSG:31982", "EPSG:4326", 1.0).expect("build proj");
        let millimeters = Reprojector::new("EPSG:31982", "EPSG:4326", 0.001).expect("build proj");
        let a = meters.transform(248_000.0, 7_396_000.0).expect("transform");
        let b = millimeters
            .transform(248_000_000.0, 7_396_000_000.0)
            .expect("transform");
        assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
    }

    #[test]
    fn unknown_crs_is_an_actionable_error() {
        let error = Reprojector::new("EPSG:999999999", "EPSG:4326", 1.0)
            .err()
            .expect("unknown CRS must fail");
        assert!(format!("{error:#}").contains("EPSG:999999999"));
    }

    #[test]
    fn identity_transform_is_exact_and_deterministic() {
        let reprojector = Reprojector::new("EPSG:31982", "EPSG:31982", 1.0).expect("build proj");
        let first = reprojector
            .transform(248_000.0, 7_396_000.0)
            .expect("transform");
        let second = reprojector
            .transform(248_000.0, 7_396_000.0)
            .expect("transform");
        assert_eq!(first, second);
        assert!((first.0 - 248_000.0).abs() < 1e-6 && (first.1 - 7_396_000.0).abs() < 1e-6);
    }
}
