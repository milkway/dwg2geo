//! Local control-point calibration for drawing coordinates.
//!
//! Although the roadmap calls this an affine calibration, this module
//! deliberately fits only a four-parameter similarity (Helmert) transform.
//! A full six-parameter affine transform can shear or scale the two axes
//! independently, distorting angles, proportions, and other engineering
//! geometry. Rotation, uniform scale, and translation establish a local
//! coordinate relationship without introducing those distortions.

// This public API is consumed by the concurrent Milestone 5 integration work.
#![allow(dead_code)]

/// One drawing-to-target correspondence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlPoint {
    pub source: (f64, f64),
    pub target: (f64, f64),
}

/// 4-parameter similarity (Helmert) transform: rotation + uniform scale +
/// translation. target = [a -b; b a] * source + [tx, ty].
#[derive(Clone, Copy, Debug)]
pub struct Calibration {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

/// Fit quality, reportable.
#[derive(Clone, Debug)]
pub struct CalibrationQuality {
    pub scale: f64,
    pub rotation_deg: f64,
    pub residuals: Vec<f64>,
    pub rms_error: f64,
    pub max_error: f64,
}

impl Calibration {
    pub fn apply(&self, point: (f64, f64)) -> (f64, f64) {
        (
            self.a * point.0 - self.b * point.1 + self.tx,
            self.b * point.0 + self.a * point.1 + self.ty,
        )
    }
}

/// Fits a least-squares similarity transform to drawing-to-target control points.
///
/// Two distinct source points determine an exact transform. Additional points
/// produce a least-squares fit and one target-unit residual per input point.
pub fn solve(points: &[ControlPoint]) -> Result<(Calibration, CalibrationQuality), String> {
    if points.len() < 2 {
        return Err(format!(
            "calibration requires at least 2 control points, but {} were provided",
            points.len()
        ));
    }

    let mut source_sum = (0.0, 0.0);
    let mut target_sum = (0.0, 0.0);
    for (index, point) in points.iter().enumerate() {
        if !point.source.0.is_finite()
            || !point.source.1.is_finite()
            || !point.target.0.is_finite()
            || !point.target.1.is_finite()
        {
            return Err(format!(
                "control point {} contains a non-finite coordinate",
                index + 1
            ));
        }
        source_sum.0 += point.source.0;
        source_sum.1 += point.source.1;
        target_sum.0 += point.target.0;
        target_sum.1 += point.target.1;
    }

    let count = points.len() as f64;
    let source_centroid = (source_sum.0 / count, source_sum.1 / count);
    let target_centroid = (target_sum.0 / count, target_sum.1 / count);

    let mut source_spread = 0.0;
    let mut dot_sum = 0.0;
    let mut cross_sum = 0.0;
    for point in points {
        let sx = point.source.0 - source_centroid.0;
        let sy = point.source.1 - source_centroid.1;
        let tx = point.target.0 - target_centroid.0;
        let ty = point.target.1 - target_centroid.1;

        source_spread += sx * sx + sy * sy;
        dot_sum += sx * tx + sy * ty;
        cross_sum += sx * ty - sy * tx;
    }

    if !source_spread.is_finite() || source_spread <= 1.0e-12 {
        return Err(
            "source control points are coincident or degenerate; no unique calibration exists"
                .to_string(),
        );
    }

    let a = dot_sum / source_spread;
    let b = cross_sum / source_spread;
    let scale = a.hypot(b);
    if !scale.is_finite() {
        return Err("calibration produced non-finite transform parameters".to_string());
    }
    if scale == 0.0 {
        return Err("target control points produce a collapsed zero-scale calibration".to_string());
    }

    let tx = target_centroid.0 - a * source_centroid.0 + b * source_centroid.1;
    let ty = target_centroid.1 - b * source_centroid.0 - a * source_centroid.1;
    if !tx.is_finite() || !ty.is_finite() {
        return Err("calibration produced non-finite transform parameters".to_string());
    }

    let calibration = Calibration { a, b, tx, ty };
    let mut residuals = Vec::with_capacity(points.len());
    let mut squared_error_sum = 0.0;
    let mut max_error: f64 = 0.0;
    for point in points {
        let fitted = calibration.apply(point.source);
        let residual = (fitted.0 - point.target.0).hypot(fitted.1 - point.target.1);
        squared_error_sum += residual * residual;
        max_error = max_error.max(residual);
        residuals.push(residual);
    }

    let rms_error = (squared_error_sum / count).sqrt();
    if !rms_error.is_finite() {
        return Err("calibration produced non-finite residual errors".to_string());
    }

    let quality = CalibrationQuality {
        scale,
        rotation_deg: b.atan2(a).to_degrees(),
        residuals,
        rms_error,
        max_error,
    };

    Ok((calibration, quality))
}

/// Parses a drawing-to-target correspondence formatted as `dx,dy=X,Y`.
pub fn parse_control_point(text: &str) -> Result<ControlPoint, String> {
    let error = |detail: &str| format!("invalid control point '{text}': {detail}");
    let (source, target) = text
        .split_once('=')
        .ok_or_else(|| error("expected 'drawing_x,drawing_y=target_x,target_y'"))?;
    if target.contains('=') {
        return Err(error("expected exactly one '=' separator"));
    }

    fn parse_pair(pair: &str, side: &str, text: &str) -> Result<(f64, f64), String> {
        let values: Vec<&str> = pair.split(',').collect();
        if values.len() != 2 {
            return Err(format!(
                "invalid control point '{text}': {side} side must contain exactly two comma-separated numbers"
            ));
        }

        let parse_number = |value: &str, axis: &str| -> Result<f64, String> {
            let number = value.trim().parse::<f64>().map_err(|_| {
                format!(
                    "invalid control point '{text}': {side} {axis} coordinate '{}' is not a number",
                    value.trim()
                )
            })?;
            if !number.is_finite() {
                return Err(format!(
                    "invalid control point '{text}': {side} {axis} coordinate must be finite"
                ));
            }
            Ok(number)
        };

        Ok((parse_number(values[0], "x")?, parse_number(values[1], "y")?))
    }

    Ok(ControlPoint {
        source: parse_pair(source, "drawing", text)?,
        target: parse_pair(target, "target", text)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f64 = 1.0e-9;

    fn assert_near(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {actual} to be within {tolerance} of {expected}"
        );
    }

    fn transformed(calibration: Calibration, source: (f64, f64)) -> ControlPoint {
        ControlPoint {
            source,
            target: calibration.apply(source),
        }
    }

    #[test]
    fn two_points_recover_exact_similarity() {
        let expected = Calibration {
            a: 0.0,
            b: 2.0,
            tx: 100.0,
            ty: -50.0,
        };
        let points = [
            transformed(expected, (1.0, 2.0)),
            transformed(expected, (4.0, 6.0)),
        ];

        let (fitted, quality) = solve(&points).unwrap();

        assert_near(fitted.a, expected.a, EPSILON);
        assert_near(fitted.b, expected.b, EPSILON);
        assert_near(fitted.tx, expected.tx, EPSILON);
        assert_near(fitted.ty, expected.ty, EPSILON);
        let actual = fitted.apply((-3.0, 7.5));
        let target = expected.apply((-3.0, 7.5));
        assert_near(actual.0, target.0, EPSILON);
        assert_near(actual.1, target.1, EPSILON);
        assert_near(quality.scale, 2.0, EPSILON);
        assert_near(quality.rotation_deg, 90.0, EPSILON);
        assert_eq!(quality.residuals.len(), 2);
        assert!(
            quality
                .residuals
                .iter()
                .all(|residual| *residual <= EPSILON)
        );
    }

    #[test]
    fn noisy_points_report_fit_residuals() {
        let expected = Calibration {
            a: 1.2,
            b: -0.4,
            tx: 10.0,
            ty: 20.0,
        };
        let sources = [(-2.0, -1.0), (3.0, -1.0), (3.0, 4.0), (-2.0, 4.0)];
        let noise = [(0.01, -0.02), (-0.03, 0.01), (0.02, 0.04), (0.0, -0.01)];
        let points: Vec<ControlPoint> = sources
            .iter()
            .zip(noise)
            .map(|(&source, noise)| {
                let target = expected.apply(source);
                ControlPoint {
                    source,
                    target: (target.0 + noise.0, target.1 + noise.1),
                }
            })
            .collect();

        let (fitted, quality) = solve(&points).unwrap();

        assert_near(fitted.a, expected.a, 0.01);
        assert_near(fitted.b, expected.b, 0.01);
        assert_near(fitted.tx, expected.tx, 0.03);
        assert_near(fitted.ty, expected.ty, 0.03);
        assert_eq!(quality.residuals.len(), 4);
        assert!(quality.rms_error > 0.0);
        assert!(quality.rms_error <= quality.max_error);
        for (point, residual) in points.iter().zip(&quality.residuals) {
            let actual = fitted.apply(point.source);
            let expected_residual = (actual.0 - point.target.0).hypot(actual.1 - point.target.1);
            assert_near(*residual, expected_residual, 1.0e-12);
        }
    }

    #[test]
    fn identity_points_fit_identity() {
        let points = [
            ControlPoint {
                source: (-5.0, 2.0),
                target: (-5.0, 2.0),
            },
            ControlPoint {
                source: (1.0, 8.0),
                target: (1.0, 8.0),
            },
            ControlPoint {
                source: (7.0, -3.0),
                target: (7.0, -3.0),
            },
        ];

        let (fitted, quality) = solve(&points).unwrap();

        assert_near(fitted.a, 1.0, EPSILON);
        assert_near(fitted.b, 0.0, EPSILON);
        assert_near(fitted.tx, 0.0, EPSILON);
        assert_near(fitted.ty, 0.0, EPSILON);
        assert_near(quality.scale, 1.0, EPSILON);
        assert_near(quality.rotation_deg, 0.0, EPSILON);
        assert!(
            quality
                .residuals
                .iter()
                .all(|residual| *residual <= EPSILON)
        );
    }

    #[test]
    fn solve_rejects_too_few_degenerate_and_non_finite_points() {
        assert!(solve(&[]).is_err());
        assert!(
            solve(&[ControlPoint {
                source: (0.0, 0.0),
                target: (1.0, 1.0),
            }])
            .is_err()
        );

        let coincident = [
            ControlPoint {
                source: (2.0, 3.0),
                target: (1.0, 1.0),
            },
            ControlPoint {
                source: (2.0, 3.0),
                target: (4.0, 5.0),
            },
        ];
        assert!(solve(&coincident).unwrap_err().contains("degenerate"));

        let non_finite = [
            ControlPoint {
                source: (0.0, f64::NAN),
                target: (1.0, 1.0),
            },
            ControlPoint {
                source: (2.0, 3.0),
                target: (f64::INFINITY, 5.0),
            },
        ];
        assert!(solve(&non_finite).unwrap_err().contains("non-finite"));
    }

    #[test]
    fn solve_rejects_collapsed_target_points() {
        let points = [
            ControlPoint {
                source: (0.0, 0.0),
                target: (5.0, 5.0),
            },
            ControlPoint {
                source: (2.0, 3.0),
                target: (5.0, 5.0),
            },
        ];

        assert!(solve(&points).unwrap_err().contains("zero-scale"));
    }

    #[test]
    fn parser_accepts_whitespace() {
        assert_eq!(
            parse_control_point(" 1000 , 2000 = 247500.0 , 7395000.0 ").unwrap(),
            ControlPoint {
                source: (1000.0, 2000.0),
                target: (247500.0, 7395000.0),
            }
        );
    }

    #[test]
    fn parser_errors_quote_the_offending_input() {
        let invalid = [
            "1,2 3,4",
            "1=2,3",
            "1,2,3=4,5",
            "1,2=3",
            "1,2=3,4,5",
            "one,2=3,4",
        ];

        for text in invalid {
            let error = parse_control_point(text).unwrap_err();
            assert!(
                error.contains(text),
                "error did not quote offending input: {error}"
            );
        }
    }

    #[test]
    fn solving_is_bit_deterministic() {
        let points = [
            ControlPoint {
                source: (-3.0, 7.0),
                target: (100.2, -5.3),
            },
            ControlPoint {
                source: (2.5, 4.0),
                target: (104.7, -1.1),
            },
            ControlPoint {
                source: (9.0, -6.0),
                target: (113.8, 4.4),
            },
        ];

        let (first, _) = solve(&points).unwrap();
        let (second, _) = solve(&points).unwrap();

        assert_eq!(first.a.to_bits(), second.a.to_bits());
        assert_eq!(first.b.to_bits(), second.b.to_bits());
        assert_eq!(first.tx.to_bits(), second.tx.to_bits());
        assert_eq!(first.ty.to_bits(), second.ty.to_bits());
    }
}
