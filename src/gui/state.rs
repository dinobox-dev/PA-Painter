use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui;
use glam::Vec2;

use practical_arcana_painter::asset_io::LoadedMesh;
use practical_arcana_painter::project::Project;
use practical_arcana_painter::types::{Layer, OutputSettings, TextureSource};

use super::generation::{GenResult, GenerationManager};
use super::mesh_preview::MeshPreviewState;
use super::preview;
use super::preview::PreviewCache;
use super::undo::{UndoHistory, UndoSnapshot};

/// What part of a guide is being dragged.
#[allow(clippy::enum_variant_names)]
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

/// Which base texture to display in the Setup tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SetupMapMode {
    #[default]
    Color,
    Normal,
}

/// Top-level viewport tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewportTab {
    UvView,
    Setup,
    Mesh3D,
}

impl ViewportTab {
    /// Cycle order must match tab-bar order in `viewport.rs:show_viewport()`.
    pub fn next(self) -> Self {
        match self {
            Self::Setup => Self::UvView,
            Self::UvView => Self::Mesh3D,
            Self::Mesh3D => Self::Setup,
        }
    }
}

/// 3D viewport orbit target: camera vs light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrbitTarget {
    #[default]
    Object,
    Light,
}

/// What to display on the 3D mesh surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResultMode {
    /// No generated texture — show base mesh.
    None,
    /// Static paint result (color + normal + height).
    #[default]
    Paint,
    /// Animated stroke time map playback.
    Drawing,
}

/// Stroke draw order for time map playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrawOrder {
    /// Strokes appear in their original compositing order.
    #[default]
    Sequential,
    /// Strokes appear in a pseudo-random order.
    Random,
}

/// Playback loop mode for Drawing animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackMode {
    /// Loop continuously from start.
    #[default]
    Loop,
    /// Play forward then backward, repeat.
    PingPong,
    /// Play once and stop at the end.
    Once,
}

/// Guide editing tool (active only in the Guide tab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuideTool {
    #[default]
    Select, // 1 — click=select, drag center=move, drag handle=direction
    AddDirectional, // 2
    AddRadial,      // 3 — creates Source; toggle Inward in popup for Sink
    AddVortex,      // 4
}

/// Viewport pan/zoom state.
pub struct ViewportState {
    /// Pan offset in UV space (0,0 = top-left corner visible).
    pub offset: Vec2,
    /// Zoom level: pixels per UV unit. E.g., 512.0 means 1 UV = 512 screen pixels.
    pub zoom: f32,
    pub show_wireframe: bool,
    /// Path overlay palette index, or None to hide paths.
    pub path_overlay_idx: Option<usize>,
}

/// Predefined path overlay colors (RGB).
pub const PATH_PALETTE: &[[u8; 3]] = &[
    [80, 220, 255],  // cyan
    [255, 220, 80],  // gold
    [255, 100, 220], // pink
    [120, 255, 80],  // lime
    [220, 220, 220], // white
];

impl Default for ViewportState {
    fn default() -> Self {
        Self {
            offset: Vec2::ZERO,
            zoom: 512.0,
            show_wireframe: true,
            path_overlay_idx: Some(0),
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

/// A single layer's proposed mapping in the mesh load popup.
#[derive(Clone)]
pub struct LayerMapping {
    pub name: String,
    pub base_color: TextureSource,
    pub base_normal: TextureSource,
    /// Whether this is a fallback default (no material info).
    pub is_default: bool,
}

/// Pending mesh load popup — shown after new project / replace mesh.
/// Holds the proposed layer mappings for user confirmation.
pub struct MeshLoadPopup {
    /// Display file name.
    pub filename: String,
    /// Mesh info.
    pub vertices: usize,
    pub triangles: usize,
    pub groups: usize,
    /// Number of materials with color textures.
    pub n_textures: usize,
    /// Number of materials with normal textures.
    pub n_normals: usize,
    /// Whether MTL materials are available (OBJ only).
    pub has_mtl: bool,
    /// User toggle: use MTL materials or not.
    pub use_mtl: bool,
    /// Proposed layer mappings.
    pub mappings: Vec<LayerMapping>,
    /// Mappings when MTL is disabled (all default).
    pub mappings_no_mtl: Vec<LayerMapping>,
    /// Whether this is a replace-mesh operation (vs new project).
    pub is_replace: bool,
    /// Per-layer toggle: whether to create/apply this layer.
    pub layer_enabled: Vec<bool>,
    /// Loaded mesh, held here until user confirms (not yet applied to AppState).
    pub pending_mesh: LoadedMesh,
    /// Original file path.
    pub pending_path: PathBuf,
    /// File format string (e.g. "glb", "obj").
    pub pending_format: String,
    /// Raw mesh bytes for .pap embedding.
    pub pending_bytes: Option<Vec<u8>>,
}

/// Summary of a mesh reload diff, shown as a dismissible window.
pub struct ReloadSummary {
    pub kept: Vec<String>,
    pub added: Vec<String>,
    pub orphaned: Vec<String>,
}

/// Cache key for the group dim overlay in the Guide viewport.
#[derive(PartialEq)]
pub struct GroupDimKey {
    pub layer_idx: usize,
    pub group_name: String,
    pub mesh_group_count: usize,
}

/// Cached dim overlay texture that dims outside the selected layer's vertex group.
#[derive(Default)]
pub struct GroupDimCache {
    pub key: Option<GroupDimKey>,
    pub texture: Option<egui::TextureHandle>,
}

impl GroupDimCache {
    pub fn invalidate(&mut self) {
        self.key = None;
        self.texture = None;
    }
}

/// Central GUI application state.
pub struct AppState {
    // ── Project ──
    pub project: Project,
    pub project_path: Option<std::path::PathBuf>,
    pub dirty: bool,

    // ── Loaded Assets ──
    pub loaded_mesh: Option<Arc<practical_arcana_painter::asset_io::LoadedMesh>>,
    pub uv_edges: Option<Vec<(Vec2, Vec2)>>,
    /// Hash of loaded mesh geometry for generation staleness detection.
    pub mesh_hash: u64,

    // ── Viewport ──
    pub viewport: ViewportState,
    pub viewport_tab: ViewportTab,
    pub map_mode: MapMode,
    pub setup_map_mode: SetupMapMode,
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
    /// Cached mesh normal data for path overlay normal-break preview.
    /// Tuple of (resolution, data); invalidated on mesh reload or resolution change.
    pub cached_mesh_normals: Option<(
        u32,
        Arc<practical_arcana_painter::object_normal::MeshNormalData>,
    )>,

    // ── Generation ──
    pub generation: GenerationManager,
    pub generated: Option<GenResult>,

    // ── Path Overlay Worker ──
    pub path_worker: preview::PathOverlayWorker,

    // ── Deferred Actions (set by child widgets, consumed by PainterApp) ──
    pub pending_open: bool,
    pub pending_new: bool,
    pub pending_save: bool,
    pub pending_export: bool,
    pub pending_reload_mesh: bool,
    pub pending_replace_mesh: bool,
    /// Re-merge cached LayerMaps without re-rendering (visibility/order/dry change).
    pub pending_remerge: bool,

    /// Mesh reload diff summary shown as a dismissible window.
    pub reload_summary: Option<ReloadSummary>,

    /// Mesh load popup pending user confirmation.
    pub mesh_load_popup: Option<MeshLoadPopup>,

    // ── Group Dim Overlay ──
    pub group_dim_cache: GroupDimCache,

    // ── Setup tab base texture cache ──
    pub setup_base_tex: Option<egui::TextureHandle>,
    /// Cache key: (layer_idx, is_color, source_hash)
    pub setup_base_key: Option<(usize, bool, u64)>,

    // ── Status ──
    pub status_message: String,

    /// Hash of (layers, settings, mesh_hash) at last generation — used to detect outdated results.
    pub generation_snapshot: Option<u64>,

    // ── Auto-Preview ──
    /// Combined render hash from previous frame, for detecting Type C/D changes.
    pub prev_render_hash: u64,
    /// When a Type C/D parameter last changed (debounce timer start).
    pub auto_preview_timer: Option<Instant>,
    /// Suppresses auto-generation when true (set on generation failure to prevent retry loop).
    /// Reset when mesh or layers change.
    pub auto_gen_suppressed: bool,

    // ── Export Settings Panel ──
    pub show_export_settings: bool,

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
            uv_edges: None,
            mesh_hash: 0,
            viewport: ViewportState::default(),
            viewport_tab: ViewportTab::Setup,
            map_mode: MapMode::Color,
            setup_map_mode: SetupMapMode::default(),
            guide_tool: GuideTool::default(),
            mesh_preview: MeshPreviewState::default(),
            selected_layer: None,
            selected_guide: None,
            guide_drag: None,
            textures: DisplayTextures::default(),
            preview_cache: PreviewCache::default(),
            path_overlay: preview::PathOverlayCache::default(),
            preset_thumbnails: preview::PresetThumbnailCache::default(),
            cached_mesh_normals: None,
            generation: GenerationManager::default(),
            generated: None,
            path_worker: preview::PathOverlayWorker::default(),
            pending_open: false,
            pending_new: false,
            pending_save: false,
            pending_export: false,
            pending_reload_mesh: false,
            pending_replace_mesh: false,
            pending_remerge: false,
            reload_summary: None,
            mesh_load_popup: None,
            group_dim_cache: GroupDimCache::default(),
            setup_base_tex: None,
            setup_base_key: None,
            status_message: "Ready".to_string(),
            generation_snapshot: None,
            prev_render_hash: 0,
            auto_preview_timer: None,
            auto_gen_suppressed: false,
            show_export_settings: false,
            undo: UndoHistory::default(),
        }
    }

    /// Capture current undoable state as a snapshot.
    pub fn take_snapshot(&self) -> UndoSnapshot {
        UndoSnapshot {
            layers: self.project.layers.clone(),
            settings: self.project.settings.clone(),
            presets: self.project.presets.clone(),
        }
    }

    /// Restore project state from an undo snapshot.
    pub fn apply_snapshot(&mut self, snap: UndoSnapshot) {
        self.project.layers = snap.layers;
        self.project.settings = snap.settings;
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
        // Fix selected_guide if the layer now has fewer guides
        if let Some(gi) = self.selected_guide {
            let valid = self
                .selected_layer
                .and_then(|li| self.project.layers.get(li))
                .is_some_and(|layer| gi < layer.guides.len());
            if !valid {
                self.selected_guide = None;
            }
        }

        // Undo/redo may restore Type A settings (visibility, dry, normal_strength, etc.)
        // that only affect merge — trigger remerge to cover those cases.
        // Type C/D changes are handled by combined_render_hash detection in update().
        self.pending_remerge = true;
    }

    /// Whether current results don't reflect current settings.
    /// Returns a label describing the state, or None if up-to-date.
    pub fn stale_reason(&self) -> Option<&'static str> {
        let Some(saved) = self.generation_snapshot else {
            // No generation snapshot yet — auto-preview will handle it
            return None;
        };
        let current =
            generation_state_hash(&self.project.layers, &self.project.settings, self.mesh_hash);
        if saved != current {
            return Some("Outdated");
        }
        None
    }
}

/// Compute a deterministic hash of project state relevant to generation output.
/// Used to detect whether the output is stale without storing a full copy.
pub fn generation_state_hash(layers: &[Layer], settings: &OutputSettings, mesh_hash: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Ok(bytes) = serde_json::to_vec(&(layers, settings)) {
        bytes.hash(&mut hasher);
    }
    mesh_hash.hash(&mut hasher);
    hasher.finish()
}
