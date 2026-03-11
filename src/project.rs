//! `.papr` project file I/O and the top-level [`Project`] data model.
//!
//! A `.papr` file is a ZIP archive with the following structure:
//!
//! - `manifest.json` — version, app name, timestamps
//! - `project.json` — layers, presets, settings, mesh reference (unified)
//! - `assets/mesh.*` — embedded mesh binary (GLB, OBJ, etc.)
//! - `assets/layer_{n}_color.png` — per-layer File-mode color texture
//! - `assets/layer_{n}_normal.png` — per-layer File-mode normal texture
//! - `output/color.png` — cached generated color map (sRGB RGBA)
//! - `output/normal.png` — cached generated normal map (linear RGB)
//! - `output/height.png` — cached generated height map (16-bit grayscale)
//! - `thumbnails/preview.png` — 256×256 preview thumbnail

use log::{info, warn};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::asset_io::{
    decode_linear_png_bytes, decode_srgb_png_bytes, encode_pixels_as_linear_png,
    encode_pixels_as_srgb_png, linear_to_srgb, load_mesh_from_bytes, LoadedMesh,
};
use crate::types::{
    Color, EmbeddedTexture, ExportSettings, Layer, OutputSettings, PaintLayer, PresetLibrary,
    StrokePath, TextureSource,
};
use crate::uv_mask::UvMask;

/// Return the current UTC time as an ISO 8601 string (e.g. `"2026-02-27T14:30:00Z"`).
pub fn utc_now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let day_secs = (secs % 86400) as u32;
    let mut days = (secs / 86400) as i64;

    let mut year = 1970i32;
    loop {
        let len = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if days < len {
            break;
        }
        days -= len;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u32;
    for &m in &mdays {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    let day = days + 1;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

// ── Data Structures ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub app_name: String,
    pub created_at: String,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshRef {
    /// Original file path — runtime only, not stored in project.json.
    #[serde(skip)]
    pub path: String,
    pub format: String,
}

/// Serialization wrapper: the subset of project data stored in `project.json`.
#[derive(Serialize, Deserialize)]
struct ProjectData {
    mesh_ref: MeshRef,
    layers: Vec<Layer>,
    presets: PresetLibrary,
    settings: OutputSettings,
    #[serde(default)]
    export_settings: ExportSettings,
}

/// Snapshot of the state that was used to generate the cached paths.
/// Compared against the current state to detect staleness.
#[derive(Debug)]
struct PathCacheKey {
    layers: Vec<Layer>,
}

/// Full project state (in-memory representation).
#[derive(Debug)]
pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub layers: Vec<Layer>,
    pub presets: PresetLibrary,
    pub settings: OutputSettings,
    pub export_settings: ExportSettings,
    /// Raw mesh file bytes for embedding in .papr ZIP.
    /// Populated on new project / load, written to `assets/mesh.*` on save.
    pub mesh_bytes: Option<Vec<u8>>,
    /// Runtime path cache — one entry per layer in order-sorted sequence.
    /// Not serialized to disk.
    pub cached_paths: Option<Vec<Vec<StrokePath>>>,
    /// Key used to validate `cached_paths` (private).
    path_cache_key: Option<PathCacheKey>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            manifest: Manifest {
                version: "1".to_string(),
                app_name: "PA Painter".to_string(),
                created_at: utc_now_iso8601(),
                modified_at: utc_now_iso8601(),
            },
            mesh_ref: MeshRef {
                path: String::new(),
                format: String::new(),
            },
            layers: Vec::new(),
            presets: PresetLibrary::default(),
            settings: OutputSettings::default(),
            export_settings: ExportSettings::default(),
            mesh_bytes: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }
}

impl Project {
    /// Convert visible layers to PaintLayers for downstream pipeline compatibility.
    pub fn paint_layers(&self) -> Vec<PaintLayer> {
        self.layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.to_paint_layer())
            .collect()
    }

    /// Build UV masks for visible layers from the loaded mesh.
    /// Returns `None` for `__all__` groups (paint entire UV space).
    pub fn build_masks(&self, mesh: &LoadedMesh, resolution: u32) -> Vec<Option<UvMask>> {
        self.layers
            .iter()
            .filter(|l| l.visible)
            .map(|layer| {
                if layer.group_name == "__all__" {
                    None
                } else {
                    mesh.groups
                        .iter()
                        .find(|g| g.name == layer.group_name)
                        .map(|group| {
                            let mut mask = UvMask::from_mesh_group(mesh, group, resolution);
                            mask.dilate(2);
                            mask
                        })
                }
            })
            .collect()
    }

    /// Return cached paths if they are still valid for the current layers
    /// and base color. Returns `None` on cache miss.
    pub fn cached_paths_if_valid(&self) -> Option<&[Vec<StrokePath>]> {
        let key = self.path_cache_key.as_ref()?;
        let paths = self.cached_paths.as_ref()?;
        if key.layers == self.layers {
            Some(paths.as_slice())
        } else {
            None
        }
    }

    /// Store newly generated paths in the cache, along with a snapshot of the
    /// current project state that produced them.
    pub fn set_cached_paths(&mut self, paths: Vec<Vec<StrokePath>>) {
        self.path_cache_key = Some(PathCacheKey {
            layers: self.layers.clone(),
        });
        self.cached_paths = Some(paths);
    }

    /// Explicitly invalidate the path cache.
    pub fn invalidate_path_cache(&mut self) {
        self.cached_paths = None;
        self.path_cache_key = None;
    }
}

// ── Save/Load Data Structures ──

/// Cached generation output for saving into the .papr file.
pub struct OutputCache<'a> {
    pub color: &'a [Color],
    pub height: &'a [f32],
    pub normal_map: &'a [[f32; 3]],
    pub stroke_time_order: &'a [f32],
    pub stroke_time_arc: &'a [f32],
    pub resolution: u32,
    /// Hash of project state at generation time — used to detect staleness after load.
    pub snapshot_hash: Option<u64>,
}

/// Generation output loaded from a .papr file.
pub struct LoadedOutput {
    pub color: Vec<Color>,
    pub height: Vec<f32>,
    pub normal_map: Vec<[f32; 3]>,
    pub stroke_time_order: Vec<f32>,
    pub stroke_time_arc: Vec<f32>,
    pub resolution: u32,
    /// Hash of project state at generation time (if saved).
    pub snapshot_hash: Option<u64>,
}

/// Result of loading a .papr file.
pub struct LoadResult {
    pub project: Project,
    /// Mesh parsed from embedded bytes.
    pub mesh: Option<LoadedMesh>,
    /// Cached generation output (if present in the file).
    pub output: Option<LoadedOutput>,
    /// Raw JSON string from `editor.json` (editor UI state).
    /// Opaque to the library — interpreted only by the GUI.
    pub editor_state_json: Option<String>,
}

impl std::fmt::Debug for LoadResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadResult")
            .field("project", &self.project)
            .field("mesh", &self.mesh.as_ref().map(|_| "..."))
            .field("output", &self.output.as_ref().map(|_| "..."))
            .finish()
    }
}

// ── Error Type ──

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
}

// ── Thumbnail Generation ──

/// Generate a 256×256 thumbnail PNG from output color data.
fn generate_thumbnail(output: &OutputCache<'_>) -> Option<Vec<u8>> {
    if output.color.is_empty() {
        return None;
    }
    let resolution = output.resolution;
    if resolution == 0 {
        return None;
    }

    let thumb_size: u32 = 256;
    let mut pixels = Vec::with_capacity((thumb_size * thumb_size * 3) as usize);

    for ty in 0..thumb_size {
        for tx in 0..thumb_size {
            let sx = (tx as f32 / thumb_size as f32 * resolution as f32) as u32;
            let sy = (ty as f32 / thumb_size as f32 * resolution as f32) as u32;
            let sx = sx.min(resolution - 1);
            let sy = sy.min(resolution - 1);
            let c = &output.color[(sy * resolution + sx) as usize];
            pixels.push((linear_to_srgb(c.r.clamp(0.0, 1.0)) * 255.0).round() as u8);
            pixels.push((linear_to_srgb(c.g.clamp(0.0, 1.0)) * 255.0).round() as u8);
            pixels.push((linear_to_srgb(c.b.clamp(0.0, 1.0)) * 255.0).round() as u8);
        }
    }

    let mut png_bytes = Vec::new();
    {
        let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
        use image::ImageEncoder;
        encoder
            .write_image(
                &pixels,
                thumb_size,
                thumb_size,
                image::ColorType::Rgb8.into(),
            )
            .ok()?;
    }
    Some(png_bytes)
}

// ── Save ──

/// Save a project to a `.papr` file.
///
/// If `output` is provided, the generated maps are stored as PNG in `output/`
/// and a 256×256 thumbnail is generated.
///
/// `editor_state_json` is an opaque JSON blob written as `editor.json` inside
/// the ZIP. The library does not interpret its contents — it is used by the GUI
/// to persist editor UI state (camera, viewport, playback settings, etc.).
pub fn save_project(
    project: &Project,
    path: &Path,
    output: Option<OutputCache<'_>>,
    editor_state_json: Option<&[u8]>,
) -> Result<(), ProjectError> {
    info!("Saving project to {}", path.display());
    // Write to a temp file first, then rename — prevents corruption if save fails midway.
    let tmp_path = path.with_extension("papr.tmp");
    let file = std::fs::File::create(&tmp_path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let raw_options =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

    // manifest.json
    let manifest_json = serde_json::to_string_pretty(&project.manifest)?;
    zip.start_file("manifest.json", options)?;
    zip.write_all(manifest_json.as_bytes())?;

    // project.json (unified: mesh_ref + layers + presets + settings)
    let data = ProjectData {
        mesh_ref: project.mesh_ref.clone(),
        layers: project.layers.clone(),
        presets: project.presets.clone(),
        settings: project.settings.clone(),
        export_settings: project.export_settings.clone(),
    };
    let project_json = serde_json::to_string_pretty(&data)?;
    zip.start_file("project.json", options)?;
    zip.write_all(project_json.as_bytes())?;

    // assets/mesh.* (raw binary, stored uncompressed)
    if let Some(ref bytes) = project.mesh_bytes {
        let ext = if project.mesh_ref.format.is_empty() {
            "glb"
        } else {
            &project.mesh_ref.format
        };
        zip.start_file(format!("assets/mesh.{ext}"), raw_options)?;
        zip.write_all(bytes)?;
    }

    // assets/layer_{n}_color.png / layer_{n}_normal.png — File-mode textures
    for (i, layer) in project.layers.iter().enumerate() {
        if let TextureSource::File(Some(ref tex)) = layer.base_color {
            if let Some(png) = encode_pixels_as_srgb_png(&tex.pixels, tex.width, tex.height) {
                zip.start_file(format!("assets/layer_{i}_color.png"), options)?;
                zip.write_all(&png)?;
            }
        }
        if let TextureSource::File(Some(ref tex)) = layer.base_normal {
            if let Some(png) = encode_pixels_as_linear_png(&tex.pixels, tex.width, tex.height) {
                zip.start_file(format!("assets/layer_{i}_normal.png"), options)?;
                zip.write_all(&png)?;
            }
        }
    }

    // output/ — cached generation results as PNG
    if let Some(ref out) = output {
        // output/color.png — sRGB RGBA
        let color_rgba: Vec<[f32; 4]> = out.color.iter().map(|c| [c.r, c.g, c.b, c.a]).collect();
        if let Some(png) = encode_pixels_as_srgb_png(&color_rgba, out.resolution, out.resolution) {
            zip.start_file("output/color.png", options)?;
            zip.write_all(&png)?;
        }

        // output/normal.png — linear RGB (stored as RGBA, alpha = 1)
        let normal_rgba: Vec<[f32; 4]> = out
            .normal_map
            .iter()
            .map(|n| [n[0], n[1], n[2], 1.0])
            .collect();
        if let Some(png) = encode_pixels_as_linear_png(&normal_rgba, out.resolution, out.resolution)
        {
            zip.start_file("output/normal.png", options)?;
            zip.write_all(&png)?;
        }

        // output/height.png — 16-bit grayscale
        if let Some(png) = encode_height_png16(out.height, out.resolution) {
            zip.start_file("output/height.png", options)?;
            zip.write_all(&png)?;
        }

        // output/stroke_time_order.png — 16-bit grayscale
        if !out.stroke_time_order.is_empty() {
            if let Some(png) = encode_height_png16(out.stroke_time_order, out.resolution) {
                zip.start_file("output/stroke_time_order.png", options)?;
                zip.write_all(&png)?;
            }
        }

        // output/stroke_time_arc.png — 16-bit grayscale
        if !out.stroke_time_arc.is_empty() {
            if let Some(png) = encode_height_png16(out.stroke_time_arc, out.resolution) {
                zip.start_file("output/stroke_time_arc.png", options)?;
                zip.write_all(&png)?;
            }
        }

        // output/snapshot.json — generation-time state hash for staleness detection
        if let Some(hash) = out.snapshot_hash {
            let json = format!("{{\"hash\":{hash}}}");
            zip.start_file("output/snapshot.json", options)?;
            zip.write_all(json.as_bytes())?;
        }

        // thumbnails/preview.png
        if let Some(png_bytes) = generate_thumbnail(out) {
            zip.start_file("thumbnails/preview.png", options)?;
            zip.write_all(&png_bytes)?;
        }
    }

    // editor.json — opaque editor UI state (camera, viewport, playback, etc.)
    if let Some(editor_json) = editor_state_json {
        zip.start_file("editor.json", options)?;
        zip.write_all(editor_json)?;
    }

    zip.finish()?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ── Load ──

/// Load a project from a `.papr` file.
///
/// Returns a [`LoadResult`] containing the project, the parsed mesh (if any),
/// and cached output maps (if present).
pub fn load_project(path: &Path) -> Result<LoadResult, ProjectError> {
    info!("Loading project: {}", path.display());
    let file = std::fs::File::open(path)?;
    let mut archive = ZipArchive::new(file)?;

    // manifest.json (required)
    let manifest: Manifest =
        read_json_entry(&mut archive, "manifest.json").map_err(|e| match e {
            ProjectError::Zip(_) => {
                ProjectError::InvalidFormat("missing manifest.json".to_string())
            }
            other => other,
        })?;

    // project.json (required — unified format)
    let data: ProjectData = read_json_entry(&mut archive, "project.json")?;

    // assets/mesh.* — find embedded mesh
    let (mesh_bytes, mesh_format) = read_mesh_asset(&mut archive, &data.mesh_ref.format);

    // Parse mesh from embedded bytes
    let loaded_mesh = if let Some(ref bytes) = mesh_bytes {
        let fmt = if mesh_format.is_empty() {
            "glb"
        } else {
            &mesh_format
        };
        match load_mesh_from_bytes(bytes, fmt) {
            Ok(mesh) => Some(mesh),
            Err(e) => {
                warn!("Failed to load embedded mesh: {e}");
                None
            }
        }
    } else {
        None
    };

    // Reconstruct layers with File-mode texture pixels from ZIP assets.
    // If decoding fails, demote to File(None) to show checkerboard warning.
    let mut layers = data.layers;
    for (i, layer) in layers.iter_mut().enumerate() {
        if let TextureSource::File(Some(_)) = layer.base_color {
            let entry_name = format!("assets/layer_{i}_color.png");
            let decoded = read_bytes_optional(&mut archive, &entry_name)
                .and_then(|bytes| decode_srgb_png_bytes(&bytes).ok());
            if let Some((pixels, w, h)) = decoded {
                if let TextureSource::File(Some(ref mut tex)) = layer.base_color {
                    tex.content_hash = EmbeddedTexture::compute_content_hash(&pixels);
                    tex.pixels = Arc::new(pixels);
                    tex.width = w;
                    tex.height = h;
                }
            } else {
                warn!("Failed to decode {entry_name}, demoting to File(None)");
                layer.base_color = TextureSource::File(None);
            }
        }
        if let TextureSource::File(Some(_)) = layer.base_normal {
            let entry_name = format!("assets/layer_{i}_normal.png");
            let decoded = read_bytes_optional(&mut archive, &entry_name)
                .and_then(|bytes| decode_linear_png_bytes(&bytes).ok());
            if let Some((pixels, w, h)) = decoded {
                if let TextureSource::File(Some(ref mut tex)) = layer.base_normal {
                    tex.content_hash = EmbeddedTexture::compute_content_hash(&pixels);
                    tex.pixels = Arc::new(pixels);
                    tex.width = w;
                    tex.height = h;
                }
            } else {
                warn!("Failed to decode {entry_name}, demoting to File(None)");
                layer.base_normal = TextureSource::File(None);
            }
        }
    }

    // Load cached output maps if present
    let output = load_output_maps(&mut archive);

    // editor.json — opaque editor UI state
    let editor_state_json = read_bytes_optional(&mut archive, "editor.json")
        .and_then(|bytes| String::from_utf8(bytes).ok());

    let project = Project {
        manifest,
        mesh_ref: MeshRef {
            path: String::new(), // Runtime only — set by caller
            format: if mesh_format.is_empty() {
                data.mesh_ref.format
            } else {
                mesh_format
            },
        },
        layers,
        presets: data.presets,
        settings: data.settings,
        export_settings: data.export_settings,
        mesh_bytes,
        cached_paths: None,
        path_cache_key: None,
    };

    Ok(LoadResult {
        project,
        mesh: loaded_mesh,
        output,
        editor_state_json,
    })
}

// ── Helpers ──

fn read_json_entry<T: serde::de::DeserializeOwned>(
    archive: &mut ZipArchive<std::fs::File>,
    name: &str,
) -> Result<T, ProjectError> {
    let mut entry = archive.by_name(name)?;
    let mut buf = String::new();
    entry.read_to_string(&mut buf)?;
    let value = serde_json::from_str(&buf)?;
    Ok(value)
}

fn read_bytes_optional(archive: &mut ZipArchive<std::fs::File>, name: &str) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(name).ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Search for the embedded mesh asset in the ZIP.
/// Tries known extensions if `format` hint is empty.
fn read_mesh_asset(
    archive: &mut ZipArchive<std::fs::File>,
    format_hint: &str,
) -> (Option<Vec<u8>>, String) {
    let candidates: Vec<&str> = if format_hint.is_empty() {
        vec!["glb", "gltf", "obj"]
    } else {
        vec![format_hint]
    };
    for ext in candidates {
        let name = format!("assets/mesh.{ext}");
        if let Some(bytes) = read_bytes_optional(archive, &name) {
            return (Some(bytes), ext.to_string());
        }
    }
    (None, String::new())
}

/// Load cached output maps (color, height, normal) from the `output/` directory.
fn load_output_maps(archive: &mut ZipArchive<std::fs::File>) -> Option<LoadedOutput> {
    let color_bytes = read_bytes_optional(archive, "output/color.png")?;
    let (color_pixels, cw, _ch) = decode_srgb_png_bytes(&color_bytes).ok()?;
    let resolution = cw;

    let color: Vec<Color> = color_pixels
        .into_iter()
        .map(|p| Color::new(p[0], p[1], p[2], p[3]))
        .collect();

    let height = if let Some(bytes) = read_bytes_optional(archive, "output/height.png") {
        decode_height_png16(&bytes)
            .map(|(h, _)| h)
            .unwrap_or_else(|| vec![0.0; color.len()])
    } else {
        vec![0.0; color.len()]
    };

    let normal_map = if let Some(bytes) = read_bytes_optional(archive, "output/normal.png") {
        decode_linear_png_bytes(&bytes)
            .map(|(pixels, _, _)| pixels.into_iter().map(|p| [p[0], p[1], p[2]]).collect())
            .unwrap_or_else(|_| vec![[0.5, 0.5, 1.0]; color.len()])
    } else {
        vec![[0.5, 0.5, 1.0]; color.len()]
    };

    let stroke_time_order =
        if let Some(bytes) = read_bytes_optional(archive, "output/stroke_time_order.png") {
            decode_height_png16(&bytes)
                .map(|(h, _)| h)
                .unwrap_or_else(|| vec![0.0; color.len()])
        } else {
            vec![0.0; color.len()]
        };

    let stroke_time_arc =
        if let Some(bytes) = read_bytes_optional(archive, "output/stroke_time_arc.png") {
            decode_height_png16(&bytes)
                .map(|(h, _)| h)
                .unwrap_or_else(|| vec![0.0; color.len()])
        } else {
            vec![0.0; color.len()]
        };

    // output/snapshot.json — generation-time state hash
    let snapshot_hash = read_bytes_optional(archive, "output/snapshot.json").and_then(|bytes| {
        #[derive(Deserialize)]
        struct Snapshot {
            hash: u64,
        }
        serde_json::from_slice::<Snapshot>(&bytes)
            .ok()
            .map(|s| s.hash)
    });

    Some(LoadedOutput {
        color,
        height,
        normal_map,
        stroke_time_order,
        stroke_time_arc,
        resolution,
        snapshot_hash,
    })
}

/// Encode a height map as 16-bit grayscale PNG.
///
/// Uses the high-level `DynamicImage` API to ensure correct byte order handling
/// across platforms (PNG spec mandates big-endian, but the `image` crate's raw
/// encoder expects native-endian byte buffers).
fn encode_height_png16(height: &[f32], resolution: u32) -> Option<Vec<u8>> {
    let pixels: Vec<u16> = height
        .iter()
        .map(|&h| (h.clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();
    let img =
        image::ImageBuffer::<image::Luma<u16>, Vec<u16>>::from_vec(resolution, resolution, pixels)?;
    let dyn_img = image::DynamicImage::ImageLuma16(img);
    let mut png = Vec::new();
    dyn_img
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// Decode a grayscale PNG (8 or 16-bit) back into float height values [0, 1].
fn decode_height_png16(bytes: &[u8]) -> Option<(Vec<f32>, u32)> {
    let img = image::load_from_memory(bytes).ok()?;
    let width = img.width();
    let luma = img.to_luma16();
    let height: Vec<f32> = luma.pixels().map(|p| p.0[0] as f32 / 65535.0).collect();
    Some((height, width))
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Guide, GuideType, Layer, OutputSettings, PaintValues, PressureCurve, PressurePreset,
        ResolutionPreset, TextureSource,
    };
    use glam::Vec2;

    fn make_manifest() -> Manifest {
        Manifest {
            version: "1".to_string(),
            app_name: "PA Painter".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            modified_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn make_mesh_ref() -> MeshRef {
        MeshRef {
            path: "models/character.obj".to_string(),
            format: "obj".to_string(),
        }
    }

    fn make_test_layer(group_name: &str, order: i32, guide_count: usize) -> Layer {
        let guides: Vec<Guide> = (0..guide_count)
            .map(|i| Guide {
                guide_type: GuideType::Directional,
                position: Vec2::new(0.1 * i as f32, 0.2 * i as f32),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.2 + 0.01 * i as f32,
                strength: 1.0,
            })
            .collect();

        Layer {
            name: group_name.to_string(),
            visible: true,
            order,
            group_name: group_name.to_string(),
            paint: PaintValues {
                brush_width: 20.0 + order as f32,
                load: 0.7,
                body_wiggle: 0.1,
                pressure_curve: PressureCurve::Preset(PressurePreset::FadeOut),
                stroke_spacing: 1.0,
                max_stroke_length: 200.0,
                angle_variation: 5.0,
                max_turn_angle: 15.0,
                color_break_threshold: None,
                normal_break_threshold: None,
                overlap_ratio: None,
                overlap_dist_factor: None,
                color_variation: 0.05,
                viscosity: 0.0,
            },
            guides,
            base_color: TextureSource::Solid([0.5, 0.5, 0.5]),
            base_normal: TextureSource::None,
            dry: 1.0,
            seed: order as u32,
        }
    }

    fn make_empty_project() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            layers: vec![],
            presets: PresetLibrary::built_in(),
            settings: OutputSettings::default(),
            export_settings: ExportSettings::default(),
            mesh_bytes: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }

    fn make_project_with_layers() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            layers: vec![
                make_test_layer("skin", 0, 3),
                make_test_layer("armor", 1, 5),
                make_test_layer("cloth", 2, 2),
            ],
            presets: PresetLibrary::built_in(),
            settings: OutputSettings {
                resolution_preset: ResolutionPreset::High,
                normal_strength: 1.5,
                ..OutputSettings::default()
            },
            export_settings: ExportSettings::default(),
            mesh_bytes: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }

    fn temp_pap_path(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("pap_test_project");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    // ── Round-Trip Tests ──

    #[test]
    fn round_trip_empty_project() {
        let project = make_empty_project();
        let path = temp_pap_path("empty.papr");

        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert_eq!(result.project.manifest.version, "1");
        assert_eq!(result.project.manifest.app_name, "PA Painter");
        assert_eq!(result.project.layers.len(), 0);
        assert!(result.output.is_none());
    }

    #[test]
    fn round_trip_with_layers() {
        let project = make_project_with_layers();
        let path = temp_pap_path("with_layers.papr");

        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert_eq!(result.project.layers.len(), 3);

        for (a, b) in project.layers.iter().zip(result.project.layers.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.visible, b.visible);
            assert_eq!(a.group_name, b.group_name);
            assert_eq!(a.order, b.order);
            assert_eq!(a.paint, b.paint);
            assert_eq!(a.guides.len(), b.guides.len());
        }
    }

    #[test]
    fn round_trip_output_maps() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;

        let height: Vec<f32> = (0..pixel_count)
            .map(|i| i as f32 / pixel_count as f32)
            .collect();
        let color: Vec<Color> = (0..pixel_count)
            .map(|i| {
                let v = i as f32 / pixel_count as f32;
                Color::new(v, v * 0.5, v * 0.3, 1.0)
            })
            .collect();
        let normal_map: Vec<[f32; 3]> = (0..pixel_count)
            .map(|i| {
                let v = i as f32 / pixel_count as f32;
                [v * 0.5 + 0.25, v * 0.5 + 0.25, 1.0]
            })
            .collect();

        let project = make_project_with_layers();
        let time_order: Vec<f32> = (0..pixel_count)
            .map(|i| i as f32 / pixel_count as f32)
            .collect();
        let time_arc: Vec<f32> = (0..pixel_count)
            .map(|i| (i as f32 / pixel_count as f32) * 0.5)
            .collect();
        let output = OutputCache {
            color: &color,
            height: &height,
            normal_map: &normal_map,
            stroke_time_order: &time_order,
            stroke_time_arc: &time_arc,
            resolution: res,
            snapshot_hash: None,
        };

        let path = temp_pap_path("with_output.papr");
        save_project(&project, &path, Some(output), None).unwrap();
        let result = load_project(&path).unwrap();

        let loaded = result.output.expect("output should be present");
        assert_eq!(loaded.resolution, res);
        assert_eq!(loaded.color.len(), pixel_count);
        assert_eq!(loaded.height.len(), pixel_count);
        assert_eq!(loaded.normal_map.len(), pixel_count);

        // Height uses 16-bit PNG — should be very precise
        for (a, b) in height.iter().zip(loaded.height.iter()) {
            assert!((a - b).abs() < 0.001, "height mismatch: {a} vs {b}");
        }
    }

    #[test]
    fn round_trip_without_output() {
        let project = make_project_with_layers();
        let path = temp_pap_path("no_output.papr");

        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert!(result.output.is_none());
    }

    #[test]
    fn round_trip_complex_guides() {
        let mut project = make_empty_project();
        project.layers = vec![make_test_layer("detailed", 0, 10)];

        let path = temp_pap_path("complex_guides.papr");
        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert_eq!(result.project.layers[0].guides.len(), 10);
        for (a, b) in project.layers[0]
            .guides
            .iter()
            .zip(result.project.layers[0].guides.iter())
        {
            assert_eq!(a.position, b.position);
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.influence, b.influence);
            assert_eq!(a.guide_type, b.guide_type);
            assert_eq!(a.strength, b.strength);
        }
    }

    // ── File Format Tests ──

    #[test]
    fn valid_zip_structure() {
        let project = make_project_with_layers();
        let path = temp_pap_path("zip_structure.papr");
        save_project(&project, &path, None, None).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();

        let names: Vec<&str> = archive.file_names().collect();
        assert!(names.contains(&"manifest.json"), "missing manifest.json");
        assert!(names.contains(&"project.json"), "missing project.json");
    }

    #[test]
    fn json_readable() {
        let project = make_project_with_layers();
        let path = temp_pap_path("json_readable.papr");
        save_project(&project, &path, None, None).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();

        let mut entry = archive.by_name("manifest.json").unwrap();
        let mut buf = String::new();
        entry.read_to_string(&mut buf).unwrap();

        assert!(buf.contains('\n'), "JSON should be pretty-printed");
        let parsed: serde_json::Value = serde_json::from_str(&buf).unwrap();
        assert_eq!(parsed["version"], "1");
        assert_eq!(parsed["app_name"], "PA Painter");
    }

    #[test]
    fn settings_round_trip() {
        let mut project = make_empty_project();
        project.settings = OutputSettings {
            resolution_preset: ResolutionPreset::Ultra,
            normal_strength: 3.0,
            ..OutputSettings::default()
        };

        let path = temp_pap_path("settings_test.papr");
        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert_eq!(
            result.project.settings.resolution_preset,
            ResolutionPreset::Ultra
        );
        assert_eq!(result.project.settings.resolution_preset.resolution(), 4096);
        assert_eq!(result.project.settings.normal_strength, 3.0);
    }

    // ── Thumbnail Test ──

    #[test]
    fn thumbnail_generated_with_output() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;
        let color: Vec<Color> = vec![Color::new(0.5, 0.3, 0.2, 1.0); pixel_count];
        let height: Vec<f32> = vec![0.5; pixel_count];
        let normal_map: Vec<[f32; 3]> = vec![[0.5, 0.5, 1.0]; pixel_count];

        let project = make_empty_project();
        let output = OutputCache {
            color: &color,
            height: &height,
            normal_map: &normal_map,
            stroke_time_order: &[],
            stroke_time_arc: &[],
            resolution: res,
            snapshot_hash: None,
        };

        let path = temp_pap_path("with_thumbnail.papr");
        save_project(&project, &path, Some(output), None).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<&str> = archive.file_names().collect();
        assert!(
            names.contains(&"thumbnails/preview.png"),
            "thumbnail should be present when output exists"
        );
    }

    #[test]
    fn no_thumbnail_without_output() {
        let project = make_empty_project();
        let path = temp_pap_path("no_thumbnail.papr");
        save_project(&project, &path, None, None).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<&str> = archive.file_names().collect();
        assert!(
            !names.contains(&"thumbnails/preview.png"),
            "no thumbnail without output"
        );
    }

    // ── Error Handling Tests ──

    #[test]
    fn load_nonexistent_file() {
        let result = load_project(Path::new("/tmp/definitely_missing_file.papr"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ProjectError::Io(_) => {}
            e => panic!("expected Io error, got: {e:?}"),
        }
    }

    #[test]
    fn load_invalid_zip() {
        let path = temp_pap_path("invalid.papr");
        std::fs::write(&path, b"this is not a zip file").unwrap();

        let result = load_project(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProjectError::Zip(_) => {}
            e => panic!("expected Zip error, got: {e:?}"),
        }
    }

    #[test]
    fn load_missing_manifest() {
        let path = temp_pap_path("no_manifest.papr");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();
            zip.start_file("dummy.txt", options).unwrap();
            zip.write_all(b"hello").unwrap();
            zip.finish().unwrap();
        }

        let result = load_project(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProjectError::InvalidFormat(msg) => {
                assert!(
                    msg.contains("manifest"),
                    "error should mention manifest: {msg}"
                );
            }
            e => panic!("expected InvalidFormat error, got: {e:?}"),
        }
    }

    // ── Data Integrity Test ──

    #[test]
    fn round_trip_integrity() {
        let project = make_project_with_layers();
        let path = temp_pap_path("integrity.papr");

        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();
        let loaded = result.project;

        assert_eq!(project.manifest.version, loaded.manifest.version);
        assert_eq!(project.manifest.app_name, loaded.manifest.app_name);
        // mesh_ref.path is #[serde(skip)] — not persisted
        assert_eq!(project.mesh_ref.format, loaded.mesh_ref.format);

        assert_eq!(project.layers.len(), loaded.layers.len());
        for (a, b) in project.layers.iter().zip(loaded.layers.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.visible, b.visible);
            assert_eq!(a.group_name, b.group_name);
            assert_eq!(a.order, b.order);
            assert_eq!(a.paint, b.paint);
        }

        assert_eq!(
            project.settings.resolution_preset,
            loaded.settings.resolution_preset
        );
        assert_eq!(
            project.settings.normal_strength,
            loaded.settings.normal_strength
        );
    }

    // ── Adapter Tests ──

    #[test]
    fn paint_layers_adapter() {
        let project = make_project_with_layers();
        let layers = project.paint_layers();

        assert_eq!(layers.len(), 3);
        for (layer_def, paint_layer) in project.layers.iter().zip(layers.iter()) {
            assert_eq!(paint_layer.name, layer_def.group_name);
            assert_eq!(paint_layer.order, layer_def.order);
            assert_eq!(paint_layer.params.brush_width, layer_def.paint.brush_width);
            assert_eq!(paint_layer.guides.len(), layer_def.guides.len());
        }
    }

    // ── Presets Round-Trip ──

    #[test]
    fn presets_round_trip() {
        let mut project = make_empty_project();
        project.presets = PresetLibrary::built_in();

        let path = temp_pap_path("presets_rt.papr");
        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert_eq!(
            result.project.presets.presets.len(),
            project.presets.presets.len()
        );

        for (a, b) in project
            .presets
            .presets
            .iter()
            .zip(result.project.presets.presets.iter())
        {
            assert_eq!(a.name, b.name);
            assert_eq!(a.values, b.values);
        }
    }

    #[test]
    fn visible_defaults_to_true() {
        let mut project = make_empty_project();
        project.layers = vec![make_test_layer("test", 0, 1)];

        let path = temp_pap_path("visible_default.papr");
        save_project(&project, &path, None, None).unwrap();
        let result = load_project(&path).unwrap();

        assert!(result.project.layers[0].visible);
    }
}
