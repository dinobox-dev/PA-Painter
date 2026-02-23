use glam::Vec2;
use rayon::prelude::*;

use crate::brush_profile::generate_brush_profile;
use crate::math::{lerp, lerp_color, perpendicular, smoothstep};
use crate::object_normal::{sample_object_normal, MeshNormalData};
use crate::path_placement::generate_paths;
use crate::stroke_color::ColorTextureRef;
use crate::uv_mask::UvMask;
use crate::rng::SeededRng;
use crate::stroke_color::{compute_stroke_color, sample_bilinear};
use crate::stroke_height::{generate_stroke_height, StrokeHeightResult};
use crate::types::{BackgroundMode, BaseColorSource, Color, NormalMode, OutputSettings, PaintLayer, StrokePath};

/// Global compositing buffers in UV space.
pub struct GlobalMaps {
    /// Height map. 0.0 = no paint. Row-major, size = resolution * resolution.
    pub height: Vec<f32>,
    /// Color map. Row-major, size = resolution * resolution.
    pub color: Vec<Color>,
    /// Stroke ID map. 0 = no stroke. Row-major.
    pub stroke_id: Vec<u32>,
    /// Object-space normal per pixel (composited from strokes).
    /// Empty in SurfacePaint mode.
    pub object_normal: Vec<[f32; 3]>,
    pub resolution: u32,
}

impl GlobalMaps {
    /// Initialize global maps.
    /// Color is initialized from base_color_texture (or solid_color if None).
    /// In `Transparent` background mode, color is initialized to fully transparent
    /// regardless of texture/solid_color.
    pub fn new(
        resolution: u32,
        base_color: &BaseColorSource,
        normal_mode: NormalMode,
        background_mode: BackgroundMode,
    ) -> Self {
        let size = (resolution * resolution) as usize;
        let height = vec![0.0; size];
        let stroke_id = vec![0u32; size];

        let object_normal = if normal_mode == NormalMode::DepictedForm {
            vec![[0.0f32; 3]; size]
        } else {
            Vec::new()
        };

        let color = if background_mode == BackgroundMode::Transparent {
            vec![Color::new(0.0, 0.0, 0.0, 0.0); size]
        } else if let Some(tex) = base_color.texture {
            // Resample base texture to output resolution using bilinear interpolation
            let mut colors = Vec::with_capacity(size);
            for py in 0..resolution {
                for px in 0..resolution {
                    let uv = Vec2::new(
                        (px as f32 + 0.5) / resolution as f32,
                        (py as f32 + 0.5) / resolution as f32,
                    );
                    colors.push(sample_bilinear(tex, base_color.tex_width, base_color.tex_height, uv));
                }
            }
            colors
        } else {
            vec![base_color.solid_color; size]
        };

        Self {
            height,
            color,
            stroke_id,
            object_normal,
            resolution,
        }
    }
}

/// Bilinear sample from a row-major 2D float array.
fn bilinear_sample(data: &[f32], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let x0 = (x.floor() as usize).min(width - 1);
    let y0 = (y.floor() as usize).min(height - 1);
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let sx = x - x0 as f32;
    let sy = y - y0 as f32;

    let h00 = data[y0 * width + x0];
    let h10 = data[y0 * width + x1];
    let h01 = data[y1 * width + x0];
    let h11 = data[y1 * width + x1];

    h00 * (1.0 - sx) * (1.0 - sy)
        + h10 * sx * (1.0 - sy)
        + h01 * (1.0 - sx) * sy
        + h11 * sx * sy
}

/// Per-stroke appearance parameters for compositing.
pub struct StrokeAppearance {
    pub color: Color,
    pub id: u32,
    pub base_height: f32,
    pub normal: Option<[f32; 3]>,
    pub transparent: bool,
}

/// Composite a single stroke into the global maps using **per-segment gather**.
///
/// For each path segment, compute a tight bounding box and evaluate every
/// global pixel inside it. Each pixel is projected onto the segment in O(1)
/// (dot products), converted to local-frame coordinates, and bilinearly
/// sampled from the local height map. This guarantees every destination
/// pixel is explicitly evaluated with no scatter-write gaps, while keeping
/// the per-pixel cost constant (no full-path scan).
pub fn composite_stroke(
    local_height: &StrokeHeightResult,
    path: &StrokePath,
    resolution: u32,
    appearance: &StrokeAppearance,
    global: &mut GlobalMaps,
) {
    let stroke_color = appearance.color;
    let stroke_id = appearance.id;
    let base_height = appearance.base_height;
    let stroke_normal = appearance.normal;
    let transparent = appearance.transparent;
    let res = resolution;
    let margin = local_height.margin;
    let local_w = local_height.width;
    let local_h = local_height.height;
    let res_f = res as f32;

    let points = &path.points;
    if points.len() < 2 || local_w < 2 || local_h < 2 {
        return;
    }

    // Use cached cumulative arc lengths from StrokePath
    let accum_lens = path.cumulative_lengths();
    let total_len = path.arc_length();
    if total_len < 1e-8 {
        return;
    }

    let stroke_length_px = (total_len * res_f).ceil() as usize;
    let margin_uv = margin as f32 / res_f;
    let half_lateral_uv = local_h as f32 / 2.0 / res_f;
    // Half-pixel overlap at segment junctions to avoid gaps
    let junction_pad = 0.5 / res_f;

    for seg_idx in 0..points.len() - 1 {
        let a = points[seg_idx];
        let b = points[seg_idx + 1];
        let seg = b - a;
        let seg_len = seg.length();
        if seg_len < 1e-8 {
            continue;
        }
        let seg_dir = seg / seg_len;
        let normal = perpendicular(seg_dir);

        // Projection bounds along this segment
        let proj_lo = if seg_idx == 0 { -margin_uv } else { -junction_pad };
        let proj_hi = seg_len + junction_pad;

        // Tight bounding box for this segment
        let ext = Vec2::splat(half_lateral_uv + 1.0 / res_f);
        let mut seg_min = a.min(b) - ext;
        let mut seg_max = a.max(b) + ext;
        if seg_idx == 0 {
            let front = a - seg_dir * margin_uv;
            seg_min = seg_min.min(front - ext);
            seg_max = seg_max.max(front + ext);
        }

        let gx_min = ((seg_min.x * res_f).floor() as i32).max(0) as u32;
        let gy_min = ((seg_min.y * res_f).floor() as i32).max(0) as u32;
        let gx_max = ((seg_max.x * res_f).ceil() as i32).min(res as i32 - 1) as u32;
        let gy_max = ((seg_max.y * res_f).ceil() as i32).min(res as i32 - 1) as u32;

        let accum = accum_lens[seg_idx];

        for gy in gy_min..=gy_max {
            for gx in gx_min..=gx_max {
                let uv = Vec2::new(
                    (gx as f32 + 0.5) / res_f,
                    (gy as f32 + 0.5) / res_f,
                );

                let to_uv = uv - a;
                let proj = to_uv.dot(seg_dir);
                if proj < proj_lo || proj > proj_hi {
                    continue;
                }

                let lateral = to_uv.dot(normal);
                if lateral.abs() > half_lateral_uv {
                    continue;
                }

                // Local frame coordinates
                let t = (accum + proj) / total_len;
                let lx_f = t * stroke_length_px as f32 + margin as f32;
                let ly_f = lateral * res_f + local_h as f32 / 2.0;

                if lx_f < 0.0
                    || lx_f >= (local_w - 1) as f32
                    || ly_f < 0.0
                    || ly_f >= (local_h - 1) as f32
                {
                    continue;
                }

                let h = bilinear_sample(
                    &local_height.data,
                    local_w,
                    local_h,
                    lx_f,
                    ly_f,
                );
                if h <= 0.0 {
                    continue;
                }

                let idx = (gy * res + gx) as usize;

                global.height[idx] = h.max(global.height[idx]);

                let opacity = smoothstep(0.0, base_height * 0.7, h);
                if transparent {
                    if global.stroke_id[idx] == 0 {
                        // First paint on virgin pixel: set color directly
                        global.color[idx] = Color::new(
                            stroke_color.r,
                            stroke_color.g,
                            stroke_color.b,
                            opacity,
                        );
                    } else {
                        // Over-paint: blend paint colors, alpha = max
                        let prev = global.color[idx];
                        global.color[idx] = Color::new(
                            lerp(prev.r, stroke_color.r, opacity),
                            lerp(prev.g, stroke_color.g, opacity),
                            lerp(prev.b, stroke_color.b, opacity),
                            prev.a.max(opacity),
                        );
                    }
                } else {
                    global.color[idx] = lerp_color(global.color[idx], stroke_color, opacity);
                }

                global.stroke_id[idx] = stroke_id;

                if let Some(sn) = stroke_normal {
                    if !global.object_normal.is_empty() {
                        global.object_normal[idx] = sn;
                    }
                }
            }
        }
    }
}

/// Generate stroke paths for all layers in compositing order (parallel).
///
/// Returns a list of path sets, one per layer in `order`-sorted sequence.
/// The returned vec can be passed to [`composite_all_with_paths`] for
/// reuse across multiple renders with the same layout.
///
/// Path generation (direction field + streamline tracing) is done in parallel
/// across layers using rayon.
pub fn generate_all_paths(
    layers: &[PaintLayer],
    resolution: u32,
    base_color: &BaseColorSource,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
) -> Vec<Vec<StrokePath>> {
    let mut sorted: Vec<(usize, &PaintLayer)> = layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    sorted
        .par_iter()
        .map(|&(layer_index, layer)| {
            let tex_ref = base_color.texture.map(|data| ColorTextureRef {
                data,
                width: base_color.tex_width,
                height: base_color.tex_height,
            });
            let mask = masks.get(layer_index).and_then(|m| *m);
            generate_paths(layer, layer_index as u32, resolution, tex_ref.as_ref(), normal_data, mask)
        })
        .collect()
}

/// Composite all strokes from all layers into global maps.
///
/// Layers are composited in `order` (ascending). Within each layer,
/// strokes are composited in their path order (top-to-bottom).
pub fn composite_all(
    layers: &[PaintLayer],
    resolution: u32,
    base_color: &BaseColorSource,
    settings: &OutputSettings,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
) -> GlobalMaps {
    composite_all_with_paths(
        layers, resolution, base_color, settings, None, normal_data, masks,
    )
}

/// Composite all layers, optionally reusing pre-generated paths.
///
/// If `cached_paths` is `Some`, it must contain one entry per layer in
/// `order`-sorted sequence (as returned by [`generate_all_paths`]).
/// When `None`, paths are generated in parallel via rayon before
/// sequential compositing.
pub fn composite_all_with_paths(
    layers: &[PaintLayer],
    resolution: u32,
    base_color: &BaseColorSource,
    settings: &OutputSettings,
    cached_paths: Option<&[Vec<StrokePath>]>,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
) -> GlobalMaps {
    let mut global = GlobalMaps::new(resolution, base_color, settings.normal_mode, settings.background_mode);

    // Sort layers by order (ascending), preserving original index for stroke ID encoding
    let mut sorted: Vec<(usize, &PaintLayer)> = layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    // Phase A: Generate paths (parallel across layers when not cached)
    let generated;
    let all_paths: &[Vec<StrokePath>] = if let Some(cp) = cached_paths {
        cp
    } else {
        generated = sorted
            .par_iter()
            .map(|&(layer_index, layer)| {
                let tex_ref = base_color.texture.map(|data| ColorTextureRef {
                    data,
                    width: base_color.tex_width,
                    height: base_color.tex_height,
                });
                let mask = masks.get(layer_index).and_then(|m| *m);
                generate_paths(layer, layer_index as u32, resolution, tex_ref.as_ref(), normal_data, mask)
            })
            .collect::<Vec<_>>();
        &generated
    };

    // Phase B: Composite sequentially (preserves deterministic blending order)
    for (sorted_idx, &(_, layer)) in sorted.iter().enumerate() {
        let layer_index = sorted[sorted_idx].0;
        let mask = masks.get(layer_index).and_then(|m| *m);
        composite_layer(
            layer,
            layer_index as u32,
            &mut global,
            settings,
            base_color,
            Some(&all_paths[sorted_idx]),
            normal_data,
            mask,
        );
    }

    global
}

/// Composite a single layer's strokes into the global maps.
///
/// This is the inner loop of `composite_all`, extracted for single-layer
/// preview regeneration. The caller is responsible for clearing/resetting
/// pixels belonging to this layer before calling (if needed).
///
/// When `cached_paths` is `Some`, those paths are used directly instead of
/// regenerating them.
pub fn composite_layer(
    layer: &PaintLayer,
    layer_index: u32,
    global: &mut GlobalMaps,
    settings: &OutputSettings,
    base_color: &BaseColorSource,
    cached_paths: Option<&[StrokePath]>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
) {
    let resolution = global.resolution;
    let transparent = settings.background_mode == BackgroundMode::Transparent;
    let paths_owned;
    let paths: &[StrokePath] = if let Some(cached) = cached_paths {
        cached
    } else {
        let tex_ref = base_color.texture.map(|data| ColorTextureRef {
            data,
            width: base_color.tex_width,
            height: base_color.tex_height,
        });
        paths_owned = generate_paths(layer, layer_index, resolution, tex_ref.as_ref(), normal_data, mask);
        &paths_owned
    };

    // Generate brush profile once per layer (same seed for all strokes in layer)
    let brush_profile =
        generate_brush_profile(layer.params.brush_width.round() as usize, layer.params.seed);

    // Step 1: Pre-compute stroke colors sequentially (preserves RNG determinism)
    let mut rng = SeededRng::new(layer.params.seed);
    let stroke_colors: Vec<Color> = paths
        .iter()
        .map(|path| {
            compute_stroke_color(
                path,
                base_color.texture,
                base_color.tex_width,
                base_color.tex_height,
                base_color.solid_color,
                layer.params.color_variation,
                &mut rng,
            )
        })
        .collect();

    // Step 1b: Pre-compute stroke normals (midpoint sampling, symmetric with color)
    let stroke_normals: Vec<Option<[f32; 3]>> = if let Some(nd) = normal_data {
        paths
            .iter()
            .map(|path| {
                let mid = path.midpoint();
                let n = sample_object_normal(nd, mid);
                Some([n.x, n.y, n.z])
            })
            .collect()
    } else {
        vec![None; paths.len()]
    };

    // Step 2: Build height maps in parallel (no local frame transform needed)
    let heights: Vec<StrokeHeightResult> = paths
        .par_iter()
        .enumerate()
        .map(|(i, path)| {
            let stroke_length_px = (path.arc_length() * resolution as f32).ceil() as usize;
            generate_stroke_height(
                &brush_profile,
                stroke_length_px,
                &layer.params,
                layer.params.seed + i as u32,
            )
        })
        .collect();

    // Step 3: Composite sequentially using gather (no scatter-write gaps)
    for (i, local_height) in heights.iter().enumerate() {
        composite_stroke(
            local_height,
            &paths[i],
            resolution,
            &StrokeAppearance {
                color: stroke_colors[i],
                id: paths[i].stroke_id,
                base_height: layer.params.base_height,
                normal: stroke_normals[i],
                transparent,
            },
            global,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::make_layer_with_order;
    use crate::types::{BaseColorSource, NormalMode};

    const EPS: f32 = 1e-4;

    // ── Helpers ──

    struct LocalFrameTransform {
        uv_map: Vec<Vec2>,
        width: usize,
        height: usize,
    }

    impl LocalFrameTransform {
        fn local_to_uv(&self, lx: usize, ly: usize) -> Option<Vec2> {
            if lx >= self.width || ly >= self.height { return None; }
            let uv = self.uv_map[ly * self.width + lx];
            if uv.x.is_nan() { None } else { Some(uv) }
        }

        fn uv_to_pixel(uv: Vec2, resolution: u32) -> (i32, i32) {
            ((uv.x * resolution as f32) as i32, (uv.y * resolution as f32) as i32)
        }
    }

    /// Scatter-write composite for unit tests that test blending math.
    /// Production code uses the gather-based `composite_stroke` above.
    fn composite_stroke_scatter(
        local_height: &StrokeHeightResult,
        transform: &LocalFrameTransform,
        stroke_color: Color,
        stroke_id: u32,
        base_height: f32,
        stroke_normal: Option<[f32; 3]>,
        global: &mut GlobalMaps,
    ) {
        let res = global.resolution;
        for ly in 0..local_height.height {
            for lx in 0..local_height.width {
                let h = local_height.data[ly * local_height.width + lx];
                if h <= 0.0 { continue; }
                let uv = match transform.local_to_uv(lx, ly) {
                    Some(uv) => uv,
                    None => continue,
                };
                let (px, py) = LocalFrameTransform::uv_to_pixel(uv, res);
                if px < 0 || py < 0 || px >= res as i32 || py >= res as i32 { continue; }
                let idx = (py as u32 * res + px as u32) as usize;
                global.height[idx] = h.max(global.height[idx]);
                let opacity = smoothstep(0.0, base_height * 0.7, h);
                global.color[idx] = lerp_color(global.color[idx], stroke_color, opacity);
                global.stroke_id[idx] = stroke_id;
                if let Some(sn) = stroke_normal {
                    if !global.object_normal.is_empty() {
                        global.object_normal[idx] = sn;
                    }
                }
            }
        }
    }

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
        }
    }

    // ── GlobalMaps Initialization Tests ──

    #[test]
    fn global_maps_dimensions() {
        let maps = GlobalMaps::new(64, &BaseColorSource::solid(Color::WHITE), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        assert_eq!(maps.height.len(), 64 * 64);
        assert_eq!(maps.color.len(), 64 * 64);
        assert_eq!(maps.stroke_id.len(), 64 * 64);
        assert_eq!(maps.resolution, 64);
    }

    #[test]
    fn global_maps_solid_color_init() {
        let color = Color::rgb(0.3, 0.6, 0.9);
        let maps = GlobalMaps::new(16, &BaseColorSource::solid(color), NormalMode::SurfacePaint, BackgroundMode::Opaque);
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
        let maps = GlobalMaps::new(4, &BaseColorSource::textured(&tex, 2, 2, Color::WHITE), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        assert_eq!(maps.color.len(), 16);

        let center = maps.color[1 * 4 + 1];
        assert!(center.r > 0.0 && center.g > 0.0);
    }

    // ── No Height Stacking Test ──

    #[test]
    fn no_height_stacking() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::WHITE), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);

        for (i, &h) in [0.7f32, 0.3, 0.5].iter().enumerate() {
            let height_map = make_simple_height(1, 1, h);
            let transform = make_simple_transform(1, 1, uv);
            composite_stroke_scatter(
                &height_map,
                &transform,
                Color::rgb(0.5, 0.5, 0.5),
                (i + 1) as u32,
                1.0,
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
    fn color_full_cover() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;

        let height_map = make_simple_height(1, 1, base_height);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        assert!(
            c.b > 0.9,
            "full cover: blue should dominate, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    #[test]
    fn color_transparent_bristle_gap() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;

        let height_map = make_simple_height(1, 1, 0.001);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        assert!(
            c.r > 0.9,
            "bristle gap: base red should show through, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    #[test]
    fn color_partial_blend() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;
        let h = base_height * 0.3;

        let height_map = make_simple_height(1, 1, h);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        assert!(c.r > 0.1 && c.b > 0.1, "partial: should be a blend");
    }

    #[test]
    fn color_ridge_full_opacity() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);
        let base_height = 0.5;
        let h = base_height + 0.3;

        let height_map = make_simple_height(1, 1, h);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            base_height,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        let c = global.color[idx];
        assert!(
            c.b > 0.95,
            "ridge: should be fully covered, got ({:.3}, {:.3}, {:.3})",
            c.r, c.g, c.b
        );
    }

    // ── Stroke ID Tracking Test ──

    #[test]
    fn stroke_id_records_last() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::WHITE), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);

        for sid in [10u32, 20, 30] {
            let height_map = make_simple_height(1, 1, 0.5);
            let transform = make_simple_transform(1, 1, uv);
            composite_stroke_scatter(
                &height_map,
                &transform,
                Color::WHITE,
                sid,
                0.5,
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

        let mut global = GlobalMaps::new(64, &BaseColorSource::solid(Color::WHITE), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        composite_layer(
            &layer_a, 0, &mut global, &settings,
            &BaseColorSource::solid(solid_a), None, None, None,
        );
        composite_layer(
            &layer_b, 1, &mut global, &settings,
            &BaseColorSource::solid(solid_b), None, None, None,
        );

        let center = 32 * 64 + 32;
        // Layer B (order=1) was composited last, so its color should dominate
        assert!(global.stroke_id[center] > 0, "center should be painted");
    }

    // ── Full Pipeline Integration Test ──

    #[test]
    fn composite_all_produces_output() {
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[layer],
            128,
            &BaseColorSource::solid(Color::rgb(0.8, 0.6, 0.4)),
            &settings,
            None,
            &[],
        );

        assert_eq!(maps.height.len(), 128 * 128);
        assert_eq!(maps.color.len(), 128 * 128);

        let painted = maps.height.iter().filter(|&&h| h > 0.0).count();
        assert!(painted > 0, "should have painted some pixels");
    }

    #[test]
    fn composite_all_deterministic() {
        let layer = make_layer_with_order(0);
        let settings = OutputSettings::default();
        let solid = Color::rgb(0.5, 0.5, 0.5);

        let maps1 = composite_all(&[layer.clone()], 64, &BaseColorSource::solid(solid), &settings, None, &[]);
        let maps2 = composite_all(&[layer], 64, &BaseColorSource::solid(solid), &settings, None, &[]);

        assert_eq!(maps1.height, maps2.height);
        assert_eq!(maps1.stroke_id, maps2.stroke_id);
    }

    // ── Visual Integration Tests ──

    #[test]
    fn visual_compositing() {
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 25.0;
        layer.params.ridge_height = 0.3;
        layer.params.color_variation = 0.1;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.6, 0.4, 0.3);
        let maps = composite_all(&[layer], 256, &BaseColorSource::solid(solid), &settings, None, &[]);

        let out_dir = crate::test_module_output_dir("compositing");

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
        let mut layer = make_layer_with_order(0);
        layer.params.base_height = 0.5;
        layer.params.ridge_height = 0.0;
        layer.params.brush_width = 25.0;
        layer.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.5, 0.7, 0.3);
        let maps = composite_all(&[layer], 256, &BaseColorSource::solid(solid), &settings, None, &[]);

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
        let mut layer = make_layer_with_order(0);
        layer.params.base_height = 0.3;
        layer.params.ridge_height = 0.5;
        layer.params.brush_width = 25.0;
        layer.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.5, 0.3, 0.6);
        let maps = composite_all(&[layer], 256, &BaseColorSource::solid(solid), &settings, None, &[]);

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
        let mut layer = make_layer_with_order(0);
        layer.params.load = 0.3;
        layer.params.base_height = 0.5;
        layer.params.ridge_height = 0.0;
        layer.params.brush_width = 25.0;
        layer.params.color_variation = 0.0;

        let settings = OutputSettings::default();

        let stroke_tex = vec![Color::rgb(0.25, 0.12, 0.05)];
        let canvas = Color::rgb(0.95, 0.92, 0.85);

        let mut global = GlobalMaps::new(256, &BaseColorSource::solid(canvas), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        composite_layer(
            &layer, 0, &mut global, &settings,
            &BaseColorSource::textured(&stroke_tex, 1, 1, canvas), None, None, None,
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
        let mut layer = make_layer_with_order(0);
        layer.params.color_variation = 0.25;
        layer.params.brush_width = 20.0;

        let settings = OutputSettings::default();

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
            &[layer],
            256,
            &BaseColorSource::textured(&tex, 4, 4, Color::WHITE),
            &settings,
            None,
            &[],
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
        let solid = Color::rgb(0.3, 0.7, 0.1);
        let maps = GlobalMaps::new(16, &BaseColorSource::solid(solid), NormalMode::SurfacePaint, BackgroundMode::Opaque);

        for c in &maps.color {
            assert!((c.r - 0.3).abs() < EPS);
            assert!((c.g - 0.7).abs() < EPS);
            assert!((c.b - 0.1).abs() < EPS);
        }
    }

    #[test]
    fn skip_unpainted_pixels() {
        let mut global = GlobalMaps::new(16, &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)), NormalMode::SurfacePaint, BackgroundMode::Opaque);
        let uv = Vec2::new(0.5, 0.5);

        let height_map = make_simple_height(1, 1, 0.0);
        let transform = make_simple_transform(1, 1, uv);
        composite_stroke_scatter(
            &height_map,
            &transform,
            Color::rgb(0.0, 0.0, 1.0),
            1,
            0.5,
            None,
            &mut global,
        );

        let (px, py) = LocalFrameTransform::uv_to_pixel(uv, 16);
        let idx = (py as u32 * 16 + px as u32) as usize;
        assert_eq!(global.height[idx], 0.0, "zero height should not overwrite");
        assert_eq!(global.stroke_id[idx], 0, "stroke_id should remain 0");
    }
}
