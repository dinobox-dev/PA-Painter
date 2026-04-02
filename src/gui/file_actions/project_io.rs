//! Project open, save, and new project operations.

use std::path::PathBuf;

use pa_painter::io::project::{save_project, utc_now_iso8601};
use pa_painter::mesh::asset_io::load_mesh;
use pa_painter::types::{PaintValues, TextureSource};

use super::mesh_loading::build_mesh_load_popup;
use crate::gui::state::{AppState, ReloadSummary};

// ── Project Operations ─────────────────────────────────────────────

/// Open a file dialog and return the chosen path (if any).
pub fn pick_project_path(state: &mut AppState) -> Option<std::path::PathBuf> {
    state.modal_dialog_active = true;
    let path = rfd::FileDialog::new()
        .add_filter("PA Painter Project", &["papr"])
        .pick_file();
    state.modal_dialog_active = false;
    path
}

/// Apply an already-loaded `LoadResult` to state.
/// `project_path` is `Some` for file-based opens, `None` for the example project.
pub fn apply_load_result(
    state: &mut AppState,
    result: pa_painter::io::project::LoadResult,
    project_path: Option<PathBuf>,
) {
    let display_name = project_path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "example project".to_string());
    state.status_message = format!("Loaded: {display_name}");

    // Apply embedded mesh if present
    if let Some(mesh) = result.mesh {
        super::apply_loaded_mesh(state, mesh);
    }

    let editor_state_json = result.editor_state_json.clone();

    // Select first layer if any (overridden below if editor state exists)
    if !result.project.layers.is_empty() {
        state.selected_layer = Some(0);
    }

    state.project = result.project;
    state.project_path = project_path;
    state.dirty = false;
    state.generation_snapshot = None;
    state.generation.discard();
    state.textures.color = None;
    state.textures.height = None;
    state.textures.normal = None;
    state.textures.stroke_id = None;
    state.path_overlay.clear();
    state.undo.clear();

    // Restore editor UI state (camera, viewport, playback, etc.)
    if let Some(json) = editor_state_json {
        if let Ok(es) = serde_json::from_str::<crate::gui::state::EditorState>(&json) {
            state.apply_editor_state(es);
        }
    }

    // Auto-preview will regenerate output after load
    state.generated = None;
    state.auto_gen_suppressed = false;
}

/// Create a new project by loading a mesh file.
/// Opens a file dialog for mesh selection, loads the mesh into a pending popup.
/// State is NOT modified until the user confirms (OK) — Cancel discards everything.
pub fn new_project(state: &mut AppState, _ctx: &eframe::egui::Context) {
    let mesh_path = if let Some(path) = state.pending_drop_mesh.take() {
        path
    } else {
        state.modal_dialog_active = true;
        let path = rfd::FileDialog::new()
            .add_filter("3D Mesh", &["glb", "gltf", "obj"])
            .set_title("Select mesh for new project")
            .pick_file();
        state.modal_dialog_active = false;
        let Some(p) = path else {
            return;
        };
        p
    };

    // Load mesh into temporary — do NOT apply to state yet.
    let mesh = match load_mesh(&mesh_path) {
        Ok(m) => m,
        Err(e) => {
            state.status_message = format!("Mesh load failed: {e}");
            return;
        }
    };

    let filename = mesh_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let format = mesh_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let mesh_bytes = std::fs::read(&mesh_path).ok();

    state.mesh_load_popup = Some(build_mesh_load_popup(
        mesh, &filename, &format, mesh_path, mesh_bytes, false,
    ));
    state.status_message = format!("New project — {}", filename);
}

/// Reload mesh from the same path and diff groups against existing layers.
/// Auto-applies: creates layers for new groups, remaps orphaned layers to "__all__".
pub fn reload_mesh(state: &mut AppState) {
    let mesh_path_str = state.project.mesh_ref.path.clone();
    if mesh_path_str.is_empty() {
        state.status_message = "No mesh path to reload.".to_string();
        return;
    }

    let mesh_path = PathBuf::from(&mesh_path_str);

    // Remember old group names from existing layers
    let old_groups: Vec<String> = state
        .project
        .layers
        .iter()
        .map(|l| l.group_name.clone())
        .filter(|g| g != "__all__")
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    match super::apply_mesh(state, &mesh_path) {
        Ok(_) => {
            // Mesh topology may have changed — undo past this point is invalid
            state.undo.clear();

            // Re-read bytes from disk for embedding
            state.project.mesh_bytes = std::fs::read(&mesh_path).ok();

            // Determine new group names from reloaded mesh
            let new_groups: Vec<String> = state
                .loaded_mesh
                .as_ref()
                .map(|m| m.groups.iter().map(|g| g.name.clone()).collect())
                .unwrap_or_default();

            let new_set: std::collections::HashSet<&str> =
                new_groups.iter().map(|s| s.as_str()).collect();
            let old_set: std::collections::HashSet<&str> =
                old_groups.iter().map(|s| s.as_str()).collect();

            let kept: Vec<String> = old_groups
                .iter()
                .filter(|g| new_set.contains(g.as_str()))
                .cloned()
                .collect();
            let added: Vec<String> = new_groups
                .iter()
                .filter(|g| !old_set.contains(g.as_str()))
                .cloned()
                .collect();
            let orphaned: Vec<String> = old_groups
                .iter()
                .filter(|g| !new_set.contains(g.as_str()))
                .cloned()
                .collect();

            // Auto-apply: create layers for new groups
            for name in &added {
                let seed = state.project.layers.len() as u32;
                let order = state.project.layers.len() as i32;
                state.project.layers.push(pa_painter::types::Layer {
                    name: name.clone(),
                    visible: true,
                    group_name: name.clone(),
                    order,
                    paint: PaintValues::default(),
                    guides: vec![],
                    base_color: TextureSource::Solid([1.0, 1.0, 1.0]),
                    base_normal: TextureSource::None,
                    dry: 1.0,
                    seed,
                });
            }

            // Auto-apply: remap orphaned layers to "__all__"
            let orphan_set: std::collections::HashSet<&str> =
                orphaned.iter().map(|s| s.as_str()).collect();
            for layer in &mut state.project.layers {
                if orphan_set.contains(layer.group_name.as_str()) {
                    layer.group_name = "__all__".to_string();
                }
            }

            state.dirty = true;

            // Show summary if anything changed
            if !added.is_empty() || !orphaned.is_empty() {
                state.reload_summary = Some(ReloadSummary {
                    kept,
                    added,
                    orphaned,
                });
            }

            state.status_message = "Mesh reloaded.".to_string();
        }
        Err(e) => {
            state.status_message = e;
        }
    }
}

/// Open a file dialog to pick a new mesh file for replacement.
/// Loads the mesh into a pending popup — state is NOT modified until user confirms.
pub fn replace_mesh(state: &mut AppState) {
    state.modal_dialog_active = true;
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Replace mesh")
        .pick_file();
    state.modal_dialog_active = false;

    let Some(path) = path else {
        return;
    };

    let mesh = match load_mesh(&path) {
        Ok(m) => m,
        Err(e) => {
            state.status_message = format!("Mesh load failed: {e}");
            return;
        }
    };

    let filename = path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let format = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let mesh_bytes = std::fs::read(&path).ok();

    state.mesh_load_popup = Some(build_mesh_load_popup(
        mesh, &filename, &format, path, mesh_bytes, true,
    ));
    state.status_message = format!("Replace mesh — {}", filename);
}

/// Save the project to its current path, or show a Save As dialog.
pub fn save_project_action(state: &mut AppState) {
    let path = if let Some(ref path) = state.project_path {
        path.clone()
    } else {
        state.modal_dialog_active = true;
        let result = rfd::FileDialog::new()
            .add_filter("PA Painter Project", &["papr"])
            .save_file();
        state.modal_dialog_active = false;
        let Some(mut path) = result else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("papr");
        }
        path
    };

    state.project.manifest.modified_at = utc_now_iso8601();

    let editor_json = serde_json::to_vec_pretty(&state.extract_editor_state()).ok();
    match save_project(&state.project, &path, editor_json.as_deref()) {
        Ok(()) => {
            state.project_path = Some(path);
            state.dirty = false;
            state.status_message = "Project saved".to_string();
        }
        Err(e) => {
            state.status_message = format!("Save failed: {e:?}");
        }
    }
}

/// Always show a Save As dialog, regardless of whether a project path exists.
pub fn save_project_as_action(state: &mut AppState) {
    state.modal_dialog_active = true;
    let mut dialog = rfd::FileDialog::new().add_filter("PA Painter Project", &["papr"]);
    if let Some(ref path) = state.project_path {
        if let Some(dir) = path.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = path.file_name() {
            dialog = dialog.set_file_name(name.to_string_lossy());
        }
    }
    let result = dialog.save_file();
    state.modal_dialog_active = false;
    let Some(mut path) = result else {
        return;
    };
    if path.extension().is_none() {
        path.set_extension("papr");
    }

    state.project.manifest.modified_at = utc_now_iso8601();

    let editor_json = serde_json::to_vec_pretty(&state.extract_editor_state()).ok();
    match save_project(&state.project, &path, editor_json.as_deref()) {
        Ok(()) => {
            state.project_path = Some(path);
            state.dirty = false;
            state.status_message = "Project saved".to_string();
        }
        Err(e) => {
            state.status_message = format!("Save failed: {e:?}");
        }
    }
}
