use eframe::egui;
use glam::Vec2;

use practical_arcana_painter::project::Project;
use practical_arcana_painter::types::Layer;

use super::generation::{GenResult, GenerationManager};
use super::mesh_preview::MeshPreviewState;
use super::preview;
use super::preview::PreviewCache;
use super::undo::{UndoHistory, UndoSnapshot};

/// What part of a guide is being dragged.
#[derive(Debug, Clone, Copy)]
pub enum DragTarget {
    GuidePosition(usize),
    GuideDirection(usize),
    GuideInfluence(usize),
}

/// Which map to display in the UV View tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMode {
    Color,
    Height,
    Normal,
    StrokeId,
}

/// Top-level viewport tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportTab {
    UvView,
    Guide,
    Mesh3D,
}

impl ViewportTab {
    pub fn next(self) -> Self {
        match self {
            Self::UvView => Self::Guide,
            Self::Guide => Self::Mesh3D,
            Self::Mesh3D => Self::UvView,
        }
    }
}

/// Guide editing tool (active only in the Guide tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideTool {
    Select,         // 1 — click=select, drag center=move, drag handle=direction
    AddDirectional, // 2
    AddRadial,      // 3 — creates Source; toggle Inward in popup for Sink
    AddVortex,      // 4
}

impl Default for GuideTool {
    fn default() -> Self {
        Self::Select
    }
}

/// Viewport pan/zoom state.
pub struct ViewportState {
    /// Pan offset in UV space (0,0 = top-left corner visible).
    pub offset: Vec2,
    /// Zoom level: pixels per UV unit. E.g., 512.0 means 1 UV = 512 screen pixels.
    pub zoom: f32,
    pub show_wireframe: bool,
    pub show_path_overlay: bool,
}

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            zoom: 512.0,
            show_wireframe: true,
            show_path_overlay: true,
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

/// Summary of a mesh reload diff, shown as a dismissible window.
pub struct ReloadSummary {
    pub kept: Vec<String>,
    pub added: Vec<String>,
    pub orphaned: Vec<String>,
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
    pub loaded_normal: Option<practical_arcana_painter::asset_io::LoadedTexture>,
    pub uv_edges: Option<Vec<(Vec2, Vec2)>>,

    // ── Viewport ──
    pub viewport: ViewportState,
    pub viewport_tab: ViewportTab,
    pub map_mode: MapMode,
    pub guide_tool: GuideTool,
    pub mesh_preview: MeshPreviewState,

    // ── Selection ──
    pub selected_layer: Option<usize>,
    pub selected_guide: Option<usize>,
    pub guide_drag: Option<DragTarget>,

    // ── Display ──
    pub textures: DisplayTextures,

    // ── Preview Caches ──
    pub preview_cache: PreviewCache,
    pub path_overlay: preview::PathOverlayCache,
    pub preset_thumbnails: preview::PresetThumbnailCache,

    // ── Generation ──
    pub generation: GenerationManager,
    pub generated: Option<GenResult>,

    // ── Deferred Actions (set by child widgets, consumed by PainterApp) ──
    pub pending_open: bool,
    pub pending_new: bool,
    pub pending_save: bool,
    pub pending_export: bool,
    pub pending_export_glb: bool,
    pub pending_generate: bool,
    pub pending_reload_mesh: bool,
    pub pending_load_texture: bool,
    pub pending_load_normal: bool,

    /// Mesh reload diff summary shown as a dismissible window.
    pub reload_summary: Option<ReloadSummary>,

    // ── Status ──
    pub status_message: String,

    /// Snapshot of layers at last generation — used to detect outdated results.
    pub generation_snapshot: Option<Vec<Layer>>,

    // ── Undo/Redo ──
    pub undo: UndoHistory,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            project: Project::default(),
            project_path: None,
            dirty: false,
            loaded_mesh: None,
            loaded_texture: None,
            loaded_normal: None,
            uv_edges: None,
            viewport: ViewportState::default(),
            viewport_tab: ViewportTab::Guide,
            map_mode: MapMode::Color,
            guide_tool: GuideTool::default(),
            mesh_preview: MeshPreviewState::default(),
            selected_layer: None,
            selected_guide: None,
            guide_drag: None,
            textures: DisplayTextures::default(),
            preview_cache: PreviewCache::default(),
            path_overlay: preview::PathOverlayCache::default(),
            preset_thumbnails: preview::PresetThumbnailCache::default(),
            generation: GenerationManager::default(),
            generated: None,
            pending_open: false,
            pending_new: false,
            pending_save: false,
            pending_export: false,
            pending_export_glb: false,
            pending_generate: false,
            pending_reload_mesh: false,
            pending_load_texture: false,
            pending_load_normal: false,
            reload_summary: None,
            status_message: "Ready".to_string(),
            generation_snapshot: None,
            undo: UndoHistory::default(),
        }
    }

    /// Capture current undoable state as a snapshot.
    pub fn take_snapshot(&self) -> UndoSnapshot {
        UndoSnapshot {
            layers: self.project.layers.clone(),
            settings: self.project.settings.clone(),
            base_color: self.project.base_color.clone(),
            base_normal: self.project.base_normal.clone(),
            presets: self.project.presets.clone(),
        }
    }

    /// Restore project state from an undo snapshot.
    pub fn apply_snapshot(&mut self, snap: UndoSnapshot) {
        self.project.layers = snap.layers;
        self.project.settings = snap.settings;
        self.project.base_color = snap.base_color;
        self.project.base_normal = snap.base_normal;
        self.project.presets = snap.presets;
        self.dirty = true;

        // Fix selection indices if out of bounds
        if let Some(idx) = self.selected_layer {
            if idx >= self.project.layers.len() {
                self.selected_layer = if self.project.layers.is_empty() {
                    None
                } else {
                    Some(self.project.layers.len() - 1)
                };
                self.selected_guide = None;
            }
        }
    }

    /// Whether current results don't reflect current settings.
    /// Returns a label describing the state, or None if up-to-date.
    pub fn stale_reason(&self) -> Option<&'static str> {
        if self.generation_snapshot.is_none() {
            if !self.project.layers.is_empty() {
                return Some("Not generated");
            }
            return None;
        }
        if let Some(ref snapshot) = self.generation_snapshot {
            if *snapshot != self.project.layers {
                return Some("Modified");
            }
        }
        None
    }
}
