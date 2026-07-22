//! PROJ-backed reprojection for the native backend (Milestone 5, behind the
//! `native-reproject` feature).
//!
//! The transformer is created with PROJ's normalize-for-visualization mode,
//! so input and output are always x=east/longitude, y=north/latitude
//! regardless of the authority's axis-order definition; that choice is
//! recorded in the report. For linear source CRSs, drawing units are converted
//! into the source CRS's own horizontal axis unit before transformation. For
//! geographic source CRSs, coordinates must already be expressed in the CRS's
//! angular unit and no linear-unit scaling is applied.

use anyhow::{Context, Result, anyhow};
use proj::Proj;
use std::{ffi::CStr, ffi::CString, ptr};

/// The horizontal axis unit PROJ reports for a source CRS.
#[derive(Debug)]
pub struct AxisUnit {
    /// PROJ's conversion factor to the unit's SI base: meters for linear
    /// units, radians for angular units.
    pub factor_to_meters: f64,
    pub name: String,
    pub is_angular: bool,
}

/// A ready-to-use drawing-to-target transform.
pub struct Reprojector {
    proj: Proj,
    /// The source CRS's horizontal axis unit, resolved through PROJ.
    pub crs_unit: AxisUnit,
    /// Applied to drawing coordinates before the CRS transform, in source-CRS
    /// units per drawing unit.
    pub coordinate_scale: f64,
}

/// Human-readable axis-order statement recorded in reports.
pub const AXIS_ORDER: &str = "x=east/longitude, y=north/latitude (normalized for visualization)";

impl Reprojector {
    pub fn new(source_crs: &str, target_crs: &str, meters_per_drawing_unit: f64) -> Result<Self> {
        let crs_unit = source_axis_unit(source_crs)?;
        let coordinate_scale = if crs_unit.is_angular {
            if meters_per_drawing_unit != 1.0 {
                return Err(anyhow!(
                    "linear drawing units ({meters_per_drawing_unit} meters per drawing unit) cannot be applied to geographic source CRS {source_crs:?}; geographic coordinates must already be in the CRS angular unit ({}) — use --source-units m only as an explicit declaration that the coordinates should be trusted without scaling",
                    crs_unit.name
                ));
            }
            1.0
        } else {
            meters_per_drawing_unit / crs_unit.factor_to_meters
        };
        if !coordinate_scale.is_finite() || coordinate_scale <= 0.0 {
            return Err(anyhow!(
                "source CRS {source_crs:?} and drawing-unit conversion produce an invalid coordinate scale ({coordinate_scale}); refusing to transform coordinates"
            ));
        }

        let proj = Proj::new_known_crs(source_crs, target_crs, None).with_context(|| {
            format!(
                "PROJ cannot build a transformation from {source_crs:?} to {target_crs:?}; \
                 use authority:code form (e.g. EPSG:31982)"
            )
        })?;
        Ok(Reprojector {
            proj,
            crs_unit,
            coordinate_scale,
        })
    }

    /// Transform one drawing coordinate to the target CRS.
    pub fn transform(&self, x: f64, y: f64) -> Result<(f64, f64)> {
        let scaled = (x * self.coordinate_scale, y * self.coordinate_scale);
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

/// Query the source CRS's first (horizontal) axis unit through PROJ's C API.
///
/// This is the module's complete unsafe surface. Both `PJ` objects created by
/// PROJ are destroyed on every path after creation, the dedicated context is
/// released after its objects, returned C strings are copied before
/// destruction, and every pointer is checked before dereference.
fn source_axis_unit(source_crs: &str) -> Result<AxisUnit> {
    let definition = CString::new(source_crs)
        .map_err(|_| anyhow!("source CRS contains an interior NUL byte: {source_crs:?}"))?;

    // SAFETY: `definition` is a live NUL-terminated string. Every pointer
    // returned by PROJ is checked, and `crs`/`coordinate_system` are destroyed
    // exactly once before their dedicated context, after all borrowed strings
    // have been copied.
    unsafe {
        let context = proj_sys::proj_context_create();
        if context.is_null() {
            return Err(anyhow!(
                "PROJ cannot create a context while determining the horizontal axis unit for source CRS {source_crs:?}"
            ));
        }

        let crs = proj_sys::proj_create(context, definition.as_ptr());
        if crs.is_null() {
            proj_sys::proj_context_destroy(context);
            return Err(anyhow!(
                "PROJ cannot resolve source CRS {source_crs:?} while determining its horizontal axis unit; use an authority:code CRS such as EPSG:31982"
            ));
        }

        let crs_type = proj_sys::proj_get_type(crs);
        let coordinate_system = proj_sys::proj_crs_get_coordinate_system(context, crs);
        if coordinate_system.is_null() {
            proj_sys::proj_destroy(crs);
            proj_sys::proj_context_destroy(context);
            return Err(anyhow!(
                "PROJ cannot determine the horizontal axis unit for source CRS {source_crs:?}; the CRS must expose a horizontal coordinate system with a known unit"
            ));
        }

        let mut unit_factor = 0.0;
        let mut unit_name = ptr::null();
        let axis_ok = proj_sys::proj_cs_get_axis_info(
            context,
            coordinate_system,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut unit_factor,
            &mut unit_name,
            ptr::null_mut(),
            ptr::null_mut(),
        );

        let result = if axis_ok == 0 || unit_name.is_null() {
            Err(anyhow!(
                "PROJ cannot determine the horizontal axis unit for source CRS {source_crs:?}; axis 0 has no known unit"
            ))
        } else if !unit_factor.is_finite() || unit_factor <= 0.0 {
            Err(anyhow!(
                "PROJ returned an invalid horizontal axis unit conversion factor ({unit_factor}) for source CRS {source_crs:?}; refusing to guess a coordinate scale"
            ))
        } else {
            match CStr::from_ptr(unit_name).to_str() {
                Err(_) => Err(anyhow!(
                    "PROJ returned a non-UTF-8 horizontal axis unit name for source CRS {source_crs:?}; refusing to guess a coordinate scale"
                )),
                Ok(name) => {
                    let name = name.to_string();
                    let normalized_name = name.to_ascii_lowercase();
                    let is_geographic = matches!(
                        crs_type,
                        proj_sys::PJ_TYPE_PJ_TYPE_GEOGRAPHIC_2D_CRS
                            | proj_sys::PJ_TYPE_PJ_TYPE_GEOGRAPHIC_3D_CRS
                    );
                    let name_is_angular = ["degree", "radian", "grad", "arc-minute", "arc-second"]
                        .iter()
                        .any(|token| normalized_name.contains(token));
                    Ok(AxisUnit {
                        factor_to_meters: unit_factor,
                        name,
                        is_angular: is_geographic || name_is_angular,
                    })
                }
            }
        };

        proj_sys::proj_destroy(coordinate_system);
        proj_sys::proj_destroy(crs);
        proj_sys::proj_context_destroy(context);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::{Reprojector, source_axis_unit};

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
    fn metric_crs_unit_scale_applies_before_the_crs_transform() {
        let meters = Reprojector::new("EPSG:31982", "EPSG:4326", 1.0).expect("build proj");
        let millimeters = Reprojector::new("EPSG:31982", "EPSG:4326", 0.001).expect("build proj");
        let a = meters.transform(248_000.0, 7_396_000.0).expect("transform");
        let b = millimeters
            .transform(248_000_000.0, 7_396_000_000.0)
            .expect("transform");
        assert!((a.0 - b.0).abs() < 1e-12 && (a.1 - b.1).abs() < 1e-12);
    }

    #[test]
    fn us_survey_foot_drawing_matches_us_survey_foot_crs() {
        let reprojector =
            Reprojector::new("EPSG:2263", "EPSG:4326", 1200.0 / 3937.0).expect("build proj");
        let (lon, lat) = reprojector
            .transform(987_000.0, 212_000.0)
            .expect("transform");
        assert!((lon - -73.990_075).abs() < 1e-4, "lon {lon}");
        assert!((lat - 40.748_567).abs() < 1e-4, "lat {lat}");
        assert!((reprojector.coordinate_scale - 1.0).abs() < 1e-12);
        assert!(
            reprojector
                .crs_unit
                .name
                .to_ascii_lowercase()
                .contains("survey foot"),
            "{}",
            reprojector.crs_unit.name
        );
    }

    #[test]
    fn meter_drawing_is_converted_to_us_survey_foot_crs_units() {
        let foot_factor = 1200.0 / 3937.0;
        let feet =
            Reprojector::new("EPSG:2263", "EPSG:4326", foot_factor).expect("build feet proj");
        let meters = Reprojector::new("EPSG:2263", "EPSG:4326", 1.0).expect("build meter proj");
        let from_feet = feet.transform(987_000.0, 212_000.0).expect("feet");
        let from_meters = meters
            .transform(987_000.0 * foot_factor, 212_000.0 * foot_factor)
            .expect("meters");
        assert!((meters.coordinate_scale - 1.0 / foot_factor).abs() < 1e-12);
        assert!((from_feet.0 - from_meters.0).abs() < 1e-12);
        assert!((from_feet.1 - from_meters.1).abs() < 1e-12);
    }

    #[test]
    fn meter_crs_keeps_scale_one_and_current_output() {
        let reprojector = Reprojector::new("EPSG:31982", "EPSG:4326", 1.0).expect("build proj");
        let direct =
            proj::Proj::new_known_crs("EPSG:31982", "EPSG:4326", None).expect("build direct proj");
        let actual = reprojector
            .transform(248_000.0, 7_396_000.0)
            .expect("transform");
        let expected: (f64, f64) = direct
            .convert((248_000.0, 7_396_000.0))
            .expect("direct transform");
        assert_eq!(reprojector.coordinate_scale, 1.0);
        assert_eq!(actual, expected);
    }

    #[test]
    fn geographic_crs_rejects_linear_scaling_but_accepts_trust_declaration() {
        let error = Reprojector::new("EPSG:4326", "EPSG:4326", 1000.0)
            .err()
            .expect("kilometers must fail");
        let message = format!("{error:#}");
        assert!(message.contains("geographic"), "{message}");
        assert!(message.contains("--source-units m"), "{message}");

        let trusted = Reprojector::new("EPSG:4326", "EPSG:4326", 1.0).expect("trust coordinates");
        assert_eq!(trusted.coordinate_scale, 1.0);
        assert_eq!(
            trusted.transform(-73.990_075, 40.748_567).unwrap(),
            (-73.990_075, 40.748_567)
        );
    }

    #[test]
    fn compound_crs_without_horizontal_coordinate_system_fails_closed() {
        let error = source_axis_unit("EPSG:7405").expect_err("compound CRS unit query must fail");
        let message = format!("{error:#}");
        assert!(message.contains("horizontal axis unit"), "{message}");
        assert!(message.contains("EPSG:7405"), "{message}");
    }

    #[test]
    fn unknown_crs_is_an_actionable_error() {
        let Err(error) = Reprojector::new("EPSG:999999999", "EPSG:4326", 1.0) else {
            panic!("unknown CRS must fail");
        };
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
