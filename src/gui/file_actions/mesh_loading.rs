//! Mesh loading, auto-mapping, and mesh load popup construction.

use std::path::PathBuf;
use std::sync::Arc;

use pa_painter::mesh::asset_io::{collect_obj_aux_files, LoadedMesh, MeshMaterialInfo};
use pa_painter::types::{EmbeddedTexture, TextureSource};

use crate::gui::state::{AppState, LayerMapping, MeshLoadPopup};

// ── Auto-Mapping ──────────────────────────────────────────────────

/// Compute auto-mapped TextureSource for a material (GLB style: MeshMaterial refs).
/// Returns (color, normal, is_default).
fn auto_map_glb(mat: &MeshMaterialInfo, idx: usize) -> (TextureSource, TextureSource, bool) {
    let color = if mat.base_color_texture.is_some() {
        TextureSource::MeshMaterial(idx)
    } else if mat.has_explicit_color {
        let f = mat.base_color_factor;
        TextureSource::Solid([f[0], f[1], f[2]])
    } else {
        TextureSource::Solid([1.0, 1.0, 1.0])
    };
    let normal = if mat.normal_texture.is_some() {
        TextureSource::MeshMaterial(idx)
    } else {
        TextureSource::None
    };
    let is_default = !mat.has_explicit_color && mat.normal_texture.is_none();
    (color, normal, is_default)
}

/// Returns (color, normal, is_default).
fn auto_map_mtl(mat: &MeshMaterialInfo) -> (TextureSource, TextureSource, bool) {
    let color = if let Some(ref tex) = mat.base_color_texture {
        let content_hash = EmbeddedTexture::compute_content_hash(&tex.pixels);
        TextureSource::File(Some(EmbeddedTexture {
            label: mat.name.clone(),
            pixels: Arc::new(tex.pixels.clone()),
            width: tex.width,
            height: tex.height,
            content_hash,
        }))
    } else if mat.has_explicit_color {
        let f = mat.base_color_factor;
        TextureSource::Solid([f[0], f[1], f[2]])
    } else {
        TextureSource::Solid([1.0, 1.0, 1.0])
    };
    let normal = if let Some(ref tex) = mat.normal_texture {
        let content_hash = EmbeddedTexture::compute_content_hash(&tex.pixels);
        TextureSource::File(Some(EmbeddedTexture {
            label: format!("{}_normal", mat.name),
            pixels: Arc::new(tex.pixels.clone()),
            width: tex.width,
            height: tex.height,
            content_hash,
        }))
    } else {
        TextureSource::None
    };
    let is_default =
        !mat.has_explicit_color && mat.base_color_texture.is_none() && mat.normal_texture.is_none();
    (color, normal, is_default)
}

/// Default mapping (no material info).
fn default_mapping() -> (TextureSource, TextureSource) {
    (TextureSource::Solid([1.0, 1.0, 1.0]), TextureSource::None)
}

/// Build proposed mappings for each layer from materials.
/// `layer_names` are the group/layer names, `materials` from the mesh.
/// `is_glb` determines whether to use MeshMaterial refs (GLB) or File refs (OBJ MTL).
fn build_mappings(
    layer_names: &[String],
    materials: &[MeshMaterialInfo],
    is_glb: bool,
) -> Vec<LayerMapping> {
    layer_names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            if let Some(m) = materials.get(i) {
                let (color, normal, is_default) = if is_glb {
                    auto_map_glb(m, i)
                } else {
                    auto_map_mtl(m)
                };
                LayerMapping {
                    name: name.clone(),
                    base_color: color,
                    base_normal: normal,
                    is_default,
                }
            } else {
                default_layer_mapping(name)
            }
        })
        .collect()
}

fn default_layer_mapping(name: &str) -> LayerMapping {
    let (color, normal) = default_mapping();
    LayerMapping {
        name: name.to_string(),
        base_color: color,
        base_normal: normal,
        is_default: true,
    }
}

// ── Mesh Load Popup ───────────────────────────────────────────────

/// Build a MeshLoadPopup holding the loaded mesh (not yet applied to state).
pub(super) fn build_mesh_load_popup(
    mesh: LoadedMesh,
    filename: &str,
    format: &str,
    path: PathBuf,
    mesh_bytes: Option<Vec<u8>>,
    is_replace: bool,
) -> MeshLoadPopup {
    let is_glb = format == "glb" || format == "gltf";
    let has_mtl = !is_glb && !mesh.materials.is_empty();

    // Collect OBJ auxiliary files (MTL + referenced textures) for .papr embedding
    let pending_obj_aux = if format == "obj" {
        collect_obj_aux_files(&path)
    } else {
        None
    };

    let layer_names: Vec<String> = mesh.groups.iter().map(|g| g.name.clone()).collect();
    let layer_names = if layer_names.is_empty() {
        vec!["__all__".to_string()]
    } else {
        layer_names
    };

    let mappings = if !mesh.materials.is_empty() {
        build_mappings(&layer_names, &mesh.materials, is_glb)
    } else {
        layer_names
            .iter()
            .map(|n| default_layer_mapping(n))
            .collect()
    };

    let mappings_no_mtl = layer_names
        .iter()
        .map(|n| default_layer_mapping(n))
        .collect();

    let layer_count = mappings.len();
    MeshLoadPopup {
        filename: filename.to_string(),
        vertices: mesh.positions.len(),
        triangles: mesh.indices.len() / 3,
        groups: mesh.groups.len(),
        n_textures: mesh
            .materials
            .iter()
            .filter(|m| m.base_color_texture.is_some())
            .count(),
        n_normals: mesh
            .materials
            .iter()
            .filter(|m| m.normal_texture.is_some())
            .count(),
        has_mtl,
        use_mtl: has_mtl,
        mappings,
        mappings_no_mtl,
        is_replace,
        layer_enabled: vec![true; layer_count],
        pending_mesh: mesh,
        pending_path: path,
        pending_format: format.to_string(),
        pending_bytes: mesh_bytes,
        pending_obj_aux,
    }
}

/// Apply the confirmed popup: load mesh into state, create/update layers.
/// This is the ONLY place where state changes for new project / replace mesh.
pub fn apply_mesh_load_popup(state: &mut AppState) {
    let Some(popup) = state.mesh_load_popup.take() else {
        return;
    };

    let active_mappings = if popup.has_mtl && !popup.use_mtl {
        popup.mappings_no_mtl.clone()
    } else {
        popup.mappings.clone()
    };

    // Apply the loaded mesh to state
    super::apply_loaded_mesh(state, popup.pending_mesh);

    // Update mesh reference
    state.project.mesh_ref.path = popup.pending_path.to_string_lossy().to_string();
    state.project.mesh_ref.format = popup.pending_format.clone();
    state.project.mesh_bytes = popup.pending_bytes;
    state.project.obj_aux = popup.pending_obj_aux;

    if popup.is_replace {
        // Remap existing layers to new groups, add new ones for unmatched groups
        let existing_names: std::collections::HashSet<String> = state
            .project
            .layers
            .iter()
            .map(|l| l.group_name.clone())
            .collect();

        for mapping in &active_mappings {
            if !existing_names.contains(&mapping.name) {
                let seed = state.project.layers.len() as u32;
                let order = state.project.layers.len() as i32;
                state.project.layers.push(pa_painter::types::Layer {
                    name: mapping.name.clone(),
                    visible: true,
                    group_name: mapping.name.clone(),
                    order,
                    paint: pa_painter::types::PaintValues::default(),
                    guides: vec![],
                    base_color: mapping.base_color.clone(),
                    base_normal: mapping.base_normal.clone(),
                    dry: 1.0,
                    seed,
                });
            }
        }
    } else {
        // New project: replace all layers with popup mappings
        state.project.layers.clear();
        for (i, mapping) in active_mappings.iter().enumerate() {
            if !popup.layer_enabled[i] {
                continue;
            }
            let seed = i as u32;
            state.project.layers.push(pa_painter::types::Layer {
                name: mapping.name.clone(),
                visible: true,
                group_name: mapping.name.clone(),
                order: i as i32,
                paint: pa_painter::types::PaintValues::default(),
                guides: vec![],
                base_color: mapping.base_color.clone(),
                base_normal: mapping.base_normal.clone(),
                dry: 1.0,
                seed,
            });
        }
        state.project_path = None;
    }

    state.selected_layer = if state.project.layers.is_empty() {
        None
    } else {
        Some(0)
    };
    state.dirty = true;
    state.undo.clear();
    state.cached_mesh_normals = None;
    state.path_worker.discard();
    state.group_dim_cache.invalidate();
}
