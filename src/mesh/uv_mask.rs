//! UV-space rasterized masks for per-mesh-group painting isolation.

use glam::Vec2;

use crate::mesh::asset_io::LoadedMesh;
use crate::types::MeshGroup;

/// A boolean bitmap mask in UV space.
///
/// `true` pixels indicate UV coverage by a mesh group's triangles.
#[derive(Clone)]
pub struct UvMask {
    pub data: Vec<bool>,
    pub resolution: u32,
}

impl UvMask {
    /// Build a mask from a mesh group by rasterizing its triangles into UV space.
    pub fn from_mesh_group(mesh: &LoadedMesh, group: &MeshGroup, resolution: u32) -> UvMask {
        let size = (resolution * resolution) as usize;
        let mut data = vec![false; size];
        let res_f = resolution as f32;

        let start = group.index_offset as usize;
        let end = start + group.index_count as usize;

        for tri in mesh.indices[start..end].chunks_exact(3) {
            let uv0 = mesh.uvs[tri[0] as usize];
            let uv1 = mesh.uvs[tri[1] as usize];
            let uv2 = mesh.uvs[tri[2] as usize];

            // AABB in pixel coordinates
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

                    if point_in_triangle(uv, uv0, uv1, uv2) {
                        data[(py * resolution + px) as usize] = true;
                    }
                }
            }
        }

        UvMask { data, resolution }
    }

    /// Dilate the mask by `radius` pixels (expand true regions).
    pub fn dilate(&mut self, radius: u32) {
        if radius == 0 {
            return;
        }
        let res = self.resolution;
        let r = radius as i32;
        let original = self.data.clone();

        for py in 0..res {
            for px in 0..res {
                if original[(py * res + px) as usize] {
                    continue; // already true
                }
                // Check if any neighbor within radius is true
                'search: for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy > r * r {
                            continue;
                        }
                        let nx = px as i32 + dx;
                        let ny = py as i32 + dy;
                        if nx >= 0
                            && nx < res as i32
                            && ny >= 0
                            && ny < res as i32
                            && original[(ny as u32 * res + nx as u32) as usize]
                        {
                            self.data[(py * res + px) as usize] = true;
                            break 'search;
                        }
                    }
                }
            }
        }
    }

    /// Bounding box of true pixels in UV space: (min, max).
    pub fn aabb(&self) -> (Vec2, Vec2) {
        let res = self.resolution;
        let res_f = res as f32;
        let mut min_x = res;
        let mut max_x = 0u32;
        let mut min_y = res;
        let mut max_y = 0u32;

        for py in 0..res {
            for px in 0..res {
                if self.data[(py * res + px) as usize] {
                    min_x = min_x.min(px);
                    max_x = max_x.max(px);
                    min_y = min_y.min(py);
                    max_y = max_y.max(py);
                }
            }
        }

        if max_x < min_x {
            // Empty mask
            return (Vec2::ZERO, Vec2::ZERO);
        }

        (
            Vec2::new(min_x as f32 / res_f, min_y as f32 / res_f),
            Vec2::new((max_x + 1) as f32 / res_f, (max_y + 1) as f32 / res_f),
        )
    }

    /// Check if a UV point is inside the mask.
    pub fn sample(&self, uv: Vec2) -> bool {
        let res = self.resolution;
        let px = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
        let py = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
        self.data[(py * res + px) as usize]
    }

    /// Compute a Manhattan distance field from the mask boundary.
    ///
    /// Each pixel stores the L1 (Manhattan) distance in pixels to the nearest
    /// `true` pixel. Pixels inside the mask have distance 0.
    /// Two-pass per axis, O(n) where n = resolution².
    pub fn distance_field(&self) -> DistanceField {
        let res = self.resolution as usize;
        let n = res * res;
        let inf = (res + res) as f32; // larger than any real Manhattan distance

        let mut dt = vec![inf; n];

        // Initialise: 0 for true pixels, inf for false
        for (d, &inside) in dt.iter_mut().zip(&self.data) {
            if inside {
                *d = 0.0;
            }
        }

        // Vertical pass (columns)
        for x in 0..res {
            // top → bottom
            for y in 1..res {
                let idx = y * res + x;
                let above = dt[(y - 1) * res + x] + 1.0;
                if above < dt[idx] {
                    dt[idx] = above;
                }
            }
            // bottom → top
            for y in (0..res - 1).rev() {
                let idx = y * res + x;
                let below = dt[(y + 1) * res + x] + 1.0;
                if below < dt[idx] {
                    dt[idx] = below;
                }
            }
        }

        // Horizontal pass (rows)
        for y in 0..res {
            let row = y * res;
            // left → right
            for x in 1..res {
                let left = dt[row + x - 1] + 1.0;
                if left < dt[row + x] {
                    dt[row + x] = left;
                }
            }
            // right → left
            for x in (0..res - 1).rev() {
                let right = dt[row + x + 1] + 1.0;
                if right < dt[row + x] {
                    dt[row + x] = right;
                }
            }
        }

        DistanceField {
            data: dt,
            resolution: self.resolution,
        }
    }

    /// All-true mask for `__full_uv__` groups.
    pub fn full(resolution: u32) -> UvMask {
        UvMask {
            data: vec![true; (resolution * resolution) as usize],
            resolution,
        }
    }
}

/// Manhattan distance field derived from a [`UvMask`].
///
/// Each pixel stores the L1 distance (in pixels) to the nearest `true` pixel
/// in the source mask. Use [`sample`](Self::sample) for UV-space queries with
/// an arbitrary distance threshold.
pub struct DistanceField {
    /// Row-major distance values. Size = `resolution²`.
    pub data: Vec<f32>,
    /// Pixel resolution (width = height).
    pub resolution: u32,
}

impl DistanceField {
    /// Check if a UV point is within `threshold` pixels of the mask.
    pub fn sample(&self, uv: Vec2, threshold: f32) -> bool {
        let res = self.resolution;
        let px = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
        let py = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
        self.data[(py * res + px) as usize] <= threshold
    }

    /// Bounding box of pixels within `threshold` distance, in UV space.
    pub fn aabb(&self, threshold: f32) -> (Vec2, Vec2) {
        let res = self.resolution;
        let res_f = res as f32;
        let mut min_x = res;
        let mut max_x = 0u32;
        let mut min_y = res;
        let mut max_y = 0u32;

        for py in 0..res {
            for px in 0..res {
                if self.data[(py * res + px) as usize] <= threshold {
                    min_x = min_x.min(px);
                    max_x = max_x.max(px);
                    min_y = min_y.min(py);
                    max_y = max_y.max(py);
                }
            }
        }

        if max_x < min_x {
            return (Vec2::ZERO, Vec2::ZERO);
        }

        (
            Vec2::new(min_x as f32 / res_f, min_y as f32 / res_f),
            Vec2::new((max_x + 1) as f32 / res_f, (max_y + 1) as f32 / res_f),
        )
    }
}

/// Barycentric point-in-triangle test.
fn point_in_triangle(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
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
        return false;
    }

    let inv_denom = 1.0 / denom;
    let v = (d11 * d20 - d01 * d21) * inv_denom;
    let w = (d00 * d21 - d01 * d20) * inv_denom;
    let u = 1.0 - v - w;

    const EDGE_EPS: f32 = -1e-4;
    u >= EDGE_EPS && v >= EDGE_EPS && w >= EDGE_EPS
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn make_half_mesh() -> (LoadedMesh, MeshGroup) {
        // Triangle covering left half of UV space: (0,0)-(0.5,0)-(0,1)
        let mesh = LoadedMesh {
            positions: vec![Vec3::ZERO; 3],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.5, 0.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
            groups: vec![MeshGroup {
                name: "left".to_string(),
                index_offset: 0,
                index_count: 3,
            }],
            materials: vec![],
        };
        let group = mesh.groups[0].clone();
        (mesh, group)
    }

    fn make_full_quad_mesh() -> (LoadedMesh, MeshGroup, MeshGroup) {
        // Two triangles covering full UV [0,1]², split into two groups
        let mesh = LoadedMesh {
            positions: vec![Vec3::ZERO; 4],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            groups: vec![
                MeshGroup {
                    name: "lower".to_string(),
                    index_offset: 0,
                    index_count: 3,
                },
                MeshGroup {
                    name: "upper".to_string(),
                    index_offset: 3,
                    index_count: 3,
                },
            ],
            materials: vec![],
        };
        let g0 = mesh.groups[0].clone();
        let g1 = mesh.groups[1].clone();
        (mesh, g0, g1)
    }

    #[test]
    fn mask_from_triangle_coverage() {
        let (mesh, group) = make_half_mesh();
        let mask = UvMask::from_mesh_group(&mesh, &group, 16);

        assert_eq!(mask.data.len(), 16 * 16);

        // Center of triangle should be true
        assert!(mask.sample(Vec2::new(0.1, 0.3)));
        // Far right should be false (outside triangle)
        assert!(!mask.sample(Vec2::new(0.9, 0.5)));
    }

    #[test]
    fn mask_sample_at_boundaries() {
        let (mesh, group) = make_half_mesh();
        let mask = UvMask::from_mesh_group(&mesh, &group, 64);

        // On the hypotenuse (u + 2v ≈ 0.5 scaled), points near the edge
        assert!(mask.sample(Vec2::new(0.05, 0.05))); // clearly inside
        assert!(!mask.sample(Vec2::new(0.9, 0.9))); // clearly outside
    }

    #[test]
    fn mask_aabb() {
        let (mesh, group) = make_half_mesh();
        let mask = UvMask::from_mesh_group(&mesh, &group, 32);
        let (lo, hi) = mask.aabb();

        assert!(lo.x < 0.1);
        assert!(lo.y < 0.1);
        assert!(hi.x > 0.3);
        assert!(hi.y > 0.8);
    }

    #[test]
    fn mask_full_all_true() {
        let mask = UvMask::full(16);
        assert!(mask.data.iter().all(|&v| v));
        assert!(mask.sample(Vec2::new(0.5, 0.5)));
        let (lo, hi) = mask.aabb();
        assert!(lo.x < 0.1 && lo.y < 0.1);
        assert!(hi.x > 0.9 && hi.y > 0.9);
    }

    #[test]
    fn mask_dilate_expands() {
        let (mesh, group) = make_half_mesh();
        let mut mask = UvMask::from_mesh_group(&mesh, &group, 32);
        let count_before: usize = mask.data.iter().filter(|&&v| v).count();

        mask.dilate(2);
        let count_after: usize = mask.data.iter().filter(|&&v| v).count();

        assert!(
            count_after > count_before,
            "dilate should expand: before={count_before}, after={count_after}"
        );
    }

    #[test]
    fn two_groups_cover_full_uv() {
        let (mesh, g0, g1) = make_full_quad_mesh();
        let mask0 = UvMask::from_mesh_group(&mesh, &g0, 32);
        let mask1 = UvMask::from_mesh_group(&mesh, &g1, 32);

        // Union should cover most of the UV space
        let union_count: usize = mask0
            .data
            .iter()
            .zip(mask1.data.iter())
            .filter(|(&a, &b)| a || b)
            .count();

        let total = 32 * 32;
        let coverage = union_count as f32 / total as f32;
        assert!(
            coverage > 0.9,
            "union of two groups should cover >90% of UV: {:.1}%",
            coverage * 100.0
        );
    }

    #[test]
    fn empty_group_produces_empty_mask() {
        let mesh = LoadedMesh {
            positions: vec![Vec3::ZERO; 3],
            uvs: vec![Vec2::ZERO; 3],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        };
        let group = MeshGroup {
            name: "empty".to_string(),
            index_offset: 0,
            index_count: 0,
        };
        let mask = UvMask::from_mesh_group(&mesh, &group, 16);
        assert!(mask.data.iter().all(|&v| !v));

        let (lo, hi) = mask.aabb();
        assert_eq!(lo, Vec2::ZERO);
        assert_eq!(hi, Vec2::ZERO);
    }

    #[test]
    fn distance_field_correctness() {
        // 8×8 mask with a single true pixel at (4, 4)
        let res = 8u32;
        let mut data = vec![false; (res * res) as usize];
        data[(4 * res + 4) as usize] = true;
        let mask = UvMask {
            data,
            resolution: res,
        };

        let df = mask.distance_field();

        // The true pixel should have distance 0
        assert_eq!(df.data[(4 * res + 4) as usize], 0.0);

        // Adjacent pixels should have distance 1.0
        assert!(
            (df.data[(4 * res + 5) as usize] - 1.0).abs() < 0.01,
            "right neighbor dist={}, expected 1.0",
            df.data[(4 * res + 5) as usize]
        );
        assert!(
            (df.data[(3 * res + 4) as usize] - 1.0).abs() < 0.01,
            "top neighbor dist={}, expected 1.0",
            df.data[(3 * res + 4) as usize]
        );

        // Diagonal pixel: Manhattan distance = |dx| + |dy| = 1 + 1 = 2
        let diag = df.data[(3 * res + 3) as usize];
        assert!(
            (diag - 2.0).abs() < 0.01,
            "diagonal dist={diag}, expected 2.0 (Manhattan)"
        );

        // Corner (0,0): Manhattan distance = |4| + |4| = 8
        let corner = df.data[0];
        assert!(
            (corner - 8.0).abs() < 0.01,
            "corner dist={corner}, expected 8.0 (Manhattan)"
        );
    }

    #[test]
    fn distance_field_sample_threshold() {
        let (mesh, group) = make_half_mesh();
        let mask = UvMask::from_mesh_group(&mesh, &group, 32);
        let df = mask.distance_field();

        // Point well inside the triangle: distance should be 0
        assert!(df.sample(Vec2::new(0.1, 0.3), 0.0));
        // Point outside should fail at threshold 0 but pass at large threshold
        let outside = Vec2::new(0.9, 0.5);
        assert!(!df.sample(outside, 0.0));
        assert!(df.sample(outside, 100.0));
    }
}
