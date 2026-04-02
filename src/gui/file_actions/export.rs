//! Map and GLB export operations with background worker support.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use pa_painter::io::glb_export;
use pa_painter::mesh::asset_io::LoadedMesh;
use pa_painter::mesh::object_normal::MeshNormalData;
use pa_painter::pipeline::compositing::LayerMaps;
use pa_painter::pipeline::output::{
    export_color_exr, export_color_png, export_height_exr, export_height_png, export_layer_maps,
    export_manifest, export_normal_png, export_stroke_id_png, export_stroke_time_exr,
    export_stroke_time_png, normalize_height_map, ExportFormat, LayerExportOptions,
    LayerManifestEntry,
};
use pa_painter::types::{BackgroundMode, Color, ExportSettings, Layer, NormalMode};

use crate::gui::state::AppState;

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

/// Count how many of the planned export files already exist in `dir`.
fn count_conflicts(dir: &Path, planned_files: &[String]) -> usize {
    if !dir.exists() {
        return 0;
    }
    planned_files
        .iter()
        .filter(|name| dir.join(name).exists())
        .count()
}

// ── Background Export ──────────────────────────────────────────────

/// Per-layer data for background export.
struct ExportLayerEntry {
    idx: usize,
    name: String,
    group_name: String,
    order: i32,
    visible: bool,
    dry: f32,
    maps: Arc<LayerMaps>,
}

/// All owned data the export worker thread needs.
struct ExportInput {
    dir: PathBuf,
    es: ExportSettings,
    resolution: u32,
    with_alpha: bool,
    // Composite map data (cloned from GenResult)
    color: Vec<Color>,
    height: Vec<f32>,
    normal_map: Vec<[f32; 3]>,
    stroke_id: Vec<u32>,
    stroke_time_order: Vec<f32>,
    stroke_time_arc: Vec<f32>,
    // Per-layer data
    per_layer_maps: Vec<ExportLayerEntry>,
    normal_strength: f32,
    normal_mode: NormalMode,
    normal_data: Option<Arc<MeshNormalData>>,
    // GLB
    do_glb: bool,
    mesh: Option<Arc<LoadedMesh>>,
}

/// Count the total export steps for progress tracking.
fn count_export_steps(es: &ExportSettings, layer_count: usize, do_glb: bool) -> u32 {
    let mut n = 0u32;
    if es.export_maps {
        if es.include_color {
            n += 1;
        }
        if es.include_height {
            n += 1;
        }
        if es.include_normal {
            n += 1;
        }
        if es.include_stroke_id {
            n += 1;
        }
        if es.include_time_map {
            n += 1;
        }
        if es.per_layer {
            n += layer_count as u32 + 1;
        }
    }
    if do_glb {
        n += 1;
    }
    n.max(1)
}

/// Build an ExportInput from the current app state.
/// Returns None if there is nothing to export.
fn gather_export_input(state: &AppState, dir: PathBuf, do_glb: bool) -> Option<ExportInput> {
    let gen = state.generated.as_ref()?;
    let es = state.project.export_settings.clone();
    let with_alpha = state.project.settings.background_mode == BackgroundMode::Transparent;
    let need_stroke_id = es.export_maps && es.include_stroke_id;
    let need_time = es.export_maps && es.include_time_map;

    // Gather per-layer data if needed
    let mut per_layer_maps = Vec::new();
    if es.per_layer {
        let mut sorted: Vec<&Layer> = state.project.layers.iter().filter(|l| l.visible).collect();
        sorted.sort_by_key(|l| l.order);
        for (idx, layer) in sorted.iter().enumerate() {
            let hash = layer.render_hash();
            if let Some((_, maps)) = state
                .generation
                .layer_cache
                .iter()
                .find(|(h, _)| *h == hash)
            {
                per_layer_maps.push(ExportLayerEntry {
                    idx,
                    name: layer.name.clone(),
                    group_name: layer.group_name.clone(),
                    order: layer.order,
                    visible: layer.visible,
                    dry: layer.dry,
                    maps: Arc::clone(maps),
                });
            }
        }
    }

    Some(ExportInput {
        dir,
        es,
        resolution: gen.resolution,
        with_alpha,
        color: gen.color.clone(),
        height: gen.height.clone(),
        normal_map: gen.normal_map.clone(),
        stroke_id: if need_stroke_id {
            gen.stroke_id.clone()
        } else {
            Vec::new()
        },
        stroke_time_order: if need_time {
            gen.stroke_time_order.clone()
        } else {
            Vec::new()
        },
        stroke_time_arc: if need_time {
            gen.stroke_time_arc.clone()
        } else {
            Vec::new()
        },
        per_layer_maps,
        normal_strength: state.project.settings.normal_strength,
        normal_mode: state.project.settings.normal_mode,
        normal_data: state
            .cached_mesh_normals
            .as_ref()
            .map(|(_, nd)| Arc::clone(nd)),
        do_glb,
        mesh: if do_glb {
            state.loaded_mesh.clone()
        } else {
            None
        },
    })
}

/// Run the export on a background thread. Called from ExportWorker.
fn run_export(input: ExportInput, step: Arc<AtomicU32>) -> Result<(u32, PathBuf), String> {
    let ExportInput {
        dir,
        es,
        resolution: res,
        with_alpha,
        color,
        height,
        normal_map,
        stroke_id,
        stroke_time_order,
        stroke_time_arc,
        per_layer_maps,
        normal_strength,
        normal_mode,
        normal_data,
        do_glb,
        mesh,
    } = input;

    let is_exr = es.format == ExportFormat::Exr;
    let mut count = 0u32;

    // ── Texture Maps ──
    if es.export_maps {
        if es.include_color {
            let result = if is_exr {
                export_color_exr(&color, res, &dir.join("color_map.exr"), with_alpha)
            } else {
                export_color_png(&color, res, &dir.join("color_map.png"), with_alpha)
            };
            result.map_err(|e| format!("Export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }
        if es.include_height {
            let normalized = normalize_height_map(&height);
            let result = if is_exr {
                export_height_exr(&normalized, res, &dir.join("height_map.exr"))
            } else {
                export_height_png(&normalized, res, &dir.join("height_map.png"))
            };
            result.map_err(|e| format!("Export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }
        if es.include_normal {
            export_normal_png(&normal_map, res, &dir.join("normal_map.png"), es.normal_y)
                .map_err(|e| format!("Export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }
        if es.include_stroke_id {
            export_stroke_id_png(&stroke_id, res, &dir.join("stroke_id_map.png"))
                .map_err(|e| format!("Export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }
        if es.include_time_map {
            let result = if is_exr {
                export_stroke_time_exr(
                    &stroke_time_order,
                    &stroke_time_arc,
                    res,
                    &dir.join("stroke_time_map.exr"),
                )
            } else {
                export_stroke_time_png(
                    &stroke_time_order,
                    &stroke_time_arc,
                    res,
                    &dir.join("stroke_time_map.png"),
                )
            };
            result.map_err(|e| format!("Export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }

        // ── Per-Layer Export ──
        if es.per_layer {
            let nd_ref = normal_data.as_deref();
            let mut manifest_entries = Vec::new();
            for entry in &per_layer_maps {
                match export_layer_maps(
                    &entry.maps,
                    entry.idx,
                    &LayerExportOptions {
                        format: es.format,
                        normal_strength,
                        normal_mode,
                        normal_data: nd_ref,
                        include_color: es.include_color,
                        include_height: es.include_height,
                        include_normal: es.include_normal,
                        include_time_map: es.include_time_map,
                        normal_y: es.normal_y,
                    },
                    &dir,
                ) {
                    Ok(n) => count += n,
                    Err(e) => {
                        return Err(format!("Export failed (layer {}): {e:?}", entry.name));
                    }
                }
                step.store(count, Ordering::Relaxed);

                manifest_entries.push(LayerManifestEntry {
                    index: entry.idx,
                    name: entry.name.clone(),
                    group: entry.group_name.clone(),
                    order: entry.order,
                    visible: entry.visible,
                    dry: entry.dry,
                });
            }

            export_manifest(&manifest_entries, es.format, &dir)
                .map_err(|e| format!("Export failed (manifest): {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        }
    }

    // ── GLB ──
    if do_glb {
        if let Some(ref mesh) = mesh {
            let normalized_height = normalize_height_map(&height);
            let result = glb_export::export_preview_glb(&glb_export::GlbExportParams {
                mesh,
                color_map: &color,
                height_map: &normalized_height,
                normal_map: &normal_map,
                resolution: res,
                displacement_scale: 0.0,
                path: &dir.join("preview.glb"),
                normal_y: es.normal_y,
                alpha_blend: with_alpha,
            });
            result.map_err(|e| format!("GLB export failed: {e:?}"))?;
            count += 1;
            step.store(count, Ordering::Relaxed);
        } else {
            return Err("No mesh loaded for GLB export".to_string());
        }
    }

    if count == 0 {
        return Err("No maps selected for export".to_string());
    }

    Ok((count, dir))
}

/// Start the background export worker. Call after folder selection + overwrite confirmation.
fn start_export_worker(state: &mut AppState, dir: PathBuf, do_glb: bool) {
    if state.export_worker.is_running() {
        state.status_message = "Export already in progress".to_string();
        return;
    }

    let Some(input) = gather_export_input(state, dir, do_glb) else {
        state.status_message = "Nothing to export — generate first".to_string();
        return;
    };

    let total = count_export_steps(&input.es, input.per_layer_maps.len(), do_glb);

    state.status_message = "Exporting…".to_string();
    state
        .export_worker
        .start(total, move |step| run_export(input, step));
}

/// Pick a folder, confirm overwrites, and start the export worker.
/// `include_glb` controls whether to write GLB and which planned files to check.
fn pick_folder_and_export(state: &mut AppState, include_glb: bool) {
    state.modal_dialog_active = true;
    let parent = rfd::FileDialog::new().pick_folder();
    state.modal_dialog_active = false;

    let Some(parent) = parent else {
        return;
    };
    let dir = parent.join(export_folder_name(state));
    let planned = planned_export_files(state, include_glb);
    let conflicts = count_conflicts(&dir, &planned);

    if conflicts > 0 {
        // Defer to egui confirmation window instead of blocking rfd::MessageDialog.
        let folder_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string();
        state.export_overwrite_confirm = Some(crate::gui::state::ExportOverwriteConfirm {
            dir,
            include_glb,
            conflict_count: conflicts,
            folder_name,
        });
        return;
    }

    if let Err(e) = std::fs::create_dir_all(&dir) {
        state.status_message = format!("Failed to create folder: {e}");
        return;
    }
    start_export_worker(state, dir, include_glb);
}

/// Called when the user confirms overwrite in the egui window.
pub fn confirm_export_overwrite(state: &mut AppState) {
    let Some(confirm) = state.export_overwrite_confirm.take() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(&confirm.dir) {
        state.status_message = format!("Failed to create folder: {e}");
        return;
    }
    start_export_worker(state, confirm.dir, confirm.include_glb);
}

/// Export generated maps to a user-selected folder (background worker).
pub fn export_maps(state: &mut AppState) {
    pick_folder_and_export(state, false);
}

/// Export both texture maps and GLB to a user-selected folder (background worker).
pub fn export_both(state: &mut AppState) {
    pick_folder_and_export(state, true);
}

/// Export a 3D preview GLB — pick folder (background worker).
pub fn export_glb(state: &mut AppState) {
    pick_folder_and_export(state, true);
}
