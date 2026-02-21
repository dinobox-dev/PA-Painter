use crate::types::PressurePreset;
use std::f32::consts::PI;

/// Evaluate pressure at stroke progress t (0.0 = start, 1.0 = end).
pub fn evaluate_pressure(preset: PressurePreset, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match preset {
        PressurePreset::Uniform => 1.0,
        PressurePreset::FadeOut => 1.0 - t * t,
        PressurePreset::FadeIn => t.sqrt(),
        PressurePreset::Bell => (PI * t).sin().max(0.0),
        PressurePreset::Taper => (PI * t).sin().max(0.0).sqrt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PressurePreset::*;

    const EPS: f32 = 1e-5;

    #[test]
    fn uniform_always_one() {
        assert!((evaluate_pressure(Uniform, 0.0) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(Uniform, 0.5) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(Uniform, 1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn fade_out_endpoints() {
        assert!((evaluate_pressure(FadeOut, 0.0) - 1.0).abs() < EPS);
        assert!((evaluate_pressure(FadeOut, 1.0) - 0.0).abs() < EPS);
    }

    #[test]
    fn fade_out_midpoint() {
        assert!((evaluate_pressure(FadeOut, 0.5) - 0.75).abs() < EPS);
    }

    #[test]
    fn fade_in_endpoints() {
        assert!((evaluate_pressure(FadeIn, 0.0) - 0.0).abs() < EPS);
        assert!((evaluate_pressure(FadeIn, 1.0) - 1.0).abs() < EPS);
    }

    #[test]
    fn bell_peak() {
        assert!((evaluate_pressure(Bell, 0.5) - 1.0).abs() < EPS);
    }

    #[test]
    fn bell_symmetry() {
        let p25 = evaluate_pressure(Bell, 0.25);
        let p75 = evaluate_pressure(Bell, 0.75);
        assert!((p25 - p75).abs() < EPS);
    }

    #[test]
    fn taper_peak() {
        assert!((evaluate_pressure(Taper, 0.5) - 1.0).abs() < EPS);
    }

    #[test]
    fn all_presets_non_negative_at_endpoints() {
        for preset in [Uniform, FadeOut, FadeIn, Bell, Taper] {
            assert!(evaluate_pressure(preset, 0.0) >= 0.0);
            assert!(evaluate_pressure(preset, 1.0) >= 0.0);
        }
    }
}
