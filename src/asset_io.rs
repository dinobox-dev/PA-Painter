//! Mesh and texture asset loading — OBJ, glTF/GLB mesh import and image I/O with
//! sRGB ↔ linear conversion.

use glam::{Vec2, Vec3};
use std::collections::HashSet;
use std::io::Cursor;
use std::path::{Path, PathBuf};

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
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("obj") => load_obj(path),
        Some("gltf") | Some("glb") => load_gltf(path),
        Some(ext) => Err(MeshError::UnsupportedFormat(ext.to_string())),
        None => Err(MeshError::UnsupportedFormat("no extension".to_string())),
    }
}

/// Load a mesh from raw bytes. `format` should be `"obj"`, `"gltf"`, or `"glb"`.
pub fn load_mesh_from_bytes(bytes: &[u8], format: &str) -> Result<LoadedMesh, MeshError> {
    match format {
        "obj" => load_obj_from_bytes(bytes),
        "gltf" | "glb" => load_gltf_from_bytes(bytes),
        other => Err(MeshError::UnsupportedFormat(other.to_string())),
    }
}

fn obj_load_options() -> tobj::LoadOptions {
    tobj::LoadOptions {
        triangulate: false, // We handle triangulation ourselves for shorter-diagonal split
        single_index: true,
        ..Default::default()
    }
}

fn load_obj(path: &Path) -> Result<LoadedMesh, MeshError> {
    let raw = std::fs::read(path)
        .map_err(|e| MeshError::ParseError(format!("Failed to open: {e}")))?;
    let text = String::from_utf8_lossy(&raw);
    let obj_dir = path.parent();

    // Try loading MTL — graceful fallback on failure
    let (models, raw_materials) = tobj::load_obj_buf(
        &mut Cursor::new(text.as_bytes()),
        &obj_load_options(),
        |mtl_path| {
            let full = if let Some(dir) = obj_dir {
                dir.join(mtl_path)
            } else {
                mtl_path.into()
            };
            let raw = std::fs::read(&full).unwrap_or_default();
            let mtl_text = String::from_utf8_lossy(&raw);
            tobj::load_mtl_buf(&mut Cursor::new(mtl_text.as_bytes()))
        },
    )
    .map_err(|e| MeshError::ParseError(e.to_string()))?;

    let mut mesh = build_mesh_from_obj(&models)?;

    // Convert MTL materials to MeshMaterialInfo
    if let Ok(materials) = raw_materials {
        mesh.materials = materials
            .iter()
            .enumerate()
            .map(|(i, mat)| mtl_to_material_info(mat, i, obj_dir))
            .collect();
    }

    Ok(mesh)
}

fn load_obj_from_bytes(bytes: &[u8]) -> Result<LoadedMesh, MeshError> {
    // OBJ files from some exporters (e.g. 3ds Max) may contain non-UTF-8
    // comments (EUC-KR, Shift-JIS, etc.). tobj uses BufRead::lines() which
    // requires valid UTF-8, so we sanitise first with lossy conversion.
    let text = String::from_utf8_lossy(bytes);
    let (models, _) = tobj::load_obj_buf(
        &mut Cursor::new(text.as_bytes()),
        &obj_load_options(),
        |_| Ok((vec![], Default::default())),
    )
    .map_err(|e| MeshError::ParseError(e.to_string()))?;
    build_mesh_from_obj(&models)
}

fn build_mesh_from_obj(models: &[tobj::Model]) -> Result<LoadedMesh, MeshError> {
    let mut positions = Vec::new();
    let mut uvs = Vec::new();
    let mut indices = Vec::new();
    let mut groups = Vec::new();

    for model in models {
        let mesh = &model.mesh;

        if mesh.texcoords.is_empty() {
            return Err(MeshError::NoUvChannel);
        }

        let base_vertex = positions.len() as u32;
        let index_offset = indices.len() as u32;

        // Extract positions (x, y, z triples)
        for chunk in mesh.positions.chunks_exact(3) {
            positions.push(Vec3::new(chunk[0], chunk[1], chunk[2]));
        }

        // Extract UVs (u, v pairs)
        for chunk in mesh.texcoords.chunks_exact(2) {
            uvs.push(Vec2::new(chunk[0], chunk[1]));
        }

        // Handle face arities
        if mesh.face_arities.is_empty() {
            // All triangles
            for &idx in &mesh.indices {
                indices.push(base_vertex + idx);
            }
        } else {
            let mut idx_offset = 0usize;
            for &arity in &mesh.face_arities {
                let arity = arity as usize;
                if arity == 3 {
                    for i in 0..3 {
                        indices.push(base_vertex + mesh.indices[idx_offset + i]);
                    }
                } else if arity == 4 {
                    let i0 = mesh.indices[idx_offset];
                    let i1 = mesh.indices[idx_offset + 1];
                    let i2 = mesh.indices[idx_offset + 2];
                    let i3 = mesh.indices[idx_offset + 3];

                    // Global indices for diagonal measurement
                    let gi0 = (base_vertex + i0) as usize;
                    let gi2 = (base_vertex + i2) as usize;
                    let gi1 = (base_vertex + i1) as usize;
                    let gi3 = (base_vertex + i3) as usize;

                    let diag_02 = (positions[gi0] - positions[gi2]).length();
                    let diag_13 = (positions[gi1] - positions[gi3]).length();

                    if diag_02 <= diag_13 {
                        // Split along 0-2
                        indices.extend_from_slice(&[
                            base_vertex + i0,
                            base_vertex + i1,
                            base_vertex + i2,
                            base_vertex + i0,
                            base_vertex + i2,
                            base_vertex + i3,
                        ]);
                    } else {
                        // Split along 1-3
                        indices.extend_from_slice(&[
                            base_vertex + i0,
                            base_vertex + i1,
                            base_vertex + i3,
                            base_vertex + i1,
                            base_vertex + i2,
                            base_vertex + i3,
                        ]);
                    }
                } else {
                    return Err(MeshError::ParseError(format!(
                        "Polygons with {arity} vertices not supported"
                    )));
                }
                idx_offset += arity;
            }
        }

        let index_count = indices.len() as u32 - index_offset;
        groups.push(MeshGroup {
            name: model.name.clone(),
            index_offset,
            index_count,
        });
    }

    Ok(LoadedMesh {
        positions,
        uvs,
        indices,
        groups,
        materials: vec![],
    })
}

/// Convert a tobj::Material to MeshMaterialInfo, loading texture files relative to `obj_dir`.
fn mtl_to_material_info(
    mat: &tobj::Material,
    idx: usize,
    obj_dir: Option<&Path>,
) -> MeshMaterialInfo {
    let name = if mat.name.is_empty() {
        format!("Material {idx}")
    } else {
        mat.name.clone()
    };

    let diffuse = mat.diffuse.unwrap_or([0.8, 0.8, 0.8]);
    let base_color_factor = [diffuse[0], diffuse[1], diffuse[2], 1.0];

    let base_color_texture = mat.diffuse_texture.as_ref().and_then(|tex_path| {
        let full = if let Some(dir) = obj_dir {
            dir.join(tex_path)
        } else {
            PathBuf::from(tex_path)
        };
        match load_texture(&full) {
            Ok(tex) => Some(tex),
            Err(e) => {
                eprintln!("MTL: failed to load diffuse texture '{}': {e}", full.display());
                None
            }
        }
    });

    let normal_texture = mat
        .normal_texture
        .as_ref()
        .and_then(|tex_path| {
            let full = if let Some(dir) = obj_dir {
                dir.join(tex_path)
            } else {
                PathBuf::from(tex_path)
            };
            match load_texture(&full) {
                Ok(tex) => Some(tex),
                Err(e) => {
                    eprintln!("MTL: failed to load normal texture '{}': {e}", full.display());
                    None
                }
            }
        });

    MeshMaterialInfo {
        name,
        base_color_factor,
        base_color_texture,
        normal_texture,
    }
}

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

fn load_gltf(path: &Path) -> Result<LoadedMesh, MeshError> {
    let (document, buffers, images) =
        gltf::import(path).map_err(|e| MeshError::ParseError(e.to_string()))?;
    build_mesh_from_gltf(document, buffers, images)
}

fn load_gltf_from_bytes(bytes: &[u8]) -> Result<LoadedMesh, MeshError> {
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

            materials.push(MeshMaterialInfo {
                name: mat_name,
                base_color_factor,
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
// Texture loading
// ---------------------------------------------------------------------------

/// Load a color texture from file. Supports PNG, TGA, EXR.
///
/// PNG and TGA are assumed sRGB and are converted to linear float.
/// EXR is assumed already linear float.
///
/// Returns texture in linear RGBA float [0, 1].
pub fn load_texture(path: &Path) -> Result<LoadedTexture, TextureError> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("png") | Some("tga") => load_png_tga(path),
        Some("exr") => load_exr(path),
        Some(ext) => Err(TextureError::UnsupportedFormat(ext.to_string())),
        None => Err(TextureError::UnsupportedFormat("no extension".to_string())),
    }
}

fn load_png_tga(path: &Path) -> Result<LoadedTexture, TextureError> {
    let img = image::open(path)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();

    let width = img.width();
    let height = img.height();

    let pixels: Vec<[f32; 4]> = img
        .pixels()
        .map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0, // Alpha is linear
            ]
        })
        .collect();

    Ok(LoadedTexture {
        pixels,
        width,
        height,
    })
}

fn load_exr(path: &Path) -> Result<LoadedTexture, TextureError> {
    use exr::prelude::*;

    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let w = resolution.width();
            let h = resolution.height();
            (w, h, vec![[0.0f32; 4]; w * h])
        },
        |(w, _h, pixels), pos, (r, g, b, a): (f32, f32, f32, f32)| {
            pixels[pos.y() * *w + pos.x()] = [r, g, b, a];
        },
    )
    .map_err(|e| TextureError::DecodeError(e.to_string()))?;

    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    let width = w as u32;
    let height = h as u32;

    Ok(LoadedTexture {
        pixels,
        width,
        height,
    })
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
// PNG encode/decode helpers (for .pap asset embedding)
// ---------------------------------------------------------------------------

/// Encode linear RGBA pixels as an sRGB PNG (for base color textures).
pub fn encode_pixels_as_srgb_png(pixels: &[[f32; 4]], width: u32, height: u32) -> Option<Vec<u8>> {
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            [
                (linear_to_srgb(p[0].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(p[1].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (linear_to_srgb(p[2].clamp(0.0, 1.0)) * 255.0).round() as u8,
                (p[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();
    let mut png = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png));
        use image::ImageEncoder;
        encoder
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    Some(png)
}

/// Decode an sRGB PNG back into linear RGBA float pixels.
pub fn decode_srgb_png_bytes(bytes: &[u8]) -> Result<(Vec<[f32; 4]>, u32, u32), TextureError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();
    let width = img.width();
    let height = img.height();
    let pixels = img
        .pixels()
        .map(|p| {
            [
                srgb_to_linear(p[0] as f32 / 255.0),
                srgb_to_linear(p[1] as f32 / 255.0),
                srgb_to_linear(p[2] as f32 / 255.0),
                p[3] as f32 / 255.0,
            ]
        })
        .collect();
    Ok((pixels, width, height))
}

/// Encode linear RGBA pixels as a linear (no gamma) PNG (for normal map textures).
pub fn encode_pixels_as_linear_png(pixels: &[[f32; 4]], width: u32, height: u32) -> Option<Vec<u8>> {
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            [
                (p[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[2].clamp(0.0, 1.0) * 255.0).round() as u8,
                (p[3].clamp(0.0, 1.0) * 255.0).round() as u8,
            ]
        })
        .collect();
    let mut png = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(Cursor::new(&mut png));
        use image::ImageEncoder;
        encoder
            .write_image(&rgba, width, height, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    Some(png)
}

/// Decode a linear (no gamma) PNG into float RGBA pixels.
pub fn decode_linear_png_bytes(bytes: &[u8]) -> Result<(Vec<[f32; 4]>, u32, u32), TextureError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| TextureError::DecodeError(e.to_string()))?
        .to_rgba8();
    let width = img.width();
    let height = img.height();
    let pixels = img
        .pixels()
        .map(|p| {
            [
                p[0] as f32 / 255.0,
                p[1] as f32 / 255.0,
                p[2] as f32 / 255.0,
                p[3] as f32 / 255.0,
            ]
        })
        .collect();
    Ok((pixels, width, height))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // -- sRGB conversion tests --

    #[test]
    fn srgb_white_to_linear() {
        let linear = srgb_to_linear(1.0);
        assert!((linear - 1.0).abs() < 1e-6);
    }

    #[test]
    fn srgb_black_to_linear() {
        let linear = srgb_to_linear(0.0);
        assert!((linear - 0.0).abs() < 1e-6);
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
    fn linear_to_srgb_boundaries() {
        assert!((linear_to_srgb(0.0) - 0.0).abs() < 1e-6);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
    }

    // -- OBJ loading tests --

    fn write_temp_file(name: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("pap_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn load_obj_triangle() {
        let obj = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 0.0 1.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 0.0 1.0
f 1/1 2/2 3/3
";
        let path = write_temp_file("tri.obj", obj);
        let mesh = load_mesh(&path).unwrap();
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.uvs.len(), 3);
        assert_eq!(mesh.indices.len(), 3);
        assert_eq!(mesh.indices.len() % 3, 0);
    }

    #[test]
    fn load_obj_quad_triangulated() {
        let obj = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 1.0 1.0 0.0
v 0.0 1.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 1.0 1.0
vt 0.0 1.0
f 1/1 2/2 3/3 4/4
";
        let path = write_temp_file("quad.obj", obj);
        let mesh = load_mesh(&path).unwrap();
        assert_eq!(mesh.positions.len(), 4);
        assert_eq!(mesh.uvs.len(), 4);
        // 1 quad = 2 triangles = 6 indices
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.indices.len() % 3, 0);
        // All indices valid
        for &idx in &mesh.indices {
            assert!((idx as usize) < mesh.positions.len());
        }
    }

    #[test]
    fn load_obj_no_uv() {
        let obj = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 0.0 1.0 0.0
f 1 2 3
";
        let path = write_temp_file("nouv.obj", obj);
        let result = load_mesh(&path);
        assert!(matches!(result, Err(MeshError::NoUvChannel)));
    }

    #[test]
    fn load_obj_non_utf8_comment() {
        // Simulate EUC-KR encoded comment (common in 3ds Max exports)
        let obj: &[u8] = b"# \xc7\xd1\xb1\xdb \xc5\xd7\xbd\xba\xc6\xae\n\
            v 0.0 0.0 0.0\nv 1.0 0.0 0.0\nv 0.0 1.0 0.0\n\
            vt 0.0 0.0\nvt 1.0 0.0\nvt 0.0 1.0\n\
            f 1/1 2/2 3/3\n";
        let mesh = load_obj_from_bytes(obj).unwrap();
        assert_eq!(mesh.positions.len(), 3);
        assert_eq!(mesh.indices.len(), 3);
    }

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

    // -- Texture loading tests --

    #[test]
    fn load_png_texture() {
        // Create a 4x4 test PNG
        let width = 4u32;
        let height = 4u32;
        let mut img = image::RgbaImage::new(width, height);

        // Set known colors: top-left white, bottom-right black
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 255]));
        img.put_pixel(3, 3, image::Rgba([0, 0, 0, 255]));
        // Mid-gray
        img.put_pixel(1, 0, image::Rgba([128, 128, 128, 255]));

        let path = std::env::temp_dir().join("pap_test").join("test.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // White pixel -> linear (1, 1, 1, 1)
        let white = tex.pixels[0];
        assert!((white[0] - 1.0).abs() < 1e-5);
        assert!((white[1] - 1.0).abs() < 1e-5);
        assert!((white[2] - 1.0).abs() < 1e-5);
        assert!((white[3] - 1.0).abs() < 1e-5);

        // Black pixel -> linear (0, 0, 0, 1)
        let black = tex.pixels[15]; // (3,3) = index 15
        assert!((black[0] - 0.0).abs() < 1e-5);
        assert!((black[1] - 0.0).abs() < 1e-5);
        assert!((black[2] - 0.0).abs() < 1e-5);

        // Mid-gray pixel -> linear ~0.2158
        let gray = tex.pixels[1]; // (1,0) = index 1
        assert!(
            (gray[0] - 0.2158).abs() < 0.001,
            "Expected ~0.2158, got {}",
            gray[0]
        );
    }

    #[test]
    fn load_unsupported_texture_format() {
        let path = Path::new("texture.bmp");
        let result = load_texture(path);
        assert!(matches!(result, Err(TextureError::UnsupportedFormat(_))));
    }

    #[test]
    fn texture_dimensions() {
        let width = 8u32;
        let height = 4u32;
        let img = image::RgbaImage::new(width, height);
        let path = std::env::temp_dir().join("pap_test").join("dim_test.png");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 8);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 32);
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

    // -- Index validity test --

    #[test]
    fn all_indices_valid_after_obj_load() {
        let obj = "\
v 0.0 0.0 0.0
v 1.0 0.0 0.0
v 1.0 1.0 0.0
v 0.0 1.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 1.0 1.0
vt 0.0 1.0
f 1/1 2/2 3/3
f 1/1 3/3 4/4
";
        let path = write_temp_file("valid_idx.obj", obj);
        let mesh = load_mesh(&path).unwrap();
        for &idx in &mesh.indices {
            assert!(
                (idx as usize) < mesh.positions.len(),
                "Index {idx} out of bounds (positions.len() = {})",
                mesh.positions.len()
            );
        }
    }

    // -- Quad triangulation direction test --

    #[test]
    fn quad_split_on_shorter_diagonal() {
        // Rectangle: 0--1 is 3 units wide, 0--3 is 1 unit tall
        // diag 0-2 = sqrt(9+1)=√10 ≈ 3.16
        // diag 1-3 = sqrt(9+1)=√10 ≈ 3.16  (same for square-ish)
        //
        // Make it asymmetric: narrow tall → diag_02 > diag_13
        let obj = "\
v 0.0 0.0 0.0
v 5.0 0.0 0.0
v 5.0 1.0 0.0
v 0.0 1.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 1.0 1.0
vt 0.0 1.0
f 1/1 2/2 3/3 4/4
";
        let path = write_temp_file("rect_quad.obj", obj);
        let mesh = load_mesh(&path).unwrap();
        assert_eq!(mesh.indices.len(), 6);

        // Kite shape: diagonals have different lengths
        let obj_kite = "\
v 0.0 1.0 0.0
v 2.0 0.0 0.0
v 0.0 -1.0 0.0
v -1.0 0.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 1.0 1.0
vt 0.0 1.0
f 1/1 2/2 3/3 4/4
";
        // diag 0-2: (0,1)→(0,-1) = 2.0
        // diag 1-3: (2,0)→(-1,0) = 3.0
        // diag_02 < diag_13 → should split on 0-2: [0,1,2, 0,2,3]
        let path = write_temp_file("kite_quad.obj", obj_kite);
        let mesh = load_mesh(&path).unwrap();
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3]);

        // Reverse: make diag_13 shorter
        let obj_kite2 = "\
v 0.0 2.0 0.0
v 1.0 0.0 0.0
v 0.0 -2.0 0.0
v -1.0 0.0 0.0
vt 0.0 0.0
vt 1.0 0.0
vt 1.0 1.0
vt 0.0 1.0
f 1/1 2/2 3/3 4/4
";
        // diag 0-2: (0,2)→(0,-2) = 4.0
        // diag 1-3: (1,0)→(-1,0) = 2.0
        // diag_13 < diag_02 → should split on 1-3: [0,1,3, 1,2,3]
        let path = write_temp_file("kite2_quad.obj", obj_kite2);
        let mesh = load_mesh(&path).unwrap();
        assert_eq!(mesh.indices.len(), 6);
        assert_eq!(mesh.indices, vec![0, 1, 3, 1, 2, 3]);
    }

    // -- glTF/glb loading tests --

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

    // -- TGA loading test --

    #[test]
    fn load_tga_texture() {
        // Create a test TGA using the image crate
        let width = 4u32;
        let height = 4u32;
        let mut img = image::RgbaImage::new(width, height);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // Red
        img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255])); // Green

        let path = std::env::temp_dir().join("pap_test").join("test.tga");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        img.save(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // Red pixel: sRGB (1,0,0) → linear (1,0,0)
        let red = tex.pixels[0];
        assert!((red[0] - 1.0).abs() < 1e-5);
        assert!((red[1] - 0.0).abs() < 1e-5);
        assert!((red[2] - 0.0).abs() < 1e-5);
    }

    // -- EXR loading test --

    #[test]
    fn load_exr_texture() {
        use exr::prelude::*;

        let width = 4usize;
        let height = 4usize;

        // Known linear values
        let mut pixels = vec![[0.0f32, 0.0, 0.0, 1.0]; width * height];
        pixels[0] = [0.5, 0.25, 0.125, 1.0]; // Known linear values (should pass through unchanged)
        pixels[1] = [1.0, 1.0, 1.0, 1.0];

        let path = std::env::temp_dir().join("pap_test").join("test.exr");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let layer = Layer::new(
            (width, height),
            LayerAttributes::named("rgba"),
            Encoding::FAST_LOSSLESS,
            AnyChannels::sort(smallvec::smallvec![
                AnyChannel::new("R", FlatSamples::F32(pixels.iter().map(|p| p[0]).collect())),
                AnyChannel::new("G", FlatSamples::F32(pixels.iter().map(|p| p[1]).collect())),
                AnyChannel::new("B", FlatSamples::F32(pixels.iter().map(|p| p[2]).collect())),
                AnyChannel::new("A", FlatSamples::F32(pixels.iter().map(|p| p[3]).collect())),
            ]),
        );

        let image = Image::from_layer(layer);
        image.write().to_file(&path).unwrap();

        let tex = load_texture(&path).unwrap();
        assert_eq!(tex.width, 4);
        assert_eq!(tex.height, 4);
        assert_eq!(tex.pixels.len(), 16);

        // EXR values should pass through unchanged (already linear)
        let p0 = tex.pixels[0];
        assert!((p0[0] - 0.5).abs() < 1e-5, "R: expected 0.5, got {}", p0[0]);
        assert!(
            (p0[1] - 0.25).abs() < 1e-5,
            "G: expected 0.25, got {}",
            p0[1]
        );
        assert!(
            (p0[2] - 0.125).abs() < 1e-5,
            "B: expected 0.125, got {}",
            p0[2]
        );
    }
}
