//! Stroke colorization — samples base color textures, applies HSV variation, and
//! handles bilinear interpolation for per-stroke color assignment.

use crate::rng::SeededRng;
use crate::types::{Color, HsvColor, StrokePath};
use glam::Vec2;

// ── Color Texture Reference ──

/// Lightweight reference to a color texture with its dimensions.
pub struct ColorTextureRef<'a> {
    pub data: &'a [Color],
    pub width: u32,
    pub height: u32,
}

// ── Color Difference ──

/// Maximum per-channel absolute difference between two colors (RGB only, ignores alpha).
pub fn channel_max_diff(a: Color, b: Color) -> f32 {
    (a.r - b.r)
        .abs()
        .max((a.g - b.g).abs())
        .max((a.b - b.b).abs())
}

// ── RGB ↔ HSV Conversion ──

/// Convert an RGB color to HSV. All channels in [0, 1].
pub fn rgb_to_hsv(c: Color) -> HsvColor {
    let max = c.r.max(c.g).max(c.b);
    let min = c.r.min(c.g).min(c.b);
    let delta = max - min;

    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };

    let h = if delta == 0.0 {
        0.0
    } else if max == c.r {
        ((c.g - c.b) / delta) % 6.0
    } else if max == c.g {
        (c.b - c.r) / delta + 2.0
    } else {
        (c.r - c.g) / delta + 4.0
    };

    let mut h = h / 6.0;
    if h < 0.0 {
        h += 1.0;
    }

    HsvColor { h, s, v }
}

/// Convert an HSV color to RGB. All channels in [0, 1].
pub fn hsv_to_rgb(hsv: HsvColor) -> Color {
    let HsvColor { h, s, v } = hsv;

    let c = v * s;
    let h6 = h * 6.0;
    let x = c * (1.0 - (h6 % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = if h6 < 1.0 {
        (c, x, 0.0)
    } else if h6 < 2.0 {
        (x, c, 0.0)
    } else if h6 < 3.0 {
        (0.0, c, x)
    } else if h6 < 4.0 {
        (0.0, x, c)
    } else if h6 < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    Color::rgb(r + m, g + m, b + m)
}

// ── Bilinear Texture Sampling ──

/// Sample a color texture at a UV coordinate using bilinear interpolation.
///
/// - `texture`: RGBA pixel data, row-major, dimensions tex_width x tex_height
/// - `uv`: UV coordinate (0–1)
///
/// Returns the interpolated color.
pub fn sample_bilinear(texture: &[Color], tex_width: u32, tex_height: u32, uv: Vec2) -> Color {
    crate::math::sample_bilinear_color(texture, tex_width, tex_height, uv)
}

// ── Per-Stroke Color with Variation ──

/// Determine the color for a stroke.
///
/// Samples the base color map at the stroke midpoint, then applies per-stroke
/// HSV variation for a natural hand-painted look.
pub fn compute_stroke_color(
    path: &StrokePath,
    color_texture: Option<&[Color]>,
    tex_width: u32,
    tex_height: u32,
    solid_color: Color,
    color_variation: f32,
    rng: &mut SeededRng,
) -> Color {
    let midpoint = path.midpoint();

    let base_color = if let Some(tex) = color_texture {
        sample_bilinear(tex, tex_width, tex_height, midpoint)
    } else {
        solid_color
    };

    // Apply HSV variation
    let mut hsv = rgb_to_hsv(base_color);

    hsv.h += (rng.next_f32() - 0.5) * color_variation * 0.5;
    hsv.s += (rng.next_f32() - 0.5) * color_variation;
    hsv.v += (rng.next_f32() - 0.5) * color_variation * 0.7;

    // Wrap hue, clamp saturation and value
    if hsv.h < 0.0 {
        hsv.h += 1.0;
    }
    if hsv.h >= 1.0 {
        hsv.h -= 1.0;
    }
    hsv.s = hsv.s.clamp(0.0, 1.0);
    hsv.v = hsv.v.clamp(0.0, 1.0);

    hsv_to_rgb(hsv)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < EPS
    }

    fn make_path(pts: Vec<Vec2>) -> StrokePath {
        StrokePath::new(pts, 0, 0)
    }

    // ── RGB ↔ HSV Tests ──

    #[test]
    fn rgb_to_hsv_red() {
        let hsv = rgb_to_hsv(Color::rgb(1.0, 0.0, 0.0));
        assert!(approx_eq(hsv.h, 0.0));
        assert!(approx_eq(hsv.s, 1.0));
        assert!(approx_eq(hsv.v, 1.0));
    }

    #[test]
    fn rgb_to_hsv_green() {
        let hsv = rgb_to_hsv(Color::rgb(0.0, 1.0, 0.0));
        assert!(approx_eq(hsv.h, 1.0 / 3.0));
        assert!(approx_eq(hsv.s, 1.0));
        assert!(approx_eq(hsv.v, 1.0));
    }

    #[test]
    fn rgb_to_hsv_blue() {
        let hsv = rgb_to_hsv(Color::rgb(0.0, 0.0, 1.0));
        assert!(approx_eq(hsv.h, 2.0 / 3.0));
        assert!(approx_eq(hsv.s, 1.0));
        assert!(approx_eq(hsv.v, 1.0));
    }

    #[test]
    fn rgb_to_hsv_white() {
        let hsv = rgb_to_hsv(Color::rgb(1.0, 1.0, 1.0));
        assert!(approx_eq(hsv.h, 0.0));
        assert!(approx_eq(hsv.s, 0.0));
        assert!(approx_eq(hsv.v, 1.0));
    }

    #[test]
    fn rgb_to_hsv_black() {
        let hsv = rgb_to_hsv(Color::rgb(0.0, 0.0, 0.0));
        assert!(approx_eq(hsv.h, 0.0));
        assert!(approx_eq(hsv.s, 0.0));
        assert!(approx_eq(hsv.v, 0.0));
    }

    #[test]
    fn rgb_to_hsv_gray() {
        let hsv = rgb_to_hsv(Color::rgb(0.5, 0.5, 0.5));
        assert!(approx_eq(hsv.h, 0.0));
        assert!(approx_eq(hsv.s, 0.0));
        assert!(approx_eq(hsv.v, 0.5));
    }

    #[test]
    fn hsv_rgb_round_trip() {
        let colors = [
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(0.5, 0.3, 0.8),
            Color::rgb(0.1, 0.9, 0.4),
            Color::rgb(0.0, 0.0, 0.0),
            Color::rgb(1.0, 1.0, 1.0),
        ];
        for c in &colors {
            let result = hsv_to_rgb(rgb_to_hsv(*c));
            assert!(approx_eq(result.r, c.r), "r: {} vs {}", result.r, c.r);
            assert!(approx_eq(result.g, c.g), "g: {} vs {}", result.g, c.g);
            assert!(approx_eq(result.b, c.b), "b: {} vs {}", result.b, c.b);
        }
    }

    // ── Bilinear Sampling Tests ──

    #[test]
    fn bilinear_center_of_2x2() {
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0), // (0,0) red
            Color::rgb(0.0, 1.0, 0.0), // (1,0) green
            Color::rgb(0.0, 0.0, 1.0), // (0,1) blue
            Color::rgb(1.0, 1.0, 0.0), // (1,1) yellow
        ];
        let c = sample_bilinear(&tex, 2, 2, Vec2::new(0.5, 0.5));
        let avg_r = (1.0 + 0.0 + 0.0 + 1.0) / 4.0;
        let avg_g = (0.0 + 1.0 + 0.0 + 1.0) / 4.0;
        let avg_b = (0.0 + 0.0 + 1.0 + 0.0) / 4.0;
        assert!(approx_eq(c.r, avg_r));
        assert!(approx_eq(c.g, avg_g));
        assert!(approx_eq(c.b, avg_b));
    }

    #[test]
    fn bilinear_corner_of_2x2() {
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(1.0, 1.0, 0.0),
        ];
        let c = sample_bilinear(&tex, 2, 2, Vec2::new(0.0, 0.0));
        assert!(approx_eq(c.r, 1.0));
        assert!(approx_eq(c.g, 0.0));
        assert!(approx_eq(c.b, 0.0));
    }

    #[test]
    fn bilinear_uniform_texture() {
        let color = Color::rgb(0.3, 0.6, 0.9);
        let tex = vec![color; 4];
        let c = sample_bilinear(&tex, 2, 2, Vec2::new(0.7, 0.3));
        assert!(approx_eq(c.r, 0.3));
        assert!(approx_eq(c.g, 0.6));
        assert!(approx_eq(c.b, 0.9));
    }

    #[test]
    fn bilinear_edge_clamp() {
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(1.0, 1.0, 0.0),
        ];
        let c = sample_bilinear(&tex, 2, 2, Vec2::new(-0.1, 0.5));
        assert!(c.r >= 0.0 && c.r <= 1.0);
    }

    // ── Color Variation Tests ──

    #[test]
    fn no_variation_preserves_color() {
        let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
        let solid = Color::rgb(0.5, 0.3, 0.8);
        let mut rng = SeededRng::new(42);
        let result = compute_stroke_color(&path, None, 0, 0, solid, 0.0, &mut rng);
        assert!(approx_eq(result.r, solid.r));
        assert!(approx_eq(result.g, solid.g));
        assert!(approx_eq(result.b, solid.b));
    }

    #[test]
    fn variation_bounded() {
        let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
        let solid = Color::rgb(0.5, 0.5, 0.5);
        let base_hsv = rgb_to_hsv(solid);

        for seed in 0..100 {
            let mut rng = SeededRng::new(seed);
            let result = compute_stroke_color(&path, None, 0, 0, solid, 0.3, &mut rng);
            let result_hsv = rgb_to_hsv(result);

            let h_diff = (result_hsv.h - base_hsv.h).abs();
            let h_diff = h_diff.min(1.0 - h_diff);
            assert!(
                h_diff <= 0.15 + EPS,
                "H shift {} too large (seed {})",
                h_diff,
                seed
            );

            let s_diff = (result_hsv.s - base_hsv.s).abs();
            assert!(
                s_diff <= 0.3 + EPS,
                "S shift {} too large (seed {})",
                s_diff,
                seed
            );

            let v_diff = (result_hsv.v - base_hsv.v).abs();
            assert!(
                v_diff <= 0.21 + EPS,
                "V shift {} too large (seed {})",
                v_diff,
                seed
            );
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let path = make_path(vec![Vec2::new(0.2, 0.3), Vec2::new(0.8, 0.7)]);
        let solid = Color::rgb(0.4, 0.6, 0.2);

        let mut rng1 = SeededRng::new(99);
        let c1 = compute_stroke_color(&path, None, 0, 0, solid, 0.2, &mut rng1);

        let mut rng2 = SeededRng::new(99);
        let c2 = compute_stroke_color(&path, None, 0, 0, solid, 0.2, &mut rng2);

        assert!(approx_eq(c1.r, c2.r));
        assert!(approx_eq(c1.g, c2.g));
        assert!(approx_eq(c1.b, c2.b));
    }

    #[test]
    fn no_texture_uses_solid() {
        let path = make_path(vec![Vec2::new(0.5, 0.5), Vec2::new(0.6, 0.5)]);
        let solid = Color::rgb(1.0, 0.0, 0.0);
        let mut rng = SeededRng::new(42);
        let result = compute_stroke_color(&path, None, 0, 0, solid, 0.0, &mut rng);
        assert!(approx_eq(result.r, 1.0));
        assert!(approx_eq(result.g, 0.0));
        assert!(approx_eq(result.b, 0.0));
    }

    #[test]
    fn with_texture_samples_midpoint() {
        // 2x2 texture: left column=red, right column=green
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
        ];
        // Path midpoint at UV ~(0.75, 0.5) — right side (green)
        let path = make_path(vec![Vec2::new(0.7, 0.5), Vec2::new(0.8, 0.5)]);
        let mut rng = SeededRng::new(0);
        let result = compute_stroke_color(&path, Some(&tex), 2, 2, Color::WHITE, 0.0, &mut rng);
        assert!(result.g > result.r);
    }

    // ── HSV Edge-Case Stress Tests ──

    #[test]
    fn hsv_round_trip_exhaustive() {
        // Sweep R/G/B in steps — find the worst-case round-trip error
        let steps = 21;
        let mut max_err: f32 = 0.0;
        let mut worst = Color::BLACK;
        for ri in 0..steps {
            for gi in 0..steps {
                for bi in 0..steps {
                    let c = Color::rgb(
                        ri as f32 / (steps - 1) as f32,
                        gi as f32 / (steps - 1) as f32,
                        bi as f32 / (steps - 1) as f32,
                    );
                    let rt = hsv_to_rgb(rgb_to_hsv(c));
                    let err = (rt.r - c.r)
                        .abs()
                        .max((rt.g - c.g).abs())
                        .max((rt.b - c.b).abs());
                    if err > max_err {
                        max_err = err;
                        worst = c;
                    }
                }
            }
        }
        eprintln!(
            "HSV round-trip worst error: {:.8} at RGB({:.3}, {:.3}, {:.3})",
            max_err, worst.r, worst.g, worst.b
        );
        assert!(
            max_err < 1e-5,
            "HSV round-trip error too large: {} at ({}, {}, {})",
            max_err,
            worst.r,
            worst.g,
            worst.b
        );
    }

    #[test]
    fn hsv_near_hue_boundaries() {
        // Test colors near hue=0/1 boundary (red/magenta range)
        let boundary_colors = [
            Color::rgb(1.0, 0.0, 0.01), // just past red toward yellow... actually magenta
            Color::rgb(1.0, 0.01, 0.0), // just past red toward yellow
            Color::rgb(0.99, 0.0, 1.0), // near magenta
            Color::rgb(1.0, 0.0, 0.99), // near magenta
        ];
        for c in &boundary_colors {
            let hsv = rgb_to_hsv(*c);
            let rt = hsv_to_rgb(hsv);
            let err = (rt.r - c.r)
                .abs()
                .max((rt.g - c.g).abs())
                .max((rt.b - c.b).abs());
            assert!(
                err < 1e-5,
                "Hue boundary error: {:.8} for RGB({:.3}, {:.3}, {:.3}), HSV(h={:.5}, s={:.5}, v={:.5})",
                err, c.r, c.g, c.b, hsv.h, hsv.s, hsv.v
            );
        }
    }

    #[test]
    fn hsv_low_saturation_stability() {
        // Near-gray colors: hue is degenerate, but round-trip should still work
        for i in 0..100 {
            let v = i as f32 / 99.0;
            let c = Color::rgb(v, v + 0.001, v); // tiny green tint
            let rt = hsv_to_rgb(rgb_to_hsv(c));
            let err = (rt.r - c.r)
                .abs()
                .max((rt.g - c.g).abs())
                .max((rt.b - c.b).abs());
            assert!(err < 1e-4, "Low-sat error: {:.8} at v={:.3}", err, v);
        }
    }

    // ── Bilinear Sampling Visual Verification ──

    #[test]
    fn visual_bilinear_upsample() {
        // INSPECT: A 4x4 color texture upsampled to 256x256 via bilinear.
        // Should show smooth gradients between distinct color patches.
        // No blocky artifacts, no banding at texel boundaries.
        let tex = vec![
            // Row 0
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(1.0, 0.5, 0.0),
            Color::rgb(1.0, 1.0, 0.0),
            Color::rgb(0.5, 1.0, 0.0),
            // Row 1
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 1.0, 0.5),
            Color::rgb(0.0, 1.0, 1.0),
            Color::rgb(0.0, 0.5, 1.0),
            // Row 2
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(0.5, 0.0, 1.0),
            Color::rgb(1.0, 0.0, 1.0),
            Color::rgb(1.0, 0.0, 0.5),
            // Row 3
            Color::rgb(0.2, 0.2, 0.2),
            Color::rgb(0.4, 0.4, 0.4),
            Color::rgb(0.6, 0.6, 0.6),
            Color::rgb(1.0, 1.0, 1.0),
        ];

        let out_size = 256usize;
        let mut pixels = vec![0u8; out_size * out_size * 3];
        for py in 0..out_size {
            for px in 0..out_size {
                let uv = Vec2::new(
                    (px as f32 + 0.5) / out_size as f32,
                    (py as f32 + 0.5) / out_size as f32,
                );
                let c = sample_bilinear(&tex, 4, 4, uv);
                let idx = (py * out_size + px) * 3;
                pixels[idx] = (c.r.clamp(0.0, 1.0) * 255.0) as u8;
                pixels[idx + 1] = (c.g.clamp(0.0, 1.0) * 255.0) as u8;
                pixels[idx + 2] = (c.b.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }

        let out_dir = crate::test_module_output_dir("stroke_color");
        let out_path = out_dir.join("bilinear_upsample.png");
        image::save_buffer(
            &out_path,
            &pixels,
            out_size as u32,
            out_size as u32,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save bilinear_upsample.png");
        eprintln!("Wrote: {}", out_path.display());
    }

    // ── Color Variation Distribution Visual ──

    #[test]
    fn visual_color_variation_swatches() {
        // INSPECT: 10 rows x 10 cols grid of color swatches.
        // Each swatch is the same base color with variation applied.
        // Should look like natural hand-mixed paint — subtle hue/sat/val shifts,
        // no jarring outliers, coherent palette.
        let base_colors = [
            ("red", Color::rgb(0.8, 0.1, 0.1)),
            ("green", Color::rgb(0.1, 0.6, 0.2)),
            ("blue", Color::rgb(0.1, 0.2, 0.8)),
            ("skin", Color::rgb(0.9, 0.7, 0.55)),
            ("gray", Color::rgb(0.5, 0.5, 0.5)),
        ];
        let variation = 0.15;
        let cols = 20;
        let rows = 5; // one row per base color
        let swatch = 32usize;
        let img_w = cols * swatch;
        let img_h = rows * swatch;
        let mut pixels = vec![0u8; img_w * img_h * 3];

        for (row, (_name, base)) in base_colors.iter().enumerate() {
            let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
            for col in 0..cols {
                let mut rng = SeededRng::new(col as u32 * 31 + row as u32 * 997);
                let c = compute_stroke_color(&path, None, 0, 0, *base, variation, &mut rng);
                // Fill swatch
                for sy in 0..swatch {
                    for sx in 0..swatch {
                        let px = col * swatch + sx;
                        let py = row * swatch + sy;
                        let idx = (py * img_w + px) * 3;
                        pixels[idx] = (c.r.clamp(0.0, 1.0) * 255.0) as u8;
                        pixels[idx + 1] = (c.g.clamp(0.0, 1.0) * 255.0) as u8;
                        pixels[idx + 2] = (c.b.clamp(0.0, 1.0) * 255.0) as u8;
                    }
                }
            }
        }

        let out_dir = crate::test_module_output_dir("stroke_color");
        let out_path = out_dir.join("color_variation_swatches.png");
        image::save_buffer(
            &out_path,
            &pixels,
            img_w as u32,
            img_h as u32,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save color_variation_swatches.png");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn visual_variation_vs_amount() {
        // INSPECT: Left-to-right shows increasing color_variation (0.0 to 0.3).
        // Each column is 10 swatches at that variation level.
        // Left edge should be uniform, right edge should show spread.
        let base = Color::rgb(0.2, 0.5, 0.8);
        let steps = 7;
        let samples_per_step = 12;
        let swatch = 28usize;
        let img_w = steps * swatch;
        let img_h = samples_per_step * swatch;
        let mut pixels = vec![40u8; img_w * img_h * 3];

        for step in 0..steps {
            let var = step as f32 / (steps - 1) as f32 * 0.3;
            for sample in 0..samples_per_step {
                let path = make_path(vec![Vec2::new(0.0, 0.0), Vec2::new(1.0, 0.0)]);
                let mut rng = SeededRng::new(sample as u32 * 53 + step as u32 * 773);
                let c = compute_stroke_color(&path, None, 0, 0, base, var, &mut rng);
                for sy in 1..swatch - 1 {
                    for sx in 1..swatch - 1 {
                        let px = step * swatch + sx;
                        let py = sample * swatch + sy;
                        let idx = (py * img_w + px) * 3;
                        pixels[idx] = (c.r.clamp(0.0, 1.0) * 255.0) as u8;
                        pixels[idx + 1] = (c.g.clamp(0.0, 1.0) * 255.0) as u8;
                        pixels[idx + 2] = (c.b.clamp(0.0, 1.0) * 255.0) as u8;
                    }
                }
            }
        }

        let out_dir = crate::test_module_output_dir("stroke_color");
        let out_path = out_dir.join("variation_vs_amount.png");
        image::save_buffer(
            &out_path,
            &pixels,
            img_w as u32,
            img_h as u32,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save color_variation_amounts.png");
        eprintln!("Wrote: {}", out_path.display());
    }

    // ── Bilinear Accuracy Tests ──

    #[test]
    fn bilinear_horizontal_gradient() {
        // 1D gradient: black→white across 4 pixels.
        // Sampled values should match linear interpolation exactly.
        let tex = vec![Color::rgb(0.0, 0.0, 0.0), Color::rgb(1.0, 1.0, 1.0)];
        for i in 0..=100 {
            let u = i as f32 / 100.0;
            let c = sample_bilinear(&tex, 2, 1, Vec2::new(u, 0.5));
            // Expected: linear interpolation considering texel centers
            // At u=0.0 → texel center of pixel 0, at u=1.0 → texel center of pixel 1
            assert!(
                c.r >= -EPS && c.r <= 1.0 + EPS,
                "Out of range at u={}: r={}",
                u,
                c.r
            );
        }
    }

    #[test]
    fn bilinear_symmetry() {
        // Symmetric texture → sampling at symmetric UVs should give same result
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(0.0, 0.0, 1.0),
        ];
        let left = sample_bilinear(&tex, 2, 2, Vec2::new(0.25, 0.5));
        let right = sample_bilinear(&tex, 2, 2, Vec2::new(0.75, 0.5));
        assert!(
            approx_eq(left.r, right.r),
            "Symmetric texture not symmetric"
        );
        assert!(
            approx_eq(left.b, right.b),
            "Symmetric texture not symmetric"
        );
    }

    // ── Float Comparison in HSV ──

    // ── channel_max_diff Tests ──

    #[test]
    fn channel_max_diff_identical() {
        let c = Color::rgb(0.5, 0.3, 0.8);
        assert!(approx_eq(channel_max_diff(c, c), 0.0));
    }

    #[test]
    fn channel_max_diff_single_channel() {
        let a = Color::rgb(1.0, 0.0, 0.0);
        let b = Color::rgb(0.0, 0.0, 0.0);
        assert!(approx_eq(channel_max_diff(a, b), 1.0));
    }

    #[test]
    fn channel_max_diff_picks_max() {
        let a = Color::rgb(0.5, 0.2, 0.9);
        let b = Color::rgb(0.4, 0.8, 0.7);
        // diffs: 0.1, 0.6, 0.2 → max = 0.6
        assert!(approx_eq(channel_max_diff(a, b), 0.6));
    }

    #[test]
    fn channel_max_diff_ignores_alpha() {
        let a = Color::new(0.5, 0.5, 0.5, 0.0);
        let b = Color::new(0.5, 0.5, 0.5, 1.0);
        assert!(approx_eq(channel_max_diff(a, b), 0.0));
    }

    // ── Float Comparison in HSV ──

    #[test]
    fn hsv_float_comparison_stress() {
        // Colors where r ≈ g ≈ b with tiny differences — tests that
        // the max == c.r / c.g / c.b comparison picks the right branch
        let tricky = [
            Color::rgb(0.5000001, 0.5, 0.5),
            Color::rgb(0.5, 0.5000001, 0.5),
            Color::rgb(0.5, 0.5, 0.5000001),
            Color::rgb(0.3, 0.3000001, 0.2999999),
        ];
        for c in &tricky {
            let rt = hsv_to_rgb(rgb_to_hsv(*c));
            let err = (rt.r - c.r)
                .abs()
                .max((rt.g - c.g).abs())
                .max((rt.b - c.b).abs());
            assert!(
                err < 1e-3,
                "Float comparison issue: err={:.8} for ({:.7}, {:.7}, {:.7})",
                err,
                c.r,
                c.g,
                c.b
            );
        }
    }
}
