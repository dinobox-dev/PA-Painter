pub mod dialogs;
pub mod generation;
pub mod guide_editor;
pub mod mesh_preview;
pub mod preview;
pub mod sidebar;
pub mod slot_editor;
pub mod state;
pub mod textures;
pub mod undo;
pub mod viewport;

use eframe::egui;
use eframe::egui_wgpu;
use state::{AppState, GuideTool, MapMode, ViewportTab};

use practical_arcana_painter::object_normal::compute_mesh_normal_data;
use practical_arcana_painter::stroke_color::ColorTextureRef;
use practical_arcana_painter::types::{Color, NormalMode};

/// Main GUI application.
pub struct PainterApp {
    state: AppState,
    checkerboard: Option<egui::TextureHandle>,
    render_state: Option<egui_wgpu::RenderState>,
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
        }
    }

    fn start_generation(&mut self) {
        if self.state.generation.is_running() {
            self.state.status_message = "Generation already running".to_string();
            return;
        }

        let resolution = self.state.project.settings.resolution_preset.resolution();
        let layers = self.state.project.paint_layers();
        let settings = self.state.project.settings.clone();

        // Base color (clone from cached conversion)
        let base_colors = self.state.cached_texture_colors.clone();
        let (base_w, base_h) = self
            .state
            .loaded_texture
            .as_ref()
            .map(|t| (t.width, t.height))
            .unwrap_or((0, 0));
        let sc = self.state.project.base_color.solid_color();
        let solid_color = Color::from(sc);

        // Normal data (computed on main thread — brief freeze at high res)
        let normal_data = if settings.normal_mode == NormalMode::DepictedForm {
            self.state
                .loaded_mesh
                .as_ref()
                .map(|mesh| compute_mesh_normal_data(mesh, resolution))
        } else {
            None
        };

        // UV masks from mesh groups
        let masks = if let Some(ref mesh) = self.state.loaded_mesh {
            self.state.project.build_masks(mesh, resolution)
        } else {
            (0..self.state.project.layers.iter().filter(|l| l.visible).count()).map(|_| None).collect()
        };

        // Base normal pixels (for UDN blending)
        let (base_normal_pixels, base_normal_w, base_normal_h) = self
            .state
            .loaded_normal
            .as_ref()
            .map(|tex| (Some(tex.pixels.clone()), tex.width, tex.height))
            .unwrap_or((None, 0, 0));

        self.state.generation.start(generation::GenInput {
            layers,
            resolution,
            base_colors,
            base_w,
            base_h,
            solid_color,
            settings,
            normal_data,
            masks,
            base_normal_pixels,
            base_normal_w,
            base_normal_h,
        });
        self.state.generation_snapshot = Some((
            self.state.project.layers.clone(),
            self.state.project.settings.clone(),
            self.state.texture_colors_hash,
            self.state.normal_tex_hash,
        ));
        self.state.status_message = format!("Generating at {}px...", resolution);
    }

    fn apply_generation_result(&mut self, ctx: &egui::Context, result: generation::GenResult) {
        let r = result.resolution;
        self.state.textures.color = Some(textures::color_buffer_to_handle(
            ctx,
            &result.color,
            r,
            r,
            "gen_color",
        ));
        self.state.textures.height =
            Some(textures::height_buffer_to_handle(ctx, &result.height, r, "gen_height"));
        self.state.textures.normal = Some(textures::normal_map_to_handle(
            ctx,
            &result.normal_map,
            r,
            "gen_normal",
        ));
        self.state.textures.stroke_id = Some(textures::stroke_id_to_handle(
            ctx,
            &result.stroke_id,
            r,
            "gen_stroke_id",
        ));

        // Upload color texture to 3D preview
        if let Some(ref rs) = self.render_state {
            if self.state.mesh_preview.gpu_ready {
                mesh_preview::upload_color_texture(rs, &result.color, r as usize);
            }
        }

        self.state.status_message = format!("Generated in {:.1}s", result.elapsed.as_secs_f32());
        self.state.generated = Some(result);
    }

    /// Initialize or update 3D preview GPU resources after mesh load.
    fn init_mesh_preview(&mut self) {
        let Some(ref rs) = self.render_state else {
            return;
        };
        let Some(ref mesh) = self.state.loaded_mesh else {
            return;
        };

        if !self.state.mesh_preview.gpu_ready {
            mesh_preview::init_gpu_resources(rs, mesh);
            self.state.mesh_preview.gpu_ready = true;
        } else {
            mesh_preview::upload_mesh(rs, mesh);
        }
        self.state.mesh_preview.fit_to_mesh(mesh);

        // If we already have generated color data, upload it
        if let Some(ref gen) = self.state.generated {
            mesh_preview::upload_color_texture(rs, &gen.color, gen.resolution as usize);
        }
    }
}

impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Thin scrollbar always visible even when not hovering
        ctx.style_mut(|s| s.spacing.scroll.dormant_handle_opacity = 0.4);

        // Lazy-init checkerboard
        if self.checkerboard.is_none() {
            self.checkerboard = Some(viewport::make_checkerboard(ctx));
        }
        if self.state.textures.base_texture.is_none() {
            self.state.textures.base_texture = self.checkerboard.clone();
        }

        // ── Undo/Redo keyboard shortcuts ──
        // Check redo first (more specific modifier combo) to prevent Cmd+Z from consuming it.
        let redo_mods = egui::Modifiers { command: true, shift: true, ..Default::default() };
        let undo_mods = egui::Modifiers { command: true, ..Default::default() };
        if ctx.input_mut(|i| i.consume_key(redo_mods, egui::Key::Z)) {
            let current = self.state.take_snapshot();
            if let Some(snap) = self.state.undo.redo(current) {
                self.state.apply_snapshot(snap);
            }
        } else if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::Z)) {
            let current = self.state.take_snapshot();
            if let Some(snap) = self.state.undo.undo(current) {
                self.state.apply_snapshot(snap);
            }
        }

        // ── Cmd+S: Save ──
        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::S)) {
            self.state.pending_save = true;
        }

        // ── Cmd+G: Generate ──
        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::G)) {
            if !self.state.generation.is_running() {
                self.state.pending_generate = true;
            }
        }

        // ── Backtick key: cycle viewport tabs ──
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backtick)) {
            self.state.viewport_tab = self.state.viewport_tab.next();
        }

        // ── Number keys 1-4: context-dependent (no text focus) ──
        {
            let has_text_focus = ctx.memory(|m| m.focused().is_some());
            if !has_text_focus {
                match self.state.viewport_tab {
                    ViewportTab::Guide => {
                        let tool_keys = [
                            (egui::Key::Num1, GuideTool::Select),
                            (egui::Key::Num2, GuideTool::AddDirectional),
                            (egui::Key::Num3, GuideTool::AddRadial),
                            (egui::Key::Num4, GuideTool::AddVortex),
                        ];
                        for (key, tool) in &tool_keys {
                            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key)) {
                                if self.state.guide_tool != *tool {
                                    if *tool != GuideTool::Select {
                                        self.state.selected_guide = None;
                                    }
                                    self.state.guide_tool = *tool;
                                }
                                break;
                            }
                        }
                    }
                    ViewportTab::UvView => {
                        let map_keys = [
                            (egui::Key::Num1, MapMode::Color),
                            (egui::Key::Num2, MapMode::Height),
                            (egui::Key::Num3, MapMode::Normal),
                            (egui::Key::Num4, MapMode::StrokeId),
                        ];
                        for (key, mode) in &map_keys {
                            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key)) {
                                self.state.map_mode = *mode;
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Undo: capture pre-frame snapshot AFTER undo/redo ──
        // This way the restore itself is invisible to the change tracker.
        let pre_frame = self.state.take_snapshot();

        // Handle deferred actions (flags set by child widgets on AppState)
        if self.state.pending_open {
            self.state.pending_open = false;
            dialogs::open_project(&mut self.state, ctx);
            self.state.cached_mesh_normals = None;
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_save {
            self.state.pending_save = false;
            dialogs::save_project_action(&mut self.state);
        }
        if self.state.pending_export {
            self.state.pending_export = false;
            dialogs::export_maps(&mut self.state);
        }
        if self.state.pending_export_glb {
            self.state.pending_export_glb = false;
            dialogs::export_glb(&mut self.state);
        }
        if self.state.pending_generate {
            self.state.pending_generate = false;
            self.start_generation();
        }
        if self.state.pending_new {
            self.state.pending_new = false;
            dialogs::new_project(&mut self.state, ctx);
            self.state.cached_mesh_normals = None;
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_reload_mesh {
            self.state.pending_reload_mesh = false;
            dialogs::reload_mesh(&mut self.state);
            self.state.cached_mesh_normals = None;
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_replace_mesh {
            self.state.pending_replace_mesh = false;
            dialogs::replace_mesh(&mut self.state);
            self.state.cached_mesh_normals = None;
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_load_texture {
            self.state.pending_load_texture = false;
            dialogs::load_texture_dialog(&mut self.state, ctx);
        }
        if self.state.pending_reload_texture {
            self.state.pending_reload_texture = false;
            dialogs::reload_texture(&mut self.state, ctx);
        }
        if self.state.pending_load_normal {
            self.state.pending_load_normal = false;
            dialogs::load_normal_dialog(&mut self.state);
        }
        if self.state.pending_reload_normal {
            self.state.pending_reload_normal = false;
            dialogs::reload_normal(&mut self.state);
        }

        // Update per-layer path overlay caches (only recomputes stale layers)
        if self.state.viewport.path_overlay_idx.is_some() {
            let res = self.state.project.settings.resolution_preset.resolution();
            let seed = self.state.project.settings.seed;

            // Prepare color texture ref for color-break preview (uses cached conversion)
            let color_ref = match (&self.state.cached_texture_colors, &self.state.loaded_texture) {
                (Some(c), Some(tex)) => Some(ColorTextureRef {
                    data: c,
                    width: tex.width,
                    height: tex.height,
                }),
                _ => None,
            };

            // Prepare normal data for normal-break preview (lazily cached)
            let needs_normal = self
                .state
                .project
                .layers
                .iter()
                .any(|l| l.paint.normal_break_threshold.is_some());
            if needs_normal {
                let stale = match &self.state.cached_mesh_normals {
                    Some((cached_res, _)) => *cached_res != res,
                    None => true,
                };
                if stale {
                    if let Some(mesh) = self.state.loaded_mesh.as_ref() {
                        self.state.cached_mesh_normals =
                            Some((res, compute_mesh_normal_data(mesh, res)));
                    }
                }
            }
            let normal_data = self
                .state
                .cached_mesh_normals
                .as_ref()
                .map(|(_, nd)| nd);

            self.state.path_overlay.update(
                &self.state.project.layers,
                seed,
                res,
                self.state.selected_layer,
                color_ref.as_ref(),
                normal_data,
                self.state.texture_colors_hash,
            );
        }

        // Poll generation results
        if let Some(result) = self.state.generation.poll() {
            self.apply_generation_result(ctx, result);
        }
        // Keep repainting while generation is in progress
        if self.state.generation.is_running() {
            ctx.request_repaint();
        }

        // ── Top menu bar ──
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui: &mut egui::Ui| {
            egui::MenuBar::new().ui(ui, |ui: &mut egui::Ui| {
                ui.menu_button("File", |ui: &mut egui::Ui| {
                    if ui.button("New Project...").clicked() {
                        ui.close();
                        self.state.pending_new = true;
                    }
                    if ui.button("Open Project...").clicked() {
                        ui.close();
                        self.state.pending_open = true;
                    }
                    if ui.button("Save  ⌘S").clicked() {
                        ui.close();
                        self.state.pending_save = true;
                    }
                    ui.separator();
                    if ui.button("Export Maps...").clicked() {
                        ui.close();
                        self.state.pending_export = true;
                    }
                    if ui.button("Export GLB...").clicked() {
                        ui.close();
                        self.state.pending_export_glb = true;
                    }
                });
                ui.menu_button("Edit", |ui: &mut egui::Ui| {
                    let can_undo = self.state.undo.can_undo();
                    let can_redo = self.state.undo.can_redo();
                    if ui.add_enabled(can_undo, egui::Button::new("Undo  ⌘Z")).clicked() {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.undo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                    if ui.add_enabled(can_redo, egui::Button::new("Redo  ⌘⇧Z")).clicked() {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.redo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                });
                ui.menu_button("View", |ui: &mut egui::Ui| {
                    ui.checkbox(&mut self.state.viewport.show_wireframe, "UV Wireframe");
                    let mut paths_on = self.state.viewport.path_overlay_idx.is_some();
                    if ui.checkbox(&mut paths_on, "Path Overlay").changed() {
                        self.state.viewport.path_overlay_idx =
                            if paths_on { Some(0) } else { None };
                    }
                });

            });
        });

        // ── Bottom status bar ──
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .show(ctx, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.label(&self.state.status_message);
                    ui.separator();
                    let res = self.state.project.settings.resolution_preset.resolution();
                    ui.label(format!("{}px", res));
                    ui.separator();
                    ui.label(format!("{} layers", self.state.project.layers.len()));
                });
            });

        // ── Left sidebar ──
        egui::SidePanel::left("left_panel")
            .default_width(260.0)
            .min_width(220.0)
            .max_width(400.0)
            .show(ctx, |ui: &mut egui::Ui| {
                // Bottom-pinned: Seed + Generate
                egui::TopBottomPanel::bottom("left_bottom")
                    .show_inside(ui, |ui: &mut egui::Ui| {
                        sidebar::show_bottom(ui, &mut self.state);
                    });
                // Fixed top: Base + Project Settings + Layers header
                sidebar::show_top(ui, &mut self.state);
                sidebar::show_layers_header(ui, &mut self.state);
                // Scrollable layer rows
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    sidebar::show_layer_rows(ui, &mut self.state);
                });
            });

        // ── Right sidebar (layer editor, only when a layer is selected) ──
        if self.state.selected_layer.is_some() {
            egui::SidePanel::right("right_panel")
                .default_width(280.0)
                .min_width(240.0)
                .max_width(420.0)
                .show(ctx, |ui: &mut egui::Ui| {
                    egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                        slot_editor::show(ui, &mut self.state);
                    });
                });
        }

        // ── Window title ──
        let title = if let Some(ref path) = self.state.project_path {
            let name = path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let dirty = if self.state.dirty { " *" } else { "" };
            format!("Practical Arcana Painter — {}{}", name, dirty)
        } else {
            "Practical Arcana Painter".to_string()
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        // ── Central viewport (or welcome screen) ──
        let render_state = self.render_state.clone();
        egui::CentralPanel::default().show(ctx, |ui: &mut egui::Ui| {
            let has_project =
                self.state.loaded_mesh.is_some() || self.state.project_path.is_some();

            if has_project {
                viewport::show(ui, &mut self.state, render_state.as_ref());
            } else {
                // Welcome screen
                ui.vertical_centered(|ui: &mut egui::Ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.heading("Practical Arcana Painter");
                    ui.add_space(16.0);
                    ui.label("Generate painterly texture maps from 3D meshes.");
                    ui.add_space(24.0);
                    if ui
                        .add(
                            egui::Button::new("Open Project...")
                                .min_size(egui::Vec2::new(200.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.state.pending_open = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new("New Project")
                                .min_size(egui::Vec2::new(200.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.state.pending_new = true;
                    }
                });
            }
        });

        // ── Reload Summary Window ──
        let mut dismiss_summary = false;
        if let Some(ref summary) = self.state.reload_summary {
            egui::Window::new("Mesh Reload Summary")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    if !summary.kept.is_empty() {
                        ui.label(format!("Kept: {}", summary.kept.join(", ")));
                    }
                    if !summary.added.is_empty() {
                        ui.label(format!(
                            "Added (new layers created): {}",
                            summary.added.join(", ")
                        ));
                    }
                    if !summary.orphaned.is_empty() {
                        ui.label(format!(
                            "Orphaned (remapped to __all__): {}",
                            summary.orphaned.join(", ")
                        ));
                    }
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        dismiss_summary = true;
                    }
                });
        }
        if dismiss_summary {
            self.state.reload_summary = None;
        }

        // ── Escape: deselect guide + return to Select tool ──
        // Runs after panels so popup consume_key takes priority.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.state.selected_guide = None;
            self.state.guide_tool = GuideTool::Select;
        }

        // ── Undo: track post-frame changes ──
        let post_frame = self.state.take_snapshot();
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.state.undo.track_frame(&pre_frame, &post_frame, pointer_down);
    }
}
