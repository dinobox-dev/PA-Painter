//! OBJ mesh loading with MTL material support.

use glam::{Vec2, Vec3};
use log::warn;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use super::texture::{load_texture, load_texture_from_bytes};
use super::{LoadedMesh, MeshError, MeshMaterialInfo};
use crate::types::MeshGroup;

// ---------------------------------------------------------------------------
// OBJ auxiliary files
// ---------------------------------------------------------------------------

/// Auxiliary files collected alongside an OBJ mesh (MTL + referenced textures).
/// Stored in the project so they can be embedded in .papr and restored on load.
#[derive(Debug, Clone, Default)]
pub struct ObjAuxFiles {
    /// Raw MTL file bytes.
    pub mtl_bytes: Vec<u8>,
    /// Texture files referenced by the MTL, keyed by their MTL-relative path.
    pub texture_files: Vec<(String, Vec<u8>)>,
}

/// Collect MTL and referenced texture files for an OBJ mesh.
/// Returns `None` if the OBJ has no `mtllib` directive or the MTL cannot be read.
pub fn collect_obj_aux_files(obj_path: &Path) -> Option<ObjAuxFiles> {
    let obj_dir = obj_path.parent();
    let raw = std::fs::read(obj_path).ok()?;
    let text = String::from_utf8_lossy(&raw);

    // Find mtllib directive (first one wins, matching tobj behavior)
    let mtl_name = text
        .lines()
        .find(|l| l.starts_with("mtllib "))
        .map(|l| l["mtllib ".len()..].trim().to_string())?;

    let mtl_path = obj_dir
        .map(|d| d.join(&mtl_name))
        .unwrap_or_else(|| PathBuf::from(&mtl_name));
    let mtl_bytes = std::fs::read(&mtl_path).ok()?;

    // Parse MTL text for texture path directives
    let mtl_text = String::from_utf8_lossy(&mtl_bytes);
    let mut seen = HashSet::new();
    let mut texture_files = Vec::new();

    for line in mtl_text.lines() {
        let trimmed = line.trim();
        // MTL texture directives we care about (matching mtl_to_material_info usage)
        let tex_name = trimmed
            .strip_prefix("map_Kd ")
            .or_else(|| trimmed.strip_prefix("map_Bump "))
            .or_else(|| trimmed.strip_prefix("bump "))
            .or_else(|| trimmed.strip_prefix("norm "))
            .or_else(|| trimmed.strip_prefix("map_Kn "))
            .map(|rest| rest.trim());

        if let Some(name) = tex_name {
            if name.is_empty() || !seen.insert(name.to_string()) {
                continue;
            }
            let tex_path = obj_dir
                .map(|d| d.join(name))
                .unwrap_or_else(|| PathBuf::from(name));
            match std::fs::read(&tex_path) {
                Ok(bytes) => texture_files.push((name.to_string(), bytes)),
                Err(e) => warn!(
                    "OBJ aux: failed to read texture '{}': {e}",
                    tex_path.display()
                ),
            }
        }
    }

    Some(ObjAuxFiles {
        mtl_bytes,
        texture_files,
    })
}

// ---------------------------------------------------------------------------
// OBJ loading
// ---------------------------------------------------------------------------

fn obj_load_options() -> tobj::LoadOptions {
    tobj::LoadOptions {
        triangulate: false, // We handle triangulation ourselves for shorter-diagonal split
        single_index: true,
        ..Default::default()
    }
}

pub(super) fn load_obj(path: &Path) -> Result<LoadedMesh, MeshError> {
    let raw =
        std::fs::read(path).map_err(|e| MeshError::ParseError(format!("Failed to open: {e}")))?;
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
        // Rename duplicate-named groups using material names.
        // tobj gives all usemtl-split fragments the same model name (e.g. "unnamed_object")
        // when the OBJ lacks explicit `g` directives, making mask lookup by name ambiguous.
        disambiguate_groups_with_materials(&mut mesh.groups, &models, &materials);

        mesh.materials = materials
            .iter()
            .enumerate()
            .map(|(i, mat)| mtl_to_material_info(mat, i, obj_dir))
            .collect();
    }

    Ok(mesh)
}

pub(super) fn load_obj_from_bytes(bytes: &[u8]) -> Result<LoadedMesh, MeshError> {
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

/// Load OBJ from bytes with auxiliary MTL + texture data (from .papr embedding).
/// Replicates the full `load_obj` pipeline: MTL parsing → disambiguate → material info.
pub(super) fn load_obj_from_bytes_with_aux(
    bytes: &[u8],
    aux: &ObjAuxFiles,
) -> Result<LoadedMesh, MeshError> {
    let text = String::from_utf8_lossy(bytes);
    let mtl_bytes = aux.mtl_bytes.clone();
    let (models, raw_materials) = tobj::load_obj_buf(
        &mut Cursor::new(text.as_bytes()),
        &obj_load_options(),
        |_| {
            let mtl_text = String::from_utf8_lossy(&mtl_bytes);
            tobj::load_mtl_buf(&mut Cursor::new(mtl_text.as_bytes()))
        },
    )
    .map_err(|e| MeshError::ParseError(e.to_string()))?;

    let mut mesh = build_mesh_from_obj(&models)?;

    if let Ok(materials) = raw_materials {
        disambiguate_groups_with_materials(&mut mesh.groups, &models, &materials);

        // Build texture lookup from aux data
        let tex_map: HashMap<&str, &[u8]> = aux
            .texture_files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();

        mesh.materials = materials
            .iter()
            .enumerate()
            .map(|(i, mat)| mtl_to_material_info_from_aux(mat, i, &tex_map))
            .collect();
    }

    Ok(mesh)
}

// ---------------------------------------------------------------------------
// OBJ mesh construction
// ---------------------------------------------------------------------------

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
        // OBJ uses bottom-left UV origin (V=0 at bottom), but our pipeline
        // assumes glTF convention (V=0 at top), so flip V.
        for chunk in mesh.texcoords.chunks_exact(2) {
            uvs.push(Vec2::new(chunk[0], 1.0 - chunk[1]));
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
                    // N-gon (5+ vertices): fan triangulation from vertex 0
                    let i0 = mesh.indices[idx_offset];
                    for j in 1..(arity - 1) {
                        indices.extend_from_slice(&[
                            base_vertex + i0,
                            base_vertex + mesh.indices[idx_offset + j],
                            base_vertex + mesh.indices[idx_offset + j + 1],
                        ]);
                    }
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

// ---------------------------------------------------------------------------
// Material info extraction
// ---------------------------------------------------------------------------

/// Rename groups that share the same name using material names from the MTL file.
/// Only groups whose name appears more than once are renamed.
fn disambiguate_groups_with_materials(
    groups: &mut [MeshGroup],
    models: &[tobj::Model],
    materials: &[tobj::Material],
) {
    // Count how many groups share each name
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    for g in groups.iter() {
        *name_counts.entry(g.name.clone()).or_insert(0) += 1;
    }

    // Only rename groups with duplicate names
    let mut used_names: HashMap<String, usize> = HashMap::new();
    for (group, model) in groups.iter_mut().zip(models.iter()) {
        if name_counts.get(&group.name).copied().unwrap_or(0) <= 1 {
            continue;
        }
        // Try to use material name
        let new_name = model
            .mesh
            .material_id
            .and_then(|id| materials.get(id))
            .map(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| group.name.clone());

        // Ensure uniqueness by appending suffix if needed
        let count = used_names.entry(new_name.clone()).or_insert(0);
        group.name = if *count == 0 {
            new_name.clone()
        } else {
            format!("{new_name}_{count}")
        };
        *used_names.get_mut(&new_name).unwrap() += 1;
    }
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

    let has_explicit_color = mat.diffuse.is_some() || mat.diffuse_texture.is_some();
    let diffuse = mat.diffuse.unwrap_or([0.8, 0.8, 0.8]);
    // MTL Kd values are in sRGB space; convert to linear to match GLB convention.
    let base_color_factor = [
        diffuse[0].powf(2.2),
        diffuse[1].powf(2.2),
        diffuse[2].powf(2.2),
        1.0,
    ];

    let base_color_texture = mat.diffuse_texture.as_ref().and_then(|tex_path| {
        let full = if let Some(dir) = obj_dir {
            dir.join(tex_path)
        } else {
            PathBuf::from(tex_path)
        };
        match load_texture(&full) {
            Ok(tex) => Some(tex),
            Err(e) => {
                warn!(
                    "MTL: failed to load diffuse texture '{}': {e}",
                    full.display()
                );
                None
            }
        }
    });

    let normal_texture = mat.normal_texture.as_ref().and_then(|tex_path| {
        let full = if let Some(dir) = obj_dir {
            dir.join(tex_path)
        } else {
            PathBuf::from(tex_path)
        };
        match load_texture(&full) {
            Ok(tex) => Some(tex),
            Err(e) => {
                warn!(
                    "MTL: failed to load normal texture '{}': {e}",
                    full.display()
                );
                None
            }
        }
    });

    MeshMaterialInfo {
        name,
        base_color_factor,
        has_explicit_color,
        base_color_texture,
        normal_texture,
    }
}

/// Like `mtl_to_material_info` but loads textures from in-memory bytes
/// instead of the filesystem. Used when restoring OBJ materials from .papr.
fn mtl_to_material_info_from_aux(
    mat: &tobj::Material,
    idx: usize,
    texture_files: &HashMap<&str, &[u8]>,
) -> MeshMaterialInfo {
    let name = if mat.name.is_empty() {
        format!("Material {idx}")
    } else {
        mat.name.clone()
    };

    let has_explicit_color = mat.diffuse.is_some() || mat.diffuse_texture.is_some();
    let diffuse = mat.diffuse.unwrap_or([0.8, 0.8, 0.8]);
    let base_color_factor = [
        diffuse[0].powf(2.2),
        diffuse[1].powf(2.2),
        diffuse[2].powf(2.2),
        1.0,
    ];

    let base_color_texture = mat.diffuse_texture.as_ref().and_then(|tex_path| {
        let bytes = texture_files.get(tex_path.as_str())?;
        match load_texture_from_bytes(bytes) {
            Ok(tex) => Some(tex),
            Err(e) => {
                warn!("MTL aux: failed to decode diffuse texture '{tex_path}': {e}");
                None
            }
        }
    });

    let normal_texture = mat.normal_texture.as_ref().and_then(|tex_path| {
        let bytes = texture_files.get(tex_path.as_str())?;
        match load_texture_from_bytes(bytes) {
            Ok(tex) => Some(tex),
            Err(e) => {
                warn!("MTL aux: failed to decode normal texture '{tex_path}': {e}");
                None
            }
        }
    });

    MeshMaterialInfo {
        name,
        base_color_factor,
        has_explicit_color,
        base_color_texture,
        normal_texture,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::asset_io::load_mesh;
    use std::io::Write;
    use std::path::Path;

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
    fn load_obj_many_groups() {
        let path = Path::new("tests/fixtures/many_groups.obj");
        let mesh = load_mesh(path).unwrap();
        assert_eq!(mesh.groups.len(), 20);
        assert_eq!(mesh.indices.len(), 20 * 3);
        assert!(mesh.groups[0].name.starts_with("group_"));
        assert_eq!(mesh.groups[19].name, "group_20");
    }

    #[test]
    fn load_obj_with_mtl() {
        let path = Path::new("tests/fixtures/with_mtl.obj");
        let mesh = load_mesh(path).unwrap();
        assert_eq!(mesh.groups.len(), 3);
        assert_eq!(mesh.materials.len(), 3);

        // Red material — Kd 0.8 0.1 0.1 (sRGB) → linear via powf(2.2)
        let red = &mesh.materials[0];
        assert_eq!(red.name, "Red");
        assert!((red.base_color_factor[0] - 0.8_f32.powf(2.2)).abs() < 0.01);
        assert!((red.base_color_factor[1] - 0.1_f32.powf(2.2)).abs() < 0.01);
        assert!(red.base_color_texture.is_none());
        assert!(red.normal_texture.is_none());

        // Blue material — Kd 0.1 0.1 0.9 (sRGB) → linear
        let blue = &mesh.materials[1];
        assert_eq!(blue.name, "Blue");
        assert!((blue.base_color_factor[2] - 0.9_f32.powf(2.2)).abs() < 0.01);

        // Default material — tobj default 0.8 0.8 0.8 (sRGB) → linear
        let def = &mesh.materials[2];
        assert_eq!(def.name, "Default");
        assert!((def.base_color_factor[0] - 0.8_f32.powf(2.2)).abs() < 0.01);
    }

    #[test]
    fn load_obj_usemtl_without_groups_disambiguates() {
        let path = Path::new("tests/fixtures/usemtl_no_groups.obj");
        let mesh = load_mesh(path).unwrap();

        // tobj splits by usemtl into 2 models, both named "unnamed_object".
        // disambiguate_groups_with_materials should rename them to material names.
        assert_eq!(mesh.groups.len(), 2);
        assert_eq!(mesh.groups[0].name, "Skin");
        assert_eq!(mesh.groups[1].name, "Fabric");

        // Each group should have its own triangle
        assert_eq!(mesh.groups[0].index_count, 3);
        assert_eq!(mesh.groups[1].index_count, 3);

        // Materials should still be loaded
        assert_eq!(mesh.materials.len(), 2);
        assert_eq!(mesh.materials[0].name, "Skin");
        assert_eq!(mesh.materials[1].name, "Fabric");
    }

    #[test]
    fn load_obj_without_mtl_file() {
        let path = Path::new("tests/fixtures/many_groups.obj");
        let mesh = load_mesh(path).unwrap();
        assert!(mesh.materials.is_empty());
    }

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
}
