use std::path::{Path, PathBuf};
use std::process;

use clap::Parser;
use log::{error, info};

use pa_painter::io::project::load_project;
use pa_painter::mesh::asset_io::load_mesh;
use pa_painter::mesh::object_normal::{compute_mesh_normal_data, MeshNormalData};
use pa_painter::mesh::stretch_map::{compute_stretch_map, StretchMap};
use pa_painter::mesh::uv_mask::UvMask;
use pa_painter::pipeline::compositing::{
    composite_all_with_paths, generate_all_paths, render_layer, resolve_base_color,
};
use pa_painter::pipeline::output::{
    export_all, export_layer_maps, export_manifest, ExportFormat, LayerExportOptions,
    LayerManifestEntry,
};
use pa_painter::types::NormalMode;

#[derive(Clone, clap::ValueEnum)]
enum CliFormat {
    Png,
    Exr,
}

#[derive(Parser)]
#[command(
    name = "pa-painter",
    about = "Procedural paint stroke generator for 3D assets"
)]
struct Cli {
    /// Project file (.papr)
    project: PathBuf,

    /// Output directory
    #[arg(short, long, default_value = "output")]
    output: PathBuf,

    /// Override output resolution (1–16384)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..=16384))]
    resolution: Option<u32>,

    /// Export format
    #[arg(short, long, default_value = "png")]
    format: CliFormat,

    /// Export each layer as separate textures
    #[arg(long)]
    per_layer: bool,
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();
    let project_path = cli.project;
    let output_dir = cli.output;
    let format = match cli.format {
        CliFormat::Png => ExportFormat::Png,
        CliFormat::Exr => ExportFormat::Exr,
    };
    let resolution_override = cli.resolution;
    let per_layer = cli.per_layer;

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

    let layer_dry: Vec<f32> = project
        .layers
        .iter()
        .filter(|l| l.visible)
        .map(|l| l.dry)
        .collect();
    let layer_group_names: Vec<&str> = project
        .layers
        .iter()
        .filter(|l| l.visible)
        .map(|l| l.group_name.as_str())
        .collect();

    let global = composite_all_with_paths(
        &layers,
        resolution,
        &layer_base_colors,
        &project.settings,
        project.cached_paths.as_deref(),
        normal_data.as_ref(),
        &mask_refs,
        stretch_ref,
        &layer_dry,
        &layer_group_names,
    );

    // Export
    let normal_y = project.export_settings.normal_y;

    export_all(
        &global,
        &project.settings,
        &output_dir,
        format,
        normal_data.as_ref(),
        normal_y,
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
                    normal_y,
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
