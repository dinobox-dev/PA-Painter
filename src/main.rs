use std::path::{Path, PathBuf};
use std::process;

use practical_arcana_painter::asset_io::{load_mesh, load_texture};
use practical_arcana_painter::compositing::{composite_all_with_paths, generate_all_paths};
use practical_arcana_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use practical_arcana_painter::output::{export_all, ExportFormat};
use practical_arcana_painter::project::load_project;
use practical_arcana_painter::types::{pixels_to_colors, BaseColorSource, Color, NormalMode};

fn usage() -> ! {
    eprintln!("Usage: practical-arcana-painter <project.pap> [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  -o, --output <dir>       Output directory (default: ./output)");
    eprintln!("  -r, --resolution <px>    Override output resolution");
    eprintln!("  -f, --format <fmt>       Export format: png (default) or exr");
    eprintln!("  -h, --help               Show this help");
    process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        usage();
    }

    let project_path = PathBuf::from(&args[0]);
    let mut output_dir = PathBuf::from("output");
    let mut resolution_override: Option<u32> = None;
    let mut format = ExportFormat::Png;

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
            other => {
                eprintln!("Error: unknown option '{other}'");
                usage();
            }
        }
        i += 1;
    }

    // Load project
    eprintln!("Loading project: {}", project_path.display());
    let mut project = load_project(&project_path).unwrap_or_else(|e| {
        eprintln!("Error loading project: {e:?}");
        process::exit(1);
    });

    let resolution = resolution_override.unwrap_or(project.settings.resolution_preset.resolution());
    eprintln!("Resolution: {resolution}px");
    eprintln!("Layers: {}", project.layers.len());

    // Load base color texture if referenced
    let (base_colors, tw, th) = if let Some(ref tex_path) = project.color_ref.path {
        let tex_file = resolve_asset_path(&project_path, tex_path);
        eprintln!("Loading texture: {}", tex_file.display());
        match load_texture(&tex_file) {
            Ok(tex) => {
                let colors = pixels_to_colors(&tex.pixels);
                (Some(colors), tex.width, tex.height)
            }
            Err(e) => {
                eprintln!("Warning: failed to load texture: {e:?}");
                (None, 0u32, 0u32)
            }
        }
    } else {
        (None, 0u32, 0u32)
    };

    let sc = {
        let c = project.color_ref.solid_color;
        Color::rgb(c[0], c[1], c[2])
    };

    let base_color = match base_colors {
        Some(ref colors) => BaseColorSource::textured(colors, tw, th, sc),
        None => BaseColorSource::solid(sc),
    };

    // Load mesh and compute normal data (DepictedForm mode only)
    let normal_data: Option<MeshNormalData> =
        if project.settings.normal_mode == NormalMode::DepictedForm {
            let mesh_file = resolve_asset_path(&project_path, &project.mesh_ref.path);
            eprintln!("Loading mesh: {}", mesh_file.display());
            match load_mesh(&mesh_file) {
                Ok(mesh) => {
                    eprintln!("Computing mesh normals...");
                    Some(compute_mesh_normal_data(&mesh, resolution))
                }
                Err(e) => {
                    eprintln!("Warning: failed to load mesh: {e}");
                    eprintln!("  Falling back to SurfacePaint normals.");
                    None
                }
            }
        } else {
            None
        };

    // Generate (with path cache)
    eprintln!("Generating...");
    if project.cached_paths_if_valid(resolution).is_none() {
        let paths = generate_all_paths(
            &project.layers,
            resolution,
            &base_color,
            normal_data.as_ref(),
        );
        project.set_cached_paths(paths, resolution);
    }

    let global = composite_all_with_paths(
        &project.layers,
        resolution,
        &base_color,
        &project.settings,
        project.cached_paths.as_deref(),
        normal_data.as_ref(),
    );

    // Export
    eprintln!("Exporting to: {}", output_dir.display());
    export_all(&global, &project.layers, &project.settings, &output_dir, format, normal_data.as_ref())
        .unwrap_or_else(|e| {
            eprintln!("Error exporting: {e:?}");
            process::exit(1);
        });

    eprintln!("Done.");
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
