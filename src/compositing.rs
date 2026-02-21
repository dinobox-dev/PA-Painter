use glam::Vec2;

use crate::brush_profile::generate_brush_profile;
use crate::local_frame::{build_local_frame, LocalFrameTransform};
use crate::math::{lerp_color, smoothstep};
use crate::path_placement::generate_paths;
use crate::rng::SeededRng;
use crate::stroke_color::{compute_stroke_color, sample_bilinear};
use crate::stroke_height::{generate_stroke_height, StrokeHeightResult};
use crate::types::{Color, OutputSettings, Region};

/// Global compositing buffers in UV space.
pub struct GlobalMaps {
    /// Height map. 0.0 = no paint. Row-major, size = resolution * resolution.
    pub height: Vec<f32>,
    /// Color map. Row-major, size = resolution * resolution.
    pub color: Vec<Color>,
    /// Stroke ID map. 0 = no stroke. Row-major.
    pub stroke_id: Vec<u32>,
    pub resolution: u32,
}

impl GlobalMaps {
    /// Initialize global maps.
    /// Color is initialized from base_color_texture (or solid_color if None).
    pub fn new(
        resolution: u32,
        base_color_texture: Option<&[Color]>,
        tex_width: u32,
        tex_height: u32,
        solid_color: Color,
    ) -> Self {
        let size = (resolution * resolution) as usize;
        let height = vec![0.0; size];
        let stroke_id = vec![0u32; size];

        let color = if let Some(tex) = base_color_texture {
            // Resample base texture to output resolution using bilinear interpolation
            let mut colors = Vec::with_capacity(size);
            for py in 0..resolution {
                for px in 0..resolution {
                    let uv = Vec2::new(
                        (px as f32 + 0.5) / resolution as f32,
                        (py as f32 + 0.5) / resolution as f32,
                    );
                    colors.push(sample_bilinear(tex, tex_width, tex_height, uv));
                }
            }
            colors
        } else {
            vec![solid_color; size]
        };

        Self {
            height,
            color,
            stroke_id,
            resolution,
        }
    }
}

/// Composite a single stroke into the global maps.
///
/// - `local_height`: stroke height map from Phase 02
/// - `transform`: local frame → UV transform from Phase 06
/// - `stroke_color`: uniform color for this stroke
/// - `stroke_id`: unique ID for this stroke
/// - `base_height`: the region's base_height param (used for opacity calc)
/// - `global`: mutable reference to global maps
pub fn composite_stroke(
    local_height: &StrokeHeightResult,
    transform: &LocalFrameTransform,
    stroke_color: Color,
    stroke_id: u32,
    base_height: f32,
    global: &mut GlobalMaps,
) {
    let res = global.resolution;

    for ly in 0..local_height.height {
        for lx in 0..local_height.width {
            let h = local_height.data[ly * local_height.width + lx];
            if h <= 0.0 {
                continue;
            }

            let uv = match transform.local_to_uv(lx, ly) {
                Some(uv) => uv,
                None => continue,
            };

            let (px, py) = LocalFrameTransform::uv_to_pixel(uv, res);
            if px < 0 || py < 0 || px >= res as i32 || py >= res as i32 {
                continue;
            }

            let idx = (py as u32 * res + px as u32) as usize;

            // Height: MAX (tallest paint wins, no accumulation)
            global.height[idx] = h.max(global.height[idx]);

            // Color: height-based blending
            let opacity = smoothstep(0.0, base_height * 0.7, h);
            global.color[idx] = lerp_color(global.color[idx], stroke_color, opacity);

            // Stroke ID: record last stroke
            global.stroke_id[idx] = stroke_id;
        }
    }
}

/// Composite all strokes from all regions into global maps.
///
/// Regions are composited in `order` (ascending). Within each region,
/// strokes are composited in their path order (top-to-bottom).
pub fn composite_all(
    regions: &[Region],
    resolution: u32,
    base_color_texture: Option<&[Color]>,
    tex_width: u32,
    tex_height: u32,
    solid_color: Color,
    settings: &OutputSettings,
) -> GlobalMaps {
    let mut global = GlobalMaps::new(resolution, base_color_texture, tex_width, tex_height, solid_color);

    // Sort regions by order (ascending)
    let mut sorted_regions: Vec<&Region> = regions.iter().collect();
    sorted_regions.sort_by(|a, b| a.order.cmp(&b.order));

    for region in sorted_regions {
        composite_region(
            region,
            resolution,
            &mut global,
            settings,
            base_color_texture,
            tex_width,
            tex_height,
            solid_color,
        );
    }

    global
}

/// Composite a single region's strokes into the global maps.
///
/// This is the inner loop of `composite_all`, extracted for single-region
/// preview regeneration. The caller is responsible for clearing/resetting
/// pixels belonging to this region before calling (if needed).
pub fn composite_region(
    region: &Region,
    resolution: u32,
    global: &mut GlobalMaps,
    _settings: &OutputSettings,
    base_color_texture: Option<&[Color]>,
    tex_width: u32,
    tex_height: u32,
    solid_color: Color,
) {
    let paths = generate_paths(region, resolution);

    // Generate brush profile once per region (same seed for all strokes in region)
    let brush_profile =
        generate_brush_profile(region.params.brush_width.round() as usize, region.params.seed);

    let mut rng = SeededRng::new(region.params.seed);

    for (i, path) in paths.iter().enumerate() {
        // Build local frame
        let transform = build_local_frame(
            path,
            region.params.brush_width.round() as usize,
            region.params.ridge_width.round() as usize,
            resolution,
        );

        // Generate height
        let stroke_length_px = (path.arc_length() * resolution as f32).ceil() as usize;
        let local_height = generate_stroke_height(
            &brush_profile,
            region.params.brush_width.round() as usize,
            stroke_length_px,
            region.params.load,
            region.params.base_height,
            region.params.ridge_height,
            region.params.ridge_width.round() as usize,
            region.params.ridge_variation,
            region.params.body_wiggle,
            region.params.pressure_preset,
            region.params.seed + i as u32,
        );

        // Compute stroke color
        let stroke_color = compute_stroke_color(
            path,
            base_color_texture,
            tex_width,
            tex_height,
            solid_color,
            region.params.color_variation,
            &mut rng,
        );

        // Composite
        composite_stroke(
            &local_height,
            &transform,
            stroke_color,
            path.stroke_id,
            region.params.base_height,
            global,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GuideVertex, Polygon, StrokeParams};

    const EPS: f32 = 1e-4;

    // ── Helpers ──

    fn make_simple_height(width: usize, height: usize, value: f32) -> StrokeHeightResult {
        StrokeHeightResult {
            data: vec![value; width * height],
            width,
            height,
            margin: 0,
        }
    }

    fn make_simple_transform(width: usize, height: usize, uv: Vec2) -> LocalFrameTransform {
        // All pixels map to the same UV coordinate
        LocalFrameTransform {
            uv_map: vec![uv; width * height],
            width,
            height,
            margin: 0,
        }
    }

    fn make_polygon(verts: Vec<Vec2>) -> Polygon {
        Polygon { vertices: verts }
    }

    fn make_square_region(min: f32, max: f32, order: i32, id: u32) -> Region {
        Region {
            id,
            name: format!("region_{}", id),
            mask: vec![make_polygon(vec![
                Vec2::new(min, min),
                Vec2::new(max, min),
                Vec2::new(max, max),
                Vec2::new(min, max),
            ])],
            order,
            params: StrokeParams::default(),
            guides: vec![GuideVertex {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
            }],
        }
    }

    // ── GlobalMaps Initialization Tests ──

    #[test]
    fn global_maps_dimensions() {
        let maps = GlobalMaps::new(64, None, 0, 0, Color::WHITE);
        assert_eq!(maps.height.len(), 64 * 64);
        assert_eq!(maps.color.len(), 64 * 64);
        assert_eq!(maps.stroke_id.len(), 64 * 64);
        assert_eq!(maps.resolution, 64);
    }

    #[test]
    fn global_maps_solid_color_init() {
        let color = Color::rgb(0.3, 0.6, 0.9);
        let maps = GlobalMaps::new(16, None, 0, 0, color);
        for c in &maps.color {
            assert!((c.r - 0.3).abs() < EPS);
            assert!((c.g - 0.6).abs() < EPS);
            assert!((c.b - 0.9).abs() < EPS);
        }
        assert!(maps.height.iter().all(|&h| h == 0.0));
        assert!(maps.stroke_id.iter().all(|&id| id == 0));
    }

    #[test]
    fn global_maps_texture_resample() {
        // 2x2 texture: corners are red, green, blue, yellow
        let tex = vec![
            Color::rgb(1.0, 0.0, 0.0),
            Color::rgb(0.0, 1.0, 0.0),
            Color::rgb(0.0, 0.0, 1.0),
            Color::rgb(1.0, 1.0, 0.0),
        ];
        let maps = GlobalMaps::new(4, Some(&tex), 2, 2, Color::WHITE);
        assert_eq!(maps.color.len(), 16);

        // Center pixel should be a blend of all four
        let center = maps.color[1 * 4 + 1]; // pixel (1,1) in a 4x4 map
        // Should have non-trivial values in r, g channels
        assert!(center.r > 0.0 && center.g > 0.0);
    }

    // ── No Height Stacking Test ──

    #[test]
    fn no_height_stacking() {
        // 3 strokes at the same UV position with heights [0.7, 0.3, 0.5]
        // Expected: Final height = 0.7 (max wins), NOT 0.5 (overwrite) or 1.5 (sum)
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::WHITE);
        let uv = Vec2::new(0.5, 0.5);

        for (i, &h) in [0.7f32, 0.3, 0.5].iter().enumerate() {
            let height_map = make_simple_height(1, 1, h);
            let transform = make_simple_transform(1, 1, uv);
            composite_stroke(
                &height_map,
                &transform,
                Color::rgb(0.5, 0.5, 0.5),
                (i + 1) as u32,
                1.0,
                &mut global,
            );
        }

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        assert!(
            (global.height[idx] - 0.7).abs() < EPS,
            "height = {}, expected 0.7 (max, not overwrite 0.5)",
            global.height[idx]
        );
    }

    // ── Color Blending Tests ──

    #[test]
    fn color_full_cover() {
        // h = base_height → opacity ≈ 1.0, should be fully covered
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::rgb(1.0, 0.0, 0.0)); // red base
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;

        let height_map = make_simple_height(1, 1, base_height);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0), // blue stroke
            1,
            base_height,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        // smoothstep(0.5, 0.0, 0.5*0.7=0.35) → 0.5 > 0.35 → saturated to 1.0
        assert!(
            c.b > 0.9,
            "full cover: blue should dominate, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    #[test]
    fn color_transparent_bristle_gap() {
        // h ≈ 0 (bristle gap) → opacity ≈ 0, should preserve base color
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::rgb(1.0, 0.0, 0.0));
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;

        let height_map = make_simple_height(1, 1, 0.001); // near-zero
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        // smoothstep(0.001, 0.0, 0.35) ≈ very small
        assert!(
            c.r > 0.9,
            "bristle gap: base red should show through, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    #[test]
    fn color_partial_blend() {
        // h = base_height * 0.3 → partial opacity
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::rgb(1.0, 0.0, 0.0));
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;
        let h = base_height * 0.3; // 0.15

        let height_map = make_simple_height(1, 1, h);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        // Should be a blend — neither fully red nor fully blue
        assert!(c.r > 0.1 && c.b > 0.1, "partial: should be a blend");
    }

    #[test]
    fn color_ridge_full_opacity() {
        // h = base_height + ridge_height (> base) → opacity = 1.0
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::rgb(1.0, 0.0, 0.0));
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;
        let h = base_height + 0.3; // ridge: 0.8

        let height_map = make_simple_height(1, 1, h);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        // Ridge height well above threshold → opacity saturated to 1.0
        assert!(
            c.b > 0.95,
            "ridge: should be fully covered, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    // ── Stroke ID Tracking Test ──

    #[test]
    fn stroke_id_records_last() {
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::WHITE);
        let uv = Vec2::new(0.5, 0.5);

        for sid in [10u32, 20, 30] {
            let height_map = make_simple_height(1, 1, 0.5);
            let transform = make_simple_transform(1, 1, uv);
            composite_stroke(
                &height_map,
                &transform,
                Color::WHITE,
                sid,
                0.5,
                &mut global,
            );
        }

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        assert_eq!(global.stroke_id[idx], 30, "stroke_id should be last stroke");
    }

    // ── Region Order Test ──

    #[test]
    fn region_order_respected() {
        // Region A (order=0, red), Region B (order=1, blue) — overlapping
        // In overlap area, blue should be on top
        let mut region_a = make_square_region(0.2, 0.8, 0, 0);
        region_a.params.color_variation = 0.0;
        region_a.params.brush_width = 40.0;

        let mut region_b = make_square_region(0.2, 0.8, 1, 1);
        region_b.params.color_variation = 0.0;
        region_b.params.brush_width = 40.0;

        let settings = OutputSettings::default();

        let solid_a = Color::rgb(1.0, 0.0, 0.0);
        let solid_b = Color::rgb(0.0, 0.0, 1.0);

        // Composite region A (order 0) first
        let mut global = GlobalMaps::new(64, None, 0, 0, Color::WHITE);
        composite_region(
            &region_a, 64, &mut global, &settings,
            None, 0, 0, solid_a,
        );

        // Composite region B (order 1) second
        composite_region(
            &region_b, 64, &mut global, &settings,
            None, 0, 0, solid_b,
        );

        // Check center pixel — region B should have painted last
        let center = 32 * 64 + 32;
        if global.stroke_id[center] > 0 {
            // If both regions painted here, the stroke_id should be from region B
            // Region B IDs are (1 << 16) | index
            let region_from_id = global.stroke_id[center] >> 16;
            assert_eq!(
                region_from_id, 1,
                "center should be painted by region B (order=1)"
            );
        }
    }

    // ── Full Pipeline Integration Test ──

    #[test]
    fn composite_all_produces_output() {
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.brush_width = 10.0; // smaller brush for low-res test

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[region],
            128,
            None, 0, 0,
            Color::rgb(0.8, 0.6, 0.4),
            &settings,
        );

        assert_eq!(maps.height.len(), 128 * 128);
        assert_eq!(maps.color.len(), 128 * 128);

        // Should have some painted pixels
        let painted = maps.height.iter().filter(|&&h| h > 0.0).count();
        assert!(painted > 0, "should have painted some pixels");
    }

    #[test]
    fn composite_all_deterministic() {
        let region = make_square_region(0.15, 0.85, 0, 0);
        let settings = OutputSettings::default();
        let solid = Color::rgb(0.5, 0.5, 0.5);

        let maps1 = composite_all(&[region.clone()], 64, None, 0, 0, solid, &settings);
        let maps2 = composite_all(&[region], 64, None, 0, 0, solid, &settings);

        assert_eq!(maps1.height, maps2.height);
        assert_eq!(maps1.stroke_id, maps2.stroke_id);
    }

    // ── Visual Integration Tests ──

    #[test]
    fn visual_compositing() {
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.brush_width = 25.0;
        region.params.ridge_height = 0.3;
        region.params.color_variation = 0.1;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.6, 0.4, 0.3);
        let maps = composite_all(&[region], 256, None, 0, 0, solid, &settings);

        let out_dir = crate::test_module_output_dir("compositing");

        // Save height map as grayscale PNG
        let max_h = maps.height.iter().cloned().fold(0.0f32, f32::max).max(1e-10);
        let height_pixels: Vec<u8> = maps
            .height
            .iter()
            .map(|&h| ((h / max_h).clamp(0.0, 1.0) * 255.0) as u8)
            .collect();
        let height_path = out_dir.join("height.png");
        image::save_buffer(
            &height_path,
            &height_pixels,
            256,
            256,
            image::ColorType::L8,
        )
        .expect("Failed to save compositing_height.png");

        // Save color map as RGB PNG
        let color_pixels: Vec<u8> = maps
            .color
            .iter()
            .flat_map(|c| {
                [
                    (c.r.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.g.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.b.clamp(0.0, 1.0) * 255.0) as u8,
                ]
            })
            .collect();
        let color_path = out_dir.join("color.png");
        image::save_buffer(
            &color_path,
            &color_pixels,
            256,
            256,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save compositing_color.png");

        eprintln!("Wrote: {}", height_path.display());
        eprintln!("Wrote: {}", color_path.display());
    }

    #[test]
    fn visual_flat_stroke_no_impasto() {
        // base_height=0.5, ridge_height=0 → bristle pattern only, no edge ridges
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.base_height = 0.5;
        region.params.ridge_height = 0.0;
        region.params.brush_width = 25.0;
        region.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.5, 0.7, 0.3);
        let maps = composite_all(&[region], 256, None, 0, 0, solid, &settings);

        let max_h = maps.height.iter().cloned().fold(0.0f32, f32::max).max(1e-10);
        let pixels: Vec<u8> = maps
            .height
            .iter()
            .map(|&h| ((h / max_h).clamp(0.0, 1.0) * 255.0) as u8)
            .collect();

        let out_path = crate::test_module_output_dir("compositing").join("flat.png");
        image::save_buffer(
            &out_path,
            &pixels,
            256,
            256,
            image::ColorType::L8,
        )
        .expect("Failed to save");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn visual_prominent_impasto() {
        // base_height=0.3, ridge_height=0.5 → pronounced ridges
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.base_height = 0.3;
        region.params.ridge_height = 0.5;
        region.params.brush_width = 25.0;
        region.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.5, 0.3, 0.6);
        let maps = composite_all(&[region], 256, None, 0, 0, solid, &settings);

        let max_h = maps.height.iter().cloned().fold(0.0f32, f32::max).max(1e-10);
        let pixels: Vec<u8> = maps
            .height
            .iter()
            .map(|&h| ((h / max_h).clamp(0.0, 1.0) * 255.0) as u8)
            .collect();

        let out_path = crate::test_module_output_dir("compositing").join("impasto.png");
        image::save_buffer(
            &out_path,
            &pixels,
            256,
            256,
            image::ColorType::L8,
        )
        .expect("Failed to save");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn visual_dry_brush() {
        // load=0.3 → underlying color visible through bristle gaps
        // Use a bright canvas with a contrasting dark stroke color (via texture)
        // so the dry brush transparency effect is clearly visible.
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.load = 0.3;
        region.params.base_height = 0.5;
        region.params.ridge_height = 0.0;
        region.params.brush_width = 25.0;
        region.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        // Dark brown 1x1 texture for stroke color sampling,
        // bright canvas as base so gaps show through
        let stroke_tex = vec![Color::rgb(0.25, 0.12, 0.05)];
        let canvas = Color::rgb(0.95, 0.92, 0.85);

        // Initialize global maps with bright canvas, then composite with dark texture
        let mut global = GlobalMaps::new(256, None, 0, 0, canvas);
        composite_region(
            &region, 256, &mut global, &settings,
            Some(&stroke_tex), 1, 1, canvas,
        );
        let maps = global;

        let color_pixels: Vec<u8> = maps
            .color
            .iter()
            .flat_map(|c| {
                [
                    (c.r.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.g.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.b.clamp(0.0, 1.0) * 255.0) as u8,
                ]
            })
            .collect();

        let out_path = crate::test_module_output_dir("compositing").join("dry_brush.png");
        image::save_buffer(
            &out_path,
            &color_pixels,
            256,
            256,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn visual_color_variation() {
        // High color_variation → impressionist-style per-stroke shifts
        let mut region = make_square_region(0.1, 0.9, 0, 0);
        region.params.color_variation = 0.25;
        region.params.brush_width = 20.0;

        let settings = OutputSettings::default();

        // Create a gradient texture (4x4)
        let mut tex = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                tex.push(Color::rgb(
                    x as f32 / 3.0,
                    y as f32 / 3.0,
                    0.5,
                ));
            }
        }

        let maps = composite_all(
            &[region],
            256,
            Some(&tex),
            4,
            4,
            Color::WHITE,
            &settings,
        );

        let color_pixels: Vec<u8> = maps
            .color
            .iter()
            .flat_map(|c| {
                [
                    (c.r.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.g.clamp(0.0, 1.0) * 255.0) as u8,
                    (c.b.clamp(0.0, 1.0) * 255.0) as u8,
                ]
            })
            .collect();

        let out_path = crate::test_module_output_dir("compositing").join("color_variation.png");
        image::save_buffer(
            &out_path,
            &color_pixels,
            256,
            256,
            image::ColorType::Rgb8,
        )
        .expect("Failed to save");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn zero_height_preserves_base() {
        // Pixels where no stroke paints should retain base color
        let solid = Color::rgb(0.3, 0.7, 0.1);
        let maps = GlobalMaps::new(16, None, 0, 0, solid);

        // Without any compositing, all colors should be the base
        for c in &maps.color {
            assert!((c.r - 0.3).abs() < EPS);
            assert!((c.g - 0.7).abs() < EPS);
            assert!((c.b - 0.1).abs() < EPS);
        }
    }

    #[test]
    fn skip_unpainted_pixels() {
        // Height = 0 pixels should not affect global maps
        let mut global = GlobalMaps::new(16, None, 0, 0, Color::rgb(1.0, 0.0, 0.0));
        let uv = Vec2::new(0.5, 0.5);

        let height_map = make_simple_height(1, 1, 0.0); // zero height
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            0.5,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        assert_eq!(global.height[idx], 0.0, "zero height should not overwrite");
        assert_eq!(global.stroke_id[idx], 0, "stroke_id should remain 0");
    }
}
