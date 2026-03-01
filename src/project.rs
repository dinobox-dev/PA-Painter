//! `.pap` project file I/O and the top-level [`Project`] data model.
//!
//! A `.pap` file is a ZIP archive containing `project.json` plus optional asset
//! references. This module handles serialization, deserialization, migration,
//! and layer/mask management.

use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::asset_io::{linear_to_srgb, LoadedMesh};
use crate::types::{Layer, OutputSettings, PaintLayer, PresetLibrary, StrokePath};
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
    pub path: String,
    pub format: String,
}

/// Base color source for the project — either a solid color or a texture file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BaseColor {
    Solid([f32; 3]),
    Texture(String),
}

impl Default for BaseColor {
    fn default() -> Self {
        BaseColor::Solid([0.5, 0.5, 0.5])
    }
}

impl BaseColor {
    /// Returns the texture path if this is a Texture variant.
    pub fn texture_path(&self) -> Option<&str> {
        match self {
            BaseColor::Texture(p) => Some(p.as_str()),
            BaseColor::Solid(_) => None,
        }
    }

    /// Returns the solid color, or a default gray for Texture variant.
    pub fn solid_color(&self) -> [f32; 3] {
        match self {
            BaseColor::Solid(c) => *c,
            BaseColor::Texture(_) => [0.5, 0.5, 0.5],
        }
    }
}

/// Snapshot of the state that was used to generate the cached paths.
/// Compared against the current state to detect staleness.
#[derive(Debug)]
struct PathCacheKey {
    layers: Vec<Layer>,
    base_color_key: String,
}

/// Full project state (in-memory representation).
#[derive(Debug)]
pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub base_color: BaseColor,
    pub base_normal: Option<String>,
    pub layers: Vec<Layer>,
    pub presets: PresetLibrary,
    pub settings: OutputSettings,
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
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
                app_name: "PracticalArcanaPainter".to_string(),
                created_at: utc_now_iso8601(),
                modified_at: utc_now_iso8601(),
            },
            mesh_ref: MeshRef {
                path: String::new(),
                format: String::new(),
            },
            base_color: BaseColor::default(),
            base_normal: None,
            layers: Vec::new(),
            presets: PresetLibrary::default(),
            settings: OutputSettings::default(),
            cached_height: None,
            cached_color: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }
}

fn base_color_cache_key(bc: &BaseColor) -> String {
    match bc {
        BaseColor::Solid(c) => format!("solid:{:.4},{:.4},{:.4}", c[0], c[1], c[2]),
        BaseColor::Texture(p) => format!("tex:{}", p),
    }
}

impl Project {
    /// Convert visible layers to PaintLayers for downstream pipeline compatibility.
    /// Each layer gets `seed + layer_index` for unique randomness.
    pub fn paint_layers(&self) -> Vec<PaintLayer> {
        let base_seed = self.settings.seed;
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.visible)
            .map(|(i, l)| l.to_paint_layer_with_seed(base_seed.wrapping_add(i as u32)))
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
        if key.layers == self.layers && key.base_color_key == base_color_cache_key(&self.base_color)
        {
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
            base_color_key: base_color_cache_key(&self.base_color),
        });
        self.cached_paths = Some(paths);
    }

    /// Explicitly invalidate the path cache.
    pub fn invalidate_path_cache(&mut self) {
        self.cached_paths = None;
        self.path_cache_key = None;
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
    #[error("Bincode error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("Invalid format: {0}")]
    InvalidFormat(String),
}

// ── Thumbnail Generation ──

/// Generate a 256x256 thumbnail PNG from a cached color map.
/// Returns None if no cached color map is available.
fn generate_thumbnail(project: &Project) -> Option<Vec<u8>> {
    let color = project.cached_color.as_ref()?;
    if color.is_empty() {
        return None;
    }

    let resolution = (color.len() as f64).sqrt() as u32;
    if resolution == 0 || (resolution * resolution) as usize != color.len() {
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
            let c = color[(sy * resolution + sx) as usize];
            pixels.push((linear_to_srgb(c[0].clamp(0.0, 1.0)) * 255.0).round() as u8);
            pixels.push((linear_to_srgb(c[1].clamp(0.0, 1.0)) * 255.0).round() as u8);
            pixels.push((linear_to_srgb(c[2].clamp(0.0, 1.0)) * 255.0).round() as u8);
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

/// Save a project to a `.pap` file.
pub fn save_project(project: &Project, path: &Path) -> Result<(), ProjectError> {
    let file = std::fs::File::create(path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // manifest.json
    let manifest_json = serde_json::to_string_pretty(&project.manifest)?;
    zip.start_file("manifest.json", options)?;
    zip.write_all(manifest_json.as_bytes())?;

    // mesh_ref.json
    let mesh_json = serde_json::to_string_pretty(&project.mesh_ref)?;
    zip.start_file("mesh_ref.json", options)?;
    zip.write_all(mesh_json.as_bytes())?;

    // base_sources.json
    #[derive(Serialize)]
    struct BaseSources<'a> {
        base_color: &'a BaseColor,
        base_normal: &'a Option<String>,
    }
    let base_json = serde_json::to_string_pretty(&BaseSources {
        base_color: &project.base_color,
        base_normal: &project.base_normal,
    })?;
    zip.start_file("base_sources.json", options)?;
    zip.write_all(base_json.as_bytes())?;

    // layer_stack.json
    let layers_json = serde_json::to_string_pretty(&project.layers)?;
    zip.start_file("layer_stack.json", options)?;
    zip.write_all(layers_json.as_bytes())?;

    // presets.json
    let presets_json = serde_json::to_string_pretty(&project.presets)?;
    zip.start_file("presets.json", options)?;
    zip.write_all(presets_json.as_bytes())?;

    // settings.json
    let settings_json = serde_json::to_string_pretty(&project.settings)?;
    zip.start_file("settings.json", options)?;
    zip.write_all(settings_json.as_bytes())?;

    // cache/height_map.bin (optional)
    if let Some(height) = &project.cached_height {
        let height_bin = bincode::serialize(height)?;
        zip.start_file("cache/height_map.bin", options)?;
        zip.write_all(&height_bin)?;
    }

    // cache/color_map.bin (optional)
    if let Some(color) = &project.cached_color {
        let color_bin = bincode::serialize(color)?;
        zip.start_file("cache/color_map.bin", options)?;
        zip.write_all(&color_bin)?;
    }

    // thumbnails/preview.png (optional)
    if let Some(png_bytes) = generate_thumbnail(project) {
        zip.start_file("thumbnails/preview.png", options)?;
        zip.write_all(&png_bytes)?;
    }

    zip.finish()?;
    Ok(())
}

// ── Load ──

/// Load a project from a `.pap` file.
pub fn load_project(path: &Path) -> Result<Project, ProjectError> {
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

    // mesh_ref.json (required)
    let mesh_ref: MeshRef = read_json_entry(&mut archive, "mesh_ref.json")?;

    // base_sources.json
    #[derive(Deserialize)]
    struct BaseSources {
        base_color: BaseColor,
        #[serde(default)]
        base_normal: Option<String>,
    }
    let base: BaseSources = read_json_entry(&mut archive, "base_sources.json")?;

    // layer_stack.json
    let layers: Vec<Layer> = read_json_entry(&mut archive, "layer_stack.json")?;

    // presets.json
    let presets: PresetLibrary = read_json_entry(&mut archive, "presets.json").unwrap_or_default();

    // settings.json (required)
    let settings: OutputSettings = read_json_entry(&mut archive, "settings.json")?;

    // cache (optional)
    let cached_height: Option<Vec<f32>> =
        read_bincode_entry_optional(&mut archive, "cache/height_map.bin")?;
    let cached_color: Option<Vec<[f32; 4]>> =
        read_bincode_entry_optional(&mut archive, "cache/color_map.bin")?;

    Ok(Project {
        manifest,
        mesh_ref,
        base_color: base.base_color,
        base_normal: base.base_normal,
        layers,
        presets,
        settings,
        cached_height,
        cached_color,
        cached_paths: None,
        path_cache_key: None,
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

fn read_bincode_entry_optional<T: serde::de::DeserializeOwned>(
    archive: &mut ZipArchive<std::fs::File>,
    name: &str,
) -> Result<Option<T>, ProjectError> {
    match archive.by_name(name) {
        Ok(mut entry) => {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let value = bincode::deserialize(&buf)?;
            Ok(Some(value))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(ProjectError::Zip(e)),
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        Guide, GuideType, Layer, OutputSettings, PaintValues, PressureCurve, PressurePreset,
        ResolutionPreset,
    };
    use glam::Vec2;

    fn make_manifest() -> Manifest {
        Manifest {
            version: "1".to_string(),
            app_name: "Practical Arcana Painter".to_string(),
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
            },
            guides,
        }
    }

    fn make_empty_project() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            base_color: BaseColor::Texture("textures/base_color.png".to_string()),
            base_normal: None,
            layers: vec![],
            presets: PresetLibrary::built_in(),
            settings: OutputSettings::default(),
            cached_height: None,
            cached_color: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }

    fn make_project_with_layers() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            base_color: BaseColor::Texture("textures/base_color.png".to_string()),
            base_normal: None,
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
            cached_height: None,
            cached_color: None,
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
        let path = temp_pap_path("empty.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.manifest.version, "1");
        assert_eq!(loaded.manifest.app_name, "Practical Arcana Painter");
        assert_eq!(loaded.layers.len(), 0);
        assert!(loaded.cached_height.is_none());
        assert!(loaded.cached_color.is_none());
    }

    #[test]
    fn round_trip_with_layers() {
        let project = make_project_with_layers();
        let path = temp_pap_path("with_layers.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.layers.len(), 3);

        for (a, b) in project.layers.iter().zip(loaded.layers.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.visible, b.visible);
            assert_eq!(a.group_name, b.group_name);
            assert_eq!(a.order, b.order);
            assert_eq!(a.paint, b.paint);
            assert_eq!(a.guides.len(), b.guides.len());
        }
    }

    #[test]
    fn round_trip_with_cache() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;

        let height: Vec<f32> = (0..pixel_count)
            .map(|i| i as f32 / pixel_count as f32)
            .collect();
        let color: Vec<[f32; 4]> = (0..pixel_count)
            .map(|i| {
                let v = i as f32 / pixel_count as f32;
                [v, v * 0.5, v * 0.3, 1.0]
            })
            .collect();

        let mut project = make_project_with_layers();
        project.cached_height = Some(height.clone());
        project.cached_color = Some(color.clone());

        let path = temp_pap_path("with_cache.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        let lh = loaded.cached_height.unwrap();
        assert_eq!(lh.len(), pixel_count);
        for (a, b) in height.iter().zip(lh.iter()) {
            assert_eq!(a, b, "height mismatch");
        }

        let lc = loaded.cached_color.unwrap();
        assert_eq!(lc.len(), pixel_count);
        for (a, b) in color.iter().zip(lc.iter()) {
            assert_eq!(a, b, "color mismatch");
        }
    }

    #[test]
    fn round_trip_without_cache() {
        let project = make_project_with_layers();
        let path = temp_pap_path("no_cache.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert!(loaded.cached_height.is_none());
        assert!(loaded.cached_color.is_none());
    }

    #[test]
    fn round_trip_complex_guides() {
        let mut project = make_empty_project();
        project.layers = vec![make_test_layer("detailed", 0, 10)];

        let path = temp_pap_path("complex_guides.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.layers[0].guides.len(), 10);
        for (a, b) in project.layers[0]
            .guides
            .iter()
            .zip(loaded.layers[0].guides.iter())
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
        let path = temp_pap_path("zip_structure.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();

        let names: Vec<&str> = archive.file_names().collect();
        assert!(names.contains(&"manifest.json"), "missing manifest.json");
        assert!(names.contains(&"mesh_ref.json"), "missing mesh_ref.json");
        assert!(
            names.contains(&"base_sources.json"),
            "missing base_sources.json"
        );
        assert!(
            names.contains(&"layer_stack.json"),
            "missing layer_stack.json"
        );
        assert!(names.contains(&"presets.json"), "missing presets.json");
        assert!(names.contains(&"settings.json"), "missing settings.json");
    }

    #[test]
    fn json_readable() {
        let project = make_project_with_layers();
        let path = temp_pap_path("json_readable.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();

        let mut entry = archive.by_name("manifest.json").unwrap();
        let mut buf = String::new();
        entry.read_to_string(&mut buf).unwrap();

        assert!(buf.contains('\n'), "JSON should be pretty-printed");
        let parsed: serde_json::Value = serde_json::from_str(&buf).unwrap();
        assert_eq!(parsed["version"], "1");
        assert_eq!(parsed["app_name"], "Practical Arcana Painter");
    }

    #[test]
    fn settings_round_trip() {
        let mut project = make_empty_project();
        project.settings = OutputSettings {
            resolution_preset: ResolutionPreset::Ultra,
            normal_strength: 3.0,
            ..OutputSettings::default()
        };

        let path = temp_pap_path("settings_test.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.settings.resolution_preset, ResolutionPreset::Ultra);
        assert_eq!(loaded.settings.resolution_preset.resolution(), 4096);
        assert_eq!(loaded.settings.normal_strength, 3.0);
    }

    #[test]
    fn base_color_solid() {
        let mut project = make_empty_project();
        project.base_color = BaseColor::Solid([0.5, 0.3, 0.8]);

        let path = temp_pap_path("solid_color.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        match loaded.base_color {
            BaseColor::Solid(c) => assert_eq!(c, [0.5, 0.3, 0.8]),
            _ => panic!("expected Solid base color"),
        }
    }

    #[test]
    fn base_color_texture() {
        let mut project = make_empty_project();
        project.base_color = BaseColor::Texture("textures/foo.png".to_string());

        let path = temp_pap_path("texture_color.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        match loaded.base_color {
            BaseColor::Texture(p) => assert_eq!(p, "textures/foo.png"),
            _ => panic!("expected Texture base color"),
        }
    }

    #[test]
    fn base_normal_round_trip() {
        let mut project = make_empty_project();
        project.base_normal = Some("normals/base.png".to_string());

        let path = temp_pap_path("base_normal.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.base_normal, Some("normals/base.png".to_string()));
    }

    // ── Thumbnail Test ──

    #[test]
    fn thumbnail_generated_with_cache() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;
        let color: Vec<[f32; 4]> = vec![[0.5, 0.3, 0.2, 1.0]; pixel_count];

        let mut project = make_empty_project();
        project.cached_color = Some(color);

        let path = temp_pap_path("with_thumbnail.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<&str> = archive.file_names().collect();
        assert!(
            names.contains(&"thumbnails/preview.png"),
            "thumbnail should be present when cache exists"
        );
    }

    #[test]
    fn no_thumbnail_without_cache() {
        let project = make_empty_project();
        let path = temp_pap_path("no_thumbnail.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();
        let names: Vec<&str> = archive.file_names().collect();
        assert!(
            !names.contains(&"thumbnails/preview.png"),
            "no thumbnail without cache"
        );
    }

    // ── Error Handling Tests ──

    #[test]
    fn load_nonexistent_file() {
        let result = load_project(Path::new("/tmp/definitely_missing_file.pap"));
        assert!(result.is_err());
        match result.unwrap_err() {
            ProjectError::Io(_) => {}
            e => panic!("expected Io error, got: {e:?}"),
        }
    }

    #[test]
    fn load_invalid_zip() {
        let path = temp_pap_path("invalid.pap");
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
        let path = temp_pap_path("no_manifest.pap");
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
        let path = temp_pap_path("integrity.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(project.manifest.version, loaded.manifest.version);
        assert_eq!(project.manifest.app_name, loaded.manifest.app_name);
        assert_eq!(project.mesh_ref.path, loaded.mesh_ref.path);
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

        let path = temp_pap_path("presets_rt.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.presets.presets.len(), project.presets.presets.len());

        for (a, b) in project
            .presets
            .presets
            .iter()
            .zip(loaded.presets.presets.iter())
        {
            assert_eq!(a.name, b.name);
            assert_eq!(a.values, b.values);
        }
    }

    #[test]
    fn visible_defaults_to_true() {
        let mut project = make_empty_project();
        project.layers = vec![make_test_layer("test", 0, 1)];

        let path = temp_pap_path("visible_default.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert!(loaded.layers[0].visible);
    }
}
