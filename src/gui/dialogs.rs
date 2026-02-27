use std::path::{Path, PathBuf};

use practical_arcana_painter::asset_io::{extract_uv_edges, load_mesh, load_texture};
use practical_arcana_painter::glb_export;
use practical_arcana_painter::output::{
    export_color_png, export_height_png, export_normal_png, export_stroke_id_png,
    normalize_height_map,
};
use practical_arcana_painter::project::{load_project, save_project, utc_now_iso8601, BaseColor, Project};
use practical_arcana_painter::types::{pixels_to_colors, BackgroundMode, Layer, PaintValues};

use super::state::ReloadSummary;

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
    state.cached_texture_colors = Some(pixels_to_colors(&tex.pixels));
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
            if let Some(tex_path) = project.base_color.texture_path() {
                let tex_file = resolve_asset_path(&path, tex_path);
                if let Err(e) = apply_texture(state, ctx, &tex_file) {
                    state.status_message = e;
                }
            }

            // Load base normal if referenced
            if let Some(ref normal_path) = project.base_normal {
                let normal_file = resolve_asset_path(&path, normal_path);
                match load_texture(&normal_file) {
                    Ok(tex) => {
                        state.loaded_normal = Some(tex);
                    }
                    Err(_) => {
                        // Normal map is optional — silently skip if missing
                    }
                }
            }

            // Select first layer if any
            if !project.layers.is_empty() {
                state.selected_layer = Some(0);
            }

            state.project = project;
            state.project_path = Some(path);
            state.dirty = false;
            state.generated = None;
            state.generation_snapshot = None;
            state.generation.discard();
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.undo.clear();
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
/// Auto-creates one layer per mesh group (or a single "__all__" layer if none).
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
    project.mesh_ref.path = mesh_path.to_string_lossy().to_string();

    // Load the mesh
    match apply_mesh(state, &mesh_path) {
        Ok(()) => {
            // Auto-create layers from mesh groups
            project.layers = create_layers_from_mesh(state);

            state.project = project;
            state.project_path = None;
            state.dirty = false;
            state.selected_layer = if state.project.layers.is_empty() {
                None
            } else {
                Some(0)
            };
            state.selected_guide = None;
            state.generated = None;
            state.generation_snapshot = None;
            state.generation.discard();
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.textures.base_texture = None;
            state.loaded_texture = None;
            state.cached_texture_colors = None;
            state.loaded_normal = None;
            state.reload_summary = None;
            state.undo.clear();

            let n = state.project.layers.len();
            state.status_message = format!(
                "New project — {} layers from {}",
                n,
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

    match apply_mesh(state, &mesh_path) {
        Ok(()) => {
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
                let order = state.project.layers.len() as i32;
                state.project.layers.push(Layer {
                    name: name.clone(),
                    visible: true,
                    group_name: name.clone(),
                    order,
                    paint: PaintValues::default(),
                    guides: vec![],
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

/// Open a file dialog to pick a new mesh file, replacing the current one.
/// Reuses the reload_mesh diff/summary logic.
pub fn replace_mesh(state: &mut AppState) {
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Replace mesh")
        .pick_file();

    let Some(path) = path else {
        return;
    };

    // Update mesh path and delegate to reload_mesh logic
    state.project.mesh_ref.path = path.to_string_lossy().to_string();
    reload_mesh(state);
    state.status_message = format!("Mesh replaced: {}", path.display());
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
            state.project.base_color = BaseColor::Texture(tex_path.to_string_lossy().to_string());
            // Invalidate generated maps so viewport shows the new base texture
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.generated = None;
            state.generation_snapshot = None;
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

/// Reload the base color texture from its current path.
pub fn reload_texture(state: &mut AppState, ctx: &eframe::egui::Context) {
    let Some(tex_path) = state.project.base_color.texture_path().map(|s| s.to_string()) else {
        state.status_message = "No texture to reload.".to_string();
        return;
    };
    match apply_texture(state, ctx, Path::new(&tex_path)) {
        Ok(()) => {
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.generated = None;
            state.generation_snapshot = None;
            state.status_message = "Texture reloaded.".to_string();
        }
        Err(e) => state.status_message = e,
    }
}

/// Reload the base normal map from its current path.
pub fn reload_normal(state: &mut AppState) {
    let Some(ref normal_path) = state.project.base_normal.clone() else {
        state.status_message = "No normal map to reload.".to_string();
        return;
    };
    match load_texture(Path::new(normal_path)) {
        Ok(tex) => {
            state.loaded_normal = Some(tex);
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.generated = None;
            state.generation_snapshot = None;
            state.status_message = "Normal reloaded.".to_string();
        }
        Err(e) => state.status_message = format!("Normal reload failed: {e}"),
    }
}

/// Open a file dialog to load (or replace) the base normal map.
pub fn load_normal_dialog(state: &mut AppState) {
    let path = rfd::FileDialog::new()
        .add_filter("Image", &["png", "jpg", "jpeg", "bmp", "tga", "exr"])
        .set_title("Load base normal map")
        .pick_file();

    let Some(tex_path) = path else {
        return;
    };

    match load_texture(&tex_path) {
        Ok(tex) => {
            state.loaded_normal = Some(tex);
            state.project.base_normal = Some(tex_path.to_string_lossy().to_string());
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.generated = None;
            state.generation_snapshot = None;
            state.dirty = true;
            state.status_message = format!(
                "Loaded normal: {}",
                tex_path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
        }
        Err(e) => {
            state.status_message = format!("Normal load failed: {e}");
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

    state.project.manifest.modified_at = utc_now_iso8601();

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

/// Create default layers from currently loaded mesh groups.
/// One layer per group, or a single "__all__" if no groups.
fn create_layers_from_mesh(state: &AppState) -> Vec<Layer> {
    let group_names: Vec<String> = state
        .loaded_mesh
        .as_ref()
        .map(|m| m.groups.iter().map(|g| g.name.clone()).collect())
        .unwrap_or_default();

    if group_names.is_empty() {
        vec![Layer {
            name: "__all__".to_string(),
            visible: true,
            group_name: "__all__".to_string(),
            order: 0,
            paint: PaintValues::default(),
            guides: vec![],
        }]
    } else {
        group_names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Layer {
                name: name.clone(),
                visible: true,
                group_name: name,
                order: i as i32,
                paint: PaintValues::default(),
                guides: vec![],
            })
            .collect()
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
