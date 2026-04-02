//! **Pipeline stage 4** — multi-layer compositing into unified global maps.
//!
//! Blends stroke height, color, gradient, and AO contributions from all visible
//! paint layers into a single [`GlobalMaps`] struct, respecting layer ordering and masks.

use std::collections::HashSet;

use glam::Vec2;
use log::{debug, info};
use rayon::prelude::*;

use crate::mesh::object_normal::{try_sample_object_normal, MeshNormalData};
use crate::mesh::stretch_map::StretchMap;
use crate::mesh::uv_mask::UvMask;
use crate::pipeline::path_placement::{generate_paths, PathContext};
use crate::pipeline::stroke_height::{generate_stroke_height, StrokeHeightResult};
use crate::types::{
    BackgroundMode, BaseColorSource, Color, LayerBaseColor, LayerBaseNormal,
    LayerCompositeSettings, NormalMode, OutputSettings, PaintLayer, StrokePath, TextureSource,
};
use crate::util::brush_profile::{generate_brush_profile, jitter_brush_profile};
use crate::util::math::{lerp, perpendicular, smoothstep};
use crate::util::rng::SeededRng;
use crate::util::stroke_color::ColorTextureRef;
use crate::util::stroke_color::{compute_stroke_color, sample_bilinear};

/// Density threshold at which a stroke pixel becomes fully opaque.
/// Below this, opacity ramps smoothly from 0 (via smoothstep).
const DENSITY_OPACITY_THRESHOLD: f32 = 0.7;

/// Minimum density for a stroke pixel to participate in compositing.
/// Below this threshold the pixel is too faint to be perceptible
/// (smoothstep(0, 0.7, 0.04) ≈ 1% opacity) and skipping it prevents
/// barely-visible edge pixels from stamping their flat-plane object normal
/// and color onto the canvas.
const DENSITY_MIN_THRESHOLD: f32 = 0.1;

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
    /// Stroke time: normalized stroke order within the layer (0.0–1.0).
    /// First stroke = 0.0, last stroke = 1.0. Uses min policy (first touch wins).
    pub stroke_time_order: Vec<f32>,
    /// Stroke time: arc-length progress within each stroke (0.0–1.0).
    /// Start of stroke = 0.0, end = 1.0.
    pub stroke_time_arc: Vec<f32>,
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

        let stroke_time_order = vec![0.0f32; size];
        let stroke_time_arc = vec![0.0f32; size];

        Self {
            height,
            color,
            stroke_id,
            object_normal,
            gradient_x,
            gradient_y,
            paint_load,
            stroke_time_order,
            stroke_time_arc,
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
    /// Stroke time: normalized stroke order within the layer (0.0–1.0).
    pub stroke_time_order: Vec<f32>,
    /// Stroke time: arc-length progress within each stroke (0.0–1.0).
    pub stroke_time_arc: Vec<f32>,
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
            stroke_time_order: vec![0.0f32; size],
            stroke_time_arc: vec![0.0f32; size],
            resolution,
        }
    }
}

/// Resolve a [`TextureSource`] into owned base color data for compositing.
///
/// Needs access to the mesh's material list for `MeshMaterial` variants.
pub fn resolve_base_color(
    source: &TextureSource,
    materials: &[crate::mesh::asset_io::MeshMaterialInfo],
) -> LayerBaseColor {
    use crate::types::{checkerboard_warning_texture, pixels_to_colors};

    match source {
        TextureSource::None => LayerBaseColor::solid(Color::WHITE),
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
                LayerBaseColor::solid(Color::WHITE)
            }
        }
        TextureSource::File(Some(tex)) => {
            if tex.pixels.is_empty() {
                LayerBaseColor::solid(Color::WHITE)
            } else {
                let colors = pixels_to_colors(&tex.pixels);
                LayerBaseColor {
                    solid_color: Color::WHITE,
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
    materials: &[crate::mesh::asset_io::MeshMaterialInfo],
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
    /// Per-pixel normal clamp: skip pixels whose mesh normal deviates too
    /// far from the stroke's reference normal. `None` = disabled.
    pub normal_break_threshold: Option<f32>,
    /// Normalized stroke order within the layer (0.0 = first, 1.0 = last).
    pub time_order: f32,
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
    let base_pad = 0.5 / res_f;

    // Pre-compute per-segment directions for junction padding.
    let seg_dirs: Vec<Vec2> = (0..points.len() - 1)
        .map(|i| {
            let seg = points[i + 1] - points[i];
            let len = seg.length();
            if len < 1e-8 {
                Vec2::ZERO
            } else {
                seg / len
            }
        })
        .collect();

    for seg_idx in 0..points.len() - 1 {
        let a = points[seg_idx];
        let b = points[seg_idx + 1];
        let seg = b - a;
        let seg_len = seg.length();
        if seg_len < 1e-8 {
            continue;
        }
        let seg_dir = seg_dirs[seg_idx];
        let normal = perpendicular(seg_dir);

        // Junction padding: base half-pixel plus extra proportional to the
        // angular change × brush half-width. At a junction, outer-edge pixels
        // of a wide brush can shift by half_lateral_uv * sin(angle) in the
        // longitudinal direction; padding by that amount closes the gap.
        let pad_lo = if seg_idx == 0 {
            0.0
        } else {
            let prev_dir = seg_dirs[seg_idx - 1];
            let sin_half =
                (1.0 - seg_dir.dot(prev_dir)).max(0.0).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
            base_pad + half_lateral_uv * sin_half
        };
        let pad_hi = if seg_idx + 1 >= seg_dirs.len() {
            base_pad
        } else {
            let next_dir = seg_dirs[seg_idx + 1];
            let sin_half =
                (1.0 - seg_dir.dot(next_dir)).max(0.0).sqrt() * std::f32::consts::FRAC_1_SQRT_2;
            base_pad + half_lateral_uv * sin_half
        };

        // Projection bounds along this segment
        let proj_lo = if seg_idx == 0 { 0.0 } else { -pad_lo };
        let proj_hi = seg_len + pad_hi;

        // Tight bounding box for this segment (expanded by max pad)
        let max_pad = pad_lo.max(pad_hi);
        let ext = Vec2::splat(half_lateral_uv + max_pad + 1.0 / res_f);
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
                if h <= DENSITY_MIN_THRESHOLD {
                    continue;
                }

                // Per-pixel normal clamp: skip this pixel if its mesh normal
                // deviates too far from the stroke's reference face normal.
                if let (Some(threshold), Some(nd), Some(sn)) = (
                    appearance.normal_break_threshold,
                    normal_data,
                    stroke_normal,
                ) {
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

                // Height: always max. Within a layer all paint is wet,
                // so the surface settles to the highest point.
                let new_h = h.max(prev_h);
                global.height[idx] = new_h;

                // Paint load and object normal: winner-takes-all by height.
                if prev_h <= 0.0 || h >= prev_h {
                    if !global.paint_load.is_empty() {
                        let r =
                            bilinear_sample(&local_height.remaining, local_w, local_h, lx_f, ly_f);
                        global.paint_load[idx] = r;
                    }

                    if let Some(sn) = stroke_normal {
                        if !global.object_normal.is_empty() {
                            global.object_normal[idx] = sn;
                        }
                    }
                }

                // Color: always subtractive mix when overlapping existing paint.
                // Within a layer, strokes are wet and pigments mix on contact.
                //
                // Internal representation is premultiplied alpha:
                //   stored.rgb = straight.rgb * alpha
                // This ensures a single compositing formula works for both
                // transparent and opaque backgrounds.
                let opacity = smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, h);
                let prev = global.color[idx];
                let prev_a = prev.a;

                // Un-premultiply previous color for subtractive mixing
                let prev_straight = if prev_a > 0.0 {
                    Color::rgb(prev.r / prev_a, prev.g / prev_a, prev.b / prev_a)
                } else {
                    prev
                };

                let blended = if !same_stroke && prev_h > 0.0 {
                    let mix_ratio = prev_h.min(1.0);
                    subtractive_mix(prev_straight, stroke_color, mix_ratio)
                } else {
                    stroke_color
                };

                // Premultiplied alpha compositing (Porter-Duff "over")
                let inv = 1.0 - opacity;
                global.color[idx] = Color::new(
                    blended.r * opacity + prev.r * inv,
                    blended.g * opacity + prev.g * inv,
                    blended.b * opacity + prev.b * inv,
                    opacity + prev_a * inv,
                );

                global.stroke_id[idx] = stroke_id;

                // Stroke time: min policy — first touch wins.
                // arc_t = progress along stroke (0.0 at start, 1.0 at end).
                let arc_t = t.clamp(0.0, 1.0);
                let order = appearance.time_order;
                if global.stroke_time_order[idx] == 0.0
                    || order < global.stroke_time_order[idx]
                    || (order == global.stroke_time_order[idx]
                        && arc_t < global.stroke_time_arc[idx])
                {
                    global.stroke_time_order[idx] = order;
                    global.stroke_time_arc[idx] = arc_t;
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
                .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
            let tex_ref = base.texture.map(|data| ColorTextureRef {
                data,
                width: base.tex_width,
                height: base.tex_height,
            });
            let mask = masks.get(layer_index).and_then(|m| *m);
            generate_paths(
                layer,
                layer_index as u32,
                &PathContext {
                    color_tex: tex_ref.as_ref(),
                    normal_data,
                    mask,
                    stretch_map,
                    ..Default::default()
                },
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
/// Convenience wrapper around [`composite_all_with_paths`] that generates
/// paths internally (no cache) and uses default dry values (1.0 per layer).
#[allow(clippy::too_many_arguments)]
pub fn composite_all(
    layers: &[PaintLayer],
    resolution: u32,
    base_colors: &[LayerBaseColor],
    settings: &OutputSettings,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
    stretch_map: Option<&StretchMap>,
    group_names: &[&str],
) -> GlobalMaps {
    let default_dry = vec![1.0_f32; layers.len()];
    composite_all_with_paths(
        layers,
        resolution,
        base_colors,
        settings,
        None,
        normal_data,
        masks,
        stretch_map,
        &default_dry,
        group_names,
    )
}

/// Composite all layers via independent per-layer rendering and merge.
///
/// Each layer is rendered into its own [`LayerMaps`] (via [`render_layer`]),
/// then all layers are combined with [`merge_layers`]. This matches the GUI
/// pipeline and ensures proper layer isolation: intra-layer strokes mix
/// wet-on-wet, while inter-layer blending respects `layer_dry`.
///
/// If `cached_paths` is `Some`, it must contain one entry per layer in
/// `order`-sorted sequence (as returned by [`generate_all_paths`]).
/// When `None`, paths are generated in parallel via rayon.
///
/// `layer_dry` controls per-layer dryness (parallel to `layers`).
/// dry=0 → wet surface (subtractive mix), dry=1 → dry surface (opaque over).
#[allow(clippy::too_many_arguments)]
pub fn composite_all_with_paths(
    layers: &[PaintLayer],
    resolution: u32,
    base_colors: &[LayerBaseColor],
    settings: &OutputSettings,
    cached_paths: Option<&[Vec<StrokePath>]>,
    normal_data: Option<&MeshNormalData>,
    masks: &[Option<&UvMask>],
    stretch_map: Option<&StretchMap>,
    layer_dry: &[f32],
    group_names: &[&str],
) -> GlobalMaps {
    info!(
        "Compositing {} layers at {}×{} (cache={})",
        layers.len(),
        resolution,
        resolution,
        cached_paths.is_some()
    );

    // Sort layers by order (ascending), preserving original index for stroke ID encoding
    let mut sorted: Vec<(usize, &PaintLayer)> = layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

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
                    .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
                let tex_ref = base.texture.map(|data| ColorTextureRef {
                    data,
                    width: base.tex_width,
                    height: base.tex_height,
                });
                let mask = masks.get(layer_index).and_then(|m| *m);
                generate_paths(
                    layer,
                    layer_index as u32,
                    &PathContext {
                        color_tex: tex_ref.as_ref(),
                        normal_data,
                        mask,
                        stretch_map,
                        ..Default::default()
                    },
                )
            })
            .collect::<Vec<_>>();
        assign_unique_stroke_ids(&mut generated);
        &generated
    };

    // Phase B: Render each layer independently into its own LayerMaps
    let layer_maps: Vec<LayerMaps> = sorted
        .iter()
        .enumerate()
        .map(|(sorted_idx, &(layer_index, layer))| {
            let base = base_colors
                .get(layer_index)
                .map(|bc| bc.as_source())
                .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
            let mask = masks.get(layer_index).and_then(|m| *m);
            render_layer(
                layer,
                layer_index as u32,
                &base,
                Some(&all_paths[sorted_idx]),
                normal_data,
                mask,
                stretch_map,
                resolution,
            )
        })
        .collect();

    // Phase C: Finalize (fill base colors → merge → gradients)
    let layer_refs: Vec<&LayerMaps> = layer_maps.iter().collect();
    let sorted_base_colors: Vec<BaseColorSource<'_>> = sorted
        .iter()
        .map(|&(layer_index, _)| {
            base_colors
                .get(layer_index)
                .map(|bc| bc.as_source())
                .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE))
        })
        .collect();
    let sorted_masks: Vec<Option<&UvMask>> = sorted
        .iter()
        .map(|&(layer_index, _)| masks.get(layer_index).and_then(|m| *m))
        .collect();
    let sorted_dry: Vec<f32> = sorted
        .iter()
        .map(|&(layer_index, _)| layer_dry.get(layer_index).copied().unwrap_or(1.0))
        .collect();
    let sorted_groups: Vec<&str> = sorted
        .iter()
        .map(|&(layer_index, _)| group_names.get(layer_index).copied().unwrap_or("__all__"))
        .collect();

    let global = finalize_layers(
        &layer_refs,
        &sorted_base_colors,
        &sorted_masks,
        &sorted_dry,
        &sorted_groups,
        resolution,
        settings,
    );
    info!("Compositing complete");
    global
}

/// Compute gradient_x / gradient_y from a height map using 3×3 Sobel.
///
/// Plain Sobel with no boundary replacement — the caller is responsible
/// for smoothing the height map beforehand (e.g. via [`diffuse_height`]).
fn sobel_3x3(height: &[f32], gradient_x: &mut [f32], gradient_y: &mut [f32], resolution: u32) {
    let res = resolution as usize;
    for y in 0..res {
        for x in 0..res {
            let idx = y * res + x;
            if height[idx] <= 0.0 {
                continue;
            }

            let xl = x.saturating_sub(1);
            let xr = (x + 1).min(res - 1);
            let yl = y.saturating_sub(1);
            let yr = (y + 1).min(res - 1);

            let s = |sy: usize, sx: usize| height[sy * res + sx];

            let tl = s(yl, xl);
            let tc = s(yl, x);
            let tr = s(yl, xr);
            let ml = s(y, xl);
            let mr = s(y, xr);
            let bl = s(yr, xl);
            let bc = s(yr, x);
            let br = s(yr, xr);

            gradient_x[idx] = (tr + 2.0 * mr + br) - (tl + 2.0 * ml + bl);
            gradient_y[idx] = (bl + 2.0 * bc + br) - (tl + 2.0 * tc + tr);
        }
    }
}

/// Diffuse (blur) the height map to simulate paint spreading.
///
/// Low-viscosity paint spreads outward, producing soft edges and a smoother
/// surface. High-viscosity paint stays put, retaining sharp bristle ridges
/// and defined boundaries.
///
/// `spread` controls the blur radius: 0.0 = no diffusion, 1.0 = maximum.
/// Three passes of separable box blur approximate a Gaussian profile.
fn diffuse_height(height: &mut [f32], resolution: u32, spread: f32) {
    if spread <= 0.0 {
        return;
    }
    let radius = (spread * 5.0).ceil() as usize;
    if radius == 0 {
        return;
    }
    let res = resolution as usize;
    let mut tmp = vec![0.0f32; res * res];

    // Three passes of separable box blur ≈ Gaussian.
    // Uses a running sum for O(1) per pixel regardless of radius.
    for _ in 0..3 {
        // Horizontal pass: height → tmp
        for y in 0..res {
            let row = y * res;
            let mut sum = 0.0f32;
            // Seed the window [0, radius]
            for x in 0..=radius.min(res - 1) {
                sum += height[row + x];
            }
            for x in 0..res {
                let win_size = (x + radius).min(res - 1) - x.saturating_sub(radius) + 1;
                tmp[row + x] = sum / win_size as f32;
                // Slide: add right edge, remove left edge
                if x + radius + 1 < res {
                    sum += height[row + x + radius + 1];
                }
                if x >= radius {
                    sum -= height[row + x - radius];
                }
            }
        }
        // Vertical pass: tmp → height
        for x in 0..res {
            let mut sum = 0.0f32;
            for y in 0..=radius.min(res - 1) {
                sum += tmp[y * res + x];
            }
            for y in 0..res {
                let win_size = (y + radius).min(res - 1) - y.saturating_sub(radius) + 1;
                height[y * res + x] = sum / win_size as f32;
                if y + radius + 1 < res {
                    sum += tmp[(y + radius + 1) * res + x];
                }
                if y >= radius {
                    sum -= tmp[(y - radius) * res + x];
                }
            }
        }
    }
}

/// Compute gradient_x / gradient_y from the global height map using 3×3 Sobel.
///
/// Legacy entry point kept for the global pipeline. Delegates to `sobel_3x3`.
pub fn compute_height_gradients(global: &mut GlobalMaps) {
    sobel_3x3(
        &global.height,
        &mut global.gradient_x,
        &mut global.gradient_y,
        global.resolution,
    );
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
    _settings: &OutputSettings,
    base_color: &BaseColorSource,
    cached_paths: Option<&[StrokePath]>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
    stretch_map: Option<&StretchMap>,
) {
    let resolution = global.resolution;
    let paths_owned;
    let paths: &[StrokePath] = if let Some(cached) = cached_paths {
        cached
    } else {
        let tex_ref = base_color.texture.map(|data| ColorTextureRef {
            data,
            width: base_color.tex_width,
            height: base_color.tex_height,
        });
        paths_owned = generate_paths(
            layer,
            layer_index,
            &PathContext {
                color_tex: tex_ref.as_ref(),
                normal_data,
                mask,
                stretch_map,
                ..Default::default()
            },
        );
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
            generate_stroke_height(&jittered, stroke_length_px, &scaled, scaled.seed + i as u32)
        })
        .collect();

    // Step 3: Composite sequentially using gather (no scatter-write gaps)
    let total_strokes = heights.len();
    for (i, local_height) in heights.iter().enumerate() {
        let time_order = if total_strokes <= 1 {
            0.0
        } else {
            i as f32 / (total_strokes - 1) as f32
        };
        composite_stroke(
            local_height,
            &paths[i],
            resolution,
            &StrokeAppearance {
                color: stroke_colors[i],
                id: paths[i].stroke_id,
                normal: stroke_normals[i],
                normal_break_threshold: scaled.normal_break_threshold,
                time_order,
            },
            normal_data,
            global,
        );
    }
}

/// Render a single layer into independent [`LayerMaps`].
///
/// This is the per-layer rendering entry point for the independent-layer
/// pipeline. Unlike [`composite_layer()`], which writes directly into shared
/// [`GlobalMaps`], this function returns an isolated result with transparent
/// background. Intra-layer strokes always use max-height and subtractive
/// color mixing (all paint within a layer is considered wet).
///
/// The returned `LayerMaps` can be cached and reused when the layer's
/// parameters haven't changed, enabling fast re-compositing via
/// `merge_layers()`.
#[allow(clippy::too_many_arguments)]
pub fn render_layer(
    layer: &PaintLayer,
    layer_index: u32,
    base_color: &BaseColorSource,
    cached_paths: Option<&[StrokePath]>,
    normal_data: Option<&MeshNormalData>,
    mask: Option<&UvMask>,
    stretch_map: Option<&StretchMap>,
    resolution: u32,
) -> LayerMaps {
    debug!(
        "Rendering layer {} at {}×{} resolution",
        layer_index, resolution, resolution
    );
    // Use GlobalMaps internally with DepictedForm (all fields allocated) and
    // Transparent background, then move the buffers into LayerMaps.
    let dummy_base = BaseColorSource::solid(Color::new(0.0, 0.0, 0.0, 0.0));
    let mut global = GlobalMaps::new(
        resolution,
        &dummy_base,
        NormalMode::DepictedForm,
        BackgroundMode::Transparent,
    );

    let settings = OutputSettings {
        background_mode: BackgroundMode::Transparent,
        normal_mode: NormalMode::DepictedForm,
        ..OutputSettings::default()
    };

    composite_layer(
        layer,
        layer_index,
        &mut global,
        &settings,
        base_color,
        cached_paths,
        normal_data,
        mask,
        stretch_map,
    );

    // Diffuse height based on viscosity: low viscosity → more spreading.
    let spread = 1.0 - layer.params.viscosity;
    diffuse_height(&mut global.height, resolution, spread);

    // Per-layer Sobel so that per-layer exports get valid gradients.
    // finalize_layers also runs Sobel on the final merged height.
    sobel_3x3(
        &global.height,
        &mut global.gradient_x,
        &mut global.gradient_y,
        resolution,
    );

    LayerMaps {
        height: global.height,
        color: global.color,
        stroke_id: global.stroke_id,
        object_normal: global.object_normal,
        gradient_x: global.gradient_x,
        gradient_y: global.gradient_y,
        paint_load: global.paint_load,
        stroke_time_order: global.stroke_time_order,
        stroke_time_arc: global.stroke_time_arc,
        resolution,
    }
}

/// Finalize independently rendered layers into a single [`GlobalMaps`].
///
/// This is the shared "Phase C" used by both the CLI (`composite_all_with_paths`)
/// and the GUI (`generation.rs`). It performs:
/// 1. [`GlobalMaps`] initialization
/// 2. Per-layer base color fill via [`fill_base_color_region`]
/// 3. Layer merge via [`merge_layers`]
/// 4. Height gradient computation via [`compute_height_gradients`]
///
/// All slices (`layer_maps`, `base_colors`, `masks`, `layer_dry`,
/// `group_names`) must be in compositing order (sorted by layer `order`).
///
/// Base color fill uses a 2-pass strategy so that specific vertex-group
/// colors always override the `__all__` backdrop within their masked
/// region, regardless of layer order:
///   - Pass 1: fill the full canvas with the lowest `__all__` layer's base color.
///   - Pass 2: fill each specific group's masked region with its lowest layer's
///     base color, overriding `__all__`.
///
/// Pass `&[]` for `group_names` to treat every layer as `__all__`.
pub fn finalize_layers(
    layer_maps: &[&LayerMaps],
    base_colors: &[BaseColorSource<'_>],
    masks: &[Option<&UvMask>],
    layer_dry: &[f32],
    group_names: &[&str],
    resolution: u32,
    settings: &OutputSettings,
) -> GlobalMaps {
    let default_base = BaseColorSource::solid(Color::WHITE);
    let mut global = GlobalMaps::new(
        resolution,
        &default_base,
        settings.normal_mode,
        settings.background_mode,
    );

    debug_assert!(
        group_names.is_empty() || group_names.len() == layer_maps.len(),
        "group_names length ({}) must match layer_maps length ({}) or be empty",
        group_names.len(),
        layer_maps.len(),
    );

    if settings.background_mode != BackgroundMode::Transparent {
        let mut filled_groups: HashSet<&str> = HashSet::new();

        // Pass 1: Fill __all__ base color first (full canvas backdrop)
        for (i, bc) in base_colors.iter().enumerate() {
            let group = group_names.get(i).copied().unwrap_or("__all__");
            if group != "__all__" || !filled_groups.insert(group) {
                continue;
            }
            debug_assert!(
                masks.get(i).and_then(|m| *m).is_none(),
                "__all__ layer at index {} has a mask — convention violated",
                i,
            );
            fill_base_color_region(&mut global, bc, None);
        }

        // Pass 2: Fill specific group base colors (override __all__ within masked regions)
        for (i, bc) in base_colors.iter().enumerate() {
            let group = group_names.get(i).copied().unwrap_or("__all__");
            if group == "__all__" || !filled_groups.insert(group) {
                continue;
            }
            let mask = masks.get(i).and_then(|m| *m);
            fill_base_color_region(&mut global, bc, mask);
        }
    }

    let layer_settings = vec![LayerCompositeSettings::default(); layer_maps.len()];
    merge_layers(layer_maps, layer_dry, &layer_settings, &mut global);
    compute_height_gradients(&mut global);
    global
}

/// Merge independently rendered [`LayerMaps`] into a [`GlobalMaps`].
///
/// Layers are applied in slice order (index 0 = bottom). The caller should
/// initialize `global` with base colors and background mode before calling
/// this function — typically via [`GlobalMaps::new()`] and
/// [`fill_base_color_region()`].
///
/// `layer_dry` controls how dry the surface below was when each layer was
/// painted (parallel to `layers`). dry=0 → painted on wet surface
/// (subtractive mix + max height), dry=1 → painted on dry surface
/// (opaque over + height accumulation).
///
/// After merging, call [`compute_height_gradients()`] on the result to
/// populate gradient fields (Sobel runs on the final merged height map).
pub fn merge_layers(
    layers: &[&LayerMaps],
    layer_dry: &[f32],
    settings: &[LayerCompositeSettings],
    global: &mut GlobalMaps,
) {
    debug!("Merging {} layers", layers.len());
    let size = (global.resolution * global.resolution) as usize;

    for (i, layer) in layers.iter().enumerate() {
        debug_assert_eq!(layer.resolution, global.resolution);
        let layer_opacity = settings.get(i).map_or(1.0, |s| s.opacity);
        let dry = layer_dry.get(i).copied().unwrap_or(1.0);
        let wet = 1.0 - dry;

        for idx in 0..size {
            let h_above = layer.height[idx];
            if h_above <= 0.0 {
                continue;
            }

            let h_below = global.height[idx];

            // ── Height ──
            // dry=0 (wet): max (paint displaces fluid below)
            // dry=1: accumulate (paint stacks on solid)
            let new_h = if h_below <= 0.0 {
                h_above
            } else {
                let max_h = h_above.max(h_below);
                let sum_h = h_above + h_below;
                lerp(max_h, sum_h, dry)
            };

            // Paint load and object normal: winner-takes-all by height
            if h_below <= 0.0 || h_above >= h_below {
                if !global.paint_load.is_empty() {
                    global.paint_load[idx] = layer.paint_load[idx];
                }
                if !global.object_normal.is_empty() {
                    global.object_normal[idx] = layer.object_normal[idx];
                }
            }

            global.height[idx] = new_h;

            // ── Color ──
            // Skip color compositing for diffusion-halo pixels (height > 0
            // from blur but never actually painted). These have alpha = 0
            // and would otherwise blend as black, creating a dark fringe.
            let above_pm = layer.color[idx];
            let above_a = above_pm.a;
            if above_a <= 0.0 {
                continue;
            }

            // dry=0 (wet): subtractive mix (pigments blend on wet surface)
            // dry=1: opaque over (paint covers dried surface)
            //
            // Both layer and global colors are premultiplied alpha.
            // Un-premultiply for subtractive mixing, then composite back.
            let above_straight = Color::rgb(
                above_pm.r / above_a,
                above_pm.g / above_a,
                above_pm.b / above_a,
            );

            let prev = global.color[idx];
            let prev_a = prev.a;
            let prev_straight = if prev_a > 0.0 {
                Color::rgb(prev.r / prev_a, prev.g / prev_a, prev.b / prev_a)
            } else {
                prev
            };

            let blended = if h_below > 0.0 && wet > 0.0 {
                let mix_ratio = wet * h_below.min(1.0);
                subtractive_mix(prev_straight, above_straight, mix_ratio)
            } else {
                above_straight
            };

            let opacity = smoothstep(0.0, DENSITY_OPACITY_THRESHOLD, h_above) * layer_opacity;

            // Premultiplied alpha compositing (Porter-Duff "over")
            let inv = 1.0 - opacity;
            global.color[idx] = Color::new(
                blended.r * opacity + prev.r * inv,
                blended.g * opacity + prev.g * inv,
                blended.b * opacity + prev.b * inv,
                opacity + prev_a * inv,
            );

            global.stroke_id[idx] = layer.stroke_id[idx];

            // Stroke time: propagate from winning layer
            global.stroke_time_order[idx] = layer.stroke_time_order[idx];
            global.stroke_time_arc[idx] = layer.stroke_time_arc[idx];
        }
    }
}

#[cfg(test)]
mod tests {
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

        let base = [LayerBaseColor::solid(solid)];
        let maps1 = composite_all(
            std::slice::from_ref(&layer),
            64,
            &base,
            &settings,
            None,
            &[],
            None,
            &[],
        );
        let maps2 = composite_all(&[layer], 64, &base, &settings, None, &[], None, &[]);

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
        let solo_a = composite_all(
            &[layer_a.clone()],
            test_res,
            &[LayerBaseColor::solid(red)],
            &settings,
            None,
            &[],
            None,
            &[],
        );
        let solo_b = composite_all(
            &[layer_b.clone()],
            test_res,
            &[LayerBaseColor::solid(blue)],
            &settings,
            None,
            &[],
            None,
            &[],
        );

        // Render both together (dry=1.0, opaque stacking)
        let combined = composite_all(
            &[layer_a, layer_b],
            test_res,
            &base,
            &settings,
            None,
            &[],
            None,
            &[],
        );

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

    /// Verify that composite_all is deterministic across runs.
    #[test]
    fn multi_layer_deterministic() {
        let mut layer_a = make_layer_with_order(0);
        layer_a.params.brush_width = 20.0;
        let mut layer_b = make_layer_with_order(1);
        layer_b.params.brush_width = 20.0;

        let settings = OutputSettings::default();
        let base = vec![
            LayerBaseColor::solid(Color::rgb(1.0, 0.0, 0.0)),
            LayerBaseColor::solid(Color::rgb(0.0, 0.0, 1.0)),
        ];

        let maps1 = composite_all(
            &[layer_a.clone(), layer_b.clone()],
            64,
            &base,
            &settings,
            None,
            &[],
            None,
            &[],
        );
        let maps2 = composite_all(
            &[layer_a, layer_b],
            64,
            &base,
            &settings,
            None,
            &[],
            None,
            &[],
        );

        assert_eq!(maps1.height, maps2.height);
        assert_eq!(maps1.stroke_id, maps2.stroke_id);
        for (a, b) in maps1.color.iter().zip(&maps2.color) {
            assert!(
                (a.r - b.r).abs() < EPS
                    && (a.g - b.g).abs() < EPS
                    && (a.b - b.b).abs() < EPS
                    && (a.a - b.a).abs() < EPS,
                "color should be deterministic"
            );
        }
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
            &[],
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

        // Two empty LayerMaps (no paint — we only test base color fill)
        let empty_layer = LayerMaps {
            height: vec![0.0; size],
            color: vec![Color::new(0.0, 0.0, 0.0, 0.0); size],
            stroke_id: vec![0; size],
            object_normal: vec![[0.0; 3]; size],
            gradient_x: vec![0.0; size],
            gradient_y: vec![0.0; size],
            paint_load: vec![0.0; size],
            stroke_time_order: vec![0.0; size],
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
        let masks: Vec<Option<&UvMask>> = vec![Some(&mask), None];
        let group_names: Vec<&str> = vec!["arm", "__all__"];

        let global = finalize_layers(
            &layer_refs,
            &base_colors,
            &masks,
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
            stroke_time_order: vec![0.0; size],
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
        let masks: Vec<Option<&UvMask>> = vec![None, None];
        let group_names: Vec<&str> = vec!["__all__", "__all__"];

        let global = finalize_layers(
            &layer_refs,
            &base_colors,
            &masks,
            &[1.0, 1.0],
            &group_names,
            res,
            &settings,
        );

        let idx = (2 * res + 2) as usize;
        assert!(
            (global.color[idx].g - green.g).abs() < EPS
                && (global.color[idx].r - green.r).abs() < EPS,
            "should use first __all__ layer's base color (green), got {:?}",
            global.color[idx],
        );
    }
}
