//! **Pipeline stage 5** — final output map generation and export.
//!
//! Converts composited global maps into color, normal, height, and AO textures,
//! then writes them as PNG or OpenEXR files.

use std::path::Path;

use glam::Vec2;

use serde::{Deserialize, Serialize};

use crate::asset_io::linear_to_srgb;
use crate::compositing::{GlobalMaps, LayerMaps};
use crate::object_normal::MeshNormalData;
use crate::stroke_color::hsv_to_rgb;
use crate::math::smoothstep;
use crate::types::{BackgroundMode, Color, HsvColor, NormalMode, OutputSettings};
use crate::uv_mask::UvMask;

// ── Error Type ──

/// Error type for output operations.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),
    #[error("EXR error: {0}")]
    ExrError(String),
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

// ── Export Format ──

/// Output format selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    #[default]
    Png,
    Exr,
}

// ── Height Map Normalization ──

/// Normalize a height (density) map for export.
///
/// Density values are already in [0, 1] range (effective_density × remaining).
///
/// Returns a new Vec<f32> with values in [0, 1].
pub fn normalize_height_map(height: &[f32]) -> Vec<f32> {
    height.iter().map(|&h| h.clamp(0.0, 1.0)).collect()
}

// ── Normal Map Generation ──

/// Generate a tangent-space normal map from pre-composited gradients.
///
/// Gradients are computed per-stroke in local space and composited into
/// global UV space during `composite_stroke()`, so no Sobel pass is needed here.
///
/// Returns Vec<[f32; 3]> of normal vectors encoded as (R, G, B) where
/// flat = (0.5, 0.5, 1.0). Row-major, resolution x resolution.
pub fn generate_normal_map(
    gradient_x: &[f32],
    gradient_y: &[f32],
    resolution: u32,
    normal_strength: f32,
) -> Vec<[f32; 3]> {
    let size = (resolution * resolution) as usize;
    let mut normals = vec![[0.5f32, 0.5, 1.0]; size];

    for i in 0..size {
        let gx = gradient_x[i];
        let gy = gradient_y[i];

        let nx = -gx * normal_strength;
        let ny = -gy * normal_strength;
        let nz = 1.0f32;

        let len = (nx * nx + ny * ny + nz * nz).sqrt();
        normals[i] = [
            nx / len * 0.5 + 0.5,
            ny / len * 0.5 + 0.5,
            nz / len * 0.5 + 0.5,
        ];
    }

    normals
}

/// Generate a tangent-space normal map using object-space normals from mesh geometry,
/// perturbed by pre-composited paint gradients.
///
/// For each pixel:
/// 1. Read pre-composited gradient (gx, gy) from global gradient buffers.
/// 2. Look up the composited object-space normal N_obj, tangent T, and bitangent B.
/// 3. Blend N_obj toward N_geom at low-density pixels so the stroke normal
///    fades out in sync with the visible paint opacity.
/// 4. Perturb: `perturbed = normalize(N_blended + strength * (-gx * T + -gy * B))`
/// 5. Convert to tangent space: `ts = (perturbed·T, perturbed·B, perturbed·N_geom)`
/// 6. Encode to [0, 1].
pub fn generate_normal_map_depicted_form(
    gradient_x: &[f32],
    gradient_y: &[f32],
    normal_data: &MeshNormalData,
    global_object_normals: &[[f32; 3]],
    paint_load: &[f32],
    resolution: u32,
    normal_strength: f32,
) -> Vec<[f32; 3]> {
    let res = resolution;
    let nd_res = normal_data.resolution;
    let mut normals = vec![[0.5f32, 0.5, 1.0]; (res * res) as usize];

    for y in 0..res {
        for x in 0..res {
            let pixel_idx = (y * res + x) as usize;
            let gx = gradient_x[pixel_idx];
            let gy = gradient_y[pixel_idx];

            // Map from output resolution to normal_data resolution
            let nd_x = ((x as f32 + 0.5) / res as f32 * nd_res as f32).floor() as u32;
            let nd_y = ((y as f32 + 0.5) / res as f32 * nd_res as f32).floor() as u32;
            let nd_idx = (nd_y.min(nd_res - 1) * nd_res + nd_x.min(nd_res - 1)) as usize;

            let n_geom = normal_data.object_normals[nd_idx];
            let t = normal_data.tangents[nd_idx];
            let b = normal_data.bitangents[nd_idx];

            // If no mesh coverage here, fall back to SurfacePaint normal
            if n_geom.length_squared() < 0.5 {
                let nx = -gx * normal_strength;
                let ny = -gy * normal_strength;
                let nz = 1.0f32;
                let len = (nx * nx + ny * ny + nz * nz).sqrt();
                normals[pixel_idx] = [
                    nx / len * 0.5 + 0.5,
                    ny / len * 0.5 + 0.5,
                    nz / len * 0.5 + 0.5,
                ];
                continue;
            }

            // Use composited stroke normal if available, else use geometric normal.
            // Blend toward n_geom at low density so the flat-plane normal
            // fades in sync with visible paint opacity.
            let n_obj = if !global_object_normals.is_empty() {
                let sn = global_object_normals[pixel_idx];
                let v = glam::Vec3::new(sn[0], sn[1], sn[2]);
                if v.length_squared() > 0.5 {
                    let load = paint_load[pixel_idx];
                    let influence = smoothstep(0.0, 0.7, load);
                    n_geom.lerp(v, influence).normalize()
                } else {
                    n_geom
                }
            } else {
                n_geom
            };

            // Perturb the object-space normal by paint gradient
            let perturbed = (n_obj + normal_strength * (-gx * t + -gy * b)).normalize();

            // Convert to tangent space
            let ts_x = perturbed.dot(t);
            let ts_y = perturbed.dot(b);
            let ts_z = perturbed.dot(n_geom);

            normals[pixel_idx] = [ts_x * 0.5 + 0.5, ts_y * 0.5 + 0.5, ts_z * 0.5 + 0.5];
        }
    }

    normals
}

// ── Base Normal Blending (UDN) ──

/// Apply UDN (Uncompressed Detail Normal) blending to combine a base normal map
/// with paint-generated detail normals.
///
/// UDN formula (in [-1,1] space):
///   result.x = base.x + detail.x
///   result.y = base.y + detail.y
///   result.z = base.z
///   normalize(result)
///
/// Both `paint_normals` and the base normal texture are [0,1] encoded
/// (0.5 = zero displacement). Output overwrites `paint_normals` in place.
pub fn blend_normals_udn(
    paint_normals: &mut [[f32; 3]],
    base_normal_pixels: &[[f32; 4]],
    base_w: u32,
    base_h: u32,
    resolution: u32,
    mask: Option<&UvMask>,
) {
    if base_w == 0 || base_h == 0 || base_normal_pixels.is_empty() {
        return;
    }

    let res_f = resolution as f32;

    for y in 0..resolution {
        for x in 0..resolution {
            let idx = (y * resolution + x) as usize;
            if idx >= paint_normals.len() {
                break;
            }

            // Skip pixels outside the layer's UV mask region
            if let Some(m) = mask {
                let uv = Vec2::new((x as f32 + 0.5) / res_f, (y as f32 + 0.5) / res_f);
                if !m.sample(uv) {
                    continue;
                }
            }

            // Sample base normal (nearest-neighbor, UV mapping)
            let sx = ((x as f32 + 0.5) / resolution as f32 * base_w as f32) as u32;
            let sy = ((y as f32 + 0.5) / resolution as f32 * base_h as f32) as u32;
            let bi = (sy.min(base_h - 1) * base_w + sx.min(base_w - 1)) as usize;
            let bp = if bi < base_normal_pixels.len() {
                base_normal_pixels[bi]
            } else {
                [0.5, 0.5, 1.0, 1.0]
            };

            // Decode base normal from [0,1] to [-1,1]
            let bx = bp[0] * 2.0 - 1.0;
            let by = bp[1] * 2.0 - 1.0;
            let bz = bp[2] * 2.0 - 1.0;

            // Decode detail (paint) normal from [0,1] to [-1,1]
            let dn = paint_normals[idx];
            let dx = dn[0] * 2.0 - 1.0;
            let dy = dn[1] * 2.0 - 1.0;
            // detail.z unused in UDN

            // UDN blend: add detail XY to base XY, keep base Z
            let rx = bx + dx;
            let ry = by + dy;
            let rz = bz;
            let len = (rx * rx + ry * ry + rz * rz).sqrt().max(1e-6);

            // Re-encode to [0,1]
            paint_normals[idx] = [
                rx / len * 0.5 + 0.5,
                ry / len * 0.5 + 0.5,
                rz / len * 0.5 + 0.5,
            ];
        }
    }
}

// ── PNG Export Functions ──

/// Export a height map as a grayscale PNG (linear, no gamma).
pub fn export_height_png(height: &[f32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    let pixels: Vec<u8> = height
        .iter()
        .map(|&h| (h.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();

    image::save_buffer(path, &pixels, resolution, resolution, image::ColorType::L8)?;
    Ok(())
}

/// Export a color map as an sRGB PNG.
/// Applies linear_to_srgb() per RGB channel before writing.
/// When `with_alpha` is true, outputs RGBA8 with the alpha channel from Color.
pub fn export_color_png(
    color: &[Color],
    resolution: u32,
    path: &Path,
    with_alpha: bool,
) -> Result<(), OutputError> {
    if with_alpha {
        let pixels: Vec<u8> = color
            .iter()
            .flat_map(|c| {
                [
                    (linear_to_srgb(c.r.clamp(0.0, 1.0)) * 255.0).round() as u8,
                    (linear_to_srgb(c.g.clamp(0.0, 1.0)) * 255.0).round() as u8,
                    (linear_to_srgb(c.b.clamp(0.0, 1.0)) * 255.0).round() as u8,
                    (c.a.clamp(0.0, 1.0) * 255.0).round() as u8,
                ]
            })
            .collect();

        image::save_buffer(
            path,
            &pixels,
            resolution,
            resolution,
            image::ColorType::Rgba8,
        )?;
    } else {
        let pixels: Vec<u8> = color
            .iter()
            .flat_map(|c| {
                [
                    (linear_to_srgb(c.r.clamp(0.0, 1.0)) * 255.0).round() as u8,
                    (linear_to_srgb(c.g.clamp(0.0, 1.0)) * 255.0).round() as u8,
                    (linear_to_srgb(c.b.clamp(0.0, 1.0)) * 255.0).round() as u8,
                ]
            })
            .collect();

        image::save_buffer(
            path,
            &pixels,
            resolution,
            resolution,
            image::ColorType::Rgb8,
        )?;
    }
    Ok(())
}

/// Export a normal map as an RGB PNG (linear, no gamma).
pub fn export_normal_png(
    normals: &[[f32; 3]],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let pixels: Vec<u8> = normals
        .iter()
        .flat_map(|n| {
            [
                (n[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();

    image::save_buffer(
        path,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}

/// Export a stroke ID map as an RGB PNG where each unique stroke gets a distinct color.
/// Uses golden-angle hue spacing so adjacent stroke IDs receive visually dissimilar colors.
/// ID 0 (unpainted) is black.
pub fn export_stroke_id_png(ids: &[u32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    let mut unique: Vec<u32> = ids.iter().copied().filter(|&id| id != 0).collect();
    unique.sort_unstable();
    unique.dedup();

    // Golden angle ≈ 137.508° ensures maximum hue separation between consecutive indices.
    const GOLDEN_ANGLE: f32 = 137.508;

    let id_to_rgb: std::collections::HashMap<u32, [u8; 3]> = unique
        .iter()
        .enumerate()
        .map(|(i, &id)| {
            let hue = (i as f32 * GOLDEN_ANGLE) % 360.0;
            // Alternate saturation/value to further separate neighbors
            let sat = if i % 2 == 0 { 0.9 } else { 0.65 };
            let val = if (i / 2) % 2 == 0 { 1.0 } else { 0.75 };
            let c = hsv_to_rgb(HsvColor {
                h: hue / 360.0,
                s: sat,
                v: val,
            });
            (
                id,
                [
                    (c.r * 255.0).round() as u8,
                    (c.g * 255.0).round() as u8,
                    (c.b * 255.0).round() as u8,
                ],
            )
        })
        .collect();

    let mut pixels = Vec::with_capacity(ids.len() * 3);
    for &id in ids {
        if id == 0 {
            pixels.extend_from_slice(&[0, 0, 0]);
        } else {
            pixels.extend_from_slice(&id_to_rgb[&id]);
        }
    }

    image::save_buffer(
        path,
        &pixels,
        resolution,
        resolution,
        image::ColorType::Rgb8,
    )?;
    Ok(())
}

/// Export a stroke time map as an RGB PNG.
/// R = stroke_time_order (normalized stroke sequence), G = stroke_time_arc (arc-length progress),
/// B = 0 (reserved for layer order in future).
pub fn export_stroke_time_png(
    order: &[f32],
    arc: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    let pixels: Vec<u8> = order
        .iter()
        .zip(arc.iter())
        .flat_map(|(&o, &a)| {
            [
                (o.clamp(0.0, 1.0) * 255.0).round() as u8,
                (a.clamp(0.0, 1.0) * 255.0).round() as u8,
                0u8,
            ]
        })
        .collect();

    image::save_buffer(path, &pixels, resolution, resolution, image::ColorType::Rgb8)?;
    Ok(())
}

// ── EXR Export Functions ──

/// Export a height map as a single-channel float32 EXR (linear).
pub fn export_height_exr(height: &[f32], resolution: u32, path: &Path) -> Result<(), OutputError> {
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(path, res, res, |x, y| {
        let h = height[y * res + x];
        (h, h, h)
    })
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

/// Export a color map as a float32 EXR (linear, NO sRGB conversion).
/// When `with_alpha` is true, outputs RGBA; otherwise RGB.
pub fn export_color_exr(
    color: &[Color],
    resolution: u32,
    path: &Path,
    with_alpha: bool,
) -> Result<(), OutputError> {
    use exr::prelude::*;

    let res = resolution as usize;

    if with_alpha {
        write_rgba_file(path, res, res, |x, y| {
            let c = &color[y * res + x];
            (c.r, c.g, c.b, c.a)
        })
        .map_err(|e| OutputError::ExrError(e.to_string()))?;
    } else {
        write_rgb_file(path, res, res, |x, y| {
            let c = &color[y * res + x];
            (c.r, c.g, c.b)
        })
        .map_err(|e| OutputError::ExrError(e.to_string()))?;
    }

    Ok(())
}

/// Export a stroke time map as a float32 EXR.
/// R = stroke_time_order, G = stroke_time_arc, B = 0 (reserved for layer order).
pub fn export_stroke_time_exr(
    order: &[f32],
    arc: &[f32],
    resolution: u32,
    path: &Path,
) -> Result<(), OutputError> {
    use exr::prelude::*;

    let res = resolution as usize;

    write_rgb_file(path, res, res, |x, y| {
        let idx = y * res + x;
        (order[idx], arc[idx], 0.0f32)
    })
    .map_err(|e| OutputError::ExrError(e.to_string()))?;

    Ok(())
}

// ── Per-Layer Export ──

/// Metadata for a single layer, written into `manifest.json`.
#[derive(Debug, Serialize)]
pub struct LayerManifestEntry {
    pub index: usize,
    pub name: String,
    pub group: String,
    pub order: i32,
    pub visible: bool,
    pub dry: f32,
}

/// Write `manifest.json` with layer metadata and file listing.
pub fn export_manifest(
    layers: &[LayerManifestEntry],
    format: ExportFormat,
    output_dir: &Path,
) -> Result<(), OutputError> {
    let ext = match format {
        ExportFormat::Png => "png",
        ExportFormat::Exr => "exr",
    };

    #[derive(Serialize)]
    struct Manifest<'a> {
        layers: &'a [LayerManifestEntry],
        format: &'a str,
    }

    let manifest = Manifest {
        layers,
        format: ext,
    };

    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(output_dir.join("manifest.json"), json)?;
    Ok(())
}

/// Export a single layer's texture maps (color, height, normal).
///
/// Files are named `layer_{index}_{map}.{ext}` (e.g. `layer_0_color.png`).
/// Normal map is generated from the layer's gradient data using the same
/// Sobel-based approach as the composite normal map.
pub fn export_layer_maps(
    layer: &LayerMaps,
    index: usize,
    format: ExportFormat,
    normal_strength: f32,
    normal_mode: NormalMode,
    normal_data: Option<&MeshNormalData>,
    include_color: bool,
    include_height: bool,
    include_normal: bool,
    include_time_map: bool,
    output_dir: &Path,
) -> Result<u32, OutputError> {
    let res = layer.resolution;
    let prefix = format!("layer_{index}");
    let mut count = 0u32;

    if include_color {
        // Per-layer color always has alpha (transparent where unpainted).
        match format {
            ExportFormat::Png => export_color_png(
                &layer.color,
                res,
                &output_dir.join(format!("{prefix}_color.png")),
                true,
            )?,
            ExportFormat::Exr => export_color_exr(
                &layer.color,
                res,
                &output_dir.join(format!("{prefix}_color.exr")),
                true,
            )?,
        }
        count += 1;
    }

    if include_height {
        let normalized = normalize_height_map(&layer.height);
        match format {
            ExportFormat::Png => export_height_png(
                &normalized,
                res,
                &output_dir.join(format!("{prefix}_height.png")),
            )?,
            ExportFormat::Exr => export_height_exr(
                &normalized,
                res,
                &output_dir.join(format!("{prefix}_height.exr")),
            )?,
        }
        count += 1;
    }

    if include_normal {
        let normals = match (normal_mode, normal_data) {
            (NormalMode::DepictedForm, Some(nd)) => generate_normal_map_depicted_form(
                &layer.gradient_x,
                &layer.gradient_y,
                nd,
                &layer.object_normal,
                &layer.paint_load,
                res,
                normal_strength,
            ),
            _ => generate_normal_map(
                &layer.gradient_x,
                &layer.gradient_y,
                res,
                normal_strength,
            ),
        };
        export_normal_png(&normals, res, &output_dir.join(format!("{prefix}_normal.png")))?;
        count += 1;
    }

    if include_time_map {
        match format {
            ExportFormat::Png => export_stroke_time_png(
                &layer.stroke_time_order,
                &layer.stroke_time_arc,
                res,
                &output_dir.join(format!("{prefix}_stroke_time.png")),
            )?,
            ExportFormat::Exr => export_stroke_time_exr(
                &layer.stroke_time_order,
                &layer.stroke_time_arc,
                res,
                &output_dir.join(format!("{prefix}_stroke_time.exr")),
            )?,
        }
        count += 1;
    }

    Ok(count)
}

// ── Export All ──

/// Export all textures at once.
pub fn export_all(
    global: &GlobalMaps,
    settings: &OutputSettings,
    output_dir: &Path,
    format: ExportFormat,
    normal_data: Option<&MeshNormalData>,
) -> Result<(), OutputError> {
    std::fs::create_dir_all(output_dir)?;

    let normalized_height = normalize_height_map(&global.height);
    let normals = match (settings.normal_mode, normal_data) {
        (NormalMode::DepictedForm, Some(nd)) => generate_normal_map_depicted_form(
            &global.gradient_x,
            &global.gradient_y,
            nd,
            &global.object_normal,
            &global.paint_load,
            global.resolution,
            settings.normal_strength,
        ),
        _ => generate_normal_map(
            &global.gradient_x,
            &global.gradient_y,
            global.resolution,
            settings.normal_strength,
        ),
    };

    let with_alpha = settings.background_mode == BackgroundMode::Transparent;

    match format {
        ExportFormat::Png => {
            export_color_png(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.png"),
                with_alpha,
            )?;
            export_height_png(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.png"),
            )?;
        }
        ExportFormat::Exr => {
            export_color_exr(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.exr"),
                with_alpha,
            )?;
            export_height_exr(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.exr"),
            )?;
        }
    }

    // Normal map and stroke ID are always PNG
    export_normal_png(
        &normals,
        global.resolution,
        &output_dir.join("normal_map.png"),
    )?;
    export_stroke_id_png(
        &global.stroke_id,
        global.resolution,
        &output_dir.join("stroke_id_map.png"),
    )?;

    // Stroke time map
    match format {
        ExportFormat::Png => {
            export_stroke_time_png(
                &global.stroke_time_order,
                &global.stroke_time_arc,
                global.resolution,
                &output_dir.join("stroke_time_map.png"),
            )?;
        }
        ExportFormat::Exr => {
            export_stroke_time_exr(
                &global.stroke_time_order,
                &global.stroke_time_arc,
                global.resolution,
                &output_dir.join("stroke_time_map.exr"),
            )?;
        }
    }

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_io::{linear_to_srgb, load_texture};
    use crate::compositing::composite_all;
    use crate::test_util::make_layer_with_order;
    use crate::types::{Color, LayerBaseColor, OutputSettings};

    const EPS: f32 = 1e-4;

    // ── Height Normalization Tests ──

    #[test]
    fn normalize_all_zeros() {
        let height = vec![0.0f32; 64];
        let normalized = normalize_height_map(&height);
        assert!(normalized.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn normalize_clamps_above_one() {
        let height = vec![3.0f32; 1];
        let normalized = normalize_height_map(&height);
        assert!(
            (normalized[0] - 1.0).abs() < EPS,
            "above 1.0: expected 1.0, got {}",
            normalized[0]
        );
    }

    // ── Normal Map Tests ──

    fn zeros(n: usize) -> Vec<f32> {
        vec![0.0f32; n]
    }

    #[test]
    fn normal_flat_surface() {
        let res = 16u32;
        let size = (res * res) as usize;
        let normals = generate_normal_map(&zeros(size), &zeros(size), res, 1.0);

        for (i, n) in normals.iter().enumerate() {
            assert!(
                (n[0] - 0.5).abs() < EPS && (n[1] - 0.5).abs() < EPS && (n[2] - 1.0).abs() < EPS,
                "flat at {i}: expected (0.5, 0.5, 1.0), got ({:.4}, {:.4}, {:.4})",
                n[0],
                n[1],
                n[2]
            );
        }
    }

    #[test]
    fn normal_slope_right() {
        let res = 16u32;
        let size = (res * res) as usize;
        // Positive gx → normal tilts left (nx < 0.5)
        let gx = vec![1.0f32; size];
        let gy = zeros(size);
        let normals = generate_normal_map(&gx, &gy, res, 1.0);
        let center = normals[(8 * res + 8) as usize];
        assert!(
            center[0] < 0.5,
            "slope right: nx should be < 0.5, got {:.4}",
            center[0]
        );
    }

    #[test]
    fn normal_slope_up() {
        let res = 16u32;
        let size = (res * res) as usize;
        let gx = zeros(size);
        let gy = vec![1.0f32; size];
        let normals = generate_normal_map(&gx, &gy, res, 1.0);
        let center = normals[(8 * res + 8) as usize];
        assert!(
            center[1] < 0.5,
            "slope up: ny should be < 0.5, got {:.4}",
            center[1]
        );
    }

    #[test]
    fn normal_strength_zero() {
        let res = 16u32;
        let size = (res * res) as usize;
        let gx = vec![1.0f32; size];
        let normals = generate_normal_map(&gx, &zeros(size), res, 0.0);
        for (i, n) in normals.iter().enumerate() {
            assert!(
                (n[0] - 0.5).abs() < EPS && (n[1] - 0.5).abs() < EPS,
                "strength=0 at {i}: expected flat, got ({:.4}, {:.4}, {:.4})",
                n[0],
                n[1],
                n[2]
            );
        }
    }

    #[test]
    fn normal_strength_scales() {
        let res = 16u32;
        let size = (res * res) as usize;
        let gx = vec![1.0f32; size];
        let gy = zeros(size);
        let normals_1 = generate_normal_map(&gx, &gy, res, 1.0);
        let normals_2 = generate_normal_map(&gx, &gy, res, 2.0);
        let n1 = normals_1[(8 * res + 8) as usize];
        let n2 = normals_2[(8 * res + 8) as usize];
        let dev_1 = (n1[0] - 0.5).abs();
        let dev_2 = (n2[0] - 0.5).abs();
        assert!(
            dev_2 > dev_1,
            "strength=2 should tilt more: s1={dev_1:.4}, s2={dev_2:.4}",
        );
    }

    #[test]
    fn normal_unit_length() {
        let res = 32u32;
        let size = (res * res) as usize;
        let mut gx = vec![0.0f32; size];
        let mut gy = vec![0.0f32; size];
        for y in 0..res {
            for x in 0..res {
                let i = (y * res + x) as usize;
                gx[i] = (x as f32 * 0.5).sin();
                gy[i] = (y as f32 * 0.3).cos();
            }
        }

        let normals = generate_normal_map(&gx, &gy, res, 1.5);
        for (i, n) in normals.iter().enumerate() {
            let dx = n[0] * 2.0 - 1.0;
            let dy = n[1] * 2.0 - 1.0;
            let dz = n[2] * 2.0 - 1.0;
            let len = (dx * dx + dy * dy + dz * dz).sqrt();
            assert!(
                (len - 1.0).abs() < 0.01,
                "normal at {i} not unit length: {len:.4}"
            );
        }
    }

    // ── PNG Export Tests ──

    #[test]
    fn png_dimensions() {
        let res = 128u32;
        let height = vec![0.5f32; (res * res) as usize];
        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dim_test_height.png");

        export_height_png(&height, res, &path).unwrap();

        let img = image::open(&path).unwrap();
        assert_eq!(img.width(), 128);
        assert_eq!(img.height(), 128);
    }

    #[test]
    fn color_png_srgb() {
        let res = 4u32;
        let linear_val = 0.5f32; // known linear value
        let colors = vec![Color::rgb(linear_val, linear_val, linear_val); (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("srgb_test.png");

        export_color_png(&colors, res, &path, false).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        let pixel = img.get_pixel(0, 0);
        let expected = (linear_to_srgb(linear_val) * 255.0).round() as u8;
        assert_eq!(
            pixel[0], expected,
            "sRGB conversion: expected {expected}, got {}",
            pixel[0]
        );
    }

    #[test]
    fn height_png_range() {
        let res = 4u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        height[0] = 0.0; // black
        height[1] = 1.0; // white

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("range_test.png");

        export_height_png(&height, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_luma8();
        assert_eq!(img.get_pixel(0, 0)[0], 0, "0.0 should map to black");
        assert_eq!(img.get_pixel(1, 0)[0], 255, "1.0 should map to white");
    }

    #[test]
    fn normal_png_flat() {
        let res = 4u32;
        let normals = vec![[0.5f32, 0.5, 1.0]; (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("normal_flat_test.png");

        export_normal_png(&normals, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        let pixel = img.get_pixel(0, 0);
        assert_eq!(
            pixel[0], 128,
            "flat normal R should be 128, got {}",
            pixel[0]
        );
        assert_eq!(
            pixel[1], 128,
            "flat normal G should be 128, got {}",
            pixel[1]
        );
        assert_eq!(
            pixel[2], 255,
            "flat normal B should be 255, got {}",
            pixel[2]
        );
    }

    #[test]
    fn stroke_id_png_mapping() {
        let res = 4u32;
        let mut ids = vec![0u32; (res * res) as usize];
        ids[0] = 0;
        ids[1] = 5;
        ids[2] = 10;

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("stroke_id_test.png");

        export_stroke_id_png(&ids, res, &path).unwrap();

        let img = image::open(&path).unwrap().to_rgb8();
        // ID 0 → black
        let p0 = img.get_pixel(0, 0);
        assert_eq!(p0[0], 0, "ID 0 R should be 0");
        assert_eq!(p0[1], 0, "ID 0 G should be 0");
        assert_eq!(p0[2], 0, "ID 0 B should be 0");
        // ID 5 and ID 10 should be different non-black colors
        let p1 = img.get_pixel(1, 0);
        let p2 = img.get_pixel(2, 0);
        assert!(
            p1[0] > 0 || p1[1] > 0 || p1[2] > 0,
            "ID 5 should not be black"
        );
        assert!(
            p2[0] > 0 || p2[1] > 0 || p2[2] > 0,
            "ID 10 should not be black"
        );
        assert_ne!(
            [p1[0], p1[1], p1[2]],
            [p2[0], p2[1], p2[2]],
            "different IDs should have different colors"
        );
    }

    // ── EXR Export Tests ──

    #[test]
    fn exr_height_dimensions_and_values() {
        let res = 16u32;
        let mut height = vec![0.0f32; (res * res) as usize];
        height[0] = 0.0;
        height[1] = 0.5;
        height[2] = 1.0;

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("height_test.exr");

        export_height_exr(&height, res, &path).unwrap();

        // Read back
        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 16);
        assert_eq!(tex.height, 16);

        // Values should be preserved (written as RGB with R=G=B=height)
        assert!(
            (tex.pixels[0][0] - 0.0).abs() < EPS,
            "height[0]: expected 0.0, got {}",
            tex.pixels[0][0]
        );
        assert!(
            (tex.pixels[1][0] - 0.5).abs() < EPS,
            "height[1]: expected 0.5, got {}",
            tex.pixels[1][0]
        );
        assert!(
            (tex.pixels[2][0] - 1.0).abs() < EPS,
            "height[2]: expected 1.0, got {}",
            tex.pixels[2][0]
        );
    }

    #[test]
    fn exr_color_linear() {
        let res = 4u32;
        let known_color = Color::rgb(0.3, 0.6, 0.9);
        let colors = vec![known_color; (res * res) as usize];

        let dir = std::env::temp_dir().join("pap_test_output");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("color_linear_test.exr");

        export_color_exr(&colors, res, &path, false).unwrap();

        // Read back
        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);

        // Values should be written as-is (no sRGB conversion)
        let p = tex.pixels[0];
        assert!((p[0] - 0.3).abs() < EPS, "R: expected 0.3, got {}", p[0]);
        assert!((p[1] - 0.6).abs() < EPS, "G: expected 0.6, got {}", p[1]);
        assert!((p[2] - 0.9).abs() < EPS, "B: expected 0.9, got {}", p[2]);
    }

    // ── Export All Test ──

    #[test]
    fn export_all_png() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[layer.clone()],
            res,
            &[LayerBaseColor::solid(Color::rgb(0.5, 0.5, 0.5))],
            &settings,
            None,
            &[],
            None,
        );

        let dir = std::env::temp_dir()
            .join("pap_test_output")
            .join("export_all_png");
        export_all(&maps, &settings, &dir, ExportFormat::Png, None).unwrap();

        // Verify files exist
        assert!(
            dir.join("color_map.png").exists(),
            "color_map.png should exist"
        );
        assert!(
            dir.join("height_map.png").exists(),
            "height_map.png should exist"
        );
        assert!(
            dir.join("normal_map.png").exists(),
            "normal_map.png should exist"
        );
        assert!(
            dir.join("stroke_id_map.png").exists(),
            "stroke_id_map.png should exist"
        );
    }

    #[test]
    fn export_all_exr() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(
            &[layer.clone()],
            res,
            &[LayerBaseColor::solid(Color::rgb(0.5, 0.5, 0.5))],
            &settings,
            None,
            &[],
            None,
        );

        let dir = std::env::temp_dir()
            .join("pap_test_output")
            .join("export_all_exr");
        export_all(&maps, &settings, &dir, ExportFormat::Exr, None).unwrap();

        assert!(
            dir.join("color_map.exr").exists(),
            "color_map.exr should exist"
        );
        assert!(
            dir.join("height_map.exr").exists(),
            "height_map.exr should exist"
        );
        assert!(
            dir.join("normal_map.png").exists(),
            "normal_map.png should exist"
        );
        assert!(
            dir.join("stroke_id_map.png").exists(),
            "stroke_id_map.png should exist"
        );
    }

    // ── Visual Integration Test ──

    // ── UDN Blending Tests ──

    #[test]
    fn udn_flat_base_is_identity() {
        // Flat base (0,0,1) should not change the detail normal
        let mut normals = vec![[0.6, 0.4, 1.0]; 4];
        let original = normals.clone();
        // No base pixels → function returns early, normals unchanged
        blend_normals_udn(&mut normals, &[], 0, 0, 2, None);
        assert_eq!(normals, original);
    }

    #[test]
    fn udn_flat_detail_preserves_base() {
        // Flat detail (0.5, 0.5, X) should approximate base
        let base = vec![[0.7, 0.3, 0.9, 1.0]; 4]; // tilted base
        let mut normals = vec![[0.5, 0.5, 1.0]; 4]; // flat detail
        blend_normals_udn(&mut normals, &base, 2, 2, 2, None);
        // base decoded: (0.4, -0.4, 0.8), detail decoded: (0, 0, _)
        // result: (0.4, -0.4, 0.8) normalized = same direction as base
        // re-encoded: should be close to base
        for n in &normals {
            assert!(
                (n[0] - 0.7).abs() < 0.02,
                "R should be ~0.7, got {:.4}",
                n[0]
            );
            assert!(
                (n[1] - 0.3).abs() < 0.02,
                "G should be ~0.3, got {:.4}",
                n[1]
            );
        }
    }

    #[test]
    fn udn_unit_length() {
        let base = vec![[0.7, 0.3, 0.9, 1.0]; 16];
        let mut normals = vec![[0.6, 0.4, 0.9]; 16];
        blend_normals_udn(&mut normals, &base, 4, 4, 4, None);
        for n in &normals {
            let dx = n[0] * 2.0 - 1.0;
            let dy = n[1] * 2.0 - 1.0;
            let dz = n[2] * 2.0 - 1.0;
            let len = (dx * dx + dy * dy + dz * dz).sqrt();
            assert!((len - 1.0).abs() < 0.01, "not unit length: {len:.4}");
        }
    }

    #[test]
    fn udn_adds_detail_to_base() {
        // A base tilted right + a detail tilted right should produce even more rightward tilt
        let base = vec![[0.6, 0.5, 0.9, 1.0]; 4]; // base tilted right (bx=0.2)
        let mut normals = vec![[0.6, 0.5, 0.9]; 4]; // detail tilted right (dx=0.2)
        blend_normals_udn(&mut normals, &base, 2, 2, 2, None);
        // Combined should be more tilted right than either alone
        assert!(
            normals[0][0] > 0.6,
            "blended R should be > 0.6, got {:.4}",
            normals[0][0]
        );
    }
}
