use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use eframe::egui;
use glam::{Vec2, Vec3};

use pa_painter::mesh::asset_io::{LoadedMesh, ObjAuxFiles};
use pa_painter::project::{LoadResult, Project};
use pa_painter::types::{ExportSettings, Layer, OutputSettings, TextureSource};

use super::generation::{GenResult, GenerationManager};
use super::mesh_preview::MeshPreviewState;
use super::preview;
use super::preview::PreviewCache;
use super::undo::{UndoHistory, UndoSnapshot};

// ── Export Worker ──────────────────────────────────────────────────

/// Which action triggered the unsaved-changes confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedAction {
    Open,
    OpenExample,
    New,
    Quit,
}

/// Pending overwrite confirmation shown as an egui window.
pub struct ExportOverwriteConfirm {
    pub dir: PathBuf,
    pub include_glb: bool,
    pub conflict_count: usize,
    pub folder_name: String,
}

/// Background export worker — runs file I/O off the main thread.
pub struct ExportWorker {
    handle: Option<thread::JoinHandle<Result<(u32, PathBuf), String>>>,
    /// Current step (numerator) for progress display.
    pub current_step: Arc<AtomicU32>,
    /// Total steps (denominator) for progress display.
    pub total_steps: Arc<AtomicU32>,
}

impl Default for ExportWorker {
    fn default() -> Self {
        Self {
            handle: None,
            current_step: Arc::new(AtomicU32::new(0)),
            total_steps: Arc::new(AtomicU32::new(1)),
        }
    }
}

impl ExportWorker {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Progress fraction 0.0–1.0.
    pub fn progress(&self) -> f32 {
        let cur = self.current_step.load(Ordering::Relaxed) as f32;
        let tot = self.total_steps.load(Ordering::Relaxed).max(1) as f32;
        (cur / tot).clamp(0.0, 1.0)
    }

    /// (current_step, total_steps) for display.
    pub fn steps(&self) -> (u32, u32) {
        (
            self.current_step.load(Ordering::Relaxed),
            self.total_steps.load(Ordering::Relaxed).max(1),
        )
    }

    /// Poll for completion. Returns `Some(Ok((count, dir)))` or `Some(Err(msg))`.
    pub fn poll(&mut self) -> Option<Result<(u32, PathBuf), String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            match self.handle.take().unwrap().join() {
                Ok(result) => Some(result),
                Err(e) => {
                    let msg = e
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown error");
                    Some(Err(format!("Export thread panicked: {msg}")))
                }
            }
        } else {
            None
        }
    }

    /// Start a background export.
    pub fn start<F>(&mut self, total: u32, work: F)
    where
        F: FnOnce(Arc<AtomicU32>) -> Result<(u32, PathBuf), String> + Send + 'static,
    {
        self.current_step = Arc::new(AtomicU32::new(0));
        self.total_steps = Arc::new(AtomicU32::new(total));
        let step = Arc::clone(&self.current_step);
        self.handle = Some(thread::spawn(move || work(step)));
    }
}

// ── Project Load Worker ──────────────────────────────────────────

/// Source of a project load (determines post-load behavior).
#[derive(Debug, Clone)]
pub enum ProjectLoadSource {
    /// File > Open (dialog already picked the path).
    Open(PathBuf),
    /// Recent file or drag-and-drop.
    Recent(PathBuf),
    /// Built-in example project.
    Example,
}

/// Background worker for loading .papr project files.
#[derive(Default)]
pub struct ProjectLoadWorker {
    handle: Option<thread::JoinHandle<Result<(LoadResult, ProjectLoadSource), String>>>,
}

impl ProjectLoadWorker {
    /// True while a load is in-flight (running or finished-but-not-yet-polled).
    pub fn is_active(&self) -> bool {
        self.handle.is_some()
    }

    pub fn poll(&mut self) -> Option<Result<(LoadResult, ProjectLoadSource), String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            match self.handle.take().unwrap().join() {
                Ok(result) => Some(result),
                Err(e) => {
                    let msg = e
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown error");
                    Some(Err(format!("Project load thread panicked: {msg}")))
                }
            }
        } else {
            None
        }
    }

    pub fn start(&mut self, source: ProjectLoadSource) {
        use pa_painter::project::load_project;

        self.handle = Some(thread::spawn(move || {
            let path = match &source {
                ProjectLoadSource::Open(p) | ProjectLoadSource::Recent(p) => p.clone(),
                ProjectLoadSource::Example => {
                    let tmp = std::env::temp_dir().join("pa_painter_example.papr");
                    let example_bytes: &[u8] = include_bytes!("../../examples/PAPainterLogo.papr");
                    std::fs::write(&tmp, example_bytes)
                        .map_err(|e| format!("Failed to write example: {e}"))?;
                    tmp
                }
            };
            let result =
                load_project(&path).map_err(|e| format!("Failed to load project: {e:?}"))?;
            // Clean up temp file for example
            if matches!(source, ProjectLoadSource::Example) {
                let _ = std::fs::remove_file(&path);
            }
            Ok((result, source))
        }));
    }
}

/// What part of a guide is being dragged.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy)]
pub enum DragTarget {
    GuidePosition(usize),
    GuideDirection(usize),
    GuideInfluence(usize),
}

/// Which map to display in the UV View tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MapMode {
    Color,
    Height,
    Normal,
    StrokeId,
}

/// Which base texture to display in the Setup tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SetupMapMode {
    #[default]
    Color,
    Normal,
}

/// Top-level viewport tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum OrbitTarget {
    #[default]
    Object,
    Light,
}

/// What to display on the 3D mesh surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum DrawOrder {
    /// Strokes appear in their original compositing order.
    #[default]
    Sequential,
    /// Strokes appear in a pseudo-random order.
    Random,
}

/// Playback loop mode for Drawing animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

// ── Persisted Editor State ────────────────────────────────────────

/// Editor UI state that is saved to `editor.json` inside the `.papr` ZIP.
/// All fields use `#[serde(default)]` so older files missing any field
/// gracefully fall back to defaults.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct EditorState {
    // ── Camera ──
    pub camera_yaw: f32,
    pub camera_pitch: f32,
    pub camera_distance: f32,
    pub camera_center: [f32; 3],

    // ── Lighting ──
    pub ambient: f32,
    pub light_yaw: f32,
    pub light_pitch: f32,
    pub orbit_target: OrbitTarget,

    // ── UV Viewport ──
    pub viewport_offset: [f32; 2],
    pub viewport_zoom: f32,
    pub show_wireframe: bool,
    pub path_overlay_idx: Option<usize>,

    // ── Tabs & Display ──
    pub viewport_tab: ViewportTab,
    pub map_mode: MapMode,
    pub setup_map_mode: SetupMapMode,
    pub guide_tool: GuideTool,
    pub selected_layer: Option<usize>,

    // ── 3D Display ──
    pub model_transform: [[f32; 4]; 4],
    pub result_mode: ResultMode,
    pub show_direction_field: bool,

    // ── Playback ──
    pub playback_speed: f32,
    pub draw_time: f32,
    pub gap: f32,
    pub chunk_size: u32,
    pub draw_order: DrawOrder,
    pub playback_mode: PlaybackMode,
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            camera_yaw: 0.5,
            camera_pitch: 0.3,
            camera_distance: 3.0,
            camera_center: [0.0; 3],
            ambient: 0.15,
            light_yaw: 0.8,
            light_pitch: 0.5,
            orbit_target: OrbitTarget::default(),
            viewport_offset: [0.0; 2],
            viewport_zoom: 512.0,
            show_wireframe: true,
            path_overlay_idx: Some(0),
            viewport_tab: ViewportTab::Setup,
            map_mode: MapMode::Color,
            setup_map_mode: SetupMapMode::default(),
            guide_tool: GuideTool::default(),
            selected_layer: None,
            model_transform: glam::Mat4::IDENTITY.to_cols_array_2d(),
            result_mode: ResultMode::default(),
            show_direction_field: false,
            playback_speed: 1.0,
            draw_time: 0.3,
            gap: 0.0,
            chunk_size: 1,
            draw_order: DrawOrder::default(),
            playback_mode: PlaybackMode::default(),
        }
    }
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

/// Cached pre-tessellated wireframe mesh. Rebuilt only when viewport transform changes.
#[derive(Default)]
pub struct WireframeCache {
    pub mesh: Option<egui::Mesh>,
    /// Cache key: (offset_x, offset_y, zoom, rect_width, rect_height)
    pub key: (i32, i32, i32, i32, i32),
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
    /// Raw mesh bytes for .papr embedding.
    pub pending_bytes: Option<Vec<u8>>,
    /// OBJ auxiliary files (MTL + textures) for .papr embedding.
    pub pending_obj_aux: Option<ObjAuxFiles>,
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
    pub loaded_mesh: Option<Arc<pa_painter::mesh::asset_io::LoadedMesh>>,
    pub uv_edges: Option<Vec<(Vec2, Vec2)>>,
    /// Hash of loaded mesh geometry for generation staleness detection.
    pub mesh_hash: u64,

    // ── Viewport ──
    pub wireframe_cache: WireframeCache,
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
    pub cached_mesh_normals: Option<(u32, Arc<pa_painter::mesh::object_normal::MeshNormalData>)>,

    // ── Generation ──
    pub generation: GenerationManager,
    pub generated: Option<GenResult>,

    // ── Path Overlay Worker ──
    pub path_worker: preview::PathOverlayWorker,

    // ── Deferred Actions (set by child widgets, consumed by PainterApp) ──
    pub pending_open: bool,
    pub pending_open_example: bool,
    pub pending_new: bool,
    pub pending_save: bool,
    pub pending_export: bool,
    pub pending_reload_mesh: bool,
    pub pending_replace_mesh: bool,
    /// Mesh path from drag-and-drop (skips file dialog in new_project).
    pub pending_drop_mesh: Option<std::path::PathBuf>,
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
    /// Temporary copy of export settings being edited in the dialog.
    pub export_settings_draft: Option<ExportSettings>,

    // ── Background Export ──
    pub export_worker: ExportWorker,
    // ── Background Project Load ──
    pub project_load_worker: ProjectLoadWorker,
    /// Pending overwrite confirmation (egui window, replaces rfd::MessageDialog).
    pub export_overwrite_confirm: Option<ExportOverwriteConfirm>,
    /// Pending unsaved-changes confirmation before Open/New.
    pub unsaved_confirm: Option<UnsavedAction>,
    /// True while a native file dialog is open (rfd).
    /// Guards against re-entrant UI actions on macOS where rfd pumps the event loop.
    pub modal_dialog_active: bool,

    // ── Remerge status (synced from PainterApp each frame) ──
    pub remerge_running: bool,
    pub remerge_progress: f32,

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
            wireframe_cache: WireframeCache::default(),
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
            pending_open_example: false,
            pending_new: false,
            pending_save: false,
            pending_export: false,
            pending_reload_mesh: false,
            pending_replace_mesh: false,
            pending_drop_mesh: None,
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
            export_settings_draft: None,
            export_worker: ExportWorker::default(),
            project_load_worker: ProjectLoadWorker::default(),
            export_overwrite_confirm: None,
            unsaved_confirm: None,
            modal_dialog_active: false,
            remerge_running: false,
            remerge_progress: 0.0,
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

    /// Extract current editor state for persistence.
    pub fn extract_editor_state(&self) -> EditorState {
        let mp = &self.mesh_preview;
        EditorState {
            camera_yaw: mp.yaw,
            camera_pitch: mp.pitch,
            camera_distance: mp.distance,
            camera_center: mp.center.to_array(),
            ambient: mp.ambient,
            light_yaw: mp.light_yaw,
            light_pitch: mp.light_pitch,
            orbit_target: mp.orbit_target,
            viewport_offset: self.viewport.offset.to_array(),
            viewport_zoom: self.viewport.zoom,
            show_wireframe: self.viewport.show_wireframe,
            path_overlay_idx: self.viewport.path_overlay_idx,
            viewport_tab: self.viewport_tab,
            map_mode: self.map_mode,
            setup_map_mode: self.setup_map_mode,
            guide_tool: self.guide_tool,
            selected_layer: self.selected_layer,
            model_transform: mp.model_transform.to_cols_array_2d(),
            result_mode: mp.result_mode,
            show_direction_field: mp.show_direction_field,
            playback_speed: mp.speed,
            draw_time: mp.draw_time,
            gap: mp.gap,
            chunk_size: mp.chunk_size,
            draw_order: mp.draw_order,
            playback_mode: mp.playback_mode,
        }
    }

    /// Apply a loaded editor state, clamping selection indices to valid ranges.
    pub fn apply_editor_state(&mut self, es: EditorState) {
        let mp = &mut self.mesh_preview;
        mp.yaw = es.camera_yaw;
        mp.pitch = es.camera_pitch;
        mp.distance = es.camera_distance;
        mp.center = Vec3::from_array(es.camera_center);
        mp.ambient = es.ambient;
        mp.light_yaw = es.light_yaw;
        mp.light_pitch = es.light_pitch;
        mp.orbit_target = es.orbit_target;
        mp.model_transform = glam::Mat4::from_cols_array_2d(&es.model_transform);
        mp.result_mode = es.result_mode;
        mp.show_direction_field = es.show_direction_field;
        mp.speed = es.playback_speed;
        mp.draw_time = es.draw_time;
        mp.gap = es.gap;
        mp.chunk_size = es.chunk_size;
        mp.draw_order = es.draw_order;
        mp.playback_mode = es.playback_mode;

        self.viewport.offset = Vec2::from_array(es.viewport_offset);
        self.viewport.zoom = es.viewport_zoom;
        self.viewport.show_wireframe = es.show_wireframe;
        self.viewport.path_overlay_idx = es.path_overlay_idx;

        self.viewport_tab = es.viewport_tab;
        self.map_mode = es.map_mode;
        self.setup_map_mode = es.setup_map_mode;
        self.guide_tool = es.guide_tool;

        // Clamp selected_layer to valid range
        self.selected_layer = es.selected_layer.and_then(|idx| {
            if idx < self.project.layers.len() {
                Some(idx)
            } else if !self.project.layers.is_empty() {
                Some(self.project.layers.len() - 1)
            } else {
                None
            }
        });
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
