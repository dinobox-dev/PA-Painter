use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use practical_arcana_painter::asset_io::{extract_uv_edges, load_mesh, LoadedMesh};
use practical_arcana_painter::glb_export;
use practical_arcana_painter::output::{
    export_color_png, export_height_png, export_normal_png, export_stroke_id_png,
    normalize_height_map,
};
use practical_arcana_painter::project::{
    load_project, save_project, utc_now_iso8601, OutputCache, Project,
};
use practical_arcana_painter::types::{BackgroundMode, Layer, PaintValues, TextureSource};

use super::state::ReloadSummary;
use super::state::AppState;
use super::textures;

// ── Helpers ────────────────────────────────────────────────────────

/// Load a mesh from the given path into app state.
/// Returns `Ok(true)` if geometry changed (hash mismatch), `Ok(false)` if identical.
fn apply_mesh(state: &mut AppState, mesh_path: &Path) -> Result<bool, String> {
    let mesh = load_mesh(mesh_path).map_err(|e| format!("Mesh load failed: {e}"))?;
    state.uv_edges = Some(extract_uv_edges(&mesh));

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
    }

    state.loaded_mesh = Some(Arc::new(mesh));
    Ok(changed)
}

/// Apply an already-loaded mesh (e.g. from a .pap file) into app state.
/// Returns `true` if geometry changed (hash mismatch), `false` if identical.
fn apply_loaded_mesh(state: &mut AppState, mesh: LoadedMesh) -> bool {
    state.uv_edges = Some(extract_uv_edges(&mesh));

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
    }

    state.loaded_mesh = Some(Arc::new(mesh));
    changed
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
        Ok(result) => {
            state.status_message = format!("Loaded: {}", path.display());

            // Apply embedded mesh if present
            if let Some(mesh) = result.mesh {
                apply_loaded_mesh(state, mesh);
            }

            // Select first layer if any
            if !result.project.layers.is_empty() {
                state.selected_layer = Some(0);
            }

            state.project = result.project;
            state.project_path = Some(path);
            state.dirty = false;
            state.generation_snapshot = None;
            state.generation.discard();
            state.post_gen_export_maps = None;
            state.post_gen_export_glb = None;
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.path_overlay.clear();
            state.undo.clear();

            // Restore cached generation output if present
            if let Some(output) = result.output {
                let pixel_count = output.color.len();
                let r = output.resolution;
                state.generation_snapshot = output.snapshot_hash;
                state.generated = Some(super::generation::GenResult {
                    color: output.color,
                    height: output.height,
                    normal_map: output.normal_map,
                    stroke_id: vec![0; pixel_count],
                    resolution: r,
                    elapsed: std::time::Duration::ZERO,
                    computed_normals: None,
                });
                // Create texture handles so UV View displays the maps
                let gen = state.generated.as_ref().unwrap();
                state.textures.color = Some(textures::color_buffer_to_handle(
                    ctx, &gen.color, r, r, "loaded_color",
                ));
                state.textures.height = Some(textures::height_buffer_to_handle(
                    ctx, &gen.height, r, "loaded_height",
                ));
                state.textures.normal = Some(textures::normal_map_to_handle(
                    ctx, &gen.normal_map, r, "loaded_normal",
                ));
                state.textures.stroke_id = Some(textures::stroke_id_to_handle(
                    ctx, &gen.stroke_id, r, "loaded_stroke_id",
                ));
            } else {
                state.generated = None;
            }

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
    project.mesh_ref.format = mesh_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    project.mesh_bytes = std::fs::read(&mesh_path).ok();

    // Load the mesh
    match apply_mesh(state, &mesh_path) {
        Ok(_) => {
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
            state.post_gen_export_maps = None;
            state.post_gen_export_glb = None;
            state.textures.color = None;
            state.textures.height = None;
            state.textures.normal = None;
            state.textures.stroke_id = None;
            state.textures.base_texture = None;
            state.reload_summary = None;
            state.path_overlay.clear();
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
                let order = state.project.layers.len() as i32;
                state.project.layers.push(Layer {
                    name: name.clone(),
                    visible: true,
                    group_name: name.clone(),
                    order,
                    paint: PaintValues::default(),
                    guides: vec![],
                    base_color: TextureSource::Solid([0.5, 0.5, 0.5]),
                    base_normal: TextureSource::None,
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

    // Update mesh path, format, and bytes; then delegate to reload_mesh logic
    state.project.mesh_ref.path = path.to_string_lossy().to_string();
    state.project.mesh_ref.format = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    state.project.mesh_bytes = std::fs::read(&path).ok();
    reload_mesh(state);
    state.status_message = format!("Mesh replaced: {}", path.display());
}

/// Save the project to its current path, or show a Save As dialog.
pub fn save_project_action(state: &mut AppState) {
    let path = if let Some(ref path) = state.project_path {
        path.clone()
    } else {
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PAP Project", &["pap"])
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("pap");
        }
        path
    };

    state.project.manifest.modified_at = utc_now_iso8601();

    let output = state.generated.as_ref().map(|gen| OutputCache {
        color: &gen.color,
        height: &gen.height,
        normal_map: &gen.normal_map,
        resolution: gen.resolution,
        snapshot_hash: state.generation_snapshot,
    });

    match save_project(&state.project, &path, output) {
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
    let Some(dir) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    export_maps_to(state, &dir);
}

/// Export generated maps to the given folder (no dialog).
pub fn export_maps_to(state: &mut AppState, dir: &Path) {
    let Some(ref gen) = state.generated else {
        state.status_message = "Nothing to export — generate first".to_string();
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
    let Some(path) = rfd::FileDialog::new()
        .add_filter("glTF Binary", &["glb"])
        .set_file_name("preview.glb")
        .save_file()
    else {
        return;
    };
    export_glb_to(state, &path);
}

/// Export a 3D preview GLB to the given path (no dialog).
pub fn export_glb_to(state: &mut AppState, path: &Path) {
    let Some(ref gen) = state.generated else {
        state.status_message = "Nothing to export — generate first".to_string();
        return;
    };
    let Some(ref mesh) = state.loaded_mesh else {
        state.status_message = "No mesh loaded".to_string();
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
            path,
        )
    } else {
        glb_export::export_preview_glb(
            mesh,
            &gen.color,
            &normalized_height,
            &gen.normal_map,
            gen.resolution,
            0.0,
            path,
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
            base_color: TextureSource::Solid([0.5, 0.5, 0.5]),
            base_normal: TextureSource::None,
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
                base_color: TextureSource::Solid([0.5, 0.5, 0.5]),
                base_normal: TextureSource::None,
            })
            .collect()
    }
}

