use crate::math::{interpolate_array, lerp, smoothstep};
use crate::pressure::evaluate_pressure;
use crate::types::{PressureCurve, StrokeParams};
use noise::{NoiseFn, Perlin};

// ── Constants ──

const MIN_WIDTH_RATIO: f32 = 0.3;
const DEPLETION_FLOOR: f32 = 0.15;
const DEPLETION_EXPONENT: f32 = 0.7;

/// Result of stroke density map generation.
pub struct StrokeHeightResult {
    /// Density map in local coordinates.
    /// Dimensions: height = brush_width_px, width = stroke_length_px
    /// Stored row-major: data[y * width + x]
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

/// Pre-computed Sobel gradients for a stroke in local coordinates.
pub struct StrokeGradientResult {
    /// Gradient in local-X direction (along stroke). Row-major.
    pub gx: Vec<f32>,
    /// Gradient in local-Y direction (across stroke). Row-major.
    pub gy: Vec<f32>,
    pub width: usize,
    pub height: usize,
}

/// Compute Sobel gradients from a local stroke density map, attenuated by pressure.
///
/// High pressure flattens paint, so gradients (and thus normals) are reduced.
/// Attenuation per column: `1 - p²`, where p = pressure at that stroke position.
/// Edge pixels are left at 0, matching the natural density falloff.
pub fn compute_stroke_gradients(
    height: &StrokeHeightResult,
    pressure_curve: &PressureCurve,
) -> StrokeGradientResult {
    let w = height.width;
    let h = height.height;
    let mut gx = vec![0.0f32; w * h];
    let mut gy = vec![0.0f32; w * h];

    if w < 3 || h < 3 {
        return StrokeGradientResult { gx, gy, width: w, height: h };
    }

    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let c = height.data[y * w + x];
            // Neighbours outside the active region (== 0.0) are replaced
            // with center value so the stroke boundary produces no gradient.
            let s = |dx: usize, dy: usize| {
                let v = height.data[dy * w + dx];
                if v > 0.0 { v } else { c }
            };

            gx[y * w + x] = -s(x - 1, y - 1) - 2.0 * s(x - 1, y) - s(x - 1, y + 1)
                + s(x + 1, y - 1) + 2.0 * s(x + 1, y) + s(x + 1, y + 1);

            gy[y * w + x] = -s(x - 1, y - 1) - 2.0 * s(x, y - 1) - s(x + 1, y - 1)
                + s(x - 1, y + 1) + 2.0 * s(x, y + 1) + s(x + 1, y + 1);
        }
    }

    // Pressure-based attenuation: √p · (1 - p²)
    // Low pressure → barely touching → weak (√p tapers to 0).
    // Medium pressure → peak bristle texture (~0.53 at p≈0.45).
    // High pressure → paint pressed flat → weak (1-p² drops to 0).
    for x in 0..w {
        let t = x as f32 / w as f32;
        let p = evaluate_pressure(pressure_curve, t);
        let atten = p.sqrt() * (1.0 - p * p);
        for y in 0..h {
            gx[y * w + x] *= atten;
            gy[y * w + x] *= atten;
        }
    }

    StrokeGradientResult { gx, gy, width: w, height: h }
}

/// Generate a stroke density map from the brush profile.
///
/// Values represent bristle coverage density (0.0–1.0), used for
/// normal map detail and opacity blending. No physical paint height.
pub fn generate_stroke_height(
    brush_profile: &[f32],
    stroke_length_px: usize,
    params: &StrokeParams,
    seed: u32,
) -> StrokeHeightResult {
    let brush_width_px = params.brush_width.round() as usize;
    let load = params.load;
    let body_wiggle = params.body_wiggle;
    let pressure_curve = &params.pressure_curve;

    let local_width = stroke_length_px;
    let local_height = brush_width_px;

    let mut data = vec![0.0f32; local_height * local_width];

    if stroke_length_px == 0 || brush_width_px == 0 {
        return StrokeHeightResult {
            data,
            width: local_width,
            height: local_height,
        };
    }

    let perlin_wiggle = Perlin::new(seed.wrapping_add(2));
    let wiggle_amplitude = body_wiggle * brush_width_px as f32;

    for x in 0..stroke_length_px {
        let t = x as f32 / stroke_length_px as f32;
        let p = evaluate_pressure(pressure_curve, t);

        // Effective width from pressure
        let active_width = brush_width_px as f32 * (MIN_WIDTH_RATIO + (1.0 - MIN_WIDTH_RATIO) * p);

        // Body wiggle: lateral shift of active region center
        let wiggle_offset = if wiggle_amplitude > 0.0 {
            perlin_wiggle.get([t as f64, 0.0]) as f32 * wiggle_amplitude
        } else {
            0.0
        };
        let center = brush_width_px as f32 * 0.5 + wiggle_offset;

        let active_start = (center - active_width * 0.5)
            .floor()
            .clamp(0.0, brush_width_px as f32) as usize;
        let active_end = ((center + active_width * 0.5)
            .ceil() as usize)
            .min(brush_width_px);
        let active_count = active_end.saturating_sub(active_start);

        // Paint depletion — load > 1.0 gradually fades out the depletion curve,
        // fully flat at load = 2.0.
        let remaining = if load > 1.0 {
            let base = lerp(1.0, DEPLETION_FLOOR, t.powf(DEPLETION_EXPONENT));
            let blend = ((load - 1.0) / 1.0).min(1.0);
            lerp(base, 1.0, blend)
        } else {
            load * lerp(1.0, DEPLETION_FLOOR, t.powf(DEPLETION_EXPONENT))
        };

        if active_count == 0 {
            continue;
        }

        // For each pixel in active range, blend two sampling strategies:
        //   Compression (high p): full profile squeezed into active width.
        //   Cutoff (low p): only the center portion of the profile is used.
        // Cutoff only below p ≈ 0.3; above that, pure compression.
        let blend = smoothstep(0.0, 0.3, p);
        let cutoff_start = (brush_width_px as f32 - active_width) * 0.5;
        let step = active_width / active_count as f32;
        for j in 0..active_count {
            let compress_idx = j as f32 * (brush_width_px as f32 / active_count as f32);
            let cutoff_idx = cutoff_start + j as f32 * step;
            let source_idx = lerp(cutoff_idx, compress_idx, blend);
            let rd = interpolate_array(brush_profile, source_idx);
            // When paint is abundant, bristle gaps fill in (uniform density).
            // As paint depletes, individual bristle tracks emerge.
            let fill = remaining.min(1.0);
            let pattern_spread = (1.0 - fill) * 5.0;
            let effective_density = p.powf(pattern_spread * (1.0 - rd) + 1.0);

            let local_y = active_start + j;
            let local_x = x;

            if local_y < local_height && local_x < local_width {
                data[local_y * local_width + local_x] = effective_density * remaining;
            }
        }
    }

    StrokeHeightResult {
        data,
        width: local_width,
        height: local_height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_profile::generate_brush_profile;
    use crate::types::{PressureCurve, PressurePreset};

    fn params(bw: f32, load: f32, wiggle: f32, preset: PressurePreset) -> StrokeParams {
        StrokeParams {
            brush_width: bw, load, body_wiggle: wiggle,
            pressure_curve: PressureCurve::Preset(preset), seed: 42,
            ..StrokeParams::default()
        }
    }

    #[test]
    fn dimensions() {
        let p = params(30.0, 0.8, 0.0, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);
        assert_eq!(result.width, 100);  // stroke_length
        assert_eq!(result.height, 30);  // brush_width
        assert_eq!(result.data.len(), 100 * 30);
    }

    #[test]
    fn full_load_uniform_max_density() {
        let p = params(30.0, 1.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let mut max_val = 0.0f32;
        for &v in &result.data {
            max_val = max_val.max(v);
        }

        // Full load + uniform pressure → max density ~1.0
        assert!(
            (max_val - 1.0).abs() < 0.05,
            "max density = {}, expected ~1.0",
            max_val
        );

        // Bristle pattern visible (variance > 0)
        let body_vals: Vec<f32> = result.data.iter().copied().filter(|&v| v > 0.0).collect();
        let mean = body_vals.iter().sum::<f32>() / body_vals.len() as f32;
        let variance =
            body_vals.iter().map(|&v| (v - mean) * (v - mean)).sum::<f32>() / body_vals.len() as f32;
        assert!(variance > 0.0, "no bristle pattern variation");
    }

    #[test]
    fn dry_brush() {
        let p = params(30.0, 0.3, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let max_val = result.data.iter().cloned().fold(0.0f32, f32::max);

        // max density ~ load = 0.3
        assert!(
            max_val < 0.35,
            "dry brush max density = {}, expected < 0.35",
            max_val
        );
    }

    #[test]
    fn depletion() {
        let p = params(30.0, 1.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 200, &p, 42);

        // Average density at start (x=0..10) vs end (x=190..200)
        let avg_start: f32 = (0..30)
            .flat_map(|y| (0..10).map(move |x| (y, x)))
            .map(|(y, x)| result.data[y * result.width + x])
            .sum::<f32>()
            / (30.0 * 10.0);

        let avg_end: f32 = (0..30)
            .flat_map(|y| (190..200).map(move |x| (y, x)))
            .map(|(y, x)| result.data[y * result.width + x])
            .sum::<f32>()
            / (30.0 * 10.0);

        let ratio = avg_end / (avg_start + 1e-10);
        assert!(
            ratio < 0.30,
            "depletion ratio = {:.2}, expected < 0.30",
            ratio
        );
    }

    #[test]
    fn pressure_narrowing() {
        let p = params(30.0, 1.0, 0.0, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        // Count active pixels at x=0 vs x=90
        let active_count = |x: usize| -> usize {
            (0..30)
                .filter(|&y| result.data[y * result.width + x] > 0.001)
                .count()
        };

        let width_at_0 = active_count(0);
        let width_at_90 = active_count(90);

        assert!(
            width_at_90 < width_at_0,
            "no narrowing: width_at_0={}, width_at_90={}",
            width_at_0,
            width_at_90
        );
        let ratio = width_at_90 as f32 / 30.0;
        assert!(
            ratio < 0.55,
            "width ratio at t=0.9: {:.2}, expected < 0.55",
            ratio
        );
    }

    #[test]
    fn determinism() {
        let p = params(30.0, 0.8, 0.15, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let a = generate_stroke_height(&profile, 100, &p, 42);
        let b = generate_stroke_height(&profile, 100, &p, 42);
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn visual_stroke_height() {
        let p = params(30.0, 0.8, 0.15, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 150, &p, 42);

        let max_val = result.data.iter().cloned().fold(0.0f32, f32::max).max(1e-10);
        let pixels: Vec<u8> = result
            .data
            .iter()
            .map(|&v| ((v / max_val).clamp(0.0, 1.0) * 255.0) as u8)
            .collect();

        let out_dir = crate::test_module_output_dir("stroke_height");
        image::save_buffer(
            out_dir.join("stroke_height.png"),
            &pixels,
            result.width as u32,
            result.height as u32,
            image::ColorType::L8,
        )
        .expect("Failed to save stroke_height.png");
    }

    #[test]
    fn body_wiggle_off_centered() {
        let p = params(30.0, 1.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);
        for x in 0..100 {
            let mut sum_y = 0.0f32;
            let mut sum_w = 0.0f32;
            for y in 0..30 {
                let v = result.data[y * result.width + x];
                sum_y += y as f32 * v;
                sum_w += v;
            }
            if sum_w > 0.0 {
                let center = sum_y / sum_w;
                assert!(
                    (center - 15.0).abs() < 2.0,
                    "column {} center={:.1}, expected ~15.0",
                    x, center
                );
            }
        }
    }

    #[test]
    fn body_wiggle_on_deviates() {
        let p = params(30.0, 1.0, 0.3, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 200, &p, 42);
        let mut centers = Vec::new();
        for x in 0..200 {
            let mut sum_y = 0.0f32;
            let mut sum_w = 0.0f32;
            for y in 0..result.height {
                let v = result.data[y * result.width + x];
                sum_y += y as f32 * v;
                sum_w += v;
            }
            if sum_w > 0.0 {
                centers.push(sum_y / sum_w);
            }
        }
        let min_c = centers.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_c = centers.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_c - min_c > 1.0,
            "wiggle had no effect: center range = {:.2}",
            max_c - min_c
        );
    }

    // ── Gradient Tests ──

    #[test]
    fn gradients_flat() {
        let p = params(30.0, 1.0, 0.0, PressurePreset::Uniform);
        let profile = vec![1.0f32; 30]; // uniform bristle density
        let height = generate_stroke_height(&profile, 100, &p, 42);
        let grad = compute_stroke_gradients(&height, &p.pressure_curve);

        // Uniform pressure (p=1.0) → atten=0, so all gradients should be zero
        for y in 2..(height.height - 2) {
            for x in 10..90 {
                let i = y * grad.width + x;
                assert!(
                    grad.gx[i].abs() < 0.1 && grad.gy[i].abs() < 0.1,
                    "at ({x},{y}): gx={:.3}, gy={:.3}",
                    grad.gx[i], grad.gy[i]
                );
            }
        }
    }

    #[test]
    fn gradients_slope_x() {
        // Synthetic height: linear ramp in X direction
        let w = 50;
        let h = 10;
        let mut data = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = x as f32 / w as f32;
            }
        }
        let height = StrokeHeightResult { data, width: w, height: h };
        // FadeIn: p=√t, so at x=25 (t=0.5) p≈0.707, atten≈0.5 → gradients halved but still positive
        let curve = PressureCurve::Preset(PressurePreset::FadeIn);
        let grad = compute_stroke_gradients(&height, &curve);

        // Interior pixels should have positive gx, near-zero gy
        let mid = 5 * w + 25;
        assert!(grad.gx[mid] > 0.0, "gx should be positive for rightward slope");
        assert!(grad.gy[mid].abs() < 0.01, "gy should be ~0 for X-only slope");
    }

    #[test]
    fn gradients_slope_y() {
        // Synthetic height: linear ramp in Y direction
        let w = 50;
        let h = 20;
        let mut data = vec![0.0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = y as f32 / h as f32;
            }
        }
        let height = StrokeHeightResult { data, width: w, height: h };
        let curve = PressureCurve::Preset(PressurePreset::FadeIn);
        let grad = compute_stroke_gradients(&height, &curve);

        let mid = 10 * w + 25;
        assert!(grad.gy[mid] > 0.0, "gy should be positive for downward slope");
        assert!(grad.gx[mid].abs() < 0.01, "gx should be ~0 for Y-only slope");
    }
}
