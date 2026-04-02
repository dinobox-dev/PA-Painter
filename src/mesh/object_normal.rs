//! Object-space normal computation — rasterizes mesh normals and tangent frames
//! into UV space for the *Depicted Form* normal mode.

use glam::{Vec2, Vec3};
use log::debug;

use crate::mesh::asset_io::LoadedMesh;

// ── MikkTSpace interface ──

/// Adapter for the `mikktspace` crate's `Geometry` trait.
///
/// Wraps a `LoadedMesh` plus pre-computed vertex normals and collects
/// per-face-vertex tangent/bitangent output from the MikkTSpace algorithm.
struct MikkTSpaceInput<'a> {
    mesh: &'a LoadedMesh,
    vertex_normals: &'a [Vec3],
    /// Per-face-vertex tangents, indexed as `[face * 3 + local_vert]`.
    face_tangents: Vec<Vec3>,
    /// Per-face-vertex bitangents, indexed as `[face * 3 + local_vert]`.
    face_bitangents: Vec<Vec3>,
}

impl<'a> mikktspace::Geometry for MikkTSpaceInput<'a> {
    fn num_faces(&self) -> usize {
        self.mesh.indices.len() / 3
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        self.mesh.positions[idx].into()
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        self.vertex_normals[idx].into()
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        let idx = self.mesh.indices[face * 3 + vert] as usize;
        self.mesh.uvs[idx].into()
    }

    fn set_tangent(
        &mut self,
        tangent: [f32; 3],
        bi_tangent: [f32; 3],
        _f_mag_s: f32,
        _f_mag_t: f32,
        _bi_tangent_preserves_orientation: bool,
        face: usize,
        vert: usize,
    ) {
        let fv = face * 3 + vert;
        self.face_tangents[fv] = Vec3::from(tangent);
        self.face_bitangents[fv] = Vec3::from(bi_tangent);
    }
}

/// Pre-computed mesh normal data rasterized into UV space.
///
/// Each buffer is `resolution * resolution` in size, row-major.
pub struct MeshNormalData {
    /// Interpolated vertex normals in object space, rasterized to UV grid.
    pub object_normals: Vec<Vec3>,
    /// Tangent vectors (T) in object space.
    pub tangents: Vec<Vec3>,
    /// Bitangent vectors (B) in object space.
    pub bitangents: Vec<Vec3>,
    pub resolution: u32,
}

/// Compute mesh normal data from a loaded mesh, rasterized to a UV-space grid.
///
/// Algorithm:
/// 1. Compute area-weighted vertex normals from face normals.
/// 2. Compute MikkTSpace tangents per face-vertex.
/// 3. Rasterize triangles in UV space using barycentric interpolation.
pub fn compute_mesh_normal_data(mesh: &LoadedMesh, resolution: u32) -> MeshNormalData {
    debug!(
        "Computing mesh normal data: {} triangles, {}×{} resolution",
        mesh.indices.len() / 3,
        resolution,
        resolution
    );
    let size = (resolution * resolution) as usize;
    let mut object_normals = vec![Vec3::ZERO; size];
    let mut tangents = vec![Vec3::ZERO; size];
    let mut bitangents = vec![Vec3::ZERO; size];

    // Step 1: Compute area-weighted vertex normals
    let vertex_normals = compute_vertex_normals(mesh);

    // Step 2: Compute MikkTSpace tangents (per-face-vertex)
    let face_count = mesh.indices.len() / 3;
    let mut mikk = MikkTSpaceInput {
        mesh,
        vertex_normals: &vertex_normals,
        face_tangents: vec![Vec3::ZERO; face_count * 3],
        face_bitangents: vec![Vec3::ZERO; face_count * 3],
    };

    if !mikktspace::generate_tangents(&mut mikk) {
        debug!("MikkTSpace generation failed; tangents will be zero");
    }

    // Step 3: Rasterize each triangle in UV space with per-face-vertex interpolation
    for (face_idx, tri) in mesh.indices.chunks_exact(3).enumerate() {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let fv = face_idx * 3;

        rasterize_triangle_uv(
            mesh.uvs[i0],
            mesh.uvs[i1],
            mesh.uvs[i2],
            [vertex_normals[i0], vertex_normals[i1], vertex_normals[i2]],
            [
                mikk.face_tangents[fv],
                mikk.face_tangents[fv + 1],
                mikk.face_tangents[fv + 2],
            ],
            [
                mikk.face_bitangents[fv],
                mikk.face_bitangents[fv + 1],
                mikk.face_bitangents[fv + 2],
            ],
            resolution,
            &mut object_normals,
            &mut tangents,
            &mut bitangents,
        );
    }

    // Normalize all rasterized normals (pixels hit by multiple triangles get averaged)
    for i in 0..size {
        let n = object_normals[i];
        if n.length_squared() > 1e-12 {
            let n = n.normalize();
            object_normals[i] = n;
            // Re-orthogonalize after barycentric interpolation.
            // Gram-Schmidt T against N, then derive B via cross product
            // preserving the handedness that MikkTSpace encoded.
            let t = tangents[i];
            let b = bitangents[i];
            if t.length_squared() > 1e-12 {
                let t_ortho = (t - n.dot(t) * n).normalize_or_zero();
                let handedness = if t.cross(b).dot(n) > 0.0 { 1.0 } else { -1.0 };
                tangents[i] = t_ortho;
                bitangents[i] = n.cross(t_ortho) * handedness;
            }
        }
    }

    MeshNormalData {
        object_normals,
        tangents,
        bitangents,
        resolution,
    }
}

/// Sample the object-space normal at a UV coordinate using nearest-neighbor lookup.
pub fn sample_object_normal(data: &MeshNormalData, uv: Vec2) -> Vec3 {
    let res = data.resolution;
    let px = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let py = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let idx = (py * res + px) as usize;
    let n = data.object_normals[idx];
    if n.length_squared() > 1e-12 {
        n
    } else {
        Vec3::Z // fallback for unpainted pixels
    }
}

/// Sample the object-space normal at a UV coordinate, returning `None` when
/// there is no mesh coverage at that location.
pub fn try_sample_object_normal(data: &MeshNormalData, uv: Vec2) -> Option<Vec3> {
    let res = data.resolution;
    let px = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let py = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let idx = (py * res + px) as usize;
    let n = data.object_normals[idx];
    if n.length_squared() > 1e-12 {
        Some(n)
    } else {
        None
    }
}

/// Sample the TBN basis at a UV coordinate.
pub fn sample_tbn(data: &MeshNormalData, uv: Vec2) -> (Vec3, Vec3, Vec3) {
    let res = data.resolution;
    let px = ((uv.x * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let py = ((uv.y * res as f32).floor() as i32).clamp(0, res as i32 - 1) as u32;
    let idx = (py * res + px) as usize;

    let n = data.object_normals[idx];
    let t = data.tangents[idx];
    let b = data.bitangents[idx];

    if n.length_squared() > 1e-12 {
        (t, b, n)
    } else {
        (Vec3::X, Vec3::Y, Vec3::Z)
    }
}

// ── Internal helpers ──

/// Compute area-weighted vertex normals from face normals.
pub fn compute_vertex_normals(mesh: &LoadedMesh) -> Vec<Vec3> {
    let mut normals = vec![Vec3::ZERO; mesh.positions.len()];

    for tri in mesh.indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let e1 = mesh.positions[i1] - mesh.positions[i0];
        let e2 = mesh.positions[i2] - mesh.positions[i0];
        // Cross product magnitude = 2x triangle area (area weighting)
        let face_normal = e1.cross(e2);
        normals[i0] += face_normal;
        normals[i1] += face_normal;
        normals[i2] += face_normal;
    }

    for n in &mut normals {
        if n.length_squared() > 1e-12 {
            *n = n.normalize();
        } else {
            *n = Vec3::Z;
        }
    }

    normals
}

/// Compute tangent and bitangent for a triangle from UV gradients.
///
/// Standard formula: T = (E1*dV2 - E2*dV1) / det, B = (E2*dU1 - E1*dU2) / det
#[cfg(test)]
fn compute_triangle_tbn(
    p0: Vec3,
    p1: Vec3,
    p2: Vec3,
    uv0: Vec2,
    uv1: Vec2,
    uv2: Vec2,
) -> (Vec3, Vec3) {
    let e1 = p1 - p0;
    let e2 = p2 - p0;
    let duv1 = uv1 - uv0;
    let duv2 = uv2 - uv0;

    let det = duv1.x * duv2.y - duv1.y * duv2.x;

    if det.abs() < 1e-10 {
        // Degenerate UV mapping; pick arbitrary tangent frame
        return (Vec3::X, Vec3::Y);
    }

    let inv_det = 1.0 / det;
    let t = (e1 * duv2.y - e2 * duv1.y) * inv_det;
    let b = (e2 * duv1.x - e1 * duv2.x) * inv_det;

    (t.normalize_or_zero(), b.normalize_or_zero())
}

/// Barycentric coordinates for point p in triangle (a, b, c).
/// Returns (u, v, w) where p = u*a + v*b + w*c.
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

    let inv_denom = 1.0 / denom;
    let v = (d11 * d20 - d01 * d21) * inv_denom;
    let w = (d00 * d21 - d01 * d20) * inv_denom;
    let u = 1.0 - v - w;

    (u, v, w)
}

/// Rasterize a single UV-space triangle, writing interpolated normals/tangents/bitangents.
///
/// All three attributes (N, T, B) are interpolated per-vertex using barycentric
/// coordinates, ensuring smooth transitions across triangle boundaries.
#[allow(clippy::too_many_arguments)]
fn rasterize_triangle_uv(
    uv0: Vec2,
    uv1: Vec2,
    uv2: Vec2,
    normals_vtx: [Vec3; 3],
    tangents_vtx: [Vec3; 3],
    bitangents_vtx: [Vec3; 3],
    resolution: u32,
    object_normals: &mut [Vec3],
    tangents: &mut [Vec3],
    bitangents: &mut [Vec3],
) {
    let res_f = resolution as f32;

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

            let (u, v, w) = barycentric(uv, uv0, uv1, uv2);

            // Inside triangle check (with small epsilon for edge coverage)
            const EDGE_EPS: f32 = -1e-4;
            if u < EDGE_EPS || v < EDGE_EPS || w < EDGE_EPS {
                continue;
            }

            let idx = (py * resolution + px) as usize;

            // Interpolate all attributes using barycentric coords
            object_normals[idx] = normals_vtx[0] * u + normals_vtx[1] * v + normals_vtx[2] * w;
            tangents[idx] = tangents_vtx[0] * u + tangents_vtx[1] * v + tangents_vtx[2] * w;
            bitangents[idx] = bitangents_vtx[0] * u + bitangents_vtx[1] * v + bitangents_vtx[2] * w;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::asset_io::LoadedMesh;

    const EPS: f32 = 1e-4;

    fn make_xy_plane_mesh() -> LoadedMesh {
        // Single triangle in XY plane at Z=0
        LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        }
    }

    fn make_full_quad_mesh() -> LoadedMesh {
        // Two triangles covering the full UV [0,1]² square in XY plane
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

    // ── Vertex Normal Tests ──

    #[test]
    fn vertex_normals_single_triangle() {
        let mesh = make_xy_plane_mesh();
        let normals = compute_vertex_normals(&mesh);

        assert_eq!(normals.len(), 3);
        for n in &normals {
            // XY plane → all normals should be +Z
            assert!(
                (n.x).abs() < EPS && (n.y).abs() < EPS && (n.z - 1.0).abs() < EPS,
                "expected +Z normal, got ({:.4}, {:.4}, {:.4})",
                n.x,
                n.y,
                n.z
            );
        }
    }

    #[test]
    fn vertex_normals_shared_edge() {
        let mesh = make_full_quad_mesh();
        let normals = compute_vertex_normals(&mesh);

        assert_eq!(normals.len(), 4);
        for n in &normals {
            assert!(
                (n.z - 1.0).abs() < EPS,
                "flat quad: all normals should be +Z, got ({:.4}, {:.4}, {:.4})",
                n.x,
                n.y,
                n.z
            );
        }
    }

    // ── TBN Tests ──

    #[test]
    fn tbn_xy_plane() {
        let (t, b) = compute_triangle_tbn(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.0, 1.0),
        );

        // Standard UV mapping on XY plane → T=X, B=Y
        assert!(
            (t.x - 1.0).abs() < EPS && t.y.abs() < EPS && t.z.abs() < EPS,
            "expected T=X, got ({:.4}, {:.4}, {:.4})",
            t.x,
            t.y,
            t.z
        );
        assert!(
            b.x.abs() < EPS && (b.y - 1.0).abs() < EPS && b.z.abs() < EPS,
            "expected B=Y, got ({:.4}, {:.4}, {:.4})",
            b.x,
            b.y,
            b.z
        );
    }

    #[test]
    fn tbn_degenerate_uv() {
        // All UVs at the same point → degenerate
        let (t, b) = compute_triangle_tbn(
            Vec3::new(0.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec2::new(0.5, 0.5),
            Vec2::new(0.5, 0.5),
            Vec2::new(0.5, 0.5),
        );

        // Should return fallback (X, Y)
        assert!((t - Vec3::X).length() < EPS);
        assert!((b - Vec3::Y).length() < EPS);
    }

    // ── Barycentric Tests ──

    #[test]
    fn barycentric_inside() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(1.0, 0.0);
        let c = Vec2::new(0.0, 1.0);

        // Centroid
        let p = Vec2::new(1.0 / 3.0, 1.0 / 3.0);
        let (u, v, w) = barycentric(p, a, b, c);

        assert!(u > 0.0 && v > 0.0 && w > 0.0, "centroid should be inside");
        assert!(
            (u + v + w - 1.0).abs() < EPS,
            "barycentric coords should sum to 1"
        );
        assert!(
            (u - 1.0 / 3.0).abs() < 0.01
                && (v - 1.0 / 3.0).abs() < 0.01
                && (w - 1.0 / 3.0).abs() < 0.01,
            "centroid should have equal bary coords"
        );
    }

    #[test]
    fn barycentric_outside() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(1.0, 0.0);
        let c = Vec2::new(0.0, 1.0);

        let p = Vec2::new(1.0, 1.0); // outside triangle
        let (u, v, w) = barycentric(p, a, b, c);

        assert!(
            u < 0.0 || v < 0.0 || w < 0.0,
            "point outside should have negative bary coord"
        );
    }

    #[test]
    fn barycentric_vertex() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(1.0, 0.0);
        let c = Vec2::new(0.0, 1.0);

        let (u, _, _) = barycentric(a, a, b, c);
        assert!((u - 1.0).abs() < EPS, "vertex A should have u=1");

        let (_, v, _) = barycentric(b, a, b, c);
        assert!((v - 1.0).abs() < EPS, "vertex B should have v=1");

        let (_, _, w) = barycentric(c, a, b, c);
        assert!((w - 1.0).abs() < EPS, "vertex C should have w=1");
    }

    // ── Rasterization Tests ──

    #[test]
    fn rasterize_full_quad() {
        let mesh = make_full_quad_mesh();
        let data = compute_mesh_normal_data(&mesh, 16);

        assert_eq!(data.object_normals.len(), 16 * 16);

        // All pixels should have valid normals (covering full UV)
        let mut painted = 0;
        for n in &data.object_normals {
            if n.length_squared() > 0.5 {
                painted += 1;
                // Should be +Z for XY plane
                assert!(
                    (n.z - 1.0).abs() < 0.1,
                    "flat quad normal Z should be ~1.0, got {:.4}",
                    n.z
                );
            }
        }

        assert!(
            painted > (16 * 16) / 2,
            "full quad should cover most pixels, got {}/{}",
            painted,
            16 * 16
        );
    }

    #[test]
    fn rasterize_single_triangle_partial() {
        let mesh = make_xy_plane_mesh();
        let data = compute_mesh_normal_data(&mesh, 16);

        let mut painted = 0;
        for n in &data.object_normals {
            if n.length_squared() > 0.5 {
                painted += 1;
            }
        }

        // Triangle covers half the UV space
        let expected_half = (16 * 16) / 2;
        assert!(
            painted > expected_half / 3 && painted < expected_half + expected_half / 3,
            "single triangle should cover roughly half of UV, got {}/{}",
            painted,
            16 * 16
        );
    }

    // ── Sampling Tests ──

    #[test]
    fn sample_object_normal_center() {
        let mesh = make_full_quad_mesh();
        let data = compute_mesh_normal_data(&mesh, 32);

        let n = sample_object_normal(&data, Vec2::new(0.5, 0.5));
        assert!(
            (n.z - 1.0).abs() < 0.1,
            "center of flat quad should have +Z normal, got ({:.4}, {:.4}, {:.4})",
            n.x,
            n.y,
            n.z
        );
    }

    #[test]
    fn sample_object_normal_boundary() {
        let mesh = make_full_quad_mesh();
        let data = compute_mesh_normal_data(&mesh, 32);

        // Corners
        for uv in &[
            Vec2::new(0.0, 0.0),
            Vec2::new(0.99, 0.0),
            Vec2::new(0.0, 0.99),
            Vec2::new(0.99, 0.99),
        ] {
            let n = sample_object_normal(&data, *uv);
            assert!(
                n.length_squared() > 0.5,
                "boundary sample at ({:.2},{:.2}) should have valid normal",
                uv.x,
                uv.y
            );
        }
    }

    #[test]
    fn sample_tbn_orthogonal() {
        let mesh = make_full_quad_mesh();
        let data = compute_mesh_normal_data(&mesh, 32);

        let (t, b, n) = sample_tbn(&data, Vec2::new(0.5, 0.5));

        // T, B, N should be approximately orthogonal
        assert!(
            t.dot(b).abs() < 0.1,
            "T and B should be orthogonal, dot = {:.4}",
            t.dot(b)
        );
        assert!(
            t.dot(n).abs() < 0.1,
            "T and N should be orthogonal, dot = {:.4}",
            t.dot(n)
        );
        assert!(
            b.dot(n).abs() < 0.1,
            "B and N should be orthogonal, dot = {:.4}",
            b.dot(n)
        );
    }

    #[test]
    fn sample_unpainted_pixel_fallback() {
        // Create a mesh that doesn't cover most of UV space
        let mesh = LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(0.1, 0.0, 0.0),
                Vec3::new(0.0, 0.1, 0.0),
            ],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(0.05, 0.0),
                Vec2::new(0.0, 0.05),
            ],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        };

        let data = compute_mesh_normal_data(&mesh, 32);

        // Sample far from the tiny triangle
        let n = sample_object_normal(&data, Vec2::new(0.9, 0.9));
        // Should get fallback +Z
        assert!(
            (n - Vec3::Z).length() < EPS,
            "unpainted pixel should return +Z fallback"
        );
    }

    // ── Handedness Tests ──

    #[test]
    fn tbn_handedness_standard_uv() {
        // Standard (non-mirrored) UV: T×B should align with N (positive handedness)
        let mesh = make_full_quad_mesh();
        let data = compute_mesh_normal_data(&mesh, 32);

        let (t, b, n) = sample_tbn(&data, Vec2::new(0.5, 0.5));
        let cross = t.cross(b);
        assert!(
            cross.dot(n) > 0.5,
            "standard UV should have positive handedness, T×B·N = {:.4}",
            cross.dot(n)
        );
    }

    #[test]
    fn tbn_handedness_mirrored_uv() {
        // Mirrored UV: U axis flipped → T×B should be opposite to N (negative handedness)
        let mesh = LoadedMesh {
            positions: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 0.0, 0.0),
                Vec3::new(1.0, 1.0, 0.0),
                Vec3::new(0.0, 1.0, 0.0),
            ],
            uvs: vec![
                // U axis flipped: (1,0) → (0,0) → (0,1) → (1,1)
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 0.0),
                Vec2::new(0.0, 1.0),
                Vec2::new(1.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            groups: vec![],
            materials: vec![],
        };

        let data = compute_mesh_normal_data(&mesh, 32);
        let (t, b, n) = sample_tbn(&data, Vec2::new(0.5, 0.5));

        // With mirrored UVs, the bitangent should flip so T×B·N < 0
        let cross = t.cross(b);
        assert!(
            cross.dot(n) < -0.5,
            "mirrored UV should have negative handedness, T×B·N = {:.4}",
            cross.dot(n)
        );
    }

    // ── Cube Fixture Test ──

    #[test]
    fn cube_fixture_normals() {
        let fixtures = crate::test_fixtures_dir();
        let mesh = crate::mesh::asset_io::load_mesh(&fixtures.join("cube_binary.glb")).unwrap();
        let data = compute_mesh_normal_data(&mesh, 64);

        // All computed normals should be unit length (where they're non-zero)
        for n in &data.object_normals {
            if n.length_squared() > 0.5 {
                let len = n.length();
                assert!(
                    (len - 1.0).abs() < 0.01,
                    "normal should be unit length, got {:.4}",
                    len
                );
            }
        }
    }

    // ── Visual Tests ──

    #[test]
    fn visual_cube_normal_map_rgb() {
        let fixtures = crate::test_fixtures_dir();
        let mesh = crate::mesh::asset_io::load_mesh(&fixtures.join("cube_binary.glb")).unwrap();
        let res = 256u32;
        let data = compute_mesh_normal_data(&mesh, res);

        let out_dir = crate::test_module_output_dir("object_normal");

        // RGB normal map: encode object-space normal as color
        // N = (nx, ny, nz) → RGB = (nx*0.5+0.5, ny*0.5+0.5, nz*0.5+0.5)
        let pixels: Vec<u8> = data
            .object_normals
            .iter()
            .flat_map(|n| {
                if n.length_squared() > 0.5 {
                    [
                        (n.x * 0.5 + 0.5).clamp(0.0, 1.0),
                        (n.y * 0.5 + 0.5).clamp(0.0, 1.0),
                        (n.z * 0.5 + 0.5).clamp(0.0, 1.0),
                    ]
                } else {
                    [0.0, 0.0, 0.0] // unpainted = black
                }
                .map(|v| (v * 255.0) as u8)
            })
            .collect();

        let path = out_dir.join("cube_normal_rgb.png");
        image::save_buffer(&path, &pixels, res, res, image::ColorType::Rgb8)
            .expect("save cube_normal_rgb.png");
        eprintln!("Wrote: {}", path.display());
    }

    #[test]
    fn visual_cube_lambert_shading() {
        let fixtures = crate::test_fixtures_dir();
        let mesh = crate::mesh::asset_io::load_mesh(&fixtures.join("cube_binary.glb")).unwrap();
        let res = 256u32;
        let data = compute_mesh_normal_data(&mesh, res);

        let out_dir = crate::test_module_output_dir("object_normal");

        // Lambert shading: dot(normal, light_dir) → grayscale
        let light_dir = Vec3::new(0.577, 0.577, 0.577).normalize(); // 45° from top-right-front

        let pixels: Vec<u8> = data
            .object_normals
            .iter()
            .map(|n| {
                if n.length_squared() > 0.5 {
                    let ndotl = n.dot(light_dir).max(0.0);
                    // Add slight ambient so shadow areas aren't pure black
                    let brightness = (0.1 + 0.9 * ndotl).clamp(0.0, 1.0);
                    (brightness * 255.0) as u8
                } else {
                    0u8 // unpainted = black
                }
            })
            .collect();

        let path = out_dir.join("cube_lambert.png");
        image::save_buffer(&path, &pixels, res, res, image::ColorType::L8)
            .expect("save cube_lambert.png");
        eprintln!("Wrote: {}", path.display());
    }

    #[test]
    fn visual_depicted_form_normal_map() {
        use crate::compositing::composite_all;
        use crate::output::{
            export_normal_png, generate_normal_map, generate_normal_map_depicted_form,
        };
        use crate::types::{
            Guide, LayerBaseColor, NormalMode, NormalYConvention, OutputSettings, PaintLayer,
            StrokeParams,
        };

        let fixtures = crate::test_fixtures_dir();
        let mesh = crate::mesh::asset_io::load_mesh(&fixtures.join("cube_binary.glb")).unwrap();
        let res = 256u32;
        let nd = compute_mesh_normal_data(&mesh, res);

        let layer = PaintLayer {
            name: "test".to_string(),
            order: 0,
            params: StrokeParams {
                brush_width: 20.0,
                color_variation: 0.0,
                ..StrokeParams::default()
            },
            guides: vec![Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
                ..Guide::default()
            }],
        };

        let settings = OutputSettings {
            normal_mode: NormalMode::DepictedForm,
            ..OutputSettings::default()
        };

        let solid = crate::types::Color::rgb(0.5, 0.4, 0.3);
        let maps = composite_all(
            std::slice::from_ref(&layer),
            res,
            &[LayerBaseColor::solid(solid)],
            &settings,
            Some(&nd),
            &[],
            None,
            &[],
        );

        let out_dir = crate::test_module_output_dir("object_normal");

        // SurfacePaint normal map (tangent-space, for comparison)
        let normals_surface = generate_normal_map(
            &maps.gradient_x,
            &maps.gradient_y,
            res,
            settings.normal_strength,
        );
        export_normal_png(
            &normals_surface,
            res,
            &out_dir.join("surface_paint_normal.png"),
            NormalYConvention::OpenGL,
        )
        .expect("save surface_paint_normal.png");

        // DepictedForm normal map (tangent-space)
        let normals_depicted = generate_normal_map_depicted_form(
            &maps.gradient_x,
            &maps.gradient_y,
            &nd,
            &maps.object_normal,
            &maps.paint_load,
            res,
            settings.normal_strength,
        );
        export_normal_png(
            &normals_depicted,
            res,
            &out_dir.join("depicted_form_normal.png"),
            NormalYConvention::OpenGL,
        )
        .expect("save depicted_form_normal.png");

        // Three-point lighting: key + fill + rim covers all 6 cube faces.
        let lights: [(Vec3, f32); 3] = [
            (Vec3::new(1.0, 1.0, 1.0).normalize(), 0.45), // key  (+X,+Y,+Z faces)
            (Vec3::new(-1.0, 0.0, 0.5).normalize(), 0.30), // fill (-X faces)
            (Vec3::new(0.0, -1.0, 0.3).normalize(), 0.25), // rim  (-Y faces)
        ];
        let ambient = 0.12;

        // Helper: map pixel index to mesh normal data index
        let nd_index = |i: usize| -> usize {
            let px = i as u32 % res;
            let py = i as u32 / res;
            let nx = ((px as f32 + 0.5) / res as f32 * nd.resolution as f32).floor() as u32;
            let ny = ((py as f32 + 0.5) / res as f32 * nd.resolution as f32).floor() as u32;
            (ny.min(nd.resolution - 1) * nd.resolution + nx.min(nd.resolution - 1)) as usize
        };

        // Shared Lambert helper: multi-light + ambient
        let lambert = |obj_n: Vec3| -> f32 {
            let diffuse: f32 = lights
                .iter()
                .map(|(dir, intensity)| obj_n.dot(*dir).max(0.0) * intensity)
                .sum();
            (ambient + diffuse).clamp(0.0, 1.0)
        };

        // Shared: decode tangent-space normal and reconstruct object-space via TBN
        let reconstruct = |i: usize, n: &[f32; 3]| -> Option<Vec3> {
            let ts = Vec3::new(n[0] * 2.0 - 1.0, n[1] * 2.0 - 1.0, n[2] * 2.0 - 1.0);
            let ni = nd_index(i);
            let t = nd.tangents[ni];
            let b = nd.bitangents[ni];
            let n_geom = nd.object_normals[ni];
            if n_geom.length_squared() < 0.5 {
                return None; // no mesh coverage
            }
            Some((ts.x * t + ts.y * b + ts.z * n_geom).normalize_or_zero())
        };

        // === DepictedForm Lambert (form + texture) ===
        let depicted_lambert_pixels: Vec<u8> = normals_depicted
            .iter()
            .enumerate()
            .map(|(i, n)| match reconstruct(i, n) {
                Some(obj_n) => (lambert(obj_n) * 255.0) as u8,
                None => 0u8,
            })
            .collect();

        let path = out_dir.join("depicted_form_lambert.png");
        image::save_buffer(
            &path,
            &depicted_lambert_pixels,
            res,
            res,
            image::ColorType::L8,
        )
        .expect("save depicted_form_lambert.png");

        // === SurfacePaint Lambert (for A/B comparison) ===
        let surface_lambert_pixels: Vec<u8> = normals_surface
            .iter()
            .enumerate()
            .map(|(i, n)| match reconstruct(i, n) {
                Some(obj_n) => (lambert(obj_n) * 255.0) as u8,
                None => 0u8,
            })
            .collect();

        let path = out_dir.join("surface_paint_lambert.png");
        image::save_buffer(
            &path,
            &surface_lambert_pixels,
            res,
            res,
            image::ColorType::L8,
        )
        .expect("save surface_paint_lambert.png");

        // === Raw composited object-space normal RGB (diagnostic) ===
        let obj_normal_pixels: Vec<u8> = (0..(res * res) as usize)
            .flat_map(|i| {
                let sn = maps.object_normal[i];
                let n = Vec3::new(sn[0], sn[1], sn[2]);
                if n.length_squared() > 0.5 {
                    let nn = n.normalize();
                    [
                        ((nn.x * 0.5 + 0.5) * 255.0) as u8,
                        ((nn.y * 0.5 + 0.5) * 255.0) as u8,
                        ((nn.z * 0.5 + 0.5) * 255.0) as u8,
                    ]
                } else {
                    [0, 0, 0]
                }
            })
            .collect();

        let path = out_dir.join("depicted_obj_normal_rgb.png");
        image::save_buffer(&path, &obj_normal_pixels, res, res, image::ColorType::Rgb8)
            .expect("save depicted_obj_normal_rgb.png");

        eprintln!("=== Phase 4 Visual Outputs ===");
        eprintln!(
            "  {}/depicted_form_lambert.png    — DepictedForm: form + texture (KEY)",
            out_dir.display()
        );
        eprintln!(
            "  {}/surface_paint_lambert.png    — SurfacePaint: texture only (compare)",
            out_dir.display()
        );
        eprintln!(
            "  {}/depicted_form_normal.png     — DepictedForm tangent-space normal",
            out_dir.display()
        );
        eprintln!(
            "  {}/surface_paint_normal.png     — SurfacePaint tangent-space normal",
            out_dir.display()
        );
        eprintln!(
            "  {}/depicted_obj_normal_rgb.png  — raw composited stroke normals",
            out_dir.display()
        );
    }

    /// INSPECT: Normal break threshold ON vs OFF on a real cube mesh.
    /// "normal_break_off_lambert.png" — strokes cross face boundaries freely.
    /// "normal_break_on_lambert.png"  — strokes stop at hard-edge face boundaries.
    #[test]
    fn visual_normal_break_comparison() {
        use crate::compositing::composite_all;
        use crate::output::{
            export_normal_png, generate_normal_map_depicted_form, normalize_height_map,
        };
        use crate::types::{
            Guide, LayerBaseColor, NormalMode, NormalYConvention, OutputSettings, PaintLayer,
            StrokeParams,
        };

        let fixtures = crate::test_fixtures_dir();
        let mesh = crate::mesh::asset_io::load_mesh(&fixtures.join("cube_binary.glb")).unwrap();
        let res = 256u32;
        let nd = compute_mesh_normal_data(&mesh, res);

        let base_layer = PaintLayer {
            name: "test".to_string(),
            order: 0,
            params: StrokeParams {
                brush_width: 15.0,
                color_variation: 0.0,
                angle_variation: 3.0,
                max_turn_angle: 20.0,
                normal_break_threshold: None,
                ..StrokeParams::default()
            },
            guides: vec![Guide {
                position: Vec2::new(0.5, 0.5),
                direction: Vec2::X,
                influence: 1.5,
                ..Guide::default()
            }],
        };

        let mut layer_on = base_layer.clone();
        layer_on.params.normal_break_threshold = Some(0.5);

        let settings = OutputSettings {
            normal_mode: NormalMode::DepictedForm,
            ..OutputSettings::default()
        };
        let solid = crate::types::Color::rgb(0.5, 0.4, 0.3);

        let base_colors = vec![LayerBaseColor::solid(solid)];
        let maps_off = composite_all(
            std::slice::from_ref(&base_layer),
            res,
            &base_colors,
            &settings,
            Some(&nd),
            &[],
            None,
            &[],
        );
        let maps_on = composite_all(
            std::slice::from_ref(&layer_on),
            res,
            &base_colors,
            &settings,
            Some(&nd),
            &[],
            None,
            &[],
        );

        let out_dir = crate::test_module_output_dir("object_normal");

        // Three-point lighting (same as depicted_form test)
        let lights: [(Vec3, f32); 3] = [
            (Vec3::new(1.0, 1.0, 1.0).normalize(), 0.45),
            (Vec3::new(-1.0, 0.0, 0.5).normalize(), 0.30),
            (Vec3::new(0.0, -1.0, 0.3).normalize(), 0.25),
        ];
        let ambient = 0.12;

        let nd_index = |i: usize| -> usize {
            let px = i as u32 % res;
            let py = i as u32 / res;
            let nx = ((px as f32 + 0.5) / res as f32 * nd.resolution as f32).floor() as u32;
            let ny = ((py as f32 + 0.5) / res as f32 * nd.resolution as f32).floor() as u32;
            (ny.min(nd.resolution - 1) * nd.resolution + nx.min(nd.resolution - 1)) as usize
        };

        let lambert = |obj_n: Vec3| -> f32 {
            let diffuse: f32 = lights
                .iter()
                .map(|(dir, intensity)| obj_n.dot(*dir).max(0.0) * intensity)
                .sum();
            (ambient + diffuse).clamp(0.0, 1.0)
        };

        let reconstruct = |i: usize, n: &[f32; 3]| -> Option<Vec3> {
            let ts = Vec3::new(n[0] * 2.0 - 1.0, n[1] * 2.0 - 1.0, n[2] * 2.0 - 1.0);
            let ni = nd_index(i);
            let t = nd.tangents[ni];
            let b = nd.bitangents[ni];
            let n_geom = nd.object_normals[ni];
            if n_geom.length_squared() < 0.5 {
                return None;
            }
            Some((ts.x * t + ts.y * b + ts.z * n_geom).normalize_or_zero())
        };

        // Render both variants
        for (label, maps, _layer) in [
            ("normal_break_off", &maps_off, &base_layer),
            ("normal_break_on", &maps_on, &layer_on),
        ] {
            let normalized_height = normalize_height_map(&maps.height);
            let normals = generate_normal_map_depicted_form(
                &maps.gradient_x,
                &maps.gradient_y,
                &nd,
                &maps.object_normal,
                &maps.paint_load,
                res,
                settings.normal_strength,
            );

            // Lambert shading
            let lambert_pixels: Vec<u8> = normals
                .iter()
                .enumerate()
                .map(|(i, n)| match reconstruct(i, n) {
                    Some(obj_n) => (lambert(obj_n) * 255.0) as u8,
                    None => 0u8,
                })
                .collect();

            let path = out_dir.join(format!("{label}_lambert.png"));
            image::save_buffer(&path, &lambert_pixels, res, res, image::ColorType::L8)
                .expect("save lambert");

            // Normal map
            let path = out_dir.join(format!("{label}_normal.png"));
            export_normal_png(&normals, res, &path, NormalYConvention::OpenGL)
                .expect("save normal");

            // Raw object-space stroke normals (diagnostic)
            let obj_pixels: Vec<u8> = (0..(res * res) as usize)
                .flat_map(|i| {
                    let sn = maps.object_normal[i];
                    let n = Vec3::new(sn[0], sn[1], sn[2]);
                    if n.length_squared() > 0.5 {
                        let nn = n.normalize();
                        [
                            ((nn.x * 0.5 + 0.5) * 255.0) as u8,
                            ((nn.y * 0.5 + 0.5) * 255.0) as u8,
                            ((nn.z * 0.5 + 0.5) * 255.0) as u8,
                        ]
                    } else {
                        [0, 0, 0]
                    }
                })
                .collect();

            let path = out_dir.join(format!("{label}_obj_normal_rgb.png"));
            image::save_buffer(&path, &obj_pixels, res, res, image::ColorType::Rgb8)
                .expect("save obj_normal_rgb");

            // GLB 3D preview
            crate::glb_export::export_preview_glb(&crate::glb_export::GlbExportParams {
                mesh: &mesh,
                color_map: &maps.color,
                height_map: &normalized_height,
                normal_map: &normals,
                resolution: res,
                displacement_scale: 0.05,
                path: &out_dir.join(format!("{label}.glb")),
                normal_y: NormalYConvention::OpenGL,
                alpha_blend: false,
            })
            .expect("save GLB preview");
        }

        eprintln!("=== Normal Break Comparison ===");
        eprintln!(
            "  {}/normal_break_off_lambert.png  — no threshold (strokes cross faces)",
            out_dir.display()
        );
        eprintln!(
            "  {}/normal_break_on_lambert.png   — threshold=0.5 (strokes stop at edges)",
            out_dir.display()
        );
    }
}
