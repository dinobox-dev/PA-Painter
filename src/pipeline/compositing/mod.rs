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
use crate::mesh::uv_mask::DistanceField;
use crate::pipeline::path_placement::{generate_paths, PathContext};
use crate::pipeline::stroke_height::{generate_stroke_height, StrokeHeightResult};
use crate::types::{
    BackgroundMode, BaseColorSource, Color, LayerBaseColor, LayerCompositeSettings, NormalMode,
    OutputSettings, PaintLayer, StrokePath,
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

        let stroke_time_order = vec![f32::MAX; size];
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
    /// Zero out all pixel data outside the clip distance of a UV island.
    ///
    /// Used after [`render_layer()`] to clip rasterization overflow back to
    /// the island boundary (with [`CLIP_DISTANCE_PX`] seam coverage).
    pub fn clip_to_dist_field(&mut self, df: &DistanceField) {
        let res = self.resolution;
        let res_f = res as f32;
        for py in 0..res {
            for px in 0..res {
                let uv = glam::Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);
                if !df.sample(uv, CLIP_DISTANCE_PX) {
                    let idx = (py * res + px) as usize;
                    self.height[idx] = 0.0;
                    self.color[idx] = Color::new(0.0, 0.0, 0.0, 0.0);
                    self.stroke_id[idx] = 0;
                    self.object_normal[idx] = [0.0; 3];
                    self.gradient_x[idx] = 0.0;
                    self.gradient_y[idx] = 0.0;
                    self.paint_load[idx] = 0.0;
                    self.stroke_time_order[idx] = f32::MAX;
                    self.stroke_time_arc[idx] = 0.0;
                }
            }
        }
    }

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
            stroke_time_order: vec![f32::MAX; size],
            stroke_time_arc: vec![0.0f32; size],
            resolution,
        }
    }
}

// ── Material resolution (TextureSource → base color / normal) ──
mod material_resolve;
pub use material_resolve::{resolve_base_color, resolve_base_normal};

/// Clip threshold (pixels) for distance-field-based mask checks.
/// Matches the legacy `dilate(2)` behaviour: island boundary + 2 px of seam cover.
pub const CLIP_DISTANCE_PX: f32 = 2.0;

/// Fill a region of the color map with a per-layer base color.
///
/// When `dist_field` is `None`, fills the entire map.
/// Only call in [`BackgroundMode::Opaque`] — in Transparent mode the canvas stays clear.
pub fn fill_base_color_region(
    global: &mut GlobalMaps,
    base_color: &BaseColorSource,
    dist_field: Option<&DistanceField>,
) {
    let res = global.resolution;
    let res_f = res as f32;
    for py in 0..res {
        for px in 0..res {
            if let Some(df) = dist_field {
                let uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);
                if !df.sample(uv, CLIP_DISTANCE_PX) {
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
                let lx_f = t * local_w as f32;
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
                if global.stroke_time_order[idx] == f32::MAX
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
/// The returned vec can be passed to [`composite_all`] for
/// reuse across multiple renders with the same layout.
///
/// Path generation (direction field + streamline tracing) is done in parallel
/// across layers using rayon.
pub fn generate_all_paths(
    layers: &[PaintLayer],
    base_colors: &[LayerBaseColor],
    normal_data: Option<&MeshNormalData>,
    dist_fields: &[Option<&DistanceField>],
    stretch_map: Option<&StretchMap>,
) -> Vec<Vec<StrokePath>> {
    debug_assert_eq!(
        base_colors.len(),
        layers.len(),
        "base_colors must be parallel to layers ({} vs {})",
        base_colors.len(),
        layers.len()
    );
    debug_assert!(
        dist_fields.is_empty() || dist_fields.len() == layers.len(),
        "dist_fields must be empty or parallel to layers ({} vs {})",
        dist_fields.len(),
        layers.len()
    );
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
            let df = dist_fields.get(layer_index).and_then(|m| *m);
            generate_paths(
                layer,
                layer_index as u32,
                &PathContext {
                    color_tex: tex_ref.as_ref(),
                    normal_data,
                    dist_field: df,
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

/// Input for [`composite_all`].
pub struct CompositeAllInput<'a> {
    pub layers: &'a [PaintLayer],
    pub resolution: u32,
    pub base_colors: &'a [LayerBaseColor],
    pub settings: &'a OutputSettings,
    /// Pre-generated paths (one `Vec<StrokePath>` per layer, order-sorted).
    /// When `None`, paths are generated internally via rayon.
    pub cached_paths: Option<&'a [Vec<StrokePath>]>,
    pub normal_data: Option<&'a MeshNormalData>,
    /// Per-layer distance fields derived from UV masks.
    pub dist_fields: &'a [Option<&'a DistanceField>],
    pub stretch_map: Option<&'a StretchMap>,
    /// Per-layer dryness, parallel to `layers`. dry=0 → wet, dry=1 → opaque over.
    /// Defaults to all 1.0 when empty.
    pub layer_dry: &'a [f32],
    pub group_names: &'a [&'a str],
}

impl<'a> CompositeAllInput<'a> {
    /// Create input with required fields; optional fields default to `None`/empty.
    pub fn new(
        layers: &'a [PaintLayer],
        resolution: u32,
        base_colors: &'a [LayerBaseColor],
        settings: &'a OutputSettings,
    ) -> Self {
        Self {
            layers,
            resolution,
            base_colors,
            settings,
            cached_paths: None,
            normal_data: None,
            dist_fields: &[],
            stretch_map: None,
            layer_dry: &[],
            group_names: &[],
        }
    }
}

/// Input for [`composite_layer`]. `global` is kept as a separate parameter
/// since it is the mutable compositing target, not an input.
pub struct CompositeLayerInput<'a> {
    pub layer: &'a PaintLayer,
    pub layer_index: u32,
    pub settings: &'a OutputSettings,
    pub base_color: &'a BaseColorSource<'a>,
    pub cached_paths: Option<&'a [StrokePath]>,
    pub normal_data: Option<&'a MeshNormalData>,
    pub dist_field: Option<&'a DistanceField>,
    pub stretch_map: Option<&'a StretchMap>,
}

/// Input for [`render_layer`].
pub struct RenderLayerInput<'a> {
    pub layer: &'a PaintLayer,
    pub layer_index: u32,
    pub base_color: &'a BaseColorSource<'a>,
    pub cached_paths: Option<&'a [StrokePath]>,
    pub normal_data: Option<&'a MeshNormalData>,
    pub dist_field: Option<&'a DistanceField>,
    pub stretch_map: Option<&'a StretchMap>,
    pub resolution: u32,
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
pub fn composite_all(input: &CompositeAllInput<'_>) -> GlobalMaps {
    let CompositeAllInput {
        layers,
        resolution,
        base_colors,
        settings,
        cached_paths,
        normal_data,
        dist_fields,
        stretch_map,
        layer_dry,
        group_names,
    } = *input;
    debug_assert_eq!(
        base_colors.len(),
        layers.len(),
        "base_colors must be parallel to layers ({} vs {})",
        base_colors.len(),
        layers.len()
    );
    debug_assert!(
        dist_fields.is_empty() || dist_fields.len() == layers.len(),
        "dist_fields must be empty or parallel to layers ({} vs {})",
        dist_fields.len(),
        layers.len()
    );
    debug_assert!(
        layer_dry.is_empty() || layer_dry.len() == layers.len(),
        "layer_dry must be empty or parallel to layers ({} vs {})",
        layer_dry.len(),
        layers.len()
    );
    debug_assert!(
        group_names.is_empty() || group_names.len() == layers.len(),
        "group_names must be empty or parallel to layers ({} vs {})",
        group_names.len(),
        layers.len()
    );
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
                let df = dist_fields.get(layer_index).and_then(|m| *m);
                generate_paths(
                    layer,
                    layer_index as u32,
                    &PathContext {
                        color_tex: tex_ref.as_ref(),
                        normal_data,
                        dist_field: df,
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
            let df = dist_fields.get(layer_index).and_then(|m| *m);
            let mut maps = render_layer(&RenderLayerInput {
                layer,
                layer_index: layer_index as u32,
                base_color: &base,
                cached_paths: Some(&all_paths[sorted_idx]),
                normal_data,
                dist_field: df,
                stretch_map,
                resolution,
            });
            if let Some(df) = df {
                maps.clip_to_dist_field(df);
            }
            maps
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
    let sorted_dfs: Vec<Option<&DistanceField>> = sorted
        .iter()
        .map(|&(layer_index, _)| dist_fields.get(layer_index).and_then(|m| *m))
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
        &sorted_dfs,
        &sorted_dry,
        &sorted_groups,
        resolution,
        settings,
    );
    info!("Compositing complete");
    global
}

// ── Height processing (diffusion + gradient) ──
mod height_processing;
pub use height_processing::compute_height_gradients;
use height_processing::{diffuse_height, sobel_3x3};

/// Composite a single layer's strokes into the global maps.
///
/// This is the inner loop of `composite_all`, extracted for single-layer
/// preview regeneration. The caller is responsible for clearing/resetting
/// pixels belonging to this layer before calling (if needed).
///
/// When `cached_paths` is `Some`, those paths are used directly instead of
/// regenerating them.
pub fn composite_layer(global: &mut GlobalMaps, input: &CompositeLayerInput<'_>) {
    let CompositeLayerInput {
        layer,
        layer_index,
        settings: _,
        base_color,
        cached_paths,
        normal_data,
        dist_field,
        stretch_map,
    } = *input;
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
                dist_field,
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
pub fn render_layer(input: &RenderLayerInput<'_>) -> LayerMaps {
    let RenderLayerInput {
        layer,
        layer_index,
        base_color,
        cached_paths,
        normal_data,
        dist_field,
        stretch_map,
        resolution,
    } = *input;
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
        &mut global,
        &CompositeLayerInput {
            layer,
            layer_index,
            settings: &settings,
            base_color,
            cached_paths,
            normal_data,
            dist_field,
            stretch_map,
        },
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
/// This is the shared "Phase C" used by both the CLI (`composite_all`)
/// and the GUI (`generation.rs`). It performs:
/// 1. [`GlobalMaps`] initialization
/// 2. Per-layer base color fill via [`fill_base_color_region`]
/// 3. Layer merge via [`merge_layers`]
/// 4. Height gradient computation via [`compute_height_gradients`]
///
/// All slices (`layer_maps`, `base_colors`, `dist_fields`, `layer_dry`,
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
    dist_fields: &[Option<&DistanceField>],
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
                dist_fields.get(i).and_then(|m| *m).is_none(),
                "__all__ layer at index {} has a dist_field — convention violated",
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
            let df = dist_fields.get(i).and_then(|m| *m);
            fill_base_color_region(&mut global, bc, df);
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
mod tests;
