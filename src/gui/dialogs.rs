use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use practical_arcana_painter::asset_io::{
    extract_uv_edges, load_mesh, LoadedMesh, MeshMaterialInfo,
};
use practical_arcana_painter::glb_export;
use practical_arcana_painter::output::{
    export_color_exr, export_color_png, export_height_exr, export_height_png, export_layer_maps,
    export_manifest, export_normal_png, export_stroke_id_png, export_stroke_time_exr,
    export_stroke_time_png, normalize_height_map, ExportFormat, LayerExportOptions,
    LayerManifestEntry,
};
use practical_arcana_painter::project::{
    load_project, save_project, utc_now_iso8601, OutputCache, Project,
};
use practical_arcana_painter::types::{
    BackgroundMode, EmbeddedTexture, Layer, PaintValues, TextureSource,
};

use super::state::{AppState, LayerMapping, MeshLoadPopup, ReloadSummary};
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
        state.auto_gen_suppressed = false;
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
        state.auto_gen_suppressed = false;
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
                    stroke_time_order: output.stroke_time_order,
                    stroke_time_arc: output.stroke_time_arc,
                    resolution: r,
                    elapsed: std::time::Duration::ZERO,
                    computed_normals: None,
                    rendered_layers: Vec::new(),
                    rendered_paths: Vec::new(),
                    gen_normal_strength: state.project.settings.normal_strength,
                    gen_normal_mode: state.project.settings.normal_mode,
                    gen_background_mode: state.project.settings.background_mode,
                });
                // Create texture handles so UV View displays the maps
                let gen = state.generated.as_ref().unwrap();
                state.textures.color = Some(textures::color_buffer_to_handle(
                    ctx,
                    &gen.color,
                    r,
                    r,
                    "loaded_color",
                ));
                state.textures.height = Some(textures::height_buffer_to_handle(
                    ctx,
                    &gen.height,
                    r,
                    "loaded_height",
                ));
                state.textures.normal = Some(textures::normal_map_to_handle(
                    ctx,
                    &gen.normal_map,
                    r,
                    "loaded_normal",
                ));
                state.textures.stroke_id = Some(textures::stroke_id_to_handle(
                    ctx,
                    &gen.stroke_id,
                    r,
                    "loaded_stroke_id",
                ));
            } else {
                state.generated = None;
                state.auto_gen_suppressed = false;
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
/// Opens a file dialog for mesh selection, loads the mesh into a pending popup.
/// State is NOT modified until the user confirms (OK) — Cancel discards everything.
pub fn new_project(state: &mut AppState, _ctx: &eframe::egui::Context) {
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Select mesh for new project")
        .pick_file();

    let Some(mesh_path) = path else {
        return;
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
                let seed = state.project.layers.len() as u32;
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
    let path = rfd::FileDialog::new()
        .add_filter("3D Mesh", &["glb", "gltf", "obj"])
        .set_title("Replace mesh")
        .pick_file();

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
        stroke_time_order: &gen.stroke_time_order,
        stroke_time_arc: &gen.stroke_time_arc,
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

/// Derive the export subfolder name from the project file path.
/// Returns `"untitled"` if no project has been saved yet.
fn export_folder_name(state: &AppState) -> String {
    state
        .project_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("untitled")
        .to_string()
}

/// Build the list of filenames that will be written for a given export config.
fn planned_export_files(state: &AppState, include_glb: bool) -> Vec<String> {
    let es = &state.project.export_settings;
    let ext = match es.format {
        ExportFormat::Png => "png",
        ExportFormat::Exr => "exr",
    };
    let mut files = Vec::new();

    if es.export_maps || include_glb {
        if es.include_color {
            files.push(format!("color_map.{ext}"));
        }
        if es.include_height {
            files.push(format!("height_map.{ext}"));
        }
        if es.include_normal {
            files.push("normal_map.png".to_string());
        }
        if es.include_stroke_id {
            files.push("stroke_id_map.png".to_string());
        }
        if es.include_time_map {
            files.push(format!("stroke_time_map.{ext}"));
        }
    }

    if es.per_layer {
        let visible_count = state.project.layers.iter().filter(|l| l.visible).count();
        for i in 0..visible_count {
            if es.include_color {
                files.push(format!("layer_{i}_color.{ext}"));
            }
            if es.include_height {
                files.push(format!("layer_{i}_height.{ext}"));
            }
            if es.include_normal {
                files.push(format!("layer_{i}_normal.png"));
            }
            if es.include_time_map {
                files.push(format!("layer_{i}_stroke_time.{ext}"));
            }
        }
        files.push("manifest.json".to_string());
    }

    if include_glb && es.export_model {
        files.push("preview.glb".to_string());
    }

    files
}

/// Check if any of the planned export files already exist in `dir`.
/// If so, show a confirmation dialog with the conflict count.
/// Returns `false` if the user cancels.
fn confirm_overwrite(dir: &Path, planned_files: &[String]) -> bool {
    if !dir.exists() {
        return true;
    }
    let conflicts: usize = planned_files
        .iter()
        .filter(|name| dir.join(name).exists())
        .count();
    if conflicts == 0 {
        return true;
    }
    rfd::MessageDialog::new()
        .set_title("Overwrite existing files?")
        .set_description(format!(
            "{conflicts} file(s) will be overwritten in \"{}\".",
            dir.file_name().and_then(|n| n.to_str()).unwrap_or("folder"),
        ))
        .set_buttons(rfd::MessageButtons::OkCancel)
        .show()
        == rfd::MessageDialogResult::Ok
}

/// Export generated maps to a user-selected folder as PNG files.
pub fn export_maps(state: &mut AppState) {
    let Some(parent) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    let dir = parent.join(export_folder_name(state));
    let planned = planned_export_files(state, false);
    if !confirm_overwrite(&dir, &planned) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        state.status_message = format!("Failed to create folder: {e}");
        return;
    }
    export_maps_to(state, &dir);
}

/// Export generated maps to the given folder (no dialog).
pub fn export_maps_to(state: &mut AppState, dir: &Path) {
    let Some(ref gen) = state.generated else {
        state.status_message = "Nothing to export — generate first".to_string();
        return;
    };

    let es = &state.project.export_settings;
    let res = gen.resolution;
    let with_alpha = state.project.settings.background_mode == BackgroundMode::Transparent;
    let is_exr = es.format == ExportFormat::Exr;
    let mut count = 0u32;

    if es.include_color {
        let result = if is_exr {
            export_color_exr(&gen.color, res, &dir.join("color_map.exr"), with_alpha)
        } else {
            export_color_png(&gen.color, res, &dir.join("color_map.png"), with_alpha)
        };
        if let Err(e) = result {
            state.status_message = format!("Export failed: {e:?}");
            return;
        }
        count += 1;
    }
    if es.include_height {
        let normalized = normalize_height_map(&gen.height);
        let result = if is_exr {
            export_height_exr(&normalized, res, &dir.join("height_map.exr"))
        } else {
            export_height_png(&normalized, res, &dir.join("height_map.png"))
        };
        if let Err(e) = result {
            state.status_message = format!("Export failed: {e:?}");
            return;
        }
        count += 1;
    }
    if es.include_normal {
        if let Err(e) = export_normal_png(&gen.normal_map, res, &dir.join("normal_map.png")) {
            state.status_message = format!("Export failed: {e:?}");
            return;
        }
        count += 1;
    }
    if es.include_stroke_id {
        if let Err(e) = export_stroke_id_png(&gen.stroke_id, res, &dir.join("stroke_id_map.png")) {
            state.status_message = format!("Export failed: {e:?}");
            return;
        }
        count += 1;
    }
    if es.include_time_map {
        let result = if is_exr {
            export_stroke_time_exr(
                &gen.stroke_time_order,
                &gen.stroke_time_arc,
                res,
                &dir.join("stroke_time_map.exr"),
            )
        } else {
            export_stroke_time_png(
                &gen.stroke_time_order,
                &gen.stroke_time_arc,
                res,
                &dir.join("stroke_time_map.png"),
            )
        };
        if let Err(e) = result {
            state.status_message = format!("Export failed: {e:?}");
            return;
        }
        count += 1;
    }

    // ── Per-Layer Export ──
    if es.per_layer {
        let mut sorted_layers: Vec<&Layer> =
            state.project.layers.iter().filter(|l| l.visible).collect();
        sorted_layers.sort_by_key(|l| l.order);

        let normal_strength = state.project.settings.normal_strength;
        let normal_mode = state.project.settings.normal_mode;
        let normal_data = state
            .cached_mesh_normals
            .as_ref()
            .map(|(_, nd)| nd.as_ref());

        let mut manifest_entries = Vec::new();
        for (idx, layer) in sorted_layers.iter().enumerate() {
            let hash = layer.render_hash();
            let Some((_, maps)) = state
                .generation
                .layer_cache
                .iter()
                .find(|(h, _)| *h == hash)
            else {
                continue; // cache miss — skip this layer
            };

            match export_layer_maps(
                maps,
                idx,
                &LayerExportOptions {
                    format: es.format,
                    normal_strength,
                    normal_mode,
                    normal_data,
                    include_color: es.include_color,
                    include_height: es.include_height,
                    include_normal: es.include_normal,
                    include_time_map: es.include_time_map,
                },
                dir,
            ) {
                Ok(n) => count += n,
                Err(e) => {
                    state.status_message = format!("Export failed (layer {}): {e:?}", layer.name);
                    return;
                }
            }

            manifest_entries.push(LayerManifestEntry {
                index: idx,
                name: layer.name.clone(),
                group: layer.group_name.clone(),
                order: layer.order,
                visible: layer.visible,
                dry: layer.dry,
            });
        }

        if let Err(e) = export_manifest(&manifest_entries, es.format, dir) {
            state.status_message = format!("Export failed (manifest): {e:?}");
            return;
        }
        count += 1; // manifest.json
    }

    if count == 0 {
        state.status_message = "No maps selected for export".to_string();
    } else {
        state.status_message = format!("Exported {count} map(s) to {}", dir.display());
    }
}

/// Export both texture maps and GLB to a user-selected folder.
pub fn export_both(state: &mut AppState) {
    let Some(parent) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    let dir = parent.join(export_folder_name(state));
    let planned = planned_export_files(state, true);
    if !confirm_overwrite(&dir, &planned) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        state.status_message = format!("Failed to create folder: {e}");
        return;
    }
    export_maps_to(state, &dir);
    // If maps export failed, status_message already set — skip GLB.
    if state.status_message.starts_with("Export failed")
        || state.status_message.starts_with("No maps")
    {
        return;
    }
    let glb_path = dir.join("preview.glb");
    export_glb_to(state, &glb_path);
    if !state.status_message.starts_with("GLB export failed") {
        let maps_msg = state.status_message.clone();
        state.status_message = format!("{maps_msg} + GLB");
    }
}

/// Export a 3D preview GLB — pick folder, output as `preview.glb`.
pub fn export_glb(state: &mut AppState) {
    let Some(parent) = rfd::FileDialog::new().pick_folder() else {
        return;
    };
    let dir = parent.join(export_folder_name(state));
    let planned = vec!["preview.glb".to_string()];
    if !confirm_overwrite(&dir, &planned) {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        state.status_message = format!("Failed to create folder: {e}");
        return;
    }
    export_glb_to(state, &dir.join("preview.glb"));
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
        TextureSource::Solid([0.5, 0.5, 0.5])
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
        TextureSource::Solid([0.5, 0.5, 0.5])
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
    (TextureSource::Solid([0.5, 0.5, 0.5]), TextureSource::None)
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

/// Build a MeshLoadPopup holding the loaded mesh (not yet applied to state).
fn build_mesh_load_popup(
    mesh: LoadedMesh,
    filename: &str,
    format: &str,
    path: PathBuf,
    mesh_bytes: Option<Vec<u8>>,
    is_replace: bool,
) -> MeshLoadPopup {
    let is_glb = format == "glb" || format == "gltf";
    let has_mtl = !is_glb && !mesh.materials.is_empty();

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
    apply_loaded_mesh(state, popup.pending_mesh);

    // Update mesh reference
    state.project.mesh_ref.path = popup.pending_path.to_string_lossy().to_string();
    state.project.mesh_ref.format = popup.pending_format;
    state.project.mesh_bytes = popup.pending_bytes;

    if popup.is_replace {
        // ── Replace mesh ──
        state.undo.clear();

        // Group diff: compare new mesh groups vs existing layers
        let new_groups: Vec<String> = state
            .loaded_mesh
            .as_ref()
            .map(|m| m.groups.iter().map(|g| g.name.clone()).collect())
            .unwrap_or_default();
        let new_set: std::collections::HashSet<&str> =
            new_groups.iter().map(|s| s.as_str()).collect();

        let has_materials = state
            .loaded_mesh
            .as_ref()
            .is_some_and(|m| !m.materials.is_empty());

        let old_groups: Vec<String> = state
            .project
            .layers
            .iter()
            .map(|l| l.group_name.clone())
            .filter(|g| g != "__all__")
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        let old_set: std::collections::HashSet<&str> =
            old_groups.iter().map(|s| s.as_str()).collect();

        let mut kept = Vec::new();
        let mut added = Vec::new();
        let mut orphaned = Vec::new();
        for g in &old_groups {
            if new_set.contains(g.as_str()) {
                kept.push(g.clone());
            } else {
                orphaned.push(g.clone());
            }
        }
        for g in &new_groups {
            if !old_set.contains(g.as_str()) {
                added.push(g.clone());
            }
        }

        // Add layers for new groups
        for name in &added {
            let seed = state.project.layers.len() as u32;
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
                dry: 1.0,
                seed,
            });
        }

        // Remap orphaned layers
        let orphan_set: std::collections::HashSet<&str> =
            orphaned.iter().map(|s| s.as_str()).collect();
        for layer in &mut state.project.layers {
            if orphan_set.contains(layer.group_name.as_str()) {
                layer.group_name = "__all__".to_string();
            }
            if !has_materials {
                if let TextureSource::MeshMaterial(_) = layer.base_color {
                    layer.base_color = TextureSource::Solid([0.5, 0.5, 0.5]);
                }
                if let TextureSource::MeshMaterial(_) = layer.base_normal {
                    layer.base_normal = TextureSource::None;
                }
            }
        }

        // Apply material mappings (skip disabled)
        for (i, lm) in active_mappings.iter().enumerate() {
            if !popup.layer_enabled.get(i).copied().unwrap_or(true) {
                continue;
            }
            if let Some(layer) = state
                .project
                .layers
                .iter_mut()
                .find(|l| l.group_name == lm.name)
            {
                if matches!(layer.base_color, TextureSource::Solid(c) if (c[0] - 0.5).abs() < 0.01 && (c[1] - 0.5).abs() < 0.01 && (c[2] - 0.5).abs() < 0.01)
                    || matches!(layer.base_color, TextureSource::MeshMaterial(_))
                {
                    layer.base_color = lm.base_color.clone();
                }
                if matches!(
                    layer.base_normal,
                    TextureSource::None | TextureSource::MeshMaterial(_)
                ) {
                    layer.base_normal = lm.base_normal.clone();
                }
            }
        }

        if !added.is_empty() || !orphaned.is_empty() {
            state.reload_summary = Some(ReloadSummary {
                kept,
                added,
                orphaned,
            });
        }

        state.dirty = true;
    } else {
        // ── New project ── reset everything, preserving mesh_ref set above
        let mut new_project = Project::default();
        std::mem::swap(&mut new_project.mesh_ref, &mut state.project.mesh_ref);
        new_project.mesh_bytes = state.project.mesh_bytes.take();
        state.project = new_project;
        state.project_path = None;
        state.dirty = false;
        state.selected_guide = None;
        state.generated = None;
        state.auto_gen_suppressed = false;
        state.generation_snapshot = None;
        state.generation.discard();
        state.textures.color = None;
        state.textures.height = None;
        state.textures.normal = None;
        state.textures.stroke_id = None;
        state.textures.base_texture = None;
        state.reload_summary = None;
        state.path_overlay.clear();
        state.undo.clear();

        // Create only enabled layers
        for (i, lm) in active_mappings.iter().enumerate() {
            if !popup.layer_enabled.get(i).copied().unwrap_or(true) {
                continue;
            }
            state.project.layers.push(Layer {
                name: lm.name.clone(),
                visible: true,
                group_name: lm.name.clone(),
                order: state.project.layers.len() as i32,
                paint: PaintValues::default(),
                guides: vec![],
                base_color: lm.base_color.clone(),
                base_normal: lm.base_normal.clone(),
                dry: 1.0,
                seed: state.project.layers.len() as u32,
            });
        }
        state.selected_layer = if state.project.layers.is_empty() {
            None
        } else {
            Some(0)
        };
    }
}
