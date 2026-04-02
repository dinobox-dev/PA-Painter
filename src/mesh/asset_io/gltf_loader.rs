//! glTF/GLB mesh loading.

use glam::{Vec2, Vec3};

use super::{srgb_to_linear, LoadedMesh, LoadedTexture, MeshError, MeshMaterialInfo};
use crate::types::MeshGroup;

// ---------------------------------------------------------------------------
// glTF loading
// ---------------------------------------------------------------------------

pub(super) fn load_gltf(path: &std::path::Path) -> Result<LoadedMesh, MeshError> {
    let (document, buffers, images) =
        gltf::import(path).map_err(|e| MeshError::ParseError(e.to_string()))?;
    build_mesh_from_gltf(document, buffers, images)
}

pub(super) fn load_gltf_from_bytes(bytes: &[u8]) -> Result<LoadedMesh, MeshError> {
    let (document, buffers, images) =
        gltf::import_slice(bytes).map_err(|e| MeshError::ParseError(e.to_string()))?;
    build_mesh_from_gltf(document, buffers, images)
}

fn build_mesh_from_gltf(
    document: gltf::Document,
    buffers: Vec<gltf::buffer::Data>,
    images: Vec<gltf::image::Data>,
) -> Result<LoadedMesh, MeshError> {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let mut groups = Vec::new();
    let mut materials = Vec::new();
    let mut prim_idx = 0u32;

    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));

            let prim_positions: Vec<Vec3> = reader
                .read_positions()
                .ok_or_else(|| MeshError::ParseError("No position data".to_string()))?
                .map(Vec3::from)
                .collect();

            let prim_uvs: Vec<Vec2> = reader
                .read_tex_coords(0)
                .ok_or(MeshError::NoUvChannel)?
                .into_f32()
                .map(Vec2::from)
                .collect();

            if prim_uvs.is_empty() {
                return Err(MeshError::NoUvChannel);
            }

            let prim_indices: Vec<u32> = reader
                .read_indices()
                .ok_or_else(|| MeshError::ParseError("No index data".to_string()))?
                .into_u32()
                .collect();

            let base_vertex = positions.len() as u32;
            let index_offset = indices.len() as u32;

            positions.extend_from_slice(&prim_positions);
            uvs.extend_from_slice(&prim_uvs);
            for &idx in &prim_indices {
                indices.push(base_vertex + idx);
            }

            let index_count = prim_indices.len() as u32;

            let material = primitive.material();
            let group_name = material
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("primitive_{prim_idx}"));

            // Extract material info
            let pbr = material.pbr_metallic_roughness();
            let base_color_factor = pbr.base_color_factor();
            let base_color_texture = pbr
                .base_color_texture()
                .map(|info| gltf_image_to_texture_srgb(&images[info.texture().source().index()]));
            let normal_texture = material
                .normal_texture()
                .map(|info| gltf_image_to_texture_linear(&images[info.texture().source().index()]));

            let mat_name = material
                .name()
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("material_{prim_idx}"));

            let f = base_color_factor;
            let is_glb_default =
                (f[0] - 1.0).abs() < 0.01 && (f[1] - 1.0).abs() < 0.01 && (f[2] - 1.0).abs() < 0.01;
            let has_explicit_color = base_color_texture.is_some() || !is_glb_default;
            materials.push(MeshMaterialInfo {
                name: mat_name,
                base_color_factor,
                has_explicit_color,
                base_color_texture,
                normal_texture,
            });

            groups.push(MeshGroup {
                name: group_name,
                index_offset,
                index_count,
            });

            prim_idx += 1;
        }
    }

    if positions.is_empty() {
        return Err(MeshError::ParseError("No meshes in glTF file".to_string()));
    }

    Ok(LoadedMesh {
        positions,
        uvs,
        indices,
        groups,
        materials,
    })
}

// ---------------------------------------------------------------------------
// glTF image → texture conversion
// ---------------------------------------------------------------------------

/// Convert a glTF image to a LoadedTexture, applying sRGB → linear conversion.
/// Used for base color textures which are stored in sRGB color space per glTF spec.
fn gltf_image_to_texture_srgb(data: &gltf::image::Data) -> LoadedTexture {
    let w = data.width;
    let h = data.height;
    let pixel_count = (w * h) as usize;
    let mut pixels = Vec::with_capacity(pixel_count);

    match data.format {
        gltf::image::Format::R8G8B8 => {
            for chunk in data.pixels.chunks_exact(3) {
                pixels.push([
                    srgb_to_linear(chunk[0] as f32 / 255.0),
                    srgb_to_linear(chunk[1] as f32 / 255.0),
                    srgb_to_linear(chunk[2] as f32 / 255.0),
                    1.0,
                ]);
            }
        }
        gltf::image::Format::R8G8B8A8 => {
            for chunk in data.pixels.chunks_exact(4) {
                pixels.push([
                    srgb_to_linear(chunk[0] as f32 / 255.0),
                    srgb_to_linear(chunk[1] as f32 / 255.0),
                    srgb_to_linear(chunk[2] as f32 / 255.0),
                    chunk[3] as f32 / 255.0, // Alpha is linear
                ]);
            }
        }
        _ => {
            // Unsupported format: fill with mid-gray
            pixels.resize(pixel_count, [0.5, 0.5, 0.5, 1.0]);
        }
    }

    LoadedTexture {
        pixels,
        width: w,
        height: h,
    }
}

/// Convert a glTF image to a LoadedTexture without gamma conversion.
/// Used for normal maps which are stored in linear color space per glTF spec.
fn gltf_image_to_texture_linear(data: &gltf::image::Data) -> LoadedTexture {
    let w = data.width;
    let h = data.height;
    let pixel_count = (w * h) as usize;
    let mut pixels = Vec::with_capacity(pixel_count);

    match data.format {
        gltf::image::Format::R8G8B8 => {
            for chunk in data.pixels.chunks_exact(3) {
                pixels.push([
                    chunk[0] as f32 / 255.0,
                    chunk[1] as f32 / 255.0,
                    chunk[2] as f32 / 255.0,
                    1.0,
                ]);
            }
        }
        gltf::image::Format::R8G8B8A8 => {
            for chunk in data.pixels.chunks_exact(4) {
                pixels.push([
                    chunk[0] as f32 / 255.0,
                    chunk[1] as f32 / 255.0,
                    chunk[2] as f32 / 255.0,
                    chunk[3] as f32 / 255.0,
                ]);
            }
        }
        _ => {
            // Unsupported format: fill with flat normal (0.5, 0.5, 1.0)
            pixels.resize(pixel_count, [0.5, 0.5, 1.0, 1.0]);
        }
    }

    LoadedTexture {
        pixels,
        width: w,
        height: h,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use crate::mesh::asset_io::load_mesh;

    fn fixtures_dir() -> std::path::PathBuf {
        crate::test_fixtures_dir()
    }

    #[test]
    fn load_glb_mesh() {
        let path = fixtures_dir().join("cube_binary.glb");
        let mesh = load_mesh(&path).unwrap();

        assert!(!mesh.positions.is_empty(), "positions should not be empty");
        assert!(!mesh.uvs.is_empty(), "uvs should not be empty");
        assert_eq!(mesh.indices.len() % 3, 0, "indices must be multiple of 3");
        assert_eq!(mesh.positions.len(), mesh.uvs.len());

        for &idx in &mesh.indices {
            assert!(
                (idx as usize) < mesh.positions.len(),
                "Index {idx} out of bounds"
            );
        }
    }

    #[test]
    fn load_gltf_mesh() {
        let path = fixtures_dir().join("cube_text.gltf");
        let mesh = load_mesh(&path).unwrap();

        assert!(!mesh.positions.is_empty());
        assert!(!mesh.uvs.is_empty());
        assert_eq!(mesh.indices.len() % 3, 0);

        for &idx in &mesh.indices {
            assert!((idx as usize) < mesh.positions.len());
        }
    }

    #[test]
    fn glb_and_gltf_produce_same_data() {
        let glb = load_mesh(&fixtures_dir().join("cube_binary.glb")).unwrap();
        let gltf = load_mesh(&fixtures_dir().join("cube_text.gltf")).unwrap();

        assert_eq!(glb.positions.len(), gltf.positions.len());
        assert_eq!(glb.uvs.len(), gltf.uvs.len());
        assert_eq!(glb.indices.len(), gltf.indices.len());
        assert_eq!(glb.indices, gltf.indices);
    }
}
