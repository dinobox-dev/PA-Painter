//! Mesh and texture asset loading — OBJ, glTF/GLB mesh import and image I/O with
//! sRGB ↔ linear conversion.

mod gltf_loader;
mod obj;
mod texture;

pub use obj::{collect_obj_aux_files, ObjAuxFiles};
pub use texture::{
    decode_linear_png_bytes, decode_srgb_png_bytes, encode_pixels_as_linear_png,
    encode_pixels_as_srgb_png, load_texture,
};

use glam::{Vec2, Vec3};
use log::info;
use std::collections::HashSet;
use std::path::Path;

use crate::types::MeshGroup;

// ---------------------------------------------------------------------------
// Mesh types
// ---------------------------------------------------------------------------

/// A loaded and validated mesh, ready for use.
pub struct LoadedMesh {
    /// 3D vertex positions.
    pub positions: Vec<Vec3>,
    /// UV coordinates per vertex (single UV channel, 0-1 normalized).
    pub uvs: Vec<Vec2>,
    /// Triangle indices (always triangulated, even if source was quads).
    /// Length is always a multiple of 3.
    pub indices: Vec<u32>,
    /// Vertex groups (submeshes). Empty means whole mesh is one group.
    pub groups: Vec<MeshGroup>,
    /// Per-primitive material info extracted from GLB/glTF.
    /// Parallel to `groups` — `materials[i]` corresponds to `groups[i]`.
    /// Empty for OBJ meshes.
    pub materials: Vec<MeshMaterialInfo>,
}

/// Material information extracted from a GLB/glTF primitive.
pub struct MeshMaterialInfo {
    /// Material name (from glTF material, or generated).
    pub name: String,
    /// PBR base color factor (linear RGBA).
    pub base_color_factor: [f32; 4],
    /// Whether the color factor was explicitly specified (vs. format default).
    pub has_explicit_color: bool,
    /// Base color texture (sRGB source, stored as linear RGBA).
    pub base_color_texture: Option<LoadedTexture>,
    /// Normal map texture (linear source).
    pub normal_texture: Option<LoadedTexture>,
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Mesh has no UV channel")]
    NoUvChannel,
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
}

// ---------------------------------------------------------------------------
// Texture types
// ---------------------------------------------------------------------------

/// A loaded texture in linear float RGBA.
pub struct LoadedTexture {
    /// Pixel data in linear float RGBA [0, 1]. Row-major.
    pub pixels: Vec<[f32; 4]>,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TextureError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Decode error: {0}")]
    DecodeError(String),
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),
}

// ---------------------------------------------------------------------------
// Color space conversion
// ---------------------------------------------------------------------------

/// Convert a single sRGB channel value [0, 1] to linear.
pub fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Convert a linear channel value [0, 1] to sRGB.
pub fn linear_to_srgb(l: f32) -> f32 {
    if l <= 0.0031308 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

// ---------------------------------------------------------------------------
// Mesh loading
// ---------------------------------------------------------------------------

/// Load a mesh from file. Supports .obj, .gltf, .glb.
///
/// - Quads are automatically triangulated (split along shortest diagonal).
/// - Validates that a UV channel exists.
/// - Does NOT validate non-overlapping UVs (too expensive; left to user).
pub fn load_mesh(path: &Path) -> Result<LoadedMesh, MeshError> {
    info!("Loading mesh: {}", path.display());
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("obj") => obj::load_obj(path),
        Some("gltf") | Some("glb") => gltf_loader::load_gltf(path),
        Some(ext) => Err(MeshError::UnsupportedFormat(ext.to_string())),
        None => Err(MeshError::UnsupportedFormat("no extension".to_string())),
    }
}

/// Load a mesh from raw bytes. `format` should be `"obj"`, `"gltf"`, or `"glb"`.
pub fn load_mesh_from_bytes(bytes: &[u8], format: &str) -> Result<LoadedMesh, MeshError> {
    match format {
        "obj" => obj::load_obj_from_bytes(bytes),
        "gltf" | "glb" => gltf_loader::load_gltf_from_bytes(bytes),
        other => Err(MeshError::UnsupportedFormat(other.to_string())),
    }
}

/// Load a mesh from raw bytes with auxiliary OBJ files (MTL + textures).
/// Falls back to plain `load_mesh_from_bytes` for non-OBJ formats or missing aux data.
pub fn load_mesh_from_bytes_with_aux(
    bytes: &[u8],
    format: &str,
    aux: Option<&ObjAuxFiles>,
) -> Result<LoadedMesh, MeshError> {
    match (format, aux) {
        ("obj", Some(aux)) => obj::load_obj_from_bytes_with_aux(bytes, aux),
        _ => load_mesh_from_bytes(bytes, format),
    }
}

// ---------------------------------------------------------------------------
// UV edge extraction
// ---------------------------------------------------------------------------

/// Extract UV-space edges for wireframe visualization.
///
/// Returns pairs of Vec2 representing line segments in UV space.
/// Duplicate edges are removed.
pub fn extract_uv_edges(mesh: &LoadedMesh) -> Vec<(Vec2, Vec2)> {
    let mut edge_set = HashSet::new();
    let mut edges = Vec::new();

    for tri in mesh.indices.chunks_exact(3) {
        let pairs = [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])];
        for (a, b) in pairs {
            let key = (a.min(b), a.max(b));
            if edge_set.insert(key) {
                edges.push((mesh.uvs[a as usize], mesh.uvs[b as usize]));
            }
        }
    }

    edges
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- sRGB conversion tests --

    #[test]
    fn srgb_roundtrip() {
        for i in 0..=255 {
            let s = i as f32 / 255.0;
            let roundtrip = linear_to_srgb(srgb_to_linear(s));
            assert!(
                (roundtrip - s).abs() < 1e-5,
                "Round-trip failed for sRGB {s}: got {roundtrip}"
            );
        }
    }

    #[test]
    fn srgb_midgray_to_linear() {
        // sRGB 128/255 ≈ 0.502, linear should be ≈ 0.2158
        let s = 128.0 / 255.0;
        let linear = srgb_to_linear(s);
        assert!(
            (linear - 0.2158).abs() < 0.001,
            "Expected ~0.2158, got {linear}"
        );
    }

    // -- Dispatcher edge-case tests --

    #[test]
    fn load_unsupported_mesh_format() {
        let path = Path::new("model.fbx");
        let result = load_mesh(path);
        assert!(matches!(result, Err(MeshError::UnsupportedFormat(_))));
    }

    #[test]
    fn load_nonexistent_mesh() {
        let path = Path::new("nonexistent.obj");
        let result = load_mesh(path);
        assert!(result.is_err());
    }

    // -- UV edge extraction tests --

    #[test]
    fn uv_edges_single_triangle() {
        let mesh = LoadedMesh {
            positions: vec![Vec3::ZERO; 3],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
            groups: vec![],
            materials: vec![],
        };
        let edges = extract_uv_edges(&mesh);
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn uv_edges_shared_edge_dedup() {
        // Two triangles sharing edge 1-2
        let mesh = LoadedMesh {
            positions: vec![Vec3::ZERO; 4],
            uvs: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(1.0, 0.0),
                Vec2::new(1.0, 1.0),
                Vec2::new(0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            groups: vec![],
            materials: vec![],
        };
        let edges = extract_uv_edges(&mesh);
        // 5 unique edges: 0-1, 1-2, 0-2, 2-3, 0-3
        assert_eq!(edges.len(), 5);
    }

    #[test]
    fn uv_edges_cube() {
        // A cube: 8 vertices, 6 faces = 12 triangles = 18 unique edges
        // For simplicity, construct a cube manually
        let positions = vec![
            Vec3::new(0.0, 0.0, 0.0), // 0
            Vec3::new(1.0, 0.0, 0.0), // 1
            Vec3::new(1.0, 1.0, 0.0), // 2
            Vec3::new(0.0, 1.0, 0.0), // 3
            Vec3::new(0.0, 0.0, 1.0), // 4
            Vec3::new(1.0, 0.0, 1.0), // 5
            Vec3::new(1.0, 1.0, 1.0), // 6
            Vec3::new(0.0, 1.0, 1.0), // 7
        ];
        let uvs = vec![Vec2::ZERO; 8]; // UVs don't matter for edge count test

        // 6 faces, each as 2 triangles
        #[rustfmt::skip]
        let indices = vec![
            // front
            0, 1, 2, 0, 2, 3,
            // back
            5, 4, 7, 5, 7, 6,
            // top
            3, 2, 6, 3, 6, 7,
            // bottom
            4, 5, 1, 4, 1, 0,
            // right
            1, 5, 6, 1, 6, 2,
            // left
            4, 0, 3, 4, 3, 7,
        ];

        let mesh = LoadedMesh {
            positions,
            uvs,
            indices,
            groups: vec![],
            materials: vec![],
        };
        let edges = extract_uv_edges(&mesh);
        assert_eq!(edges.len(), 18);
    }

    // -- OBJ ↔ bytes+aux integration test --

    /// Regression: loading Demo.obj from disk vs from bytes+aux must
    /// produce identical group names and material counts.
    #[test]
    fn demo_obj_disk_vs_bytes_aux_groups_match() {
        let obj_path = crate::test_fixtures_dir().join("usemtl_disambiguate.obj");

        let disk_mesh = load_mesh(&obj_path).expect("disk load failed");
        let disk_groups: Vec<&str> = disk_mesh.groups.iter().map(|g| g.name.as_str()).collect();

        let obj_bytes = std::fs::read(&obj_path).unwrap();
        let aux = collect_obj_aux_files(&obj_path);
        let aux_mesh = load_mesh_from_bytes_with_aux(&obj_bytes, "obj", aux.as_ref())
            .expect("bytes+aux load failed");
        let aux_groups: Vec<&str> = aux_mesh.groups.iter().map(|g| g.name.as_str()).collect();

        assert_eq!(disk_groups, aux_groups, "Group names must match");
        assert_eq!(
            disk_mesh.materials.len(),
            aux_mesh.materials.len(),
            "Material counts must match"
        );
    }
}
