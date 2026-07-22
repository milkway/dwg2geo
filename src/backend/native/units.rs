//! Drawing-unit policy for CRS-aware conversion (Milestone 5).
//!
//! DWG headers carry two unit hints: `$INSUNITS` (insertion units) and
//! `$MEASUREMENT` (metric/imperial). Neither is authoritative for
//! georeferencing — engineering drawings are routinely drawn with UTM-metre
//! coordinates while the header still says millimetres. The fail-closed rule
//! here: the header is trusted only when it is unambiguous AND internally
//! consistent; otherwise conversion demands an explicit `--source-units`
//! override. The chosen unit and its provenance are always recorded in the
//! report.

/// A resolved linear drawing unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrawingUnit {
    pub name: &'static str,
    pub meters_per_unit: f64,
}

const METER: DrawingUnit = DrawingUnit {
    name: "meters",
    meters_per_unit: 1.0,
};

/// Units accepted for `--source-units` and recognized from `$INSUNITS`.
const KNOWN_UNITS: &[(&[&str], DrawingUnit)] = &[
    (&["m", "meter", "meters", "metre", "metres"], METER),
    (
        &["mm", "millimeter", "millimeters"],
        DrawingUnit {
            name: "millimeters",
            meters_per_unit: 0.001,
        },
    ),
    (
        &["cm", "centimeter", "centimeters"],
        DrawingUnit {
            name: "centimeters",
            meters_per_unit: 0.01,
        },
    ),
    (
        &["dm", "decimeter", "decimeters"],
        DrawingUnit {
            name: "decimeters",
            meters_per_unit: 0.1,
        },
    ),
    (
        &["km", "kilometer", "kilometers"],
        DrawingUnit {
            name: "kilometers",
            meters_per_unit: 1000.0,
        },
    ),
    (
        &["in", "inch", "inches"],
        DrawingUnit {
            name: "inches",
            meters_per_unit: 0.0254,
        },
    ),
    (
        &["ft", "foot", "feet"],
        DrawingUnit {
            name: "feet",
            meters_per_unit: 0.3048,
        },
    ),
    (
        &["usft", "us-survey-foot", "us-survey-feet"],
        DrawingUnit {
            name: "US survey feet",
            meters_per_unit: 1200.0 / 3937.0,
        },
    ),
];

/// Parse a `--source-units` override.
pub fn parse_override(text: &str) -> Result<DrawingUnit, String> {
    let normalized = text.trim().to_ascii_lowercase();
    for (aliases, unit) in KNOWN_UNITS {
        if aliases.contains(&normalized.as_str()) {
            return Ok(*unit);
        }
    }
    Err(format!(
        "unknown unit {text:?}; supported: m, mm, cm, dm, km, in, ft, usft"
    ))
}

/// Resolve the drawing unit from the header, failing closed on anything
/// ambiguous. `insunits` is `$INSUNITS`, `measurement` is `$MEASUREMENT`
/// (0 = english/imperial, 1 = metric).
pub fn from_header(insunits: i16, measurement: i16) -> Result<DrawingUnit, String> {
    let (unit, metric) = match insunits {
        1 => (parse_override("in").unwrap(), false),
        2 => (parse_override("ft").unwrap(), false),
        4 => (parse_override("mm").unwrap(), true),
        5 => (parse_override("cm").unwrap(), true),
        6 => (METER, true),
        7 => (parse_override("km").unwrap(), true),
        14 => (parse_override("dm").unwrap(), true),
        21 => (parse_override("usft").unwrap(), false),
        0 => {
            return Err("the drawing declares unitless coordinates ($INSUNITS = 0)".to_string());
        }
        other => {
            return Err(format!(
                "the drawing declares $INSUNITS code {other}, which is not a supported linear unit"
            ));
        }
    };

    let measurement_metric = match measurement {
        0 => false,
        1 => true,
        other => {
            return Err(format!(
                "the drawing declares an unknown $MEASUREMENT value {other}"
            ));
        }
    };
    if metric != measurement_metric {
        return Err(format!(
            "the drawing's unit hints disagree: $INSUNITS says {} but $MEASUREMENT says {}",
            unit.name,
            if measurement_metric {
                "metric"
            } else {
                "english/imperial"
            }
        ));
    }
    Ok(unit)
}

#[cfg(test)]
mod tests {
    use super::{from_header, parse_override};

    #[test]
    fn overrides_parse_with_aliases_and_case() {
        assert_eq!(parse_override("m").unwrap().meters_per_unit, 1.0);
        assert_eq!(parse_override(" Meters ").unwrap().meters_per_unit, 1.0);
        assert_eq!(parse_override("mm").unwrap().meters_per_unit, 0.001);
        assert_eq!(
            parse_override("usft").unwrap().meters_per_unit,
            1200.0 / 3937.0
        );
        let error = parse_override("furlongs").unwrap_err();
        assert!(error.contains("furlongs"), "{error}");
    }

    #[test]
    fn consistent_headers_resolve() {
        assert_eq!(from_header(6, 1).unwrap().name, "meters");
        assert_eq!(from_header(4, 1).unwrap().name, "millimeters");
        assert_eq!(from_header(1, 0).unwrap().name, "inches");
        assert_eq!(from_header(21, 0).unwrap().name, "US survey feet");
    }

    #[test]
    fn ambiguous_or_inconsistent_headers_fail_closed() {
        // The real reference drawing: metric INSUNITS with english MEASUREMENT.
        let error = from_header(4, 0).unwrap_err();
        assert!(error.contains("disagree"), "{error}");
        let error = from_header(0, 1).unwrap_err();
        assert!(error.contains("unitless"), "{error}");
        let error = from_header(18, 1).unwrap_err();
        assert!(error.contains("18"), "{error}");
        let error = from_header(6, 9).unwrap_err();
        assert!(error.contains("MEASUREMENT"), "{error}");
    }
}
