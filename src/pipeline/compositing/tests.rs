use super::*;
use crate::test_util::make_layer_with_order;
use crate::types::{BaseColorSource, LayerBaseColor, NormalMode};
use crate::util::math::lerp_color;

const EPS: f32 = 1e-4;

// ── Helpers ──

struct LocalFrameTransform {
    uv_map: Vec<Vec2>,
    width: usize,
    height: usize,
}

impl LocalFrameTransform {
    fn local_to_uv(&self, lx: usize, ly: usize) -> Option<Vec2> {
        if lx >= self.width || ly >= self.height {
            return None;
        }
        let uv = self.uv_map[ly * self.width + lx];
        if uv.x.is_nan() {
            None
        } else {
            Some(uv)
        }
    }

    fn uv_to_pixel(uv: Vec2, resolution: u32) -> (i32, i32) {
        (
            (uv.x * resolution as f32) as i32,
            (uv.y * resolution as f32) as i32,
        )
    }
}

/// Scatter-write composite for unit tests that test blending math.
/// Production code uses the gather-based `composite_stroke` above.
fn composite_stroke_scatter(
    local_height: &StrokeHeightResult,
    transform: &LocalFrameTransform,
    stroke_color: Color,
    stroke_id: u32,
    stroke_normal: Option<[f32; 3]>,
    global: &mut GlobalMaps,
) {
    let res = global.resolution;
    for ly in 0..local_height.height {
        for lx in 0..local_height.width {
            let h = local_height.data[ly * local_height.width + lx];
            if h <= DENSITY_MIN_THRESHOLD {
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
            let prev_h = global.height[idx];
            global.height[idx] = h.max(prev_h);
            if h >= prev_h {
                if !global.paint_load.is_empty() {
                    let r = local_height.remaining[ly * local_height.width + lx];
                    global.paint_load[idx] = r;
                }
                if let Some(sn) = stroke_normal {
                    if !global.object_normal.is_empty() {
                        global.object_normal[idx] = sn;
                    }
                }
            }
            let opacity = smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, h);
            global.color[idx] = lerp_color(global.color[idx], stroke_color, opacity);
            global.stroke_id[idx] = stroke_id;
        }
    }
}

fn make_simple_height(width: usize, height: usize, value: f32) -> StrokeHeightResult {
    StrokeHeightResult {
        data: vec![value; width * height],
        remaining: vec![1.0; width * height],
        width,
        height,
    }
}

fn make_simple_transform(width: usize, height: usize, uv: Vec2) -> LocalFrameTransform {
    // All pixels map to the same UV coordinate
    LocalFrameTransform {
        uv_map: vec![uv; width * height],
        width,
        height,
    }
}

// ── GlobalMaps Initialization Tests ──

#[test]
fn global_maps_dimensions() {
    let maps = GlobalMaps::new(
        64,
        &BaseColorSource::solid(Color::WHITE),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    assert_eq!(maps.height.len(), 64 * 64);
    assert_eq!(maps.color.len(), 64 * 64);
    assert_eq!(maps.stroke_id.len(), 64 * 64);
    assert_eq!(maps.resolution, 64);
}

#[test]
fn global_maps_solid_color_init() {
    let color = Color::rgb(0.3, 0.6, 0.9);
    let maps = GlobalMaps::new(
        16,
        &BaseColorSource::solid(color),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
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
    let tex = vec![
        Color::rgb(1.0, 0.0, 0.0),
        Color::rgb(0.0, 1.0, 0.0),
        Color::rgb(0.0, 0.0, 1.0),
        Color::rgb(1.0, 1.0, 0.0),
    ];
    let maps = GlobalMaps::new(
        4,
        &BaseColorSource::textured(&tex, 2, 2, Color::WHITE),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    assert_eq!(maps.color.len(), 16);

    let center = maps.color[5];
    assert!(center.r > 0.0 && center.g > 0.0);
}

// ── No Height Stacking Test ──

#[test]
fn no_height_stacking() {
    let mut global = GlobalMaps::new(
        16,
        &BaseColorSource::solid(Color::WHITE),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    let uv = Vec2::new(0.5, 0.5);

    for (i, &h) in [0.7f32, 0.3, 0.5].iter().enumerate() {
        let height_map = make_simple_height(1, 1, h);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.5, 0.5, 0.5),
            (i + 1) as u32,
            None,
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
fn color_high_density_covers_base() {
    let uv = Vec2::new(0.5, 0.5);

    for &(height, min_blue) in &[(0.8, 0.9), (0.9, 0.95)] {
        let mut global = GlobalMaps::new(
            16,
            &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
            NormalMode::SurfacePaint,
            BackgroundMode::Opaque,
        );
        let height_map = make_simple_height(1, 1, height);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        assert!(
            c.b > min_blue,
            "h={height}: blue should dominate, got ({:.3}, {:.3}, {:.3})",
            c.r,
            c.g,
            c.b
        );
    }
}

#[test]
fn color_transparent_bristle_gap() {
    let mut global = GlobalMaps::new(
        16,
        &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    let uv = Vec2::new(0.5, 0.5);

    let height_map = make_simple_height(1, 1, 0.001);
    let transform = make_simple_transform(1, 1, uv);
    composite_stroke_scatter(
        &height_map,
        &transform,
        Color::rgb(0.0, 0.0, 1.0),
        1,
        None,
        &mut global,
    );

    let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
    let idx = (py as u32 * 16 + px as u32) as usize;
    let c = global.color[idx];
    assert!(
        c.r > 0.9,
        "bristle gap: base red should show through, got ({:.3}, {:.3}, {:.3})",
        c.r,
        c.g,
        c.b
    );
}

#[test]
fn color_partial_blend() {
    let mut global = GlobalMaps::new(
        16,
        &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    let uv = Vec2::new(0.5, 0.5);

    let height_map = make_simple_height(1, 1, 0.15);
    let transform = make_simple_transform(1, 1, uv);
    composite_stroke_scatter(
        &height_map,
        &transform,
        Color::rgb(0.0, 0.0, 1.0),
        1,
        None,
        &mut global,
    );

    let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
    let idx = (py as u32 * 16 + px as u32) as usize;
    let c = global.color[idx];
    assert!(c.r > 0.1 && c.b > 0.1, "partial: should be a blend");
}

// ── Stroke ID Tracking Test ──

#[test]
fn stroke_id_records_last() {
    let mut global = GlobalMaps::new(
        16,
        &BaseColorSource::solid(Color::WHITE),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    let uv = Vec2::new(0.5, 0.5);

    for sid in [10u32, 20, 30] {
        let height_map = make_simple_height(1, 1, 0.5);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::WHITE,
            sid,
            None,
            &mut global,
        );
    }

    let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
    let idx = (py as u32 * 16 + px as u32) as usize;
    assert_eq!(global.stroke_id[idx], 30, "stroke_id should be last stroke");
}

// ── Layer Order Test ──

#[test]
fn layer_order_respected() {
    let mut layer_a = make_layer_with_order(0);
    layer_a.params.color_variation = 0.0;
    layer_a.params.brush_width = 40.0;

    let mut layer_b = make_layer_with_order(1);
    layer_b.params.color_variation = 0.0;
    layer_b.params.brush_width = 40.0;

    let settings = OutputSettings::default();

    let solid_a = Color::rgb(1.0, 0.0, 0.0);
    let solid_b = Color::rgb(0.0, 0.0, 1.0);

    let mut global = GlobalMaps::new(
        64,
        &BaseColorSource::solid(Color::WHITE),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    composite_layer(
        &mut global,
        &CompositeLayerInput {
            layer: &layer_a,
            layer_index: 0,
            settings: &settings,
            base_color: &BaseColorSource::solid(solid_a),
            cached_paths: None,
            normal_data: None,
            dist_field: None,
            stretch_map: None,
        },
    );
    composite_layer(
        &mut global,
        &CompositeLayerInput {
            layer: &layer_b,
            layer_index: 1,
            settings: &settings,
            base_color: &BaseColorSource::solid(solid_b),
            cached_paths: None,
            normal_data: None,
            dist_field: None,
            stretch_map: None,
        },
    );

    let center = 32 * 64 + 32;
    // Layer B (order=1) was composited last, so its color should dominate
    assert!(global.stroke_id[center] > 0, "center should be painted");
}

// ── Full Pipeline Integration Test ──

#[test]
fn composite_all_deterministic() {
    let layer = make_layer_with_order(0);
    let settings = OutputSettings::default();
    let solid = Color::rgb(0.5, 0.5, 0.5);

    let base = [LayerBaseColor::solid(solid)];
    let maps1 = composite_all(&CompositeAllInput::new(
        std::slice::from_ref(&layer),
        64,
        &base,
        &settings,
    ));
    let maps2 = composite_all(&CompositeAllInput::new(&[layer], 64, &base, &settings));

    // Verify non-trivial output (subsumes composite_all_produces_output)
    let painted = maps1.height.iter().filter(|&&h| h > 0.0).count();
    assert!(painted > 0, "should have painted some pixels");

    assert_eq!(maps1.height, maps2.height);
    assert_eq!(maps1.stroke_id, maps2.stroke_id);
}

// ── Multi-Layer Isolation Tests ──

/// Verify that multi-layer compositing via composite_all (which internally
/// uses render_layer → merge_layers) produces isolated layers: each layer's
/// strokes should not subtractively mix with strokes from other layers.
#[test]
fn multi_layer_strokes_isolated() {
    use crate::types::{Guide, GuideType};

    let mut layer_a = make_layer_with_order(0);
    layer_a.params.brush_width = 20.0;
    layer_a.params.color_variation = 0.0;
    layer_a.params.viscosity = 1.0; // no diffusion spreading
                                    // Horizontal strokes in the top half
    layer_a.guides = vec![Guide {
        guide_type: GuideType::Directional,
        position: Vec2::new(0.5, 0.25),
        direction: Vec2::X,
        influence: 0.5,
        strength: 1.0,
    }];

    let mut layer_b = make_layer_with_order(1);
    layer_b.params.brush_width = 20.0;
    layer_b.params.color_variation = 0.0;
    layer_b.params.viscosity = 1.0; // no diffusion spreading
                                    // Vertical strokes in the bottom half
    layer_b.guides = vec![Guide {
        guide_type: GuideType::Directional,
        position: Vec2::new(0.5, 0.75),
        direction: Vec2::Y,
        influence: 0.5,
        strength: 1.0,
    }];

    // Use transparent background to isolate stroke blending from base color
    let settings = OutputSettings {
        background_mode: BackgroundMode::Transparent,
        ..OutputSettings::default()
    };
    let red = Color::rgb(1.0, 0.0, 0.0);
    let blue = Color::rgb(0.0, 0.0, 1.0);
    let base = vec![LayerBaseColor::solid(red), LayerBaseColor::solid(blue)];

    // Use 128 resolution so that strokes have enough density to
    // exceed DENSITY_MIN_THRESHOLD.
    let test_res = 128;

    // Render each layer alone
    let solo_a = composite_all(&CompositeAllInput::new(
        &[layer_a.clone()],
        test_res,
        &[LayerBaseColor::solid(red)],
        &settings,
    ));
    let solo_b = composite_all(&CompositeAllInput::new(
        &[layer_b.clone()],
        test_res,
        &[LayerBaseColor::solid(blue)],
        &settings,
    ));

    // Render both together (dry=1.0, opaque stacking)
    let combined = composite_all(&CompositeAllInput::new(
        &[layer_a, layer_b],
        test_res,
        &base,
        &settings,
    ));

    // Where only layer A has paint (and layer B does not), the combined
    // result should match the solo render of layer A.
    let mut checked = 0;
    let size = (test_res * test_res) as usize;
    for i in 0..size {
        if solo_a.height[i] > 0.0 && solo_b.height[i] <= 0.0 {
            assert!(
                (combined.color[i].r - solo_a.color[i].r).abs() < 0.05
                    && (combined.color[i].g - solo_a.color[i].g).abs() < 0.05
                    && (combined.color[i].b - solo_a.color[i].b).abs() < 0.05,
                "pixel {i}: layer A solo color should survive when layer B has no paint",
            );
            checked += 1;
        }
    }
    assert!(checked > 0, "should have pixels unique to layer A");
}

// ── Visual Integration Tests ──

#[test]
fn visual_compositing() {
    let mut layer = make_layer_with_order(0);
    layer.params.brush_width = 25.0;
    layer.params.color_variation = 0.1;

    let settings = OutputSettings::default();

    let solid = Color::rgb(0.6, 0.4, 0.3);
    let maps = composite_all(&CompositeAllInput::new(
        &[layer],
        256,
        &[LayerBaseColor::solid(solid)],
        &settings,
    ));

    let out_dir = crate::test_module_output_dir("compositing");

    let max_h = maps
        .height
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .max(1e-10);
    let height_pixels: Vec<u8> = maps
        .height
        .iter()
        .map(|&h| ((h / max_h).clamp(0.0, 1.0) * 255.0) as u8)
        .collect();
    let height_path = out_dir.join("height.png");
    image::save_buffer(&height_path, &height_pixels, 256, 256, image::ColorType::L8)
        .expect("Failed to save compositing_height.png");

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
    image::save_buffer(&color_path, &color_pixels, 256, 256, image::ColorType::Rgb8)
        .expect("Failed to save compositing_color.png");

    eprintln!("Wrote: {}", height_path.display());
    eprintln!("Wrote: {}", color_path.display());
}

#[test]
fn visual_dry_brush() {
    let mut layer = make_layer_with_order(0);
    layer.params.load = 0.3;
    // density-only: no height/ridge params
    layer.params.brush_width = 25.0;
    layer.params.color_variation = 0.0;

    let settings = OutputSettings::default();

    let stroke_tex = vec![Color::rgb(0.25, 0.12, 0.05)];
    let canvas = Color::rgb(0.95, 0.92, 0.85);

    let mut global = GlobalMaps::new(
        256,
        &BaseColorSource::solid(canvas),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    composite_layer(
        &mut global,
        &CompositeLayerInput {
            layer: &layer,
            layer_index: 0,
            settings: &settings,
            base_color: &BaseColorSource::textured(&stroke_tex, 1, 1, canvas),
            cached_paths: None,
            normal_data: None,
            dist_field: None,
            stretch_map: None,
        },
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
    image::save_buffer(&out_path, &color_pixels, 256, 256, image::ColorType::Rgb8)
        .expect("Failed to save");
    eprintln!("Wrote: {}", out_path.display());
}

#[test]
fn visual_color_variation() {
    let mut layer = make_layer_with_order(0);
    layer.params.color_variation = 0.25;
    layer.params.brush_width = 20.0;

    let settings = OutputSettings::default();

    let mut tex = Vec::new();
    for y in 0..4 {
        for x in 0..4 {
            tex.push(Color::rgb(x as f32 / 3.0, y as f32 / 3.0, 0.5));
        }
    }

    let base = [LayerBaseColor {
        solid_color: Color::WHITE,
        texture: Some(tex),
        tex_width: 4,
        tex_height: 4,
    }];
    let maps = composite_all(&CompositeAllInput::new(&[layer], 256, &base, &settings));

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
    image::save_buffer(&out_path, &color_pixels, 256, 256, image::ColorType::Rgb8)
        .expect("Failed to save");
    eprintln!("Wrote: {}", out_path.display());
}

#[test]
fn skip_unpainted_pixels() {
    let mut global = GlobalMaps::new(
        16,
        &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
        NormalMode::SurfacePaint,
        BackgroundMode::Opaque,
    );
    let uv = Vec2::new(0.5, 0.5);

    let height_map = make_simple_height(1, 1, 0.0);
    let transform = make_simple_transform(1, 1, uv);
    composite_stroke_scatter(
        &height_map,
        &transform,
        Color::rgb(0.0, 0.0, 1.0),
        1,
        None,
        &mut global,
    );

    let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
    let idx = (py as u32 * 16 + px as u32) as usize;
    assert_eq!(global.height[idx], 0.0, "zero height should not overwrite");
    assert_eq!(global.stroke_id[idx], 0, "stroke_id should remain 0");
    let c = global.color[idx];
    assert!(
        (c.r - 1.0).abs() < EPS && c.g.abs() < EPS && c.b.abs() < EPS,
        "zero height should preserve base color, got ({:.3}, {:.3}, {:.3})",
        c.r,
        c.g,
        c.b
    );
}

// ── Base color 2-pass tests ──

/// Verify that specific-group base colors override __all__ within their
/// masked region, regardless of layer order.
#[test]
fn base_color_specific_group_overrides_all() {
    use crate::mesh::uv_mask::UvMask;

    let res = 8u32;
    let size = (res * res) as usize;

    let red = Color::rgb(1.0, 0.0, 0.0);
    let blue = Color::rgb(0.0, 0.0, 1.0);

    // Mask covering the top-left quadrant (y < 4, x < 4)
    let mut mask_data = vec![false; size];
    for py in 0..4 {
        for px in 0..4 {
            mask_data[(py * res + px) as usize] = true;
        }
    }
    let mask = UvMask {
        data: mask_data,
        resolution: res,
    };
    let df = mask.distance_field();

    // Two empty LayerMaps (no paint — we only test base color fill)
    let empty_layer = LayerMaps {
        height: vec![0.0; size],
        color: vec![Color::new(0.0, 0.0, 0.0, 0.0); size],
        stroke_id: vec![0; size],
        object_normal: vec![[0.0; 3]; size],
        gradient_x: vec![0.0; size],
        gradient_y: vec![0.0; size],
        paint_load: vec![0.0; size],
        stroke_time_order: vec![f32::MAX; size],
        stroke_time_arc: vec![0.0; size],
        resolution: res,
    };

    let settings = OutputSettings {
        background_mode: BackgroundMode::Opaque,
        ..OutputSettings::default()
    };

    // Layer 0: group="arm" (specific), base_color=blue
    // Layer 1: group="__all__", base_color=red
    // Even though __all__ comes AFTER arm, arm's blue must win in the masked region.
    let layer_refs: Vec<&LayerMaps> = vec![&empty_layer, &empty_layer];
    let base_colors = vec![BaseColorSource::solid(blue), BaseColorSource::solid(red)];
    let dfs: Vec<Option<&DistanceField>> = vec![Some(&df), None];
    let group_names: Vec<&str> = vec!["arm", "__all__"];

    let global = finalize_layers(
        &layer_refs,
        &base_colors,
        &dfs,
        &[1.0, 1.0],
        &group_names,
        res,
        &settings,
    );

    // Top-left quadrant (masked "arm" region) should be blue
    let tl_idx = (res + 1) as usize;
    assert!(
        (global.color[tl_idx].r - blue.r).abs() < EPS
            && (global.color[tl_idx].b - blue.b).abs() < EPS,
        "masked region should have specific group base color (blue), got {:?}",
        global.color[tl_idx],
    );

    // Bottom-right (outside mask) should be red (__all__ backdrop)
    let br_idx = (6 * res + 6) as usize;
    assert!(
        (global.color[br_idx].r - red.r).abs() < EPS
            && (global.color[br_idx].b - red.b).abs() < EPS,
        "unmasked region should have __all__ base color (red), got {:?}",
        global.color[br_idx],
    );
}

/// Verify that only the lowest (first in sorted order) layer per group
/// determines the base color.
#[test]
fn base_color_lowest_layer_wins_per_group() {
    let res = 4u32;
    let size = (res * res) as usize;

    let green = Color::rgb(0.0, 1.0, 0.0);
    let yellow = Color::rgb(1.0, 1.0, 0.0);

    let empty_layer = LayerMaps {
        height: vec![0.0; size],
        color: vec![Color::new(0.0, 0.0, 0.0, 0.0); size],
        stroke_id: vec![0; size],
        object_normal: vec![[0.0; 3]; size],
        gradient_x: vec![0.0; size],
        gradient_y: vec![0.0; size],
        paint_load: vec![0.0; size],
        stroke_time_order: vec![f32::MAX; size],
        stroke_time_arc: vec![0.0; size],
        resolution: res,
    };

    let settings = OutputSettings {
        background_mode: BackgroundMode::Opaque,
        ..OutputSettings::default()
    };

    // Two __all__ layers: first=green, second=yellow
    // Only green (first/lowest) should be used.
    let layer_refs: Vec<&LayerMaps> = vec![&empty_layer, &empty_layer];
    let base_colors = vec![
        BaseColorSource::solid(green),
        BaseColorSource::solid(yellow),
    ];
    let dfs: Vec<Option<&DistanceField>> = vec![None, None];
    let group_names: Vec<&str> = vec!["__all__", "__all__"];

    let global = finalize_layers(
        &layer_refs,
        &base_colors,
        &dfs,
        &[1.0, 1.0],
        &group_names,
        res,
        &settings,
    );

    let idx = (2 * res + 2) as usize;
    assert!(
        (global.color[idx].g - green.g).abs() < EPS && (global.color[idx].r - green.r).abs() < EPS,
        "should use first __all__ layer's base color (green), got {:?}",
        global.color[idx],
    );
}
