use crate::math::{interpolate_array, lerp};
use crate::pressure::evaluate_pressure;
use crate::types::StrokeParams;
use noise::{NoiseFn, Perlin};

// ── Constants ──

const MIN_WIDTH_RATIO: f32 = 0.3;
const DEPLETION_FLOOR: f32 = 0.15;
const DEPLETION_EXPONENT: f32 = 0.7;
const RIDGE_NOISE_FREQ: f32 = 1.0;

/// Result of stroke height generation.
pub struct StrokeHeightResult {
    /// Height map in local coordinates.
    /// Dimensions: height = brush_width_px + ridge_width_px * 2,
    ///             width  = stroke_length_px + ridge_width_px
    /// Stored row-major: data[y * width + x]
    pub data: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub margin: usize,
}

/// Ridge profile: smooth falloff from 1.0 at d=0 to 0.0 at d=w (inverse smoothstep).
fn ridge_profile(d: f32, w: f32) -> f32 {
    let t = (d / w).clamp(0.0, 1.0);
    (1.0 - t) * (1.0 - t) * (1.0 + 2.0 * t)
}

/// Generate a complete stroke height map (body + ridges).
pub fn generate_stroke_height(
    brush_profile: &[f32],
    stroke_length_px: usize,
    params: &StrokeParams,
    seed: u32,
) -> StrokeHeightResult {
    let brush_width_px = params.brush_width.round() as usize;
    let ridge_width_px = params.ridge_width.round() as usize;
    let load = params.load;
    let base_height = params.base_height;
    let ridge_height = params.ridge_height;
    let ridge_variation = params.ridge_variation;
    let body_wiggle = params.body_wiggle;
    let pressure_preset = params.pressure_preset;

    let margin = ridge_width_px;
    let local_width = stroke_length_px + margin;
    let local_height = brush_width_px + margin * 2;

    let mut data = vec![0.0f32; local_height * local_width];

    if stroke_length_px == 0 || brush_width_px == 0 {
        return StrokeHeightResult {
            data,
            width: local_width,
            height: local_height,
            margin,
        };
    }

    // Noise generators for ridge variation and body wiggle
    let perlin_h = Perlin::new(seed);
    let perlin_w = Perlin::new(seed.wrapping_add(1));
    let perlin_wiggle = Perlin::new(seed.wrapping_add(2));
    let wiggle_amplitude = body_wiggle * brush_width_px as f32;

    // Per-column data for ridges
    let mut col_active_start = vec![0usize; stroke_length_px];
    let mut col_active_end = vec![0usize; stroke_length_px];
    let mut col_remaining = vec![0.0f32; stroke_length_px];

    // ── Step 1: Body Height ──
    for x in 0..stroke_length_px {
        let t = x as f32 / stroke_length_px as f32;
        let p = evaluate_pressure(pressure_preset, t);

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

        col_active_start[x] = active_start;
        col_active_end[x] = active_end;

        // Paint depletion
        let remaining = load * lerp(1.0, DEPLETION_FLOOR, t.powf(DEPLETION_EXPONENT));
        col_remaining[x] = remaining;

        if active_count == 0 {
            continue;
        }

        // For each pixel in active range
        // Narrow (compress) full brush profile into active width,
        // so all bristle patterns squeeze together at lower pressure
        // rather than cropping the outer bristles.
        for j in 0..active_count {
            let source_idx = j as f32 * (brush_width_px as f32 / active_count as f32);
            let rd = interpolate_array(brush_profile, source_idx);
            let effective_density = p.powf(5.0 * (1.0 - rd) + 1.0);

            let local_y = margin + active_start + j;
            let local_x = margin + x;

            if local_y < local_height && local_x < local_width {
                data[local_y * local_width + local_x] = effective_density * remaining * base_height;
            }
        }
    }

    // ── Step 2: Ridge Generation ──
    let stroke_len_f = stroke_length_px as f32;

    // Helper closures for noise-based ridge variation
    let effective_ridge_height = |x: usize| -> f32 {
        let nx = x as f64 / stroke_len_f as f64 * RIDGE_NOISE_FREQ as f64;
        let noise_h = perlin_h.get([nx, 0.0]) as f32;
        ridge_height * (1.0 + noise_h * ridge_variation)
    };

    let effective_ridge_width = |x: usize| -> f32 {
        let nx = x as f64 / stroke_len_f as f64 * RIDGE_NOISE_FREQ as f64;
        let noise_w = perlin_w.get([nx, 1000.0]) as f32;
        ridge_width_px as f32 * (1.0 + noise_w * ridge_variation * 0.5)
    };

    // a) Side ridges
    for x in 0..stroke_length_px {
        let rh = effective_ridge_height(x) * col_remaining[x];
        let rw = effective_ridge_width(x);
        let active_start = col_active_start[x];
        let active_end = col_active_end[x];

        let rw_ceil = rw.ceil() as usize;

        // Top ridge: at and above active_start (d=0 overlaps with body boundary)
        for d in 0..=rw_ceil {
            let py_signed = (margin + active_start) as isize - d as isize;
            if py_signed >= 0 {
                let py = py_signed as usize;
                let lx = margin + x;
                if py < local_height && lx < local_width {
                    data[py * local_width + lx] += rh * ridge_profile(d as f32, rw);
                }
            }
        }

        // Bottom ridge: at last body pixel and below
        // active_end - 1 is the last body pixel; d=0 overlaps there
        if active_end > 0 {
            for d in 0..=rw_ceil {
                let py = margin + active_end - 1 + d;
                let lx = margin + x;
                if py < local_height && lx < local_width {
                    data[py * local_width + lx] += rh * ridge_profile(d as f32, rw);
                }
            }
        }
    }

    // b) Front ridge — before stroke start
    let rh_front = effective_ridge_height(0) * load;
    let rw_front = effective_ridge_width(0);
    let rw_front_ceil = rw_front.ceil() as usize;

    let active_start_0 = col_active_start.first().copied().unwrap_or(0);
    let active_end_0 = col_active_end.first().copied().unwrap_or(0);

    for d in 1..=rw_front_ceil {
        let lx_signed = margin as isize - d as isize;
        if lx_signed >= 0 {
            let lx = lx_signed as usize;
            for j in active_start_0..active_end_0 {
                let py = margin + j;
                if py < local_height && lx < local_width {
                    data[py * local_width + lx] += rh_front * ridge_profile(d as f32, rw_front);
                }
            }
        }
    }

    StrokeHeightResult {
        data,
        width: local_width,
        height: local_height,
        margin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush_profile::generate_brush_profile;
    use crate::types::PressurePreset;

    #[allow(clippy::too_many_arguments)]
    fn params(bw: f32, load: f32, base_h: f32, ridge_h: f32, rw: f32, rv: f32, wiggle: f32, preset: PressurePreset) -> StrokeParams {
        StrokeParams {
            brush_width: bw, load, base_height: base_h, ridge_height: ridge_h,
            ridge_width: rw, ridge_variation: rv, body_wiggle: wiggle,
            pressure_preset: preset, seed: 42,
            ..StrokeParams::default()
        }
    }

    #[test]
    fn dimensions() {
        let p = params(30.0, 0.8, 0.5, 0.3, 5.0, 0.1, 0.0, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);
        assert_eq!(result.width, 105); // stroke_length + ridge_width
        assert_eq!(result.height, 40); // brush_width + ridge_width * 2
        assert_eq!(result.margin, 5);
        assert_eq!(result.data.len(), 105 * 40);
    }

    #[test]
    fn full_load_uniform_max_body() {
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        // Find max in body zone
        let margin = result.margin;
        let mut max_body = 0.0f32;
        for y in margin..(margin + 30) {
            for x in margin..(margin + 100) {
                let v = result.data[y * result.width + x];
                max_body = max_body.max(v);
            }
        }

        assert!(
            (max_body - 0.5).abs() < 0.05,
            "max body = {}, expected ~0.5",
            max_body
        );

        // Bristle pattern visible (variance > 0)
        let body_vals: Vec<f32> = (margin..(margin + 30))
            .flat_map(|y| (margin..(margin + 100)).map(move |x| (y, x)))
            .map(|(y, x)| result.data[y * result.width + x])
            .filter(|&v| v > 0.0)
            .collect();
        let mean = body_vals.iter().sum::<f32>() / body_vals.len() as f32;
        let variance =
            body_vals.iter().map(|&v| (v - mean) * (v - mean)).sum::<f32>() / body_vals.len() as f32;
        assert!(variance > 0.0, "no bristle pattern variation");
    }

    #[test]
    fn ridge_peaks() {
        let p = params(30.0, 1.0, 0.5, 0.5, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let max_pixel = result.data.iter().cloned().fold(0.0f32, f32::max);
        // Max pixel in ridge zone should be approximately base_height + ridge_height
        assert!(
            (max_pixel - 1.0).abs() < 0.15,
            "max pixel = {}, expected ~1.0 (base_height + ridge_height)",
            max_pixel
        );
    }

    #[test]
    fn no_ridges_no_outside_paint() {
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let margin = result.margin;
        // Check pixels outside body zone are 0
        for y in 0..margin {
            for x in 0..result.width {
                assert_eq!(
                    result.data[y * result.width + x], 0.0,
                    "non-zero outside body at ({}, {})",
                    x, y
                );
            }
        }
        for y in (margin + 30)..result.height {
            for x in 0..result.width {
                assert_eq!(
                    result.data[y * result.width + x], 0.0,
                    "non-zero outside body at ({}, {})",
                    x, y
                );
            }
        }
    }

    #[test]
    fn dry_brush() {
        let p = params(30.0, 0.3, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let margin = result.margin;
        let mut max_body = 0.0f32;
        for y in margin..(margin + 30) {
            for x in margin..(margin + 100) {
                max_body = max_body.max(result.data[y * result.width + x]);
            }
        }

        // max body ~ load * base_height = 0.3 * 0.5 = 0.15
        assert!(
            max_body < 0.20,
            "dry brush max body = {}, expected < 0.20",
            max_body
        );
    }

    #[test]
    fn depletion() {
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 200, &p, 42);

        let margin = result.margin;
        // Average height at start (x=0..10) vs end (x=190..200)
        let avg_start: f32 = (margin..(margin + 30))
            .flat_map(|y| (margin..(margin + 10)).map(move |x| (y, x)))
            .map(|(y, x)| result.data[y * result.width + x])
            .sum::<f32>()
            / (30.0 * 10.0);

        let avg_end: f32 = (margin..(margin + 30))
            .flat_map(|y| ((margin + 190)..(margin + 200)).map(move |x| (y, x)))
            .map(|(y, x)| result.data[y * result.width + x])
            .sum::<f32>()
            / (30.0 * 10.0);

        let ratio = avg_end / (avg_start + 1e-10);
        // End should be ~15% of start (±10%)
        assert!(
            ratio < 0.30,
            "depletion ratio = {:.2}, expected < 0.30",
            ratio
        );
    }

    #[test]
    fn pressure_narrowing() {
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);

        let margin = result.margin;

        // Count active pixels at x=0 (t=0, full pressure)
        let active_start = |x: usize| -> usize {
            let lx = margin + x;
            let mut count = 0;
            for y in margin..(margin + 30) {
                if result.data[y * result.width + lx] > 0.001 {
                    count += 1;
                }
            }
            count
        };

        let width_at_0 = active_start(0);
        let width_at_90 = active_start(90); // t=0.9, FadeOut pressure = 1 - 0.81 = 0.19

        assert!(
            width_at_90 < width_at_0,
            "no narrowing: width_at_0={}, width_at_90={}",
            width_at_0,
            width_at_90
        );
        // At t=0.9 with FadeOut: active_width ≈ 30 * (0.3 + 0.7*0.19) ≈ 30 * 0.433 ≈ 13
        // So width_at_90 should be roughly 30-40% of brush_width
        let ratio = width_at_90 as f32 / 30.0;
        assert!(
            ratio < 0.55,
            "width ratio at t=0.9: {:.2}, expected < 0.55",
            ratio
        );
    }

    #[test]
    fn determinism() {
        let p = params(30.0, 0.8, 0.5, 0.3, 5.0, 0.1, 0.15, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let a = generate_stroke_height(&profile, 100, &p, 42);
        let b = generate_stroke_height(&profile, 100, &p, 42);
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn visual_stroke_height() {
        let p = params(30.0, 0.8, 0.5, 0.3, 5.0, 0.1, 0.15, PressurePreset::FadeOut);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 150, &p, 42);

        // Save as grayscale PNG
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
        // With wiggle=0, body center should be constant at brush_width/2
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.0, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);
        let margin = result.margin;
        // Find center of mass for each column — should all be at ~15
        for x in 0..100 {
            let lx = margin + x;
            let mut sum_y = 0.0f32;
            let mut sum_w = 0.0f32;
            for y in margin..(margin + 30) {
                let v = result.data[y * result.width + lx];
                sum_y += (y - margin) as f32 * v;
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
        let p = params(30.0, 1.0, 0.5, 0.0, 5.0, 0.0, 0.3, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 200, &p, 42);
        let margin = result.margin;
        // Collect center of mass per column
        let mut centers = Vec::new();
        for x in 0..200 {
            let lx = margin + x;
            let mut sum_y = 0.0f32;
            let mut sum_w = 0.0f32;
            for y in 0..result.height {
                let v = result.data[y * result.width + lx];
                sum_y += y as f32 * v;
                sum_w += v;
            }
            if sum_w > 0.0 {
                centers.push(sum_y / sum_w);
            }
        }
        // Centers should NOT all be equal (wiggle causes variation)
        let min_c = centers.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_c = centers.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(
            max_c - min_c > 1.0,
            "wiggle had no effect: center range = {:.2}",
            max_c - min_c
        );
    }

    #[test]
    fn body_wiggle_stays_in_bounds() {
        // Max wiggle should not cause OOB writes
        let p = params(30.0, 1.0, 0.5, 0.3, 5.0, 0.0, 0.5, PressurePreset::Uniform);
        let profile = generate_brush_profile(30, 42);
        let result = generate_stroke_height(&profile, 100, &p, 42);
        // If we got here without panic, bounds are respected
        assert_eq!(result.data.len(), result.width * result.height);
    }
}
