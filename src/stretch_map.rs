//! UV stretch map — per-pixel ratio of 3D surface area to UV area.
//!
//! Used to compensate for UV distortion so that stroke density, size, and
//! arc length are uniform in 3D space regardless of UV unwrap quality.

use glam::{Vec2, Vec3};

use crate::asset_io::LoadedMesh;

/// Pre-computed stretch factor rasterized into UV space.
///
/// Each pixel stores `sqrt(3D_area / UV_area)` for the triangle covering it.
/// A value of 1.0 means no distortion; >1 means UV is compressed (large 3D
/// area packed into small UV area); <1 means UV is stretched.
pub struct StretchMap {
    data: Vec<f32>,
    resolution: u32,
}

impl StretchMap {
    /// Sample the stretch factor at a UV coordinate (nearest-neighbor).
    ///
    /// Returns 1.0 for pixels outside any triangle (no mesh coverage).
    pub fn sample(&self, uv: Vec2) -> f32 {
        let res = self.resolution;
        let x = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as usize;
        let y = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as usize;
        let v = self.data[y * res as usize + x];
        if v > 0.0 {
            v
        } else {
            1.0
        }
    }

    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// Compute the stretch-weighted arc length of a path.
    ///
    /// Each UV segment's length is multiplied by the stretch factor at its
    /// midpoint, giving an approximate 3D arc length.
    pub fn weighted_arc_length(&self, points: &[Vec2]) -> f32 {
        points
            .windows(2)
            .map(|w| {
                let mid = (w[0] + w[1]) * 0.5;
                let uv_len = (w[1] - w[0]).length();
                uv_len * self.sample(mid)
            })
            .sum()
    }
}

/// Compute a stretch map from a loaded mesh.
///
/// For each triangle, computes `sqrt(3D_face_area / UV_face_area)` and
/// rasterizes it into a UV-space grid. Overlapping triangles are averaged
/// via accumulate-then-divide.
pub fn compute_stretch_map(mesh: &LoadedMesh, resolution: u32) -> StretchMap {
    let size = (resolution * resolution) as usize;
    let mut accum = vec![0.0_f32; size];
    let mut count = vec![0u16; size];

    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);

        let p0 = mesh.positions[i0];
        let p1 = mesh.positions[i1];
        let p2 = mesh.positions[i2];
        let uv0 = mesh.uvs[i0];
        let uv1 = mesh.uvs[i1];
        let uv2 = mesh.uvs[i2];

        let area_3d = triangle_area_3d(p0, p1, p2);
        let area_uv = triangle_area_2d(uv0, uv1, uv2);

        // Degenerate triangle guard
        if area_3d < 1e-12 || area_uv < 1e-12 {
            continue;
        }

        let stretch = (area_3d / area_uv).sqrt() as f32;

        // Rasterize: flat-fill all pixels inside this triangle
        rasterize_triangle_flat(uv0, uv1, uv2, stretch, resolution, &mut accum, &mut count);
    }

    // Average where multiple triangles overlap
    let data: Vec<f32> = accum
        .iter()
        .zip(count.iter())
        .map(|(&sum, &c)| if c > 0 { sum / c as f32 } else { 0.0 })
        .collect();

    // Normalize: divide by the median so that "typical" stretch ≈ 1.0.
    // This makes the stretch map relative rather than absolute, so brush
    // parameters don't need rescaling for meshes of different world-space size.
    let mut sorted: Vec<f32> = data.iter().copied().filter(|&v| v > 0.0).collect();
    let median = if sorted.is_empty() {
        1.0
    } else {
        sorted.sort_unstable_by(|a, b| a.total_cmp(b));
        sorted[sorted.len() / 2]
    };

    let inv_median = if median > 1e-10 { 1.0 / median } else { 1.0 };
    let data: Vec<f32> = data.iter().map(|&v| if v > 0.0 { v * inv_median } else { 0.0 }).collect();

    StretchMap { data, resolution }
}

fn triangle_area_3d(a: Vec3, b: Vec3, c: Vec3) -> f64 {
    let e1 = b - a;
    let e2 = c - a;
    e1.cross(e2).length() as f64 * 0.5
}

fn triangle_area_2d(a: Vec2, b: Vec2, c: Vec2) -> f64 {
    let e1 = b - a;
    let e2 = c - a;
    (e1.x * e2.y - e1.y * e2.x).abs() as f64 * 0.5
}

/// Rasterize a triangle in UV space, writing a flat value to all covered pixels.
fn rasterize_triangle_flat(
    uv0: Vec2,
    uv1: Vec2,
    uv2: Vec2,
    value: f32,
    resolution: u32,
    accum: &mut [f32],
    count: &mut [u16],
) {
    let res_f = resolution as f32;

    let min_u = uv0.x.min(uv1.x).min(uv2.x);
    let max_u = uv0.x.max(uv1.x).max(uv2.x);
    let min_v = uv0.y.min(uv1.y).min(uv2.y);
    let max_v = uv0.y.max(uv1.y).max(uv2.y);

    let px_min = ((min_u * res_f).floor() as i32).max(0) as u32;
    let px_max = ((max_u * res_f).ceil() as i32).min(resolution as i32 - 1) as u32;
    let py_min = ((min_v * res_f).floor() as i32).max(0) as u32;
    let py_max = ((max_v * res_f).ceil() as i32).min(resolution as i32 - 1) as u32;

    for py in py_min..=py_max {
        for px in px_min..=px_max {
            let uv = Vec2::new((px as f32 + 0.5) / res_f, (py as f32 + 0.5) / res_f);
            let (u, v, w) = barycentric(uv, uv0, uv1, uv2);

            const EDGE_EPS: f32 = -1e-4;
            if u < EDGE_EPS || v < EDGE_EPS || w < EDGE_EPS {
                continue;
            }

            let idx = (py * resolution + px) as usize;
            accum[idx] += value;
            count[idx] = count[idx].saturating_add(1);
        }
    }
}

/// Barycentric coordinates of point `p` in triangle `(a, b, c)`.
fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> (f32, f32, f32) {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;

    let d00 = v0.dot(v0);
    let d01 = v0.dot(v1);
    let d11 = v1.dot(v1);
    let d20 = v2.dot(v0);
    let d21 = v2.dot(v1);

    let denom = d00 * d11 - d01 * d01;
    if denom.abs() < 1e-12 {
        return (-1.0, -1.0, -1.0);
    }

    let inv = 1.0 / denom;
    let v = (d11 * d20 - d01 * d21) * inv;
    let w = (d00 * d21 - d01 * d20) * inv;
    let u = 1.0 - v - w;
    (u, v, w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_io::LoadedMesh;

    fn make_unit_square_mesh() -> LoadedMesh {
        // Two triangles forming a 1×1 square in XY plane, UV = position.xy
        // 3D area = UV area = 1.0, so stretch = 1.0
        LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            groups: vec![],
            materials: vec![],
        }
    }

    fn make_stretched_mesh() -> LoadedMesh {
        // 3D: 2×1 rectangle (area=2), UV: 1×1 square (area=1)
        // stretch = sqrt(2/1) ≈ 1.414
        LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            groups: vec![],
            materials: vec![],
        }
    }

    #[test]
    fn uniform_mesh_stretch_is_one() {
        let mesh = make_unit_square_mesh();
        let sm = compute_stretch_map(&mesh, 16);
        // All covered pixels should be ≈ 1.0 (normalized by median)
        let center = sm.sample(Vec2::new(0.5, 0.5));
        assert!(
            (center - 1.0).abs() < 0.01,
            "uniform mesh should have stretch ≈ 1.0, got {}",
            center
        );
    }

    #[test]
    fn uncovered_pixels_return_one() {
        // Mesh covers only bottom-left quarter
        let mesh = LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.5, 0.0, 0.0),
                Vec3::new(0.0, 0.5, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(0.0, 0.5),
            ],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        };
        let sm = compute_stretch_map(&mesh, 16);
        // Far corner has no coverage
        assert_eq!(sm.sample(Vec2::new(0.9, 0.9)), 1.0);
    }

    #[test]
    fn non_uniform_mesh_has_variation() {
        // Left half: 1×1 3D → 0.5×1 UV (stretch = sqrt(2))
        // Right half: 1×1 3D → 0.5×1 UV (stretch = sqrt(2))
        // Both halves same → uniform after median normalization.
        // Use different 3D sizes to create actual variation.
        let mesh = LoadedMesh {
            positions: vec![
                // Left triangle: 1×1 3D, 0.5×1 UV → stretch = sqrt(2)
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
                // Right triangle: 4×1 3D, 0.5×1 UV → stretch = sqrt(8) = 2√2
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(4.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(0.0, 1.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.5, 1.0),
            ],
            indices: vec![0, 1, 2, 3, 4, 5],
            groups: vec![],
            materials: vec![],
        };
        let sm = compute_stretch_map(&mesh, 32);
        let left = sm.sample(Vec2::new(0.15, 0.3));
        let right = sm.sample(Vec2::new(0.75, 0.3));
        assert!(
            right > left * 1.5,
            "right (4× 3D area) should have higher stretch than left: {} vs {}",
            right,
            left,
        );
    }

    #[test]
    fn median_normalization() {
        let mesh = make_stretched_mesh();
        let sm = compute_stretch_map(&mesh, 16);
        // Single uniform stretch → median = raw stretch → normalized to 1.0
        let center = sm.sample(Vec2::new(0.5, 0.5));
        assert!(
            (center - 1.0).abs() < 0.05,
            "single-stretch mesh should normalize to ~1.0, got {}",
            center,
        );
    }

    #[test]
    fn triangle_area_calculations() {
        // Unit right triangle: area = 0.5
        let a3 = triangle_area_3d(Vec3::ZERO, Vec3::X, Vec3::Y);
        assert!((a3 - 0.5).abs() < 1e-6);

        let a2 = triangle_area_2d(Vec2::ZERO, Vec2::X, Vec2::Y);
        assert!((a2 - 0.5).abs() < 1e-6);
    }

    #[test]
    fn degenerate_triangle_skipped() {
        // Collinear points: UV area = 0
        let mesh = LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(2.0, 0.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(1.0, 0.0),
            ],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        };
        let sm = compute_stretch_map(&mesh, 8);
        // No valid triangles → all pixels uncovered → sample returns 1.0
        assert_eq!(sm.sample(Vec2::new(0.5, 0.5)), 1.0);
    }
}
