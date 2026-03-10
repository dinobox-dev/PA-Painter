use std::path::{Path, PathBuf};
use std::process;

use log::{error, info};

use practical_arcana_painter::asset_io::load_mesh;
use practical_arcana_painter::compositing::{
    composite_all_with_paths, generate_all_paths, render_layer, resolve_base_color,
};
use practical_arcana_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use practical_arcana_painter::output::{
    export_all, export_layer_maps, export_manifest, ExportFormat, LayerExportOptions,
    LayerManifestEntry,
};
use practical_arcana_painter::project::load_project;
use practical_arcana_painter::stretch_map::{compute_stretch_map, StretchMap};
use practical_arcana_painter::types::NormalMode;
use practical_arcana_painter::uv_mask::UvMask;

fn usage() -> ! {
    eprintln!("Usage: practical-arcana-painter <project.pap> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <dir>       Output directory (default: ./output)");
    eprintln!("  -r, --resolution <px>    Override output resolution");
    eprintln!("  -f, --format <fmt>       Export format: png (default) or exr");
    eprintln!("      --per-layer          Export each layer as separate textures");
    eprintln!("  -h, --help               Show this help");
    process::exit(1);
}

fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }

    let project_path = PathBuf::from(&args[0]);
    let mut output_dir = PathBuf::from("output");
    let mut resolution_override: Option<u32> = None;
    let mut format = ExportFormat::Png;
    let mut per_layer = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                output_dir = PathBuf::from(args.get(i).unwrap_or_else(|| {
                    eprintln!("Error: --output requires a directory path");
                    process::exit(1);
                }));
            }
            "-r" | "--resolution" => {
                i += 1;
                let val = args.get(i).unwrap_or_else(|| {
                    eprintln!("Error: --resolution requires a number");
                    process::exit(1);
                });
                let parsed: u32 = val.parse().unwrap_or_else(|_| {
                    eprintln!("Error: invalid resolution '{val}'");
                    process::exit(1);
                });
                if parsed == 0 || parsed > 16384 {
                    eprintln!("Error: resolution must be between 1 and 16384, got {parsed}");
                    process::exit(1);
                }
                resolution_override = Some(parsed);
            }
            "-f" | "--format" => {
                i += 1;
                let val = args.get(i).unwrap_or_else(|| {
                    eprintln!("Error: --format requires png or exr");
                    process::exit(1);
                });
                format = match val.as_str() {
                    "png" => ExportFormat::Png,
                    "exr" => ExportFormat::Exr,
                    other => {
                        eprintln!("Error: unknown format '{other}' (use png or exr)");
                        process::exit(1);
                    }
                };
            }
            "--per-layer" => {
                per_layer = true;
            }
            other => {
                eprintln!("Error: unknown option '{other}'");
                usage();
            }
        }
        i += 1;
    }

    // Load project
    let load_result = load_project(&project_path).unwrap_or_else(|e| {
        error!("Failed to load project: {e:?}");
        process::exit(1);
    });
    let mut project = load_result.project;

    let resolution = resolution_override.unwrap_or(project.settings.resolution_preset.resolution());
    info!("Resolution: {resolution}px");
    info!("Layers: {}", project.layers.len());

    // Load mesh: prefer embedded, fall back to file path
    let loaded_mesh = if let Some(mesh) = load_result.mesh {
        info!("Loaded embedded mesh: {} groups", mesh.groups.len());
        Some(mesh)
    } else {
        let mesh_file = resolve_asset_path(&project_path, &project.mesh_ref.path);
        match load_mesh(&mesh_file) {
            Ok(mesh) => {
                info!("Loaded mesh: {} groups", mesh.groups.len());
                Some(mesh)
            }
            Err(e) => {
                error!("Failed to load mesh: {e}");
                None
            }
        }
    };

    let normal_data: Option<MeshNormalData> =
        if project.settings.normal_mode == NormalMode::DepictedForm {
            if let Some(ref mesh) = loaded_mesh {
                info!("Computing mesh normals...");
                Some(compute_mesh_normal_data(mesh, resolution))
            } else {
                info!("Falling back to SurfacePaint normals");
                None
            }
        } else {
            None
        };

    // Compute stretch map for UV distortion compensation
    let stretch_data: Option<StretchMap> = loaded_mesh.as_ref().map(|mesh| {
        info!("Computing stretch map...");
        compute_stretch_map(mesh, resolution)
    });
    let stretch_ref = stretch_data.as_ref();

    // Build UV masks from mesh groups
    let masks: Vec<Option<UvMask>> = if let Some(ref mesh) = loaded_mesh {
        project.build_masks(mesh, resolution)
    } else {
        (0..project.layers.iter().filter(|l| l.visible).count())
            .map(|_| None)
            .collect()
    };
    let mask_refs: Vec<Option<&UvMask>> = masks.iter().map(|m| m.as_ref()).collect();

    // Convert slots to paint layers for pipeline
    let layers = project.paint_layers();

    // Resolve per-layer base color and normal from TextureSource.
    let materials: &[_] = loaded_mesh
        .as_ref()
        .map(|m| m.materials.as_slice())
        .unwrap_or(&[]);
    let layer_base_colors: Vec<_> = project
        .layers
        .iter()
        .filter(|l| l.visible)
        .map(|l| resolve_base_color(&l.base_color, materials))
        .collect();

    // Generate (with path cache)
    info!("Generating...");
    if project.cached_paths_if_valid().is_none() {
        let paths = generate_all_paths(
            &layers,
            &layer_base_colors,
            normal_data.as_ref(),
            &mask_refs,
            stretch_ref,
        );
        project.set_cached_paths(paths);
    }

    let global = composite_all_with_paths(
        &layers,
        resolution,
        &layer_base_colors,
        &project.settings,
        project.cached_paths.as_deref(),
        normal_data.as_ref(),
        &mask_refs,
        stretch_ref,
    );

    // Export
    export_all(
        &global,
        &project.settings,
        &output_dir,
        format,
        normal_data.as_ref(),
    )
    .unwrap_or_else(|e| {
        error!("Export failed: {e:?}");
        process::exit(1);
    });

    // Per-layer export
    if per_layer {
        info!("Exporting per-layer maps...");
        let mut manifest_entries = Vec::new();
        let visible_layers: Vec<_> = project.layers.iter().filter(|l| l.visible).collect();

        for (idx, (layer, paint_layer)) in visible_layers.iter().zip(layers.iter()).enumerate() {
            let base_color = &layer_base_colors[idx];
            let base = base_color.as_source();
            let mask = mask_refs.get(idx).and_then(|m| *m);
            let cached = project
                .cached_paths
                .as_ref()
                .and_then(|cp| cp.get(idx))
                .map(|v| v.as_slice());

            let layer_maps = render_layer(
                paint_layer,
                idx as u32,
                &base,
                cached,
                normal_data.as_ref(),
                mask,
                stretch_ref,
                resolution,
            );

            export_layer_maps(
                &layer_maps,
                idx,
                &LayerExportOptions {
                    format,
                    normal_strength: project.settings.normal_strength,
                    normal_mode: project.settings.normal_mode,
                    normal_data: normal_data.as_ref(),
                    include_color: true,
                    include_height: true,
                    include_normal: true,
                    include_time_map: false,
                },
                &output_dir,
            )
            .unwrap_or_else(|e| {
                error!("Export failed for layer {}: {e:?}", layer.name);
                process::exit(1);
            });

            manifest_entries.push(LayerManifestEntry {
                index: idx,
                name: layer.name.clone(),
                group: layer.group_name.clone(),
                order: layer.order,
                visible: layer.visible,
                dry: layer.dry,
            });
        }

        export_manifest(&manifest_entries, format, &output_dir).unwrap_or_else(|e| {
            error!("Manifest export failed: {e:?}");
            process::exit(1);
        });
    }

    info!("Done.");
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
