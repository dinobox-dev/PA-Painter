//! # Practical Arcana Painter
//!
//! Procedural paint stroke generator for 3D assets. This crate provides a
//! 5-stage CPU pipeline that turns any UV-mapped mesh into a hand-painted look
//! with full control over stroke direction, density, pressure, and layering.
//!
//! ## Pipeline stages
//!
//! 1. **Direction field** ([`direction_field`]) — compute stroke flow from user-placed guides
//! 2. **Path placement** ([`path_placement`]) — lay out stroke paths along the flow via Poisson-disk sampling
//! 3. **Stroke height** ([`stroke_height`]) — map pressure curves and brush profiles into height values
//! 4. **Compositing** ([`compositing`]) — blend multiple paint layers into unified output maps
//! 5. **Output** ([`output`]) — generate final color, normal, height, and AO maps (PNG / EXR)
//!
//! ## Quick start
//!
//! ```no_run
//! use std::path::Path;
//! use practical_arcana_painter::project::load_project;
//!
//! let result = load_project(Path::new("scene.pap")).unwrap();
//! let project = result.project;
//! // … see the CLI binary (src/main.rs) for a full usage example.
//! ```
//!
//! ## Module overview
//!
//! | Module | Role |
//! |--------|------|
//! | [`types`] | Core data structures: `Color`, `PaintValues`, `StrokeParams`, `Layer` |
//! | [`project`] | `.pap` project file I/O and data model |
//! | [`asset_io`] | Mesh (OBJ / glTF) and texture loading |
//! | [`pressure`] | Pressure curve evaluation (presets + custom Bézier) |
//! | [`glb_export`] | GLB export with baked textures for 3D preview |
//! | [`error`] | Unified `PainterError` type aggregating all sub-errors |

#[cfg(test)]
pub(crate) fn test_module_output_dir(module: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/results")
        .join(module);
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[cfg(test)]
pub(crate) fn test_fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub mod error;
#[cfg(test)]
pub mod test_util;

// ── Pipeline stages ──
pub mod compositing;
pub mod direction_field;
pub mod output;
pub mod path_placement;
pub mod stroke_height;

// ── Data & types ──
pub mod brush_profile;
pub mod pressure;
pub mod project;
pub mod types;

// ── I/O & export ──
pub mod asset_io;
pub mod glb_export;
pub mod stroke_color;

// ── Utilities ──
pub mod math;
pub mod object_normal;
pub mod rng;
pub mod stretch_map;
pub mod uv_mask;
