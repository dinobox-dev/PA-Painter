use eframe::egui;
use glam::Vec2;

use practical_arcana_painter::project::Project;
use practical_arcana_painter::types::PaintSlot;

use super::generation::{GenResult, GenerationManager};
use super::mesh_preview::MeshPreviewState;
use super::preview::PreviewCache;

/// What part of a guide is being dragged.
#[derive(Debug, Clone, Copy)]
pub enum DragTarget {
    GuidePosition(usize),
    GuideDirection(usize),
}

/// Which map to display in the main viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Color,
    Height,
    Normal,
    StrokeId,
    Mesh3D,
}

/// Viewport pan/zoom state.
pub struct ViewportState {
    /// Pan offset in UV space (0,0 = top-left corner visible).
    pub offset: Vec2,
    /// Zoom level: pixels per UV unit. E.g., 512.0 means 1 UV = 512 screen pixels.
    pub zoom: f32,
    pub show_wireframe: bool,
    pub show_guides: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            zoom: 512.0,
            show_wireframe: true,
            show_guides: true,
        }
    }
}

/// Texture handles for the four output maps.
#[derive(Default)]
pub struct DisplayTextures {
    pub color: Option<egui::TextureHandle>,
    pub height: Option<egui::TextureHandle>,
    pub normal: Option<egui::TextureHandle>,
    pub stroke_id: Option<egui::TextureHandle>,
    pub base_texture: Option<egui::TextureHandle>,
}

/// Central GUI application state.
pub struct AppState {
    // ── Project ──
    pub project: Project,
    pub project_path: Option<std::path::PathBuf>,
    pub dirty: bool,

    // ── Loaded Assets ──
    pub loaded_mesh: Option<practical_arcana_painter::asset_io::LoadedMesh>,
    pub loaded_texture: Option<practical_arcana_painter::asset_io::LoadedTexture>,
    pub uv_edges: Option<Vec<(Vec2, Vec2)>>,

    // ── Viewport ──
    pub viewport: ViewportState,
    pub view_mode: ViewMode,
    pub mesh_preview: MeshPreviewState,

    // ── Selection ──
    pub selected_slot: Option<usize>,
    pub selected_guide: Option<usize>,
    pub guide_drag: Option<DragTarget>,

    // ── Display ──
    pub textures: DisplayTextures,

    // ── Preview Caches ──
    pub preview_cache: PreviewCache,

    // ── Generation ──
    pub generation: GenerationManager,
    pub generated: Option<GenResult>,

    // ── Deferred Actions (set by child widgets, consumed by PainterApp) ──
    pub pending_open: bool,
    pub pending_new: bool,
    pub pending_save: bool,
    pub pending_export: bool,
    pub pending_generate: bool,
    pub pending_load_mesh: bool,
    pub pending_load_texture: bool,

    // ── Status ──
    pub status_message: String,

    /// Snapshot of slots at last generation — used to detect outdated results.
    pub generation_snapshot: Option<Vec<PaintSlot>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            project: Project::default(),
            project_path: None,
            dirty: false,
            loaded_mesh: None,
            loaded_texture: None,
            uv_edges: None,
            viewport: ViewportState::default(),
            view_mode: ViewMode::Color,
            mesh_preview: MeshPreviewState::default(),
            selected_slot: None,
            selected_guide: None,
            guide_drag: None,
            textures: DisplayTextures::default(),
            preview_cache: PreviewCache::default(),
            generation: GenerationManager::default(),
            generated: None,
            pending_open: false,
            pending_new: false,
            pending_save: false,
            pending_export: false,
            pending_generate: false,
            pending_load_mesh: false,
            pending_load_texture: false,
            status_message: "Ready".to_string(),
            generation_snapshot: None,
        }
    }

    /// Whether current results don't reflect current settings.
    /// Returns a label describing the state, or None if up-to-date.
    pub fn stale_reason(&self) -> Option<&'static str> {
        if self.generation_snapshot.is_none() {
            if !self.project.slots.is_empty() {
                return Some("Not generated");
            }
            return None;
        }
        if let Some(ref snapshot) = self.generation_snapshot {
            if *snapshot != self.project.slots {
                return Some("Modified");
            }
        }
        None
    }
}
