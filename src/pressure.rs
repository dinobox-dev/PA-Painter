use crate::types::{CurveKnot, PressureCurve, PressurePreset};
use std::f32::consts::PI;

/// Evaluate pressure at stroke progress t (0.0 = start, 1.0 = end).
pub fn evaluate_pressure(curve: &PressureCurve, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match curve {
        PressureCurve::Preset(preset) => evaluate_preset(*preset, t),
        PressureCurve::Custom(knots) => evaluate_bezier_spline(knots, t),
    }
}

fn evaluate_preset(preset: PressurePreset, t: f32) -> f32 {
    match preset {
        PressurePreset::Uniform => 1.0,
        PressurePreset::FadeOut => 1.0 - t * t,
        PressurePreset::FadeIn => t.sqrt(),
        PressurePreset::Bell => (PI * t).sin().max(0.0),
        PressurePreset::Taper => (PI * t).sin().max(0.0).sqrt(),
    }
}

/// Evaluate a piecewise cubic Bézier spline at parameter t.
///
/// For each segment between adjacent knots, the four control points are:
///   P0 = knots[i].pos,  P1 = knots[i].handle_out,
///   P2 = knots[i+1].handle_in,  P3 = knots[i+1].pos
///
/// Because handle x-values may differ from a linear parameterization,
/// we use Newton's method to find the Bézier parameter u that yields
/// the desired x-coordinate, then evaluate y at that u.
fn evaluate_bezier_spline(knots: &[CurveKnot], t: f32) -> f32 {
    if knots.len() < 2 {
        return if knots.is_empty() {
            1.0
        } else {
            knots[0].pos[1].max(0.0)
        };
    }

    // Find segment
    let n = knots.len();
    let mut seg = 0;
    for i in 0..n - 1 {
        if knots[i + 1].pos[0] >= t {
            seg = i;
            break;
        }
        seg = i;
    }

    let p0 = knots[seg].pos;
    let p1 = knots[seg].handle_out;
    let p2 = knots[seg + 1].handle_in;
    let p3 = knots[seg + 1].pos;

    // Linear initial guess
    let span = p3[0] - p0[0];
    let mut u = if span > 1e-6 {
        ((t - p0[0]) / span).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Newton iterations to solve B_x(u) = t
    for _ in 0..8 {
        let mu = 1.0 - u;
        let mu2 = mu * mu;
        let mu3 = mu2 * mu;
        let u2 = u * u;
        let u3 = u2 * u;

        let bx = mu3 * p0[0] + 3.0 * mu2 * u * p1[0] + 3.0 * mu * u2 * p2[0] + u3 * p3[0];
        let dbx = 3.0 * mu2 * (p1[0] - p0[0])
            + 6.0 * mu * u * (p2[0] - p1[0])
            + 3.0 * u2 * (p3[0] - p2[0]);

        if dbx.abs() < 1e-10 {
            break;
        }
        let delta = (bx - t) / dbx;
        u = (u - delta).clamp(0.0, 1.0);
        if delta.abs() < 1e-6 {
            break;
        }
    }

    // Evaluate y at the solved u
    let mu = 1.0 - u;
    let y = mu * mu * mu * p0[1]
        + 3.0 * mu * mu * u * p1[1]
        + 3.0 * mu * u * u * p2[1]
        + u * u * u * p3[1];

    y.clamp(0.0, 2.0)
}

/// Convert a preset to a custom Bézier spline by sampling at evenly-spaced
/// points and computing smooth handles.
pub fn preset_to_custom(preset: PressurePreset) -> PressureCurve {
    let n = 4; // 5 sample points: 0, 0.25, 0.5, 0.75, 1.0
    let samples: Vec<[f32; 2]> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            [t, evaluate_preset(preset, t)]
        })
        .collect();

    let len = samples.len();
    let knots: Vec<CurveKnot> = (0..len)
        .map(|i| {
            let prev = if i > 0 { Some(samples[i - 1]) } else { None };
            let next = if i + 1 < len {
                Some(samples[i + 1])
            } else {
                None
            };
            CurveKnot::smooth(samples[i], prev, next)
        })
        .collect();

    PressureCurve::Custom(knots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PressurePreset::*;

    const EPS: f32 = 1e-5;

    fn preset(p: PressurePreset) -> PressureCurve {
        PressureCurve::Preset(p)
    }

    #[test]
    fn uniform_always_one() {
        assert!((evaluate_pressure(&preset(Uniform), 0.0) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(&preset(Uniform), 0.5) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(&preset(Uniform), 1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn fade_out_endpoints() {
        assert!((evaluate_pressure(&preset(FadeOut), 0.0) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(&preset(FadeOut), 1.0) - 0.0).abs() < EPS);
    }

    #[test]
    fn fade_out_midpoint() {
        assert!((evaluate_pressure(&preset(FadeOut), 0.5) - 0.75).abs() < EPS);
    }

    #[test]
    fn fade_in_endpoints() {
        assert!((evaluate_pressure(&preset(FadeIn), 0.0) - 0.0).abs() < EPS);
        assert!((evaluate_pressure(&preset(FadeIn), 1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn bell_peak() {
        assert!((evaluate_pressure(&preset(Bell), 0.5) - 1.0).abs() < EPS);
    }

    #[test]
    fn bell_symmetry() {
        let p25 = evaluate_pressure(&preset(Bell), 0.25);
        let p75 = evaluate_pressure(&preset(Bell), 0.75);
        assert!((p25 - p75).abs() < EPS);
    }

    #[test]
    fn taper_peak() {
        assert!((evaluate_pressure(&preset(Taper), 0.5) - 1.0).abs() < EPS);
    }

    #[test]
    fn all_presets_non_negative_at_endpoints() {
        for p in [Uniform, FadeOut, FadeIn, Bell, Taper] {
            assert!(evaluate_pressure(&preset(p), 0.0) >= 0.0);
            assert!(evaluate_pressure(&preset(p), 1.0) >= 0.0);
        }
    }

    #[test]
    fn custom_two_knot_linear() {
        // Straight line from y=1 at x=0 to y=0 at x=1,
        // with handles at 1/3 and 2/3 along the line.
        let curve = PressureCurve::Custom(vec![
            CurveKnot {
                pos: [0.0, 1.0],
                handle_in: [0.0, 1.0],
                handle_out: [0.333, 0.667],
            },
            CurveKnot {
                pos: [1.0, 0.0],
                handle_in: [0.667, 0.333],
                handle_out: [1.0, 0.0],
            },
        ]);
        assert!((evaluate_pressure(&curve, 0.0) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(&curve, 1.0) - 0.0).abs() < EPS);
        assert!((evaluate_pressure(&curve, 0.5) - 0.5).abs() < 0.05);
    }

    #[test]
    fn custom_flat() {
        let curve = PressureCurve::Custom(vec![
            CurveKnot {
                pos: [0.0, 1.0],
                handle_in: [0.0, 1.0],
                handle_out: [0.33, 1.0],
            },
            CurveKnot {
                pos: [1.0, 1.0],
                handle_in: [0.67, 1.0],
                handle_out: [1.0, 1.0],
            },
        ]);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            assert!(
                (evaluate_pressure(&curve, t) - 1.0).abs() < 0.01,
                "at t={t}: {}",
                evaluate_pressure(&curve, t)
            );
        }
    }

    #[test]
    fn custom_passes_through_knots() {
        let curve = preset_to_custom(FadeOut);
        // Should pass through the knot positions (especially endpoints)
        if let PressureCurve::Custom(ref knots) = curve {
            let start_y = evaluate_pressure(&curve, knots[0].pos[0]);
            let end_y = evaluate_pressure(&curve, knots.last().unwrap().pos[0]);
            assert!((start_y - knots[0].pos[1]).abs() < 0.01);
            assert!((end_y - knots.last().unwrap().pos[1]).abs() < 0.01);
        }
    }

    #[test]
    fn preset_to_custom_endpoints_match() {
        for p in [Uniform, FadeOut, FadeIn, Bell, Taper] {
            let custom = preset_to_custom(p);
            let orig_start = evaluate_pressure(&preset(p), 0.0);
            let orig_end = evaluate_pressure(&preset(p), 1.0);
            assert!(
                (evaluate_pressure(&custom, 0.0) - orig_start).abs() < 0.05,
                "preset {:?} start mismatch",
                p
            );
            assert!(
                (evaluate_pressure(&custom, 1.0) - orig_end).abs() < 0.05,
                "preset {:?} end mismatch",
                p
            );
        }
    }
}
