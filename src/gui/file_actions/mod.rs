mod export;
mod mesh_loading;
mod project_io;

pub use export::{confirm_export_overwrite, export_both, export_glb, export_maps};
pub use mesh_loading::apply_mesh_load_popup;
pub use project_io::{
    apply_load_result, new_project, pick_project_path, reload_mesh, replace_mesh,
    save_project_action, save_project_as_action,
};

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use pa_painter::mesh::asset_io::{extract_uv_edges, load_mesh, LoadedMesh};

use super::state::AppState;

// ── Mesh Helpers (shared by project_io and mesh_loading) ──

/// Load a mesh from the given path into app state.
/// Returns `Ok(true)` if geometry changed (hash mismatch), `Ok(false)` if identical.
pub(super) fn apply_mesh(
    state: &mut AppState,
    mesh_path: &std::path::Path,
) -> Result<bool, String> {
    let mesh = load_mesh(mesh_path).map_err(|e| format!("Mesh load failed: {e}"))?;
    state.uv_edges = Some(extract_uv_edges(&mesh));
    state.wireframe_cache = super::state::WireframeCache::default();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for p in &mesh.positions {
        p.x.to_bits().hash(&mut hasher);
        p.y.to_bits().hash(&mut hasher);
        p.z.to_bits().hash(&mut hasher);
    }
    for uv in &mesh.uvs {
        uv.x.to_bits().hash(&mut hasher);
        uv.y.to_bits().hash(&mut hasher);
    }
    mesh.indices.hash(&mut hasher);
    let new_hash = hasher.finish();
    let changed = new_hash != state.mesh_hash;
    state.mesh_hash = new_hash;

    if changed {
        state.textures.color = None;
        state.textures.height = None;
        state.textures.normal = None;
        state.textures.stroke_id = None;
        state.generated = None;
        state.path_overlay.clear();
        state.generation.path_cache.clear();
        state.auto_gen_suppressed = false;
    }

    state.loaded_mesh = Some(Arc::new(mesh));
    Ok(changed)
}

/// Apply an already-loaded mesh (e.g. from a .papr file) into app state.
/// Returns `true` if geometry changed (hash mismatch), `false` if identical.
pub(super) fn apply_loaded_mesh(state: &mut AppState, mesh: LoadedMesh) -> bool {
    state.uv_edges = Some(extract_uv_edges(&mesh));
    state.wireframe_cache = super::state::WireframeCache::default();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for p in &mesh.positions {
        p.x.to_bits().hash(&mut hasher);
        p.y.to_bits().hash(&mut hasher);
        p.z.to_bits().hash(&mut hasher);
    }
    for uv in &mesh.uvs {
        uv.x.to_bits().hash(&mut hasher);
        uv.y.to_bits().hash(&mut hasher);
    }
    mesh.indices.hash(&mut hasher);
    let new_hash = hasher.finish();
    let changed = new_hash != state.mesh_hash;
    state.mesh_hash = new_hash;

    if changed {
        state.textures.color = None;
        state.textures.height = None;
        state.textures.normal = None;
        state.textures.stroke_id = None;
        state.generated = None;
        state.path_overlay.clear();
        state.generation.path_cache.clear();
        state.auto_gen_suppressed = false;
    }

    state.loaded_mesh = Some(Arc::new(mesh));
    changed
}
