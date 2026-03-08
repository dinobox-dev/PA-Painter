//! **Pipeline stage 4** — multi-layer compositing into unified global maps.
//!
//! Blends stroke height, color, gradient, and AO contributions from all visible
//! paint layers into a single [`GlobalMaps`] struct, respecting layer ordering and masks.

use glam::Vec2;
use rayon::prelude::*;

use crate::brush_profile::{generate_brush_profile, jitter_brush_profile};
use crate::math::{lerp, lerp_color, perpendicular, smoothstep};
use crate::object_normal::{try_sample_object_normal, MeshNormalData};
use crate::path_placement::generate_paths;
use crate::rng::SeededRng;
use crate::stretch_map::StretchMap;
use crate::stroke_color::ColorTextureRef;
use crate::stroke_color::{compute_stroke_color, sample_bilinear};
use crate::stroke_height::{generate_stroke_height, StrokeHeightResult};
use crate::types::{
    BackgroundMode, BaseColorSource, Color, LayerBaseColor, LayerBaseNormal, NormalMode,
    OutputSettings, PaintLayer, StrokePath, TextureSource,
};
use crate::uv_mask::UvMask;

/// Density threshold at which a stroke pixel becomes fully opaque.
/// Below this, opacity ramps smoothly from 0 (via smoothstep).
const DENSITY_OPACITY_THRESHOLD: f32 = 0.7;

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
    /// Paint detail gradient (X component in UV space), composited per-stroke.
    pub gradient_x: Vec<f32>,
    /// Paint detail gradient (Y component in UV space), composited per-stroke.
    pub gradient_y: Vec<f32>,
    /// Paint remaining (load depletion) per pixel. Tracks the winning stroke's
    /// `remaining` value, used by DepictedForm to fade the flat-plane normal
    /// based on actual paint thickness rather than pressure-driven density.
    /// Empty in SurfacePaint mode.
    pub paint_load: Vec<f32>,
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
                    colors.push(sample_bilinear(
                        tex,
                        base_color.tex_width,
                        base_color.tex_height,
                        uv,
                    ));
                }
            }
            colors
        } else {
            vec![base_color.solid_color; size]
        };

        let gradient_x = vec![0.0f32; size];
        let gradient_y = vec![0.0f32; size];

        let paint_load = if normal_mode == NormalMode::DepictedForm {
            vec![0.0f32; size]
        } else {
            Vec::new()
        };

        Self {
            height,
            color,
            stroke_id,
            object_normal,
            gradient_x,
            gradient_y,
            paint_load,
            resolution,
        }
    }
}

/// Per-layer compositing buffers in UV space.
///
/// Contains the same pixel-level data as [`GlobalMaps`], but for a single
/// layer rendered in isolation (transparent background). Produced by
/// [`render_layer()`] and consumed by [`merge_layers()`].
pub struct LayerMaps {
    /// Height map. 0.0 = no paint. Row-major, size = resolution².
    pub height: Vec<f32>,
    /// Color map with alpha = density. Transparent where unpainted.
    pub color: Vec<Color>,
    /// Stroke ID map. 0 = no stroke.
    pub stroke_id: Vec<u32>,
    /// Object-space normal per pixel (composited from strokes).
    pub object_normal: Vec<[f32; 3]>,
    /// Paint detail gradient (X component), composited per-stroke.
    pub gradient_x: Vec<f32>,
    /// Paint detail gradient (Y component), composited per-stroke.
    pub gradient_y: Vec<f32>,
    /// Paint remaining (load depletion) per pixel.
    pub paint_load: Vec<f32>,
    pub resolution: u32,
}

impl LayerMaps {
    /// Create empty layer maps with transparent background.
    pub fn new(resolution: u32) -> Self {
        let size = (resolution * resolution) as usize;
        Self {
            height: vec![0.0; size],
            color: vec![Color::new(0.0, 0.0, 0.0, 0.0); size],
            stroke_id: vec![0u32; size],
            object_normal: vec![[0.0f32; 3]; size],
            gradient_x: vec![0.0f32; size],
            gradient_y: vec![0.0f32; size],
            paint_load: vec![0.0f32; size],
            resolution,
        }
    }
}

/// Resolve a [`TextureSource`] into owned base color data for compositing.
///
/// Needs access to the mesh's material list for `MeshMaterial` variants.
pub fn resolve_base_color(
    source: &TextureSource,
    materials: &[crate::asset_io::MeshMaterialInfo],
) -> LayerBaseColor {
    use crate::types::{checkerboard_warning_texture, pixels_to_colors};

    match source {
        TextureSource::None => LayerBaseColor::solid(Color::rgb(0.5, 0.5, 0.5)),
        TextureSource::Solid(rgb) => LayerBaseColor::solid(Color::rgb(rgb[0], rgb[1], rgb[2])),
        TextureSource::MeshMaterial(idx) => {
            if let Some(mat) = materials.get(*idx) {
                if let Some(ref tex) = mat.base_color_texture {
                    let colors = pixels_to_colors(&tex.pixels);
                    LayerBaseColor {
                        solid_color: Color::from(mat.base_color_factor),
                        texture: Some(colors),
                        tex_width: tex.width,
                        tex_height: tex.height,
                    }
                } else {
                    let f = mat.base_color_factor;
                    LayerBaseColor::solid(Color::rgb(f[0], f[1], f[2]))
                }
            } else {
                LayerBaseColor::solid(Color::rgb(0.5, 0.5, 0.5))
            }
        }
        TextureSource::File(Some(tex)) => {
            if tex.pixels.is_empty() {
                LayerBaseColor::solid(Color::rgb(0.5, 0.5, 0.5))
            } else {
                let colors = pixels_to_colors(&tex.pixels);
                LayerBaseColor {
                    solid_color: Color::rgb(0.5, 0.5, 0.5),
                    texture: Some(colors),
                    tex_width: tex.width,
                    tex_height: tex.height,
                }
            }
        }
        TextureSource::File(None) => {
            let cb = checkerboard_warning_texture();
            let colors = pixels_to_colors(&cb.pixels);
            LayerBaseColor {
                solid_color: Color::rgb(1.0, 0.0, 1.0),
                texture: Some(colors),
                tex_width: cb.width,
                tex_height: cb.height,
            }
        }
    }
}

/// Resolve a [`TextureSource`] into owned base normal data for UDN blending.
pub fn resolve_base_normal(
    source: &TextureSource,
    materials: &[crate::asset_io::MeshMaterialInfo],
) -> LayerBaseNormal {
    match source {
        TextureSource::None | TextureSource::Solid(_) => LayerBaseNormal::none(),
        TextureSource::MeshMaterial(idx) => {
            if let Some(mat) = materials.get(*idx) {
                if let Some(ref tex) = mat.normal_texture {
                    LayerBaseNormal {
                        pixels: Some(tex.pixels.clone()),
                        width: tex.width,
                        height: tex.height,
                    }
                } else {
                    LayerBaseNormal::none()
                }
            } else {
                LayerBaseNormal::none()
            }
        }
        TextureSource::File(Some(tex)) => {
            if tex.pixels.is_empty() {
                LayerBaseNormal::none()
            } else {
                LayerBaseNormal {
                    pixels: Some(tex.pixels.as_ref().clone()),
                    width: tex.width,
                    height: tex.height,
                }
            }
        }
        TextureSource::File(None) => LayerBaseNormal::none(),
    }
}

/// Fill a region of the color map with a per-layer base color.
///
/// When `mask` is `None`, fills the entire map.
/// Only call in [`BackgroundMode::Opaque`] — in Transparent mode the canvas stays clear.
pub fn fill_base_color_region(
    global: &mut GlobalMaps,
    base_color: &BaseColorSource,
    mask: Option<&UvMask>,
) {
    let res = global.resolution;
    let res_f = res as f32;
    for py in 0..res {
        for px in 0..res {
            if let Some(m) = mask {
                let uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);
                if !m.sample(uv) {
                    continue;
                }
            }
            let idx = (py * res + px) as usize;
            global.color[idx] = if let Some(tex) = base_color.texture {
                let uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);
                sample_bilinear(tex, base_color.tex_width, base_color.tex_height, uv)
            } else {
                base_color.solid_color
            };
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

    h00 * (1.0 - sx) * (1.0 - sy) + h10 * sx * (1.0 - sy) + h01 * (1.0 - sx) * sy + h11 * sx * sy
}

/// Per-stroke appearance parameters for compositing.
pub struct StrokeAppearance {
    pub color: Color,
    pub id: u32,
    pub normal: Option<[f32; 3]>,
    pub transparent: bool,
    /// Per-pixel normal clamp: skip pixels whose mesh normal deviates too
    /// far from the stroke's reference normal. `None` = disabled.
    pub normal_break_threshold: Option<f32>,
    /// Wet-on-wet subtractive mixing strength (0.0–1.0).
    pub wet_on_wet: f32,
}

/// Kubelka-Munk approximate subtractive mix between two colors.
///
/// Uses geometric mean per channel as a cheap approximation of pigment mixing:
/// `sqrt(a * b)` produces darker intermediates (red + blue → dark purple),
/// which is the hallmark of subtractive (pigment) blending vs additive (light).
/// `t` controls how far toward the geometric mean we go from `b`.
fn subtractive_mix(a: Color, b: Color, t: f32) -> Color {
    let mix = |ca: f32, cb: f32| {
        let geo = (ca.max(0.001) * cb.max(0.001)).sqrt();
        lerp(cb, geo, t)
    };
    Color::rgb(mix(a.r, b.r), mix(a.g, b.g), mix(a.b, b.b))
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
    normal_data: Option<&MeshNormalData>,
    global: &mut GlobalMaps,
) {
    let stroke_color = appearance.color;
    let stroke_id = appearance.id;
    let stroke_normal = appearance.normal;
    let transparent = appearance.transparent;
    let res = resolution;
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
        let proj_lo = if seg_idx == 0 { 0.0 } else { -junction_pad };
        let proj_hi = seg_len + junction_pad;

        // Tight bounding box for this segment
        let ext = Vec2::splat(half_lateral_uv + 1.0 / res_f);
        let seg_min = a.min(b) - ext;
        let seg_max = a.max(b) + ext;

        let gx_min = ((seg_min.x * res_f).floor() as i32).max(0) as u32;
        let gy_min = ((seg_min.y * res_f).floor() as i32).max(0) as u32;
        let gx_max = ((seg_max.x * res_f).ceil() as i32).min(res as i32 - 1) as u32;
        let gy_max = ((seg_max.y * res_f).ceil() as i32).min(res as i32 - 1) as u32;

        let accum = accum_lens[seg_idx];

        for gy in gy_min..=gy_max {
            for gx in gx_min..=gx_max {
                let uv = Vec2::new((gx as f32 + 0.5) / res_f, (gy as f32 + 0.5) / res_f);

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
                let lx_f = t * stroke_length_px as f32;
                let ly_f = lateral * res_f + local_h as f32 / 2.0;

                if lx_f < 0.0
                    || lx_f >= (local_w - 1) as f32
                    || ly_f < 0.0
                    || ly_f >= (local_h - 1) as f32
                {
                    continue;
                }

                let h = bilinear_sample(&local_height.data, local_w, local_h, lx_f, ly_f);
                if h <= 0.0 {
                    continue;
                }

                // Per-pixel normal clamp: skip this pixel if its mesh normal
                // deviates too far from the stroke's reference face normal.
                if let (Some(threshold), Some(nd), Some(sn)) =
                    (appearance.normal_break_threshold, normal_data, stroke_normal)
                {
                    let uv = Vec2::new((gx as f32 + 0.5) / res_f, (gy as f32 + 0.5) / res_f);
                    if let Some(pixel_n) = try_sample_object_normal(nd, uv) {
                        let sn_vec = glam::Vec3::new(sn[0], sn[1], sn[2]);
                        if sn_vec.dot(pixel_n) < threshold {
                            continue;
                        }
                    }
                }

                let idx = (gy * res + gx) as usize;

                let prev_h = global.height[idx];
                let same_stroke = global.stroke_id[idx] == stroke_id;

                // Wet-on-wet height yielding: same formula as color mixing
                // (wet_on_wet × prev_h) so thick wet paint resists more.
                // Skip yielding when revisiting the same stroke (segment overlap).
                let new_h = if same_stroke {
                    h.max(prev_h)
                } else if h >= prev_h {
                    h
                } else {
                    let yield_ratio = appearance.wet_on_wet * prev_h.min(1.0);
                    lerp(prev_h, h, yield_ratio)
                };
                global.height[idx] = new_h;

                // Paint load and object normal: blend proportionally to
                // height ownership. Gradients are computed post-compositing
                // from the global height map (see compute_height_gradients).
                let blend_t = if prev_h <= 0.0 {
                    1.0
                } else if h >= prev_h {
                    1.0
                } else {
                    let span = prev_h - h;
                    if span > 1e-8 { (prev_h - new_h) / span } else { 0.0 }
                };

                if blend_t > 0.0 {
                    if !global.paint_load.is_empty() {
                        let r = bilinear_sample(
                            &local_height.remaining,
                            local_w,
                            local_h,
                            lx_f,
                            ly_f,
                        );
                        global.paint_load[idx] = lerp(global.paint_load[idx], r, blend_t);
                    }

                    if let Some(sn) = stroke_normal {
                        if !global.object_normal.is_empty() {
                            let prev_n = global.object_normal[idx];
                            global.object_normal[idx] = [
                                lerp(prev_n[0], sn[0], blend_t),
                                lerp(prev_n[1], sn[1], blend_t),
                                lerp(prev_n[2], sn[2], blend_t),
                            ];
                        }
                    }
                }

                let opacity = smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, h);
                if transparent {
                    if global.stroke_id[idx] == 0 {
                        // First paint on virgin pixel: set color directly
                        global.color[idx] =
                            Color::new(stroke_color.r, stroke_color.g, stroke_color.b, opacity);
                    } else {
                        // Over-paint: blend paint colors, alpha accumulates (Porter-Duff "over")
                        let prev = global.color[idx];
                        let blended = if !same_stroke && appearance.wet_on_wet > 0.0 && prev_h > 0.0
                        {
                            let mix_ratio = appearance.wet_on_wet * prev_h.min(1.0);
                            subtractive_mix(prev, stroke_color, mix_ratio)
                        } else {
                            stroke_color
                        };
                        global.color[idx] = Color::new(
                            lerp(prev.r, blended.r, opacity),
                            lerp(prev.g, blended.g, opacity),
                            lerp(prev.b, blended.b, opacity),
                            prev.a + opacity * (1.0 - prev.a),
                        );
                    }
                } else {
                    let existing = global.color[idx];
                    let blended = if !same_stroke && appearance.wet_on_wet > 0.0 && prev_h > 0.0 {
                        let mix_ratio = appearance.wet_on_wet * prev_h.min(1.0);
                        subtractive_mix(existing, stroke_color, mix_ratio)
                    } else {
                        stroke_color
                    };
                    global.color[idx] = lerp_color(existing, blended, opacity);
                }

                global.stroke_id[idx] = stroke_id;
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
    base_colors: &[LayerBaseColor],
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
    stretch_map: Option<&StretchMap>,
) -> Vec<Vec<StrokePath>> {
    let mut sorted: Vec<(usize, &PaintLayer)> = layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    let mut all_paths: Vec<Vec<StrokePath>> = sorted
        .par_iter()
        .map(|&(layer_index, layer)| {
            let base = base_colors
                .get(layer_index)
                .map(|bc| bc.as_source())
                .unwrap_or_else(|| BaseColorSource::solid(Color::rgb(0.5, 0.5, 0.5)));
            let tex_ref = base.texture.map(|data| ColorTextureRef {
                data,
                width: base.tex_width,
                height: base.tex_height,
            });
            let mask = masks.get(layer_index).and_then(|m| *m);
            generate_paths(
                layer,
                layer_index as u32,
                tex_ref.as_ref(),
                normal_data,
                mask,
                stretch_map,
            )
        })
        .collect();

    assign_unique_stroke_ids(&mut all_paths);
    all_paths
}

/// Assign globally unique 1-based stroke IDs across all layers.
/// 0 is reserved for "no stroke" in the compositing system.
fn assign_unique_stroke_ids(all_paths: &mut [Vec<StrokePath>]) {
    let mut next_id = 1u32;
    for layer_paths in all_paths.iter_mut() {
        for path in layer_paths.iter_mut() {
            path.stroke_id = next_id;
            next_id = next_id.wrapping_add(1);
        }
    }
}

/// Composite all strokes from all layers into global maps.
///
/// Layers are composited in `order` (ascending). Within each layer,
/// strokes are composited in their path order (top-to-bottom).
pub fn composite_all(
    layers: &[PaintLayer],
    resolution: u32,
    base_colors: &[LayerBaseColor],
    settings: &OutputSettings,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
    stretch_map: Option<&StretchMap>,
) -> GlobalMaps {
    composite_all_with_paths(
        layers,
        resolution,
        base_colors,
        settings,
        None,
        normal_data,
        masks,
        stretch_map,
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
    base_colors: &[LayerBaseColor],
    settings: &OutputSettings,
    cached_paths: Option<&[Vec<StrokePath>]>,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
    stretch_map: Option<&StretchMap>,
) -> GlobalMaps {
    // Initialize with neutral gray; per-layer base colors are painted into regions below.
    let default_base = BaseColorSource::solid(Color::rgb(0.5, 0.5, 0.5));
    let mut global = GlobalMaps::new(
        resolution,
        &default_base,
        settings.normal_mode,
        settings.background_mode,
    );

    // Sort layers by order (ascending), preserving original index for stroke ID encoding
    let mut sorted: Vec<(usize, &PaintLayer)> = layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    // Fill per-layer base color into each layer's mask region (in compositing order).
    if settings.background_mode != BackgroundMode::Transparent {
        for &(layer_index, _) in &sorted {
            if let Some(bc) = base_colors.get(layer_index) {
                let src = bc.as_source();
                let mask = masks.get(layer_index).and_then(|m| *m);
                fill_base_color_region(&mut global, &src, mask);
            }
        }
    }

    // Phase A: Generate paths (parallel across layers when not cached)
    let mut generated;
    let all_paths: &[Vec<StrokePath>] = if let Some(cp) = cached_paths {
        cp
    } else {
        generated = sorted
            .par_iter()
            .map(|&(layer_index, layer)| {
                let base = base_colors
                    .get(layer_index)
                    .map(|bc| bc.as_source())
                    .unwrap_or_else(|| BaseColorSource::solid(Color::rgb(0.5, 0.5, 0.5)));
                let tex_ref = base.texture.map(|data| ColorTextureRef {
                    data,
                    width: base.tex_width,
                    height: base.tex_height,
                });
                let mask = masks.get(layer_index).and_then(|m| *m);
                generate_paths(
                    layer,
                    layer_index as u32,
                    tex_ref.as_ref(),
                    normal_data,
                    mask,
                    stretch_map,
                )
            })
            .collect::<Vec<_>>();
        assign_unique_stroke_ids(&mut generated);
        &generated
    };

    // Phase B: Composite sequentially (preserves deterministic blending order)
    for (sorted_idx, &(layer_index, layer)) in sorted.iter().enumerate() {
        let base = base_colors
            .get(layer_index)
            .map(|bc| bc.as_source())
            .unwrap_or_else(|| BaseColorSource::solid(Color::rgb(0.5, 0.5, 0.5)));
        let mask = masks.get(layer_index).and_then(|m| *m);
        composite_layer(
            layer,
            layer_index as u32,
            &mut global,
            settings,
            &base,
            Some(&all_paths[sorted_idx]),
            normal_data,
            mask,
            stretch_map,
        );
    }

    compute_height_gradients(&mut global);
    global
}

/// Compute gradient_x / gradient_y from the global height map using 3×3 Sobel.
///
/// Replaces per-stroke gradient compositing.  Because both strokes share
/// nearly equal height at their competition boundary, the Sobel naturally
/// produces a smooth gradient there — no winner-take-all seam.
///
/// At stroke edges (paint → no-paint), unpainted neighbours are replaced
/// with the centre value so that the boundary does not produce a hard
/// gradient ridge.
pub fn compute_height_gradients(global: &mut GlobalMaps) {
    let res = global.resolution as usize;
    let h = &global.height;

    for y in 0..res {
        for x in 0..res {
            let idx = y * res + x;
            let c = h[idx];
            if c <= 0.0 {
                continue;
            }

            // Clamp-to-edge indices
            let xl = x.saturating_sub(1);
            let xr = (x + 1).min(res - 1);
            let yl = y.saturating_sub(1);
            let yr = (y + 1).min(res - 1);

            // Sample with boundary replacement: unpainted neighbours → centre
            let s = |sy: usize, sx: usize| {
                let v = h[sy * res + sx];
                if v <= 0.0 { c } else { v }
            };

            let tl = s(yl, xl);
            let tc = s(yl, x);
            let tr = s(yl, xr);
            let ml = s(y, xl);
            let mr = s(y, xr);
            let bl = s(yr, xl);
            let bc = s(yr, x);
            let br = s(yr, xr);

            global.gradient_x[idx] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            global.gradient_y[idx] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
}

/// Composite a single layer's strokes into the global maps.
///
/// This is the inner loop of `composite_all`, extracted for single-layer
/// preview regeneration. The caller is responsible for clearing/resetting
/// pixels belonging to this layer before calling (if needed).
///
/// When `cached_paths` is `Some`, those paths are used directly instead of
/// regenerating them.
#[allow(clippy::too_many_arguments)]
pub fn composite_layer(
    layer: &PaintLayer,
    layer_index: u32,
    global: &mut GlobalMaps,
    settings: &OutputSettings,
    base_color: &BaseColorSource,
    cached_paths: Option<&[StrokePath]>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
    stretch_map: Option<&StretchMap>,
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
        paths_owned = generate_paths(layer, layer_index, tex_ref.as_ref(), normal_data, mask, stretch_map);
        &paths_owned
    };

    // Scale pixel-unit params so painting proportions stay fixed across resolutions
    let scaled = layer.params.scaled_for_resolution(resolution);

    // Generate brush profile once per layer (same seed for all strokes in layer)
    let brush_profile = generate_brush_profile(scaled.brush_width.round() as usize, scaled.seed);

    // Step 1: Pre-compute stroke colors sequentially (preserves RNG determinism)
    let mut rng = SeededRng::new(scaled.seed);
    let stroke_colors: Vec<Color> = paths
        .iter()
        .map(|path| {
            compute_stroke_color(
                path,
                base_color.texture,
                base_color.tex_width,
                base_color.tex_height,
                base_color.solid_color,
                scaled.color_variation,
                &mut rng,
            )
        })
        .collect();

    // Step 1b: Pre-compute stroke normals (midpoint sampling, symmetric with color).
    // Try midpoint first; if it has no mesh coverage, search outward from the
    // midpoint segment toward the start, then toward the end.
    let stroke_normals: Vec<Option<[f32; 3]>> = if let Some(nd) = normal_data {
        paths
            .iter()
            .map(|path| {
                let mid = path.midpoint();
                if let Some(n) = try_sample_object_normal(nd, mid) {
                    return Some([n.x, n.y, n.z]);
                }
                let len = path.points.len();
                let mid_idx = len / 2;
                // Search midpoint → start
                for i in (0..mid_idx).rev() {
                    if let Some(n) = try_sample_object_normal(nd, path.points[i]) {
                        return Some([n.x, n.y, n.z]);
                    }
                }
                // Search midpoint → end
                for i in mid_idx..len {
                    if let Some(n) = try_sample_object_normal(nd, path.points[i]) {
                        return Some([n.x, n.y, n.z]);
                    }
                }
                None
            })
            .collect()
    } else {
        vec![None; paths.len()]
    };

    // Step 2: Build height maps in parallel
    let heights: Vec<StrokeHeightResult> = paths
        .par_iter()
        .enumerate()
        .map(|(i, path)| {
            // Use stretch-weighted arc length when available so that height
            // map resolution matches 3D stroke length, not UV length.
            let arc_len = match stretch_map {
                Some(sm) => sm.weighted_arc_length(&path.points),
                None => path.arc_length(),
            };
            let stroke_length_px = (arc_len * resolution as f32).ceil() as usize;
            let jittered = jitter_brush_profile(&brush_profile, scaled.seed + i as u32, 0.15);
            generate_stroke_height(
                &jittered,
                stroke_length_px,
                &scaled,
                scaled.seed + i as u32,
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
                normal: stroke_normals[i],
                transparent,
                normal_break_threshold: scaled.normal_break_threshold,
                wet_on_wet: scaled.wet_on_wet,
            },
            normal_data,
            global,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::make_layer_with_order;
    use crate::types::{BaseColorSource, LayerBaseColor, NormalMode};

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
    fn color_full_cover() {
        let mut global = GlobalMaps::new(
            16,
            &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
            NormalMode::SurfacePaint,
            BackgroundMode::Opaque,
        );
        let uv = Vec2::new(0.5, 0.5);

        let height_map = make_simple_height(1, 1, 0.8);
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
            c.b > 0.9,
            "full cover: blue should dominate, got ({:.3}, {:.3}, {:.3})",
            c.r,
            c.g,
            c.b
        );
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

    #[test]
    fn color_high_density_full_opacity() {
        let mut global = GlobalMaps::new(
            16,
            &BaseColorSource::solid(Color::rgb(1.0, 0.0, 0.0)),
            NormalMode::SurfacePaint,
            BackgroundMode::Opaque,
        );
        let uv = Vec2::new(0.5, 0.5);

        let height_map = make_simple_height(1, 1, 0.9);
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
            c.b > 0.95,
            "high density: should be fully covered, got ({:.3}, {:.3}, {:.3})",
            c.r,
            c.g,
            c.b
        );
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
            &layer_a,
            0,
            &mut global,
            &settings,
            &BaseColorSource::solid(solid_a),
            None,
            None,
            None,
            None,
        );
        composite_layer(
            &layer_b,
            1,
            &mut global,
            &settings,
            &BaseColorSource::solid(solid_b),
            None,
            None,
            None,
            None,
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
            &[LayerBaseColor::solid(Color::rgb(0.8, 0.6, 0.4))],
            &settings,
            None,
            &[],
            None,
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

        let base = [LayerBaseColor::solid(solid)];
        let maps1 = composite_all(
            std::slice::from_ref(&layer),
            64,
            &base,
            &settings,
            None,
            &[],
            None,
        );
        let maps2 = composite_all(
            &[layer],
            64,
            &base,
            &settings,
            None,
            &[],
            None,
        );

        assert_eq!(maps1.height, maps2.height);
        assert_eq!(maps1.stroke_id, maps2.stroke_id);
    }

    // ── Visual Integration Tests ──

    #[test]
    fn visual_compositing() {
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 25.0;
        layer.params.color_variation = 0.1;

        let settings = OutputSettings::default();

        let solid = Color::rgb(0.6, 0.4, 0.3);
        let maps = composite_all(
            &[layer],
            256,
            &[LayerBaseColor::solid(solid)],
            &settings,
            None,
            &[],
            None,
        );

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
            &layer,
            0,
            &mut global,
            &settings,
            &BaseColorSource::textured(&stroke_tex, 1, 1, canvas),
            None,
            None,
            None,
            None,
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

        let maps = composite_all(
            &[layer],
            256,
            &[LayerBaseColor {
                solid_color: Color::WHITE,
                texture: Some(tex),
                tex_width: 4,
                tex_height: 4,
            }],
            &settings,
            None,
            &[],
            None,
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
        image::save_buffer(&out_path, &color_pixels, 256, 256, image::ColorType::Rgb8)
            .expect("Failed to save");
        eprintln!("Wrote: {}", out_path.display());
    }

    #[test]
    fn zero_height_preserves_base() {
        let solid = Color::rgb(0.3, 0.7, 0.1);
        let maps = GlobalMaps::new(
            16,
            &BaseColorSource::solid(solid),
            NormalMode::SurfacePaint,
            BackgroundMode::Opaque,
        );

        for c in &maps.color {
            assert!((c.r - 0.3).abs() < EPS);
            assert!((c.g - 0.7).abs() < EPS);
            assert!((c.b - 0.1).abs() < EPS);
        }
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
    }
}
