use crate::rng::SeededRng;
use noise::{Fbm, NoiseFn, Perlin};

// ── Constants ──

const FBM_FREQ: f32 = 0.3;
const FBM_LACUNARITY: f64 = 2.0;
const FBM_GAIN: f64 = 0.5;
const FBM_OCTAVES: usize = 4;
const GAP_DENSITY: usize = 15;

/// Generate brush density profile.
///
/// Returns: Vec<f32> of length `width`, values in [0, 1].
/// Each call with the same seed produces the same result.
pub fn generate_brush_profile(width: usize, seed: u32) -> Vec<f32> {
    if width == 0 {
        return Vec::new();
    }

    // Step 1: fBm-based bristle pattern
    let mut fbm = Fbm::<Perlin>::new(seed);
    fbm.octaves = FBM_OCTAVES;
    fbm.lacunarity = FBM_LACUNARITY;
    fbm.persistence = FBM_GAIN;

    let mut density: Vec<f32> = (0..width)
        .map(|j| {
            let x = j as f64 * FBM_FREQ as f64;
            let y = seed as f64 * 1000.0;
            fbm.get([x, y]) as f32
        })
        .collect();

    // Normalize to [0, 1]
    let min_val = density.iter().cloned().fold(f32::INFINITY, f32::min);
    let max_val = density.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let range = max_val - min_val + 1e-10;
    for d in density.iter_mut() {
        *d = (*d - min_val) / range;
    }

    // Step 2: Bristle gap insertion
    let mut rng = SeededRng::new(seed + 1000);
    let max_gaps = (width / GAP_DENSITY).max(1) as i32;
    let gap_count = rng.next_i32_range(1, max_gaps);

    for _ in 0..gap_count {
        if width <= 5 {
            break;
        }
        let center = rng.next_i32_range(2, width as i32 - 3);
        let gap_width = rng.next_i32_range(1, 3);
        let depth = rng.next_f32_range(0.7, 1.0);

        for k in (center - gap_width)..=(center + gap_width) {
            if k >= 0 && (k as usize) < width {
                let falloff =
                    1.0 - (k - center).unsigned_abs() as f32 / (gap_width as f32 + 1.0);
                density[k as usize] *= 1.0 - depth * falloff;
            }
        }
    }

    density
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_length() {
        let profile = generate_brush_profile(30, 42);
        assert_eq!(profile.len(), 30);
    }

    #[test]
    fn value_range() {
        let profile = generate_brush_profile(60, 99);
        for &v in &profile {
            assert!(v >= 0.0 && v <= 1.0, "value {} out of range", v);
        }
    }

    #[test]
    fn determinism() {
        let a = generate_brush_profile(30, 42);
        let b = generate_brush_profile(30, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds() {
        let a = generate_brush_profile(30, 42);
        let b = generate_brush_profile(30, 43);
        assert_ne!(a, b);
    }

    #[test]
    fn has_variation() {
        let profile = generate_brush_profile(60, 42);
        let min = profile.iter().cloned().fold(f32::INFINITY, f32::min);
        let max = profile.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(max - min > 0.1, "profile too flat: range = {}", max - min);
    }

    #[test]
    fn zero_width() {
        let profile = generate_brush_profile(0, 42);
        assert!(profile.is_empty());
    }
}
