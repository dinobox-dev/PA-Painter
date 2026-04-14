//! **Pipeline stage 5** — final output map generation and export.
//!
//! Converts composited global maps into color, normal, height, and AO textures,
//! then writes them as PNG or OpenEXR files.

mod exr_export;
mod normal_map;
mod png_export;

pub use exr_export::{export_color_exr, export_height_exr, export_stroke_time_exr};
pub use normal_map::{
    blend_normals_udn, generate_normal_map, generate_normal_map_depicted_form, normals_to_pixels,
};
pub use png_export::{
    export_color_png, export_height_png, export_normal_png, export_stroke_id_png,
    export_stroke_time_png,
};

use std::path::Path;

use log::info;
use serde::{Deserialize, Serialize};

use crate::mesh::object_normal::MeshNormalData;
use crate::pipeline::compositing::{GlobalMaps, LayerMaps};
use crate::types::{BackgroundMode, NormalMode, NormalYConvention, OutputSettings};

// ── Error Type ──

/// Error type for output operations.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Image error: {0}")]
    ImageError(#[from] image::ImageError),
    #[error("EXR error: {0}")]
    ExrError(String),
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

// ── Export Format ──

/// Output format selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExportFormat {
    #[default]
    Png,
    Exr,
}

/// Options for per-layer map export.
pub struct LayerExportOptions<'a> {
    pub format: ExportFormat,
    pub normal_strength: f32,
    pub normal_mode: NormalMode,
    pub normal_data: Option<&'a MeshNormalData>,
    pub include_color: bool,
    pub include_height: bool,
    pub include_normal: bool,
    pub include_time_map: bool,
    pub normal_y: NormalYConvention,
}

// ── Height Map Normalization ──

/// Normalize a height (density) map for export.
///
/// Density values are already in \[0, 1\] range (effective_density × remaining).
///
/// Returns a new `Vec<f32>` with values in \[0, 1\].
pub fn normalize_height_map(height: &[f32]) -> Vec<f32> {
    height.iter().map(|&h| h.clamp(0.0, 1.0)).collect()
}

// ── Per-Layer Export ──

/// Metadata for a single layer, written into `manifest.json`.
#[derive(Debug, Serialize)]
pub struct LayerManifestEntry {
    pub index: usize,
    pub name: String,
    pub group: String,
    pub order: i32,
    pub visible: bool,
    pub dry: f32,
}

/// Write `manifest.json` with layer metadata and file listing.
pub fn export_manifest(
    layers: &[LayerManifestEntry],
    format: ExportFormat,
    output_dir: &Path,
) -> Result<(), OutputError> {
    let ext = match format {
        ExportFormat::Png => "png",
        ExportFormat::Exr => "exr",
    };

    #[derive(Serialize)]
    struct Manifest<'a> {
        layers: &'a [LayerManifestEntry],
        format: &'a str,
    }

    let manifest = Manifest {
        layers,
        format: ext,
    };

    let json = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(output_dir.join("manifest.json"), json)?;
    Ok(())
}

/// Export a single layer's texture maps (color, height, normal).
///
/// Files are named `layer_{index}_{map}.{ext}` (e.g. `layer_0_color.png`).
/// Normal map is generated from the layer's gradient data using the same
/// Sobel-based approach as the composite normal map.
pub fn export_layer_maps(
    layer: &LayerMaps,
    index: usize,
    opts: &LayerExportOptions<'_>,
    output_dir: &Path,
) -> Result<u32, OutputError> {
    info!("Exporting layer {} maps to {}", index, output_dir.display());
    let res = layer.resolution;
    let prefix = format!("layer_{index}");
    let mut count = 0u32;

    if opts.include_color {
        // Per-layer color always has alpha (transparent where unpainted).
        match opts.format {
            ExportFormat::Png => export_color_png(
                &layer.color,
                res,
                &output_dir.join(format!("{prefix}_color.png")),
                true,
            )?,
            ExportFormat::Exr => export_color_exr(
                &layer.color,
                res,
                &output_dir.join(format!("{prefix}_color.exr")),
                true,
            )?,
        }
        count += 1;
    }

    if opts.include_height {
        let normalized = normalize_height_map(&layer.height);
        match opts.format {
            ExportFormat::Png => export_height_png(
                &normalized,
                res,
                &output_dir.join(format!("{prefix}_height.png")),
            )?,
            ExportFormat::Exr => export_height_exr(
                &normalized,
                res,
                &output_dir.join(format!("{prefix}_height.exr")),
            )?,
        }
        count += 1;
    }

    if opts.include_normal {
        let normals = match (opts.normal_mode, opts.normal_data) {
            (NormalMode::DepictedForm, Some(nd)) => generate_normal_map_depicted_form(
                &layer.gradient_x,
                &layer.gradient_y,
                nd,
                &layer.object_normal,
                &layer.paint_load,
                res,
                opts.normal_strength,
            ),
            _ => generate_normal_map(
                &layer.gradient_x,
                &layer.gradient_y,
                res,
                opts.normal_strength,
            ),
        };
        export_normal_png(
            &normals,
            res,
            &output_dir.join(format!("{prefix}_normal.png")),
            opts.normal_y,
        )?;
        count += 1;
    }

    if opts.include_time_map {
        match opts.format {
            ExportFormat::Png => export_stroke_time_png(
                &layer.stroke_time_order,
                &layer.stroke_time_arc,
                res,
                &output_dir.join(format!("{prefix}_stroke_time.png")),
            )?,
            ExportFormat::Exr => export_stroke_time_exr(
                &layer.stroke_time_order,
                &layer.stroke_time_arc,
                res,
                &output_dir.join(format!("{prefix}_stroke_time.exr")),
            )?,
        }
        count += 1;
    }

    Ok(count)
}

// ── Export All ──

/// Export all textures at once.
pub fn export_all(
    global: &GlobalMaps,
    settings: &OutputSettings,
    output_dir: &Path,
    format: ExportFormat,
    normal_data: Option<&MeshNormalData>,
    normal_y: NormalYConvention,
) -> Result<(), OutputError> {
    info!(
        "Exporting all maps to {} ({:?})",
        output_dir.display(),
        format
    );
    std::fs::create_dir_all(output_dir)?;

    let normalized_height = normalize_height_map(&global.height);
    let normals = match (settings.normal_mode, normal_data) {
        (NormalMode::DepictedForm, Some(nd)) => generate_normal_map_depicted_form(
            &global.gradient_x,
            &global.gradient_y,
            nd,
            &global.object_normal,
            &global.paint_load,
            global.resolution,
            settings.normal_strength,
        ),
        _ => generate_normal_map(
            &global.gradient_x,
            &global.gradient_y,
            global.resolution,
            settings.normal_strength,
        ),
    };

    let with_alpha = settings.background_mode == BackgroundMode::Transparent;

    match format {
        ExportFormat::Png => {
            export_color_png(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.png"),
                with_alpha,
            )?;
            export_height_png(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.png"),
            )?;
        }
        ExportFormat::Exr => {
            export_color_exr(
                &global.color,
                global.resolution,
                &output_dir.join("color_map.exr"),
                with_alpha,
            )?;
            export_height_exr(
                &normalized_height,
                global.resolution,
                &output_dir.join("height_map.exr"),
            )?;
        }
    }

    // Normal map and stroke ID are always PNG
    export_normal_png(
        &normals,
        global.resolution,
        &output_dir.join("normal_map.png"),
        normal_y,
    )?;
    export_stroke_id_png(
        &global.stroke_id,
        global.resolution,
        &output_dir.join("stroke_id_map.png"),
    )?;

    // Stroke time map
    match format {
        ExportFormat::Png => {
            export_stroke_time_png(
                &global.stroke_time_order,
                &global.stroke_time_arc,
                global.resolution,
                &output_dir.join("stroke_time_map.png"),
            )?;
        }
        ExportFormat::Exr => {
            export_stroke_time_exr(
                &global.stroke_time_order,
                &global.stroke_time_arc,
                global.resolution,
                &output_dir.join("stroke_time_map.exr"),
            )?;
        }
    }

    Ok(())
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::compositing::{composite_all, CompositeAllInput};
    use crate::test_util::make_layer_with_order;
    use crate::types::{Color, LayerBaseColor};

    // ── Height Normalization Tests ──

    #[test]
    fn normalize_all_zeros() {
        let height = vec![0.0f32; 64];
        let normalized = normalize_height_map(&height);
        assert!(normalized.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn normalize_clamps_above_one() {
        let height = vec![3.0f32; 1];
        let normalized = normalize_height_map(&height);
        assert!(
            (normalized[0] - 1.0).abs() < 1e-4,
            "above 1.0: expected 1.0, got {}",
            normalized[0]
        );
    }

    // ── Export All Tests ──

    #[test]
    fn export_all_png() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(&CompositeAllInput::new(
            &[layer.clone()],
            res,
            &[LayerBaseColor::solid(Color::WHITE)],
            &settings,
        ));

        let dir = std::env::temp_dir()
            .join("pap_test_output")
            .join("export_all_png");
        export_all(
            &maps,
            &settings,
            &dir,
            ExportFormat::Png,
            None,
            NormalYConvention::OpenGL,
        )
        .unwrap();

        assert!(
            dir.join("color_map.png").exists(),
            "color_map.png should exist"
        );
        assert!(
            dir.join("height_map.png").exists(),
            "height_map.png should exist"
        );
        assert!(
            dir.join("normal_map.png").exists(),
            "normal_map.png should exist"
        );
        assert!(
            dir.join("stroke_id_map.png").exists(),
            "stroke_id_map.png should exist"
        );
    }

    #[test]
    fn export_all_exr() {
        let res = 32u32;
        let mut layer = make_layer_with_order(0);
        layer.params.brush_width = 10.0;

        let settings = OutputSettings::default();

        let maps = composite_all(&CompositeAllInput::new(
            &[layer.clone()],
            res,
            &[LayerBaseColor::solid(Color::WHITE)],
            &settings,
        ));

        let dir = std::env::temp_dir()
            .join("pap_test_output")
            .join("export_all_exr");
        export_all(
            &maps,
            &settings,
            &dir,
            ExportFormat::Exr,
            None,
            NormalYConvention::OpenGL,
        )
        .unwrap();

        assert!(
            dir.join("color_map.exr").exists(),
            "color_map.exr should exist"
        );
        assert!(
            dir.join("height_map.exr").exists(),
            "height_map.exr should exist"
        );
        assert!(
            dir.join("normal_map.png").exists(),
            "normal_map.png should exist"
        );
        assert!(
            dir.join("stroke_id_map.png").exists(),
            "stroke_id_map.png should exist"
        );
    }
}
