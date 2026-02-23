use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::asset_io::{linear_to_srgb, LoadedMesh};
use crate::types::{
    OutputSettings, PaintLayer, PaintSlot, PresetLibrary, StrokePath,
};
use crate::uv_mask::UvMask;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorRef {
    pub path: Option<String>,
    pub solid_color: [f32; 3],
}

/// Snapshot of the state that was used to generate the cached paths.
/// Compared against the current state to detect staleness.
#[derive(Debug)]
struct PathCacheKey {
    slots: Vec<PaintSlot>,
    resolution: u32,
    color_ref_path: Option<String>,
}

/// Full project state (in-memory representation).
#[derive(Debug)]
pub struct Project {
    pub manifest: Manifest,
    pub mesh_ref: MeshRef,
    pub color_ref: ColorRef,
    pub slots: Vec<PaintSlot>,
    pub presets: PresetLibrary,
    pub settings: OutputSettings,
    pub cached_height: Option<Vec<f32>>,
    pub cached_color: Option<Vec<[f32; 4]>>,
    /// Runtime path cache — one entry per slot in order-sorted sequence.
    /// Not serialized to disk.
    pub cached_paths: Option<Vec<Vec<StrokePath>>>,
    /// Key used to validate `cached_paths` (private).
    path_cache_key: Option<PathCacheKey>,
}

impl Project {
    /// Convert all slots to PaintLayers for downstream pipeline compatibility.
    pub fn paint_layers(&self) -> Vec<PaintLayer> {
        self.slots.iter().map(|s| s.to_paint_layer()).collect()
    }

    /// Build UV masks for each slot from the loaded mesh.
    /// Returns `None` for `__full_uv__` groups (paint entire UV space).
    pub fn build_masks(&self, mesh: &LoadedMesh, resolution: u32) -> Vec<Option<UvMask>> {
        self.slots
            .iter()
            .map(|slot| {
                if slot.group_name == "__full_uv__" {
                    None
                } else {
                    mesh.groups
                        .iter()
                        .find(|g| g.name == slot.group_name)
                        .map(|group| {
                            let mut mask = UvMask::from_mesh_group(mesh, group, resolution);
                            mask.dilate(2);
                            mask
                        })
                }
            })
            .collect()
    }

    /// Return cached paths if they are still valid for the current slots,
    /// resolution, and color texture path. Returns `None` on cache miss.
    pub fn cached_paths_if_valid(
        &self,
        resolution: u32,
    ) -> Option<&[Vec<StrokePath>]> {
        let key = self.path_cache_key.as_ref()?;
        let paths = self.cached_paths.as_ref()?;
        if key.resolution == resolution
            && key.slots == self.slots
            && key.color_ref_path == self.color_ref.path
        {
            Some(paths.as_slice())
        } else {
            None
        }
    }

    /// Store newly generated paths in the cache, along with a snapshot of the
    /// current project state that produced them.
    pub fn set_cached_paths(&mut self, paths: Vec<Vec<StrokePath>>, resolution: u32) {
        self.path_cache_key = Some(PathCacheKey {
            slots: self.slots.clone(),
            resolution,
            color_ref_path: self.color_ref.path.clone(),
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
        let encoder =
            image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
        use image::ImageEncoder;
        encoder
            .write_image(&pixels, thumb_size, thumb_size, image::ColorType::Rgb8.into())
            .ok()?;
    }
    Some(png_bytes)
}

// ── Save ──

/// Save a project to a .pap file (v3 format).
pub fn save_project(project: &Project, path: &Path) -> Result<(), ProjectError> {
    let file = std::fs::File::create(path)?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // manifest.json
    let manifest_json = serde_json::to_string_pretty(&project.manifest)?;
    zip.start_file("manifest.json", options)?;
    zip.write_all(manifest_json.as_bytes())?;

    // mesh_ref.json
    let mesh_json = serde_json::to_string_pretty(&project.mesh_ref)?;
    zip.start_file("mesh_ref.json", options)?;
    zip.write_all(mesh_json.as_bytes())?;

    // color_ref.json
    let color_json = serde_json::to_string_pretty(&project.color_ref)?;
    zip.start_file("color_ref.json", options)?;
    zip.write_all(color_json.as_bytes())?;

    // slots.json (v3)
    let slots_json = serde_json::to_string_pretty(&project.slots)?;
    zip.start_file("slots.json", options)?;
    zip.write_all(slots_json.as_bytes())?;

    // presets.json (v3)
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

/// Load a project from a .pap file.
///
/// Supports v3 (slots.json + presets.json), v2 (layers.json),
/// and v1 (regions.json) formats with automatic migration.
pub fn load_project(path: &Path) -> Result<Project, ProjectError> {
    let file = std::fs::File::open(path)?;
    let mut archive = ZipArchive::new(file)?;

    // manifest.json (required)
    let manifest: Manifest = read_json_entry(&mut archive, "manifest.json")
        .map_err(|e| match e {
            ProjectError::Zip(_) => {
                ProjectError::InvalidFormat("missing manifest.json".to_string())
            }
            other => other,
        })?;

    // mesh_ref.json (required)
    let mesh_ref: MeshRef = read_json_entry(&mut archive, "mesh_ref.json")?;

    // color_ref.json (required)
    let color_ref: ColorRef = read_json_entry(&mut archive, "color_ref.json")?;

    // Try v3 (slots.json) first, then v2 (layers.json), then v1 (regions.json)
    let (slots, presets) = match read_json_entry::<Vec<PaintSlot>>(&mut archive, "slots.json") {
        Ok(slots) => {
            let presets: PresetLibrary =
                read_json_entry(&mut archive, "presets.json").unwrap_or_default();
            (slots, presets)
        }
        Err(ProjectError::Zip(zip::result::ZipError::FileNotFound)) => {
            // Fall back to v2 layers.json or v1 regions.json
            let layers: Vec<PaintLayer> =
                match read_json_entry(&mut archive, "layers.json") {
                    Ok(layers) => layers,
                    Err(ProjectError::Zip(zip::result::ZipError::FileNotFound)) => {
                        read_json_entry(&mut archive, "regions.json")?
                    }
                    Err(e) => return Err(e),
                };
            migrate_v2_to_v3(&layers)
        }
        Err(e) => return Err(e),
    };

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
        color_ref,
        slots,
        presets,
        settings,
        cached_height,
        cached_color,
        cached_paths: None,
        path_cache_key: None,
    })
}

/// Migrate v2 PaintLayers to v3 PaintSlots + PresetLibrary.
fn migrate_v2_to_v3(layers: &[PaintLayer]) -> (Vec<PaintSlot>, PresetLibrary) {
    let slots: Vec<PaintSlot> = layers
        .iter()
        .map(PaintSlot::from_paint_layer)
        .collect();
    let mut presets = PresetLibrary::built_in();
    for (i, slot) in slots.iter().enumerate() {
        use crate::types::{StrokePreset, PatternPreset};
        let _ = presets.try_add_stroke_preset(StrokePreset {
            name: format!("migrated_{}_stroke", layers[i].name),
            values: slot.stroke.clone(),
        });
        let _ = presets.try_add_pattern_preset(PatternPreset {
            name: format!("migrated_{}_pattern", layers[i].name),
            values: slot.pattern.clone(),
        });
    }
    (slots, presets)
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
        GuideVertex, OutputSettings, PaintLayer, PaintSlot, PatternValues,
        PressurePreset, ResolutionPreset, StrokeParams, StrokeValues,
    };
    use glam::Vec2;

    fn make_manifest() -> Manifest {
        Manifest {
            version: "3.0.0".to_string(),
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

    fn make_color_ref() -> ColorRef {
        ColorRef {
            path: Some("textures/base_color.png".to_string()),
            solid_color: [1.0, 1.0, 1.0],
        }
    }

    fn make_test_slot(group_name: &str, order: i32, guide_count: usize) -> PaintSlot {
        let guides: Vec<GuideVertex> = (0..guide_count)
            .map(|i| GuideVertex {
                position: Vec2::new(0.1 * i as f32, 0.2 * i as f32),
                direction: Vec2::new(1.0, 0.0),
                influence: 0.2 + 0.01 * i as f32,
            })
            .collect();

        PaintSlot {
            group_name: group_name.to_string(),
            order,
            stroke: StrokeValues {
                brush_width: 20.0 + order as f32,
                load: 0.7,
                base_height: 0.4,
                ridge_height: 0.2,
                ridge_width: 4.0,
                ridge_variation: 0.1,
                body_wiggle: 0.1,
                pressure_preset: PressurePreset::FadeOut,
            },
            pattern: PatternValues {
                guides,
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
            seed: 100 + order as u32,
        }
    }

    fn make_empty_project() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            color_ref: make_color_ref(),
            slots: vec![],
            presets: PresetLibrary::built_in(),
            settings: OutputSettings::default(),
            cached_height: None,
            cached_color: None,
            cached_paths: None,
            path_cache_key: None,
        }
    }

    fn make_project_with_slots() -> Project {
        Project {
            manifest: make_manifest(),
            mesh_ref: make_mesh_ref(),
            color_ref: make_color_ref(),
            slots: vec![
                make_test_slot("skin", 0, 3),
                make_test_slot("armor", 1, 5),
                make_test_slot("cloth", 2, 2),
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
        let path = temp_pap_path("empty_v3.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.manifest.version, "3.0.0");
        assert_eq!(loaded.manifest.app_name, "Practical Arcana Painter");
        assert_eq!(loaded.slots.len(), 0);
        assert!(loaded.cached_height.is_none());
        assert!(loaded.cached_color.is_none());
    }

    #[test]
    fn round_trip_with_slots() {
        let project = make_project_with_slots();
        let path = temp_pap_path("with_slots.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.slots.len(), 3);

        for (a, b) in project.slots.iter().zip(loaded.slots.iter()) {
            assert_eq!(a.group_name, b.group_name);
            assert_eq!(a.order, b.order);
            assert_eq!(a.stroke.brush_width, b.stroke.brush_width);
            assert_eq!(a.stroke.load, b.stroke.load);
            assert_eq!(a.stroke.base_height, b.stroke.base_height);
            assert_eq!(a.stroke.ridge_height, b.stroke.ridge_height);
            assert_eq!(a.stroke.ridge_width, b.stroke.ridge_width);
            assert_eq!(a.stroke.ridge_variation, b.stroke.ridge_variation);
            assert_eq!(a.stroke.body_wiggle, b.stroke.body_wiggle);
            assert_eq!(a.stroke.pressure_preset, b.stroke.pressure_preset);
            assert_eq!(a.pattern.stroke_spacing, b.pattern.stroke_spacing);
            assert_eq!(a.pattern.color_variation, b.pattern.color_variation);
            assert_eq!(a.pattern.max_stroke_length, b.pattern.max_stroke_length);
            assert_eq!(a.pattern.angle_variation, b.pattern.angle_variation);
            assert_eq!(a.pattern.max_turn_angle, b.pattern.max_turn_angle);
            assert_eq!(a.seed, b.seed);
            assert_eq!(a.pattern.guides.len(), b.pattern.guides.len());
        }
    }

    #[test]
    fn round_trip_with_cache() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;

        let height: Vec<f32> = (0..pixel_count).map(|i| i as f32 / pixel_count as f32).collect();
        let color: Vec<[f32; 4]> = (0..pixel_count)
            .map(|i| {
                let v = i as f32 / pixel_count as f32;
                [v, v * 0.5, v * 0.3, 1.0]
            })
            .collect();

        let mut project = make_project_with_slots();
        project.cached_height = Some(height.clone());
        project.cached_color = Some(color.clone());

        let path = temp_pap_path("with_cache_v3.pap");
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
        let project = make_project_with_slots();
        let path = temp_pap_path("no_cache_v3.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert!(loaded.cached_height.is_none());
        assert!(loaded.cached_color.is_none());
    }

    #[test]
    fn round_trip_complex_guides() {
        let mut project = make_empty_project();
        project.slots = vec![make_test_slot("detailed", 0, 10)];

        let path = temp_pap_path("complex_guides_v3.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.slots[0].pattern.guides.len(), 10);
        for (a, b) in project.slots[0]
            .pattern
            .guides
            .iter()
            .zip(loaded.slots[0].pattern.guides.iter())
        {
            assert_eq!(a.position, b.position);
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.influence, b.influence);
        }
    }

    // ── File Format Tests ──

    #[test]
    fn valid_zip_structure() {
        let project = make_project_with_slots();
        let path = temp_pap_path("zip_structure_v3.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let archive = ZipArchive::new(file).unwrap();

        let names: Vec<&str> = archive.file_names().collect();
        assert!(names.contains(&"manifest.json"), "missing manifest.json");
        assert!(names.contains(&"mesh_ref.json"), "missing mesh_ref.json");
        assert!(names.contains(&"color_ref.json"), "missing color_ref.json");
        assert!(names.contains(&"slots.json"), "missing slots.json");
        assert!(names.contains(&"presets.json"), "missing presets.json");
        assert!(names.contains(&"settings.json"), "missing settings.json");
    }

    #[test]
    fn json_readable() {
        let project = make_project_with_slots();
        let path = temp_pap_path("json_readable_v3.pap");
        save_project(&project, &path).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let mut archive = ZipArchive::new(file).unwrap();

        let mut entry = archive.by_name("manifest.json").unwrap();
        let mut buf = String::new();
        entry.read_to_string(&mut buf).unwrap();

        assert!(buf.contains('\n'), "JSON should be pretty-printed");
        let parsed: serde_json::Value = serde_json::from_str(&buf).unwrap();
        assert_eq!(parsed["version"], "3.0.0");
        assert_eq!(parsed["app_name"], "Practical Arcana Painter");
    }

    #[test]
    fn version_field_preserved() {
        let mut project = make_empty_project();
        project.manifest.version = "3.1.0".to_string();

        let path = temp_pap_path("version_test_v3.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.manifest.version, "3.1.0");
    }

    #[test]
    fn settings_round_trip() {
        let mut project = make_empty_project();
        project.settings = OutputSettings {
            resolution_preset: ResolutionPreset::Ultra,
            normal_strength: 3.0,
            ..OutputSettings::default()
        };

        let path = temp_pap_path("settings_test_v3.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(
            loaded.settings.resolution_preset,
            ResolutionPreset::Ultra
        );
        assert_eq!(loaded.settings.resolution_preset.resolution(), 4096);
        assert_eq!(loaded.settings.normal_strength, 3.0);
    }

    #[test]
    fn color_ref_no_texture() {
        let mut project = make_empty_project();
        project.color_ref = ColorRef {
            path: None,
            solid_color: [0.5, 0.3, 0.8],
        };

        let path = temp_pap_path("no_texture_v3.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert!(loaded.color_ref.path.is_none());
        assert_eq!(loaded.color_ref.solid_color, [0.5, 0.3, 0.8]);
    }

    // ── Thumbnail Test ──

    #[test]
    fn thumbnail_generated_with_cache() {
        let res = 16u32;
        let pixel_count = (res * res) as usize;
        let color: Vec<[f32; 4]> = vec![[0.5, 0.3, 0.2, 1.0]; pixel_count];

        let mut project = make_empty_project();
        project.cached_color = Some(color);

        let path = temp_pap_path("with_thumbnail_v3.pap");
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
        let path = temp_pap_path("no_thumbnail_v3.pap");
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
        let path = temp_pap_path("invalid_v3.pap");
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
        let path = temp_pap_path("no_manifest_v3.pap");
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
                assert!(msg.contains("manifest"), "error should mention manifest: {msg}");
            }
            e => panic!("expected InvalidFormat error, got: {e:?}"),
        }
    }

    #[test]
    fn load_corrupt_json() {
        let path = temp_pap_path("corrupt_json_v3.pap");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();

            let manifest = serde_json::to_string_pretty(&make_manifest()).unwrap();
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();

            let mesh = serde_json::to_string_pretty(&make_mesh_ref()).unwrap();
            zip.start_file("mesh_ref.json", options).unwrap();
            zip.write_all(mesh.as_bytes()).unwrap();

            let color = serde_json::to_string_pretty(&make_color_ref()).unwrap();
            zip.start_file("color_ref.json", options).unwrap();
            zip.write_all(color.as_bytes()).unwrap();

            // Invalid JSON in slots.json
            zip.start_file("slots.json", options).unwrap();
            zip.write_all(b"{ this is not valid json !!!").unwrap();

            let settings = serde_json::to_string_pretty(&OutputSettings::default()).unwrap();
            zip.start_file("settings.json", options).unwrap();
            zip.write_all(settings.as_bytes()).unwrap();

            zip.finish().unwrap();
        }

        let result = load_project(&path);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProjectError::Json(_) => {}
            e => panic!("expected Json error, got: {e:?}"),
        }
    }

    // ── Data Integrity Test ──

    #[test]
    fn round_trip_integrity() {
        let project = make_project_with_slots();
        let path = temp_pap_path("integrity_v3.pap");

        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(project.manifest.version, loaded.manifest.version);
        assert_eq!(project.manifest.app_name, loaded.manifest.app_name);
        assert_eq!(project.manifest.created_at, loaded.manifest.created_at);
        assert_eq!(project.manifest.modified_at, loaded.manifest.modified_at);

        assert_eq!(project.mesh_ref.path, loaded.mesh_ref.path);
        assert_eq!(project.mesh_ref.format, loaded.mesh_ref.format);

        assert_eq!(project.color_ref.path, loaded.color_ref.path);
        assert_eq!(project.color_ref.solid_color, loaded.color_ref.solid_color);

        assert_eq!(project.slots.len(), loaded.slots.len());
        for (a, b) in project.slots.iter().zip(loaded.slots.iter()) {
            assert_eq!(a.group_name, b.group_name);
            assert_eq!(a.order, b.order);
            assert_eq!(a.stroke, b.stroke);
            assert_eq!(a.pattern, b.pattern);
            assert_eq!(a.seed, b.seed);
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

    #[test]
    fn missing_cache_in_saved_zip_loads_as_none() {
        let res = 4u32;
        let mut project = make_empty_project();
        project.cached_height = Some(vec![0.5; (res * res) as usize]);
        project.cached_color = Some(vec![[0.5, 0.5, 0.5, 1.0]; (res * res) as usize]);

        let path_with_cache = temp_pap_path("has_cache_v3.pap");
        save_project(&project, &path_with_cache).unwrap();

        let path_no_cache = temp_pap_path("stripped_cache_v3.pap");
        {
            let src_file = std::fs::File::open(&path_with_cache).unwrap();
            let mut src_archive = ZipArchive::new(src_file).unwrap();

            let dst_file = std::fs::File::create(&path_no_cache).unwrap();
            let mut dst_zip = ZipWriter::new(dst_file);
            let options = SimpleFileOptions::default();

            for name in &[
                "manifest.json",
                "mesh_ref.json",
                "color_ref.json",
                "slots.json",
                "presets.json",
                "settings.json",
            ] {
                let mut entry = src_archive.by_name(name).unwrap();
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf).unwrap();
                dst_zip.start_file(*name, options).unwrap();
                dst_zip.write_all(&buf).unwrap();
            }
            dst_zip.finish().unwrap();
        }

        let loaded = load_project(&path_no_cache).unwrap();
        assert!(loaded.cached_height.is_none());
        assert!(loaded.cached_color.is_none());
    }

    // ── v2 → v3 Migration Tests ──

    #[test]
    fn load_v2_layers_json_migrates_to_slots() {
        let path = temp_pap_path("v2_migration.pap");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();

            let manifest = Manifest {
                version: "2.0.0".to_string(),
                app_name: "Practical Arcana Painter".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                modified_at: "2026-01-01T00:00:00Z".to_string(),
            };
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes()).unwrap();

            zip.start_file("mesh_ref.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&make_mesh_ref()).unwrap().as_bytes()).unwrap();

            zip.start_file("color_ref.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&make_color_ref()).unwrap().as_bytes()).unwrap();

            // v2 format: layers.json
            let layer = PaintLayer {
                name: "skin".to_string(),
                order: 0,
                params: StrokeParams {
                    brush_width: 30.0,
                    load: 0.8,
                    base_height: 0.5,
                    ridge_height: 0.3,
                    ridge_width: 5.0,
                    ridge_variation: 0.1,
                    body_wiggle: 0.15,
                    stroke_spacing: 1.0,
                    pressure_preset: PressurePreset::FadeOut,
                    color_variation: 0.1,
                    max_stroke_length: 240.0,
                    angle_variation: 5.0,
                    max_turn_angle: 15.0,
                    color_break_threshold: None,
                    normal_break_threshold: None,
                    overlap_ratio: None,
                    overlap_dist_factor: None,
                    seed: 42,
                },
                guides: vec![GuideVertex {
                    position: Vec2::new(0.5, 0.5),
                    direction: Vec2::X,
                    influence: 1.5,
                }],
            };

            zip.start_file("layers.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&[&layer]).unwrap().as_bytes()).unwrap();

            zip.start_file("settings.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&OutputSettings::default()).unwrap().as_bytes()).unwrap();

            zip.finish().unwrap();
        }

        let loaded = load_project(&path).unwrap();

        // Should have been migrated to slots
        assert_eq!(loaded.slots.len(), 1);
        assert_eq!(loaded.slots[0].group_name, "__full_uv__");
        assert_eq!(loaded.slots[0].order, 0);
        assert_eq!(loaded.slots[0].stroke.brush_width, 30.0);
        assert_eq!(loaded.slots[0].stroke.load, 0.8);
        assert_eq!(loaded.slots[0].pattern.guides.len(), 1);
        assert_eq!(loaded.slots[0].seed, 42);

        // Should have built-in + migrated presets
        assert!(!loaded.presets.strokes.is_empty());
        assert!(!loaded.presets.patterns.is_empty());
    }

    // ── v1 Compatibility Test ──

    #[test]
    fn load_v1_regions_json() {
        // Create a .pap with regions.json (v1 format) instead of layers.json
        let path = temp_pap_path("v1_compat_v3.pap");
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut zip = ZipWriter::new(file);
            let options = SimpleFileOptions::default();

            let manifest = Manifest {
                version: "1.0.0".to_string(),
                ..make_manifest()
            };
            zip.start_file("manifest.json", options).unwrap();
            zip.write_all(serde_json::to_string_pretty(&manifest).unwrap().as_bytes()).unwrap();

            let mesh = serde_json::to_string_pretty(&make_mesh_ref()).unwrap();
            zip.start_file("mesh_ref.json", options).unwrap();
            zip.write_all(mesh.as_bytes()).unwrap();

            let color = serde_json::to_string_pretty(&make_color_ref()).unwrap();
            zip.start_file("color_ref.json", options).unwrap();
            zip.write_all(color.as_bytes()).unwrap();

            // v1 format: regions.json with id and mask fields
            let regions_json = r#"[
                {
                    "id": 0,
                    "name": "skin",
                    "mask": [{"vertices": [[0.1,0.1],[0.9,0.1],[0.9,0.9],[0.1,0.9]]}],
                    "order": 0,
                    "params": {
                        "brush_width": 30.0, "load": 0.8, "base_height": 0.5,
                        "ridge_height": 0.3, "ridge_width": 5.0, "ridge_variation": 0.1,
                        "body_wiggle": 0.15, "stroke_spacing": 1.0,
                        "pressure_preset": "FadeOut", "color_variation": 0.1,
                        "max_stroke_length": 240.0, "angle_variation": 5.0,
                        "max_turn_angle": 15.0, "seed": 42
                    },
                    "guides": [{"position": [0.5, 0.5], "direction": [1.0, 0.0], "influence": 1.5}]
                }
            ]"#;
            zip.start_file("regions.json", options).unwrap();
            zip.write_all(regions_json.as_bytes()).unwrap();

            let settings = serde_json::to_string_pretty(&OutputSettings::default()).unwrap();
            zip.start_file("settings.json", options).unwrap();
            zip.write_all(settings.as_bytes()).unwrap();

            zip.finish().unwrap();
        }

        let loaded = load_project(&path).unwrap();
        // v1 regions are migrated through PaintLayer → PaintSlot
        assert_eq!(loaded.slots.len(), 1);
        assert_eq!(loaded.slots[0].group_name, "__full_uv__");
        assert_eq!(loaded.slots[0].order, 0);
        assert_eq!(loaded.slots[0].stroke.brush_width, 30.0);
        assert_eq!(loaded.slots[0].pattern.guides.len(), 1);
    }

    // ── Adapter Tests ──

    #[test]
    fn paint_layers_adapter() {
        let project = make_project_with_slots();
        let layers = project.paint_layers();

        assert_eq!(layers.len(), 3);
        for (slot, layer) in project.slots.iter().zip(layers.iter()) {
            assert_eq!(layer.name, slot.group_name);
            assert_eq!(layer.order, slot.order);
            assert_eq!(layer.params.brush_width, slot.stroke.brush_width);
            assert_eq!(layer.params.seed, slot.seed);
            assert_eq!(layer.guides.len(), slot.pattern.guides.len());
        }
    }

    #[test]
    fn slot_round_trip_through_paint_layer() {
        let slot = make_test_slot("test_group", 5, 3);
        let layer = slot.to_paint_layer();
        let slot2 = PaintSlot::from_paint_layer(&layer);

        assert_eq!(slot.stroke, slot2.stroke);
        assert_eq!(slot.pattern, slot2.pattern);
        assert_eq!(slot.seed, slot2.seed);
        assert_eq!(slot.order, slot2.order);
    }

    // ── Presets Round-Trip ──

    #[test]
    fn presets_round_trip() {
        let mut project = make_empty_project();
        project.presets = PresetLibrary::built_in();

        let path = temp_pap_path("presets_rt.pap");
        save_project(&project, &path).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded.presets.strokes.len(), project.presets.strokes.len());
        assert_eq!(loaded.presets.patterns.len(), project.presets.patterns.len());

        for (a, b) in project.presets.strokes.iter().zip(loaded.presets.strokes.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.values, b.values);
        }
    }
}
