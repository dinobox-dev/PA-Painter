//! "Extract" track — ad-hoc DCC-targeted output of individual maps.
//!
//! Distinct from the project-level [`super::export_all`] flow: extract
//! produces a single file per invocation, with parameters that are
//! request-time only (not persisted in `.papr`). v1 supports object-space
//! normal map output with selectable up axis (Y / Z) and encoding
//! (PNG 8-bit / PNG 16-bit / EXR float32).

use std::path::Path;

use glam::Vec3;
use log::debug;
use serde::{Deserialize, Serialize};

use super::OutputError;
use crate::mesh::object_normal::MeshNormalData;
use crate::util::math::smoothstep;

/// Up-axis convention applied to the exported object-space vector.
///
/// The renderer's internal convention is +Y up (matches glTF / Maya / Unity).
/// Selecting [`UpAxis::Z`] applies a +90° rotation about X so that the
/// internal +Y axis maps to the output +Z axis (Blender / Unreal / 3ds Max).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum UpAxis {
    /// +Y up — Maya, Unity, glTF, OpenGL.
    #[default]
    Y,
    /// +Z up — Blender, Unreal Engine, 3ds Max.
    Z,
}

/// Encoding for the extracted map file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExtractEncoding {
    /// 8-bit PNG, 256 levels per channel.
    Png8,
    /// 16-bit PNG, 65,536 levels per channel. Recommended default for normals.
    #[default]
    Png16,
    /// 32-bit float EXR, linear.
    Exr,
}

impl ExtractEncoding {
    /// File extension (without leading dot) appropriate for this encoding.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png8 | Self::Png16 => "png",
            Self::Exr => "exr",
        }
    }
}

/// Inputs to [`extract_object_normal`].
///
/// Mirrors the data shape of
/// [`super::generate_normal_map_depicted_form`](super::normal_map::generate_normal_map_depicted_form)
/// but stops before the tangent-space conversion: the perturbed vector
/// stays in object space and is encoded directly (with optional axis swap).
pub struct ExtractObjectNormalParams<'a> {
    pub gradient_x: &'a [f32],
    pub gradient_y: &'a [f32],
    pub normal_data: &'a MeshNormalData,
    /// Per-stroke composited object-space normals (from
    /// [`crate::pipeline::compositing::GlobalMaps::object_normal`]). May be
    /// empty if no strokes contributed; falls back to geometric normal.
    pub global_object_normals: &'a [[f32; 3]],
    /// Per-pixel paint load used to fade stroke normals toward the geometric
    /// normal at low coverage.
    pub paint_load: &'a [f32],
    pub resolution: u32,
    pub normal_strength: f32,
    pub up_axis: UpAxis,
    pub encoding: ExtractEncoding,
    pub output_path: &'a Path,
}

/// Compute and write a single object-space normal map.
pub fn extract_object_normal(params: &ExtractObjectNormalParams<'_>) -> Result<(), OutputError> {
    debug!(
        "Extracting object normal: {} (up={:?}, enc={:?})",
        params.output_path.display(),
        params.up_axis,
        params.encoding,
    );
    let normals = compute_object_normals(params);
    write_normal_map(&normals, params.resolution, params.encoding, params.output_path)
}

/// Compute the perturbed object-space normal per pixel (encoded to [0, 1]
/// for output-friendly storage). The vector is in the requested up-axis
/// frame.
fn compute_object_normals(params: &ExtractObjectNormalParams<'_>) -> Vec<[f32; 3]> {
    let res = params.resolution;
    let nd_res = params.normal_data.resolution;
    let size = (res * res) as usize;
    let mut out = vec![[0.5f32, 0.5, 0.5]; size];

    for y in 0..res {
        for x in 0..res {
            let pixel_idx = (y * res + x) as usize;
            let gx = params.gradient_x[pixel_idx];
            let gy = params.gradient_y[pixel_idx];

            // Resample mesh-normal data to the output resolution.
            let nd_x = ((x as f32 + 0.5) / res as f32 * nd_res as f32).floor() as u32;
            let nd_y = ((y as f32 + 0.5) / res as f32 * nd_res as f32).floor() as u32;
            let nd_idx = (nd_y.min(nd_res - 1) * nd_res + nd_x.min(nd_res - 1)) as usize;

            let n_geom = params.normal_data.object_normals[nd_idx];
            let t = params.normal_data.tangents[nd_idx];
            let b = params.normal_data.bitangents[nd_idx];

            // Pixel without geometric normal coverage: emit "no data" (flat
            // mid-grey). Object-space output has no defined fallback — unlike
            // tangent space, there is no canonical "flat" object-space normal
            // because every pixel can point in a different direction.
            if n_geom.length_squared() < 0.5 {
                out[pixel_idx] = [0.5, 0.5, 0.5];
                continue;
            }

            // Blend stroke object-normal toward geometric normal by paint coverage.
            let n_obj = if !params.global_object_normals.is_empty() {
                let sn = params.global_object_normals[pixel_idx];
                let v = Vec3::new(sn[0], sn[1], sn[2]);
                if v.length_squared() > 0.5 {
                    let load = params.paint_load[pixel_idx];
                    let influence = smoothstep(0.0, 0.7, load);
                    n_geom.lerp(v, influence).normalize()
                } else {
                    n_geom
                }
            } else {
                n_geom
            };

            let perturbed = (n_obj + params.normal_strength * (-gx * t + -gy * b)).normalize();
            let oriented = orient(perturbed, params.up_axis);
            out[pixel_idx] = [
                oriented.x * 0.5 + 0.5,
                oriented.y * 0.5 + 0.5,
                oriented.z * 0.5 + 0.5,
            ];
        }
    }

    out
}

/// Apply the chosen up-axis convention to an object-space vector.
///
/// Internal frame is +Y up. For [`UpAxis::Z`] the vector is rotated +90°
/// about X so that internal +Y → output +Z and internal +Z → output -Y.
fn orient(v: Vec3, up: UpAxis) -> Vec3 {
    match up {
        UpAxis::Y => v,
        UpAxis::Z => Vec3::new(v.x, -v.z, v.y),
    }
}

fn write_normal_map(
    normals: &[[f32; 3]],
    resolution: u32,
    encoding: ExtractEncoding,
    path: &Path,
) -> Result<(), OutputError> {
    match encoding {
        ExtractEncoding::Png8 => write_png8(normals, resolution, path),
        ExtractEncoding::Png16 => write_png16(normals, resolution, path),
        ExtractEncoding::Exr => write_exr(normals, resolution, path),
    }
}

fn write_png8(normals: &[[f32; 3]], resolution: u32, path: &Path) -> Result<(), OutputError> {
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

fn write_png16(normals: &[[f32; 3]], resolution: u32, path: &Path) -> Result<(), OutputError> {
    let mut pixels: Vec<u16> = Vec::with_capacity(normals.len() * 3);
    for n in normals {
        pixels.push((n[0].clamp(0.0, 1.0) * 65535.0).round() as u16);
        pixels.push((n[1].clamp(0.0, 1.0) * 65535.0).round() as u16);
        pixels.push((n[2].clamp(0.0, 1.0) * 65535.0).round() as u16);
    }
    // Only fails if the buffer length doesn't match resolution²·3 — caller
    // contract guarantees it, so this is structurally unreachable.
    let buf: image::ImageBuffer<image::Rgb<u16>, Vec<u16>> =
        image::ImageBuffer::from_raw(resolution, resolution, pixels).ok_or_else(|| {
            OutputError::Io(std::io::Error::other(
                "16-bit PNG buffer size mismatch",
            ))
        })?;
    buf.save(path)?;
    Ok(())
}

fn write_exr(normals: &[[f32; 3]], resolution: u32, path: &Path) -> Result<(), OutputError> {
    use exr::prelude::*;
    let res = resolution as usize;
    write_rgb_file(path, res, res, |x, y| {
        let n = normals[y * res + x];
        (n[0], n[1], n[2])
    })
    .map_err(|e| OutputError::ExrError(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_matches_encoding() {
        assert_eq!(ExtractEncoding::Png8.extension(), "png");
        assert_eq!(ExtractEncoding::Png16.extension(), "png");
        assert_eq!(ExtractEncoding::Exr.extension(), "exr");
    }

    #[test]
    fn orient_y_up_is_identity() {
        let v = Vec3::new(0.3, 0.6, 0.7);
        assert_eq!(orient(v, UpAxis::Y), v);
    }

    #[test]
    fn orient_z_up_swaps_y_and_z() {
        // +Y → +Z, +Z → -Y, +X → +X (rotation +90° about X).
        let py = Vec3::Y;
        let oriented = orient(py, UpAxis::Z);
        assert!((oriented - Vec3::Z).length() < 1e-6);

        let pz = Vec3::Z;
        let oriented = orient(pz, UpAxis::Z);
        assert!((oriented - (-Vec3::Y)).length() < 1e-6);

        let px = Vec3::X;
        let oriented = orient(px, UpAxis::Z);
        assert!((oriented - Vec3::X).length() < 1e-6);
    }

    #[test]
    fn orient_z_up_preserves_length() {
        let v = Vec3::new(0.2, -0.7, 0.4).normalize();
        let oriented = orient(v, UpAxis::Z);
        assert!((oriented.length() - 1.0).abs() < 1e-6);
    }
}
