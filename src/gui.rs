mod dialogs;
pub mod file_actions;
mod frame_update;
pub mod generation;
mod gpu_sync;
pub mod guide_editor;
pub mod mesh_preview;
mod panels;
mod pipeline;
pub mod preview;
pub mod recent_files;
pub mod sidebar;
pub mod slot_editor;
pub mod state;
pub mod textures;
pub mod undo;
pub mod update_check;
pub mod viewport;
pub mod widgets;

use eframe::egui;
use eframe::egui_wgpu;
use state::{AppState, UnsavedAction};

/// Main GUI application.
pub struct PainterApp {
    pub(super) state: AppState,
    pub(super) checkerboard: Option<egui::TextureHandle>,
    pub(super) render_state: Option<egui_wgpu::RenderState>,
    /// Previous frame's result_mode for toggle change detection.
    pub(super) prev_result_mode: state::ResultMode,
    /// Previous frame's draw_order for change detection (re-upload time texture).
    pub(super) prev_draw_order: state::DrawOrder,
    pub(super) prev_chunk_size: u32,
    /// Hash of base texture state for 3D preview invalidation when show_result is off.
    pub(super) prev_base_tex_hash: u64,
    /// Background remerge worker.
    pub(super) remerge_worker: generation::RemergeWorker,
    /// Previous show_direction_field state for toggle change detection.
    pub(super) prev_show_direction_field: bool,
    /// Hash of guide state for direction field overlay invalidation.
    pub(super) prev_direction_field_hash: u64,
    /// Background update checker.
    pub(super) update_checker: update_check::UpdateChecker,
    /// Whether the user dismissed the update banner.
    pub(super) update_dismissed: bool,
    /// Recent project entries.
    pub(super) recent_files: Vec<recent_files::RecentEntry>,
    /// Path to open from recent files list (deferred to next frame).
    pub(super) pending_open_recent: Option<std::path::PathBuf>,
    /// Alert message shown as a modal dialog (dismissed with OK).
    pub(super) alert_message: Option<String>,
}

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Register Phosphor icon font
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Fill);
        cc.egui_ctx.set_fonts(fonts);

        Self {
            state: AppState::new(),
            checkerboard: None,
            render_state: cc.wgpu_render_state.clone(),
            prev_result_mode: state::ResultMode::Paint,
            prev_draw_order: state::DrawOrder::Sequential,
            prev_chunk_size: 1,
            prev_base_tex_hash: 0,
            remerge_worker: generation::RemergeWorker::default(),
            prev_show_direction_field: false,
            prev_direction_field_hash: 0,
            update_checker: update_check::UpdateChecker::spawn(),
            update_dismissed: false,
            recent_files: recent_files::load(),
            pending_open_recent: None,
            alert_message: None,
        }
    }
}

impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept window close when there are unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && self.state.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.state.unsaved_confirm = Some(UnsavedAction::Quit);
        }

        // On macOS, rfd dialogs pump the event loop and can re-enter update().
        // Absorb all input so the UI renders (keeping egui state consistent)
        // but nothing is interactive.
        if self.state.modal_dialog_active {
            egui::Area::new(egui::Id::new("modal_input_blocker"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Foreground)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let size = ctx.content_rect().size();
                    ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                });
        }

        // Repaint while the pointer is over the window so hover reactions are instant.
        if ctx.input(|i| i.pointer.has_pointer()) {
            ctx.request_repaint();
        }
        self.init_lazy(ctx);
        self.handle_keyboard(ctx);
        self.handle_file_drop(ctx);

        // Capture pre-frame snapshot AFTER undo/redo so the restore itself
        // is invisible to the change tracker.
        let pre_frame = self.state.take_snapshot();
        // Project-replacing actions explicitly set dirty=false; skip auto-dirty for those frames.
        let project_replacing = self.state.pending_open
            || self.state.pending_new
            || self.state.pending_open_example
            || self.pending_open_recent.is_some()
            || self.state.project_load_worker.is_active();

        self.dispatch_deferred(ctx);
        self.sync_gpu_textures();
        self.poll_workers(ctx);

        // ── UI panels (order matters for egui layout) ──
        self.show_menu_bar(ctx);
        self.show_update_banner(ctx);
        self.show_status_bar(ctx);
        self.show_sidebars(ctx);
        self.show_central_panel(ctx);
        self.show_mesh_load_popup(ctx);
        self.show_auxiliary_windows(ctx);

        // Drag-and-drop overlay
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to open",
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }

        // Project loading overlay
        if self.state.project_load_worker.is_active() {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("project_load_overlay"),
            ));
            painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Loading...",
                egui::FontId::proportional(20.0),
                egui::Color32::WHITE,
            );
        }

        self.auto_preview_tick(ctx, project_replacing);

        // ── Undo: track post-frame changes ──
        let post_frame = self.state.take_snapshot();
        if pre_frame != post_frame && !project_replacing {
            self.state.dirty = true;
        }
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.state
            .undo
            .track_frame(&pre_frame, &post_frame, pointer_down);
    }
}

// ── Helpers that stay in mod.rs ──

impl PainterApp {
    /// One-time lazy initialization (checkerboard texture, etc.).
    fn init_lazy(&mut self, ctx: &egui::Context) {
        ctx.style_mut(|s| s.spacing.scroll.dormant_handle_opacity = 0.4);
        if self.checkerboard.is_none() {
            self.checkerboard = Some(viewport::make_checkerboard(ctx));
        }
        if self.state.textures.base_texture.is_none() {
            self.state.textures.base_texture = self.checkerboard.clone();
        }
    }

    /// Post-load housekeeping shared by all project open paths.
    fn after_project_loaded(&mut self) {
        if let Some(ref path) = self.state.project_path {
            self.recent_files = recent_files::push(path);
        }
        self.state.cached_mesh_normals = None;
        self.state.path_worker.discard();
        self.state.group_dim_cache.invalidate();
        // Older projects lack model_transform in editor.json — if it
        // deserialized as identity, recompute from mesh bounds.
        let needs_recompute = self.state.mesh_preview.model_transform == glam::Mat4::IDENTITY;
        if needs_recompute {
            if let Some(ref mesh) = self.state.loaded_mesh {
                self.state.mesh_preview.recompute_model_transform(mesh);
            }
        }
        self.init_mesh_preview_no_fit();
    }

    /// Execute Open Project: show file dialog, then start background load.
    fn do_open_project(&mut self, _ctx: &egui::Context) {
        if let Some(path) = file_actions::pick_project_path(&mut self.state) {
            self.state.status_message = format!("Opening {}…", path.display());
            self.state
                .project_load_worker
                .start(state::ProjectLoadSource::Open(path));
        }
    }

    /// Execute New Project (file dialog + mesh load).
    fn do_new_project(&mut self, ctx: &egui::Context) {
        file_actions::new_project(&mut self.state, ctx);
    }
}
