use std::path::{Path, PathBuf};

use practical_arcana_painter::asset_io::{extract_uv_edges, load_mesh, load_texture};
use practical_arcana_painter::glb_export;
use practical_arcana_painter::output::{
    export_color_png, export_height_png, export_normal_png, export_stroke_id_png,
    normalize_height_map,
};
use practical_arcana_painter::project::{load_project, save_project, Project};
use practical_arcana_painter::types::BackgroundMode;

use super::state::AppState;
use super::textures;

// ── Helpers ────────────────────────────────────────────────────────

/// Load a mesh from the given path into app state.
fn apply_mesh(state: &mut AppState, mesh_path: &Path) -> Result<(), String> {
    let mesh = load_mesh(mesh_path).map_err(|e| format!("Mesh load failed: {e}"))?;
    state.uv_edges = Some(extract_uv_edges(&mesh));
    state.loaded_mesh = Some(mesh);
    Ok(())
}

/// Load a texture from the given path into app state.
fn apply_texture(
    state: &mut AppState,
    ctx: &eframe::egui::Context,
    tex_path: &Path,
) -> Result<(), String> {
    let tex = load_texture(tex_path).map_err(|e| format!("Texture load failed: {e}"))?;
    let handle = textures::loaded_texture_to_handle(ctx, &tex, "base_color");
    state.textures.base_texture = Some(handle);
    state.loaded_texture = Some(tex);
    Ok(())
}

// ── Project Operations ─────────────────────────────────────────────

/// Open a file dialog and load a .pap project.
/// Returns true if a project was successfully loaded.
pub fn open_project(state: &mut AppState, ctx: &eframe::egui::Context) -> bool {
    let path = rfd::FileDialog::new()
        .add_filter("PAP Project", &["pap"])
        .pick_file();

    let Some(path) = path else {
        return false;
    };

    match load_project(&path) {
        Ok(project) => {
            state.status_message = format!("Loaded: {}", path.display());

            // Load mesh if referenced
            let mesh_path = resolve_asset_path(&path, &project.mesh_ref.path);
            if let Err(e) = apply_mesh(state, &mesh_path) {
                state.status_message = e;
            }

            // Load texture if referenced
            if let Some(ref tex_path) = project.color_ref.path {
                let tex_file = resolve_asset_path(&path, tex_path);
                if let Err(e) = apply_texture(state, ctx, &tex_file) {
                    state.status_message = e;
                }
            }

            // Select first slot if any
            if !project.slots.is_empty() {
                state.selected_slot = Some(0);
            }

            state.project = project;
            state.project_path = Some(path);
            state.dirty = false;
            true
        }
        Err(e) => {
            state.status_message = format!("Failed to load project: {e:?}");
            false
        }
    }
}

/// Create a new project by loading a mesh file.
/// Opens a file dialog for mesh selection, then starts a fresh project.
pub fn new_project(state: &mut AppState, _ctx: &eframe::egui::Context) {
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Select mesh for new project")
        .pick_file();

    let Some(mesh_path) = path else {
        return;
    };

    // Reset to a fresh project
    let mut project = Project::default();

    // Store relative mesh path
    project.mesh_ref.path = mesh_path.to_string_lossy().to_string();

    // Load the mesh
    match apply_mesh(state, &mesh_path) {
        Ok(()) => {
            state.project = project;
            state.project_path = None;
            state.dirty = false;
            state.selected_slot = None;
            state.selected_guide = None;
            state.generated = None;
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.loaded_texture = None;
            state.status_message = format!(
                "New project — mesh: {}",
                mesh_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
        Err(e) => {
            state.status_message = e;
        }
    }
}

/// Open a file dialog to load (or replace) the mesh in the current project.
pub fn load_mesh_dialog(state: &mut AppState, _ctx: &eframe::egui::Context) {
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Load mesh")
        .pick_file();

    let Some(mesh_path) = path else {
        return;
    };

    match apply_mesh(state, &mesh_path) {
        Ok(()) => {
            state.project.mesh_ref.path = mesh_path.to_string_lossy().to_string();
            state.dirty = true;
            state.status_message = format!(
                "Loaded mesh: {}",
                mesh_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
        Err(e) => {
            state.status_message = e;
        }
    }
}

/// Open a file dialog to load (or replace) the base color texture.
pub fn load_texture_dialog(state: &mut AppState, ctx: &eframe::egui::Context) {
    let path = rfd::FileDialog::new()
        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tga"])
        .set_title("Load base texture")
        .pick_file();

    let Some(tex_path) = path else {
        return;
    };

    match apply_texture(state, ctx, &tex_path) {
        Ok(()) => {
            state.project.color_ref.path = Some(tex_path.to_string_lossy().to_string());
            state.dirty = true;
            state.status_message = format!(
                "Loaded texture: {}",
                tex_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
        Err(e) => {
            state.status_message = e;
        }
    }
}

/// Save the project to its current path, or show a Save As dialog.
pub fn save_project_action(state: &mut AppState) {
    let path = if let Some(ref path) = state.project_path {
        path.clone()
    } else {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PAP Project", &["pap"])
            .save_file()
        else {
            return;
        };
        path
    };

    match save_project(&state.project, &path) {
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

/// Export generated maps to a user-selected folder as PNG files.
pub fn export_maps(state: &mut AppState) {
    let Some(ref gen) = state.generated else {
        state.status_message = "Nothing to export — generate first".to_string();
        return;
    };

    let Some(dir) = rfd::FileDialog::new().pick_folder() else {
        return;
    };

    let res = gen.resolution;
    let with_alpha = state.project.settings.background_mode == BackgroundMode::Transparent;

    if let Err(e) = export_color_png(&gen.color, res, &dir.join("color_map.png"), with_alpha) {
        state.status_message = format!("Export failed: {e:?}");
        return;
    }
    if let Err(e) = export_height_png(&gen.height, res, &dir.join("height_map.png")) {
        state.status_message = format!("Export failed: {e:?}");
        return;
    }
    if let Err(e) = export_normal_png(&gen.normal_map, res, &dir.join("normal_map.png")) {
        state.status_message = format!("Export failed: {e:?}");
        return;
    }
    if let Err(e) = export_stroke_id_png(&gen.stroke_id, res, &dir.join("stroke_id_map.png")) {
        state.status_message = format!("Export failed: {e:?}");
        return;
    }

    state.status_message = format!("Exported 4 maps to {}", dir.display());
}

/// Export a 3D preview GLB with paint textures baked onto the mesh.
pub fn export_glb(state: &mut AppState) {
    let Some(ref gen) = state.generated else {
        state.status_message = "Nothing to export — generate first".to_string();
        return;
    };
    let Some(ref mesh) = state.loaded_mesh else {
        state.status_message = "No mesh loaded".to_string();
        return;
    };

    let Some(path) = rfd::FileDialog::new()
        .add_filter("glTF Binary", &["glb"])
        .set_file_name("preview.glb")
        .save_file()
    else {
        return;
    };

    let normalized_height = normalize_height_map(&gen.height);
    let transparent = state.project.settings.background_mode == BackgroundMode::Transparent;

    let result = if transparent {
        glb_export::export_preview_glb_transparent(
            mesh,
            &gen.color,
            &normalized_height,
            &gen.normal_map,
            gen.resolution,
            0.0,
            &path,
        )
    } else {
        glb_export::export_preview_glb(
            mesh,
            &gen.color,
            &normalized_height,
            &gen.normal_map,
            gen.resolution,
            0.0,
            &path,
        )
    };

    match result {
        Ok(()) => {
            state.status_message = format!("Exported GLB to {}", path.display());
        }
        Err(e) => {
            state.status_message = format!("GLB export failed: {e:?}");
        }
    }
}

fn resolve_asset_path(project_path: &Path, asset_path: &str) -> PathBuf {
    let p = Path::new(asset_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(asset_path)
    }
}
