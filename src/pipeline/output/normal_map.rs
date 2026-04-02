//! Normal map generation and UDN blending.

use glam::Vec2;

use crate::mesh::object_normal::MeshNormalData;
use crate::mesh::uv_mask::UvMask;
use crate::types::NormalYConvention;
use crate::util::math::smoothstep;

// ── Normal Map Generation ──

/// Generate a tangent-space normal map from pre-computed gradients.
///
/// Gradients are computed via Sobel on the (optionally diffused) height map,
/// either per-layer in [`compositing::render_layer`](super::super::compositing::render_layer) or globally in [`compositing::finalize_layers`](super::super::compositing::finalize_layers).
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
/// Both `paint_normals` and the base normal texture are \[0,1\] encoded
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

/// Encode normal-map floats to RGB8 pixels, applying the chosen Y-axis convention.
///
/// The internal tangent-space convention uses bitangent = +V (image-downward).
/// For OpenGL convention (+Y = up), the green channel is flipped;
/// for DirectX convention (+Y = down), it is kept as-is.
pub fn normals_to_pixels(normals: &[[f32; 3]], convention: NormalYConvention) -> Vec<u8> {
    let flip_y = convention == NormalYConvention::OpenGL;
    normals
        .iter()
        .flat_map(|n| {
            let g = if flip_y { 1.0 - n[1] } else { n[1] };
            [
                (n[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (n[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect()
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

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

    // ── UDN Blending Tests ──

    #[test]
    fn udn_flat_base_is_identity() {
        let mut normals = vec![[0.6, 0.4, 1.0]; 4];
        let original = normals.clone();
        blend_normals_udn(&mut normals, &[], 0, 0, 2, None);
        assert_eq!(normals, original);
    }

    #[test]
    fn udn_flat_detail_preserves_base() {
        let base = vec![[0.7, 0.3, 0.9, 1.0]; 4];
        let mut normals = vec![[0.5, 0.5, 1.0]; 4];
        blend_normals_udn(&mut normals, &base, 2, 2, 2, None);
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
        let base = vec![[0.6, 0.5, 0.9, 1.0]; 4];
        let mut normals = vec![[0.6, 0.5, 0.9]; 4];
        blend_normals_udn(&mut normals, &base, 2, 2, 2, None);
        assert!(
            normals[0][0] > 0.6,
            "blended R should be > 0.6, got {:.4}",
            normals[0][0]
        );
    }
}
