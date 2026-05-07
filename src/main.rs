use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};
use log::{error, info};

use pa_painter::io::project::{Project, load_project};
use pa_painter::mesh::asset_io::load_mesh;
use pa_painter::mesh::object_normal::{MeshNormalData, compute_mesh_normal_data};
use pa_painter::mesh::stretch_map::{StretchMap, compute_stretch_map};
use pa_painter::mesh::uv_mask::DistanceField;
use pa_painter::pipeline::compositing::{
    CompositeAllInput, GlobalMaps, RenderLayerInput, composite_all, generate_all_paths,
    render_layer, resolve_base_color,
};
use pa_painter::pipeline::output::{
    ExportFormat, ExtractEncoding, ExtractObjectNormalParams, LayerExportOptions,
    LayerManifestEntry, UpAxis, export_all, export_layer_maps, export_manifest,
    extract_object_normal,
};
use pa_painter::types::NormalMode;

#[derive(Clone, clap::ValueEnum)]
enum CliFormat {
    Png,
    Exr,
}

#[derive(Clone, clap::ValueEnum)]
enum CliMap {
    /// Object-space normal map (DepictedForm + mesh required).
    ObjectNormal,
}

#[derive(Clone, clap::ValueEnum)]
enum CliUp {
    Y,
    Z,
}

#[derive(Clone, clap::ValueEnum)]
enum CliEncoding {
    Png8,
    Png16,
    Exr,
}

#[derive(Parser)]
#[command(
    name = "pa-painter",
    about = "Procedural paint stroke generator for 3D assets",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(flatten)]
    render: RenderArgs,

    #[command(subcommand)]
    command: Option<Subcmd>,
}

#[derive(clap::Args)]
struct RenderArgs {
    /// Project file (.papr)
    project: Option<PathBuf>,

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

#[derive(Subcommand)]
enum Subcmd {
    /// Extract a single map with custom output options (DCC-targeted).
    Extract(ExtractArgs),
}

#[derive(clap::Args)]
struct ExtractArgs {
    /// Project file (.papr)
    project: PathBuf,

    /// Which map to extract.
    #[arg(long, value_enum, default_value = "object-normal")]
    map: CliMap,

    /// Up-axis convention for the output.
    #[arg(long, value_enum, default_value = "y")]
    up: CliUp,

    /// Output encoding.
    #[arg(long, value_enum, default_value = "png16")]
    encoding: CliEncoding,

    /// Output file path.
    #[arg(short, long)]
    output: PathBuf,

    /// Override output resolution (1–16384)
    #[arg(short, long, value_parser = clap::value_parser!(u32).range(1..=16384))]
    resolution: Option<u32>,
}

fn main() {
    env_logger::init();

    let cli = Cli::parse();
    match cli.command {
        Some(Subcmd::Extract(args)) => run_extract(args),
        None => run_render(cli.render),
    }
}

// ── Render (default) ────────────────────────────────────────────────

fn run_render(args: RenderArgs) {
    let project_path = args.project.unwrap_or_else(|| {
        error!("Missing required argument: project");
        process::exit(2);
    });
    let output_dir = args.output;
    let format = match args.format {
        CliFormat::Png => ExportFormat::Png,
        CliFormat::Exr => ExportFormat::Exr,
    };
    let resolution_override = args.resolution;
    let per_layer = args.per_layer;

    // Load project
    let load_result = load_project(&project_path).unwrap_or_else(|e| {
        error!("Failed to load project: {e}");
        process::exit(1);
    });
    let project = load_result.project;

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

    let materials: &[_] = loaded_mesh
        .as_ref()
        .map(|m| m.materials.as_slice())
        .unwrap_or(&[]);

    let visible_layers: Vec<_> = project.layers.iter().filter(|l| l.visible).collect();
    let dist_fields: Vec<Option<DistanceField>> = if let Some(ref mesh) = loaded_mesh {
        Project::build_dist_fields(&visible_layers, mesh, resolution)
    } else {
        visible_layers.iter().map(|_| None).collect()
    };
    let df_refs: Vec<Option<&DistanceField>> = dist_fields.iter().map(|m| m.as_ref()).collect();
    let layers: Vec<_> = visible_layers.iter().map(|l| l.to_paint_layer()).collect();
    let layer_base_colors: Vec<_> = visible_layers
        .iter()
        .map(|l| resolve_base_color(&l.base_color, materials))
        .collect();
    let layer_dry: Vec<f32> = visible_layers.iter().map(|l| l.dry).collect();
    let layer_group_names: Vec<&str> = visible_layers
        .iter()
        .map(|l| l.group_name.as_str())
        .collect();

    info!("Generating...");
    let paths = generate_all_paths(&layers, &layer_base_colors, &df_refs, stretch_ref);

    // paths[i] corresponds to layers sorted by order (generate_all_paths contract).
    let mut path_idx_for = vec![0usize; layers.len()];
    {
        let mut order_idx: Vec<usize> = (0..layers.len()).collect();
        order_idx.sort_by_key(|&i| layers[i].order);
        for (sorted_pos, &orig_idx) in order_idx.iter().enumerate() {
            path_idx_for[orig_idx] = sorted_pos;
        }
    }

    let global = composite_all(&CompositeAllInput {
        layers: &layers,
        resolution,
        base_colors: &layer_base_colors,
        settings: &project.settings,
        cached_paths: Some(&paths),
        normal_data: normal_data.as_ref(),
        dist_fields: &df_refs,
        stretch_map: stretch_ref,
        layer_dry: &layer_dry,
        group_names: &layer_group_names,
    });

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
        error!("Export failed: {e}");
        process::exit(1);
    });

    // Per-layer export
    if per_layer {
        info!("Exporting per-layer maps...");
        let mut manifest_entries = Vec::new();

        for (idx, (layer, paint_layer)) in visible_layers.iter().zip(layers.iter()).enumerate() {
            let base_color = &layer_base_colors[idx];
            let base = base_color.as_source();
            let df = df_refs.get(idx).and_then(|m| *m);
            let cached = paths.get(path_idx_for[idx]).map(|v| v.as_slice());

            let mut layer_maps = render_layer(&RenderLayerInput {
                layer: paint_layer,
                layer_index: idx as u32,
                base_color: &base,
                cached_paths: cached,
                normal_data: normal_data.as_ref(),
                dist_field: df,
                stretch_map: stretch_ref,
                resolution,
            });
            if let Some(df) = df {
                layer_maps.clip_to_dist_field(df);
            }

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
                error!("Export failed for layer {}: {e}", layer.name);
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
            error!("Manifest export failed: {e}");
            process::exit(1);
        });
    }

    info!("Done.");
}

// ── Extract (subcommand) ────────────────────────────────────────────

fn run_extract(args: ExtractArgs) {
    let CliMap::ObjectNormal = args.map;
    let up_axis = match args.up {
        CliUp::Y => UpAxis::Y,
        CliUp::Z => UpAxis::Z,
    };
    let encoding = match args.encoding {
        CliEncoding::Png8 => ExtractEncoding::Png8,
        CliEncoding::Png16 => ExtractEncoding::Png16,
        CliEncoding::Exr => ExtractEncoding::Exr,
    };

    let load_result = load_project(&args.project).unwrap_or_else(|e| {
        error!("Failed to load project: {e}");
        process::exit(1);
    });
    let project = load_result.project;

    // Match GUI behavior: refuse SurfacePaint instead of silently changing it.
    // Object-space normal output is undefined without geometric tangent frame.
    if project.settings.normal_mode == NormalMode::SurfacePaint {
        error!(
            "Object-normal extract requires DepictedForm normal mode \
             (project's normal_mode is SurfacePaint)"
        );
        process::exit(1);
    }

    let resolution = args
        .resolution
        .unwrap_or(project.settings.resolution_preset.resolution());

    let loaded_mesh = if let Some(mesh) = load_result.mesh {
        Some(mesh)
    } else {
        let mesh_file = resolve_asset_path(&args.project, &project.mesh_ref.path);
        match load_mesh(&mesh_file) {
            Ok(mesh) => Some(mesh),
            Err(e) => {
                error!("Failed to load mesh: {e}");
                process::exit(1);
            }
        }
    };
    let Some(loaded_mesh) = loaded_mesh else {
        error!("Object-normal extract requires a mesh");
        process::exit(1);
    };

    info!("Computing mesh normals...");
    let normal_data = compute_mesh_normal_data(&loaded_mesh, resolution);

    let global = compose_global_for_extract(&project, &loaded_mesh, &normal_data, resolution);

    extract_object_normal(&ExtractObjectNormalParams {
        gradient_x: &global.gradient_x,
        gradient_y: &global.gradient_y,
        normal_data: &normal_data,
        global_object_normals: &global.object_normal,
        paint_load: &global.paint_load,
        resolution,
        normal_strength: project.settings.normal_strength,
        up_axis,
        encoding,
        output_path: &args.output,
    })
    .unwrap_or_else(|e| {
        error!("Extract failed: {e}");
        process::exit(1);
    });

    info!("Extracted {} → {}", "object-normal", args.output.display());
}

/// Run the pipeline up to and including compositing, in DepictedForm mode
/// (which is required for object-space normal extraction).
fn compose_global_for_extract(
    project: &Project,
    mesh: &pa_painter::mesh::asset_io::LoadedMesh,
    normal_data: &MeshNormalData,
    resolution: u32,
) -> GlobalMaps {
    let stretch_data = compute_stretch_map(mesh, resolution);
    let stretch_ref = Some(&stretch_data);

    let materials: &[_] = mesh.materials.as_slice();
    let visible_layers: Vec<_> = project.layers.iter().filter(|l| l.visible).collect();
    let dist_fields: Vec<Option<DistanceField>> =
        Project::build_dist_fields(&visible_layers, mesh, resolution);
    let df_refs: Vec<Option<&DistanceField>> = dist_fields.iter().map(|m| m.as_ref()).collect();
    let layers: Vec<_> = visible_layers.iter().map(|l| l.to_paint_layer()).collect();
    let layer_base_colors: Vec<_> = visible_layers
        .iter()
        .map(|l| resolve_base_color(&l.base_color, materials))
        .collect();
    let layer_dry: Vec<f32> = visible_layers.iter().map(|l| l.dry).collect();
    let layer_group_names: Vec<&str> = visible_layers
        .iter()
        .map(|l| l.group_name.as_str())
        .collect();

    info!("Generating...");
    let paths = generate_all_paths(&layers, &layer_base_colors, &df_refs, stretch_ref);
    composite_all(&CompositeAllInput {
        layers: &layers,
        resolution,
        base_colors: &layer_base_colors,
        settings: &project.settings,
        cached_paths: Some(&paths),
        normal_data: Some(normal_data),
        dist_fields: &df_refs,
        stretch_map: stretch_ref,
        layer_dry: &layer_dry,
        group_names: &layer_group_names,
    })
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
