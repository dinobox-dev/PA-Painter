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
pub mod widgets;

use eframe::egui;
use eframe::egui_wgpu;
use state::{AppState, GuideTool};

use practical_arcana_painter::types::{TextureSource, BASE_RESOLUTION};

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

        // Show feedback immediately — no heavy work before this point.
        self.state.status_message = format!("Generating at {}px...", resolution);
        self.state.generation.start_time = Some(std::time::Instant::now());

        let layers = self.state.project.paint_layers();
        let settings = self.state.project.settings.clone();

        let mesh = self.state.loaded_mesh.clone(); // Arc clone
        let cached_normals = self.state.cached_mesh_normals.clone(); // (u32, Arc) clone

        // Resolve per-layer base color and normal from TextureSource.
        let materials: &[_] = mesh
            .as_ref()
            .map(|m| m.materials.as_slice())
            .unwrap_or(&[]);
        let visible_layers: Vec<&practical_arcana_painter::types::Layer> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .collect();
        let layer_base_colors: Vec<_> = visible_layers
            .iter()
            .map(|l| {
                practical_arcana_painter::compositing::resolve_base_color(&l.base_color, materials)
            })
            .collect();
        let layer_base_normals: Vec<_> = visible_layers
            .iter()
            .map(|l| {
                practical_arcana_painter::compositing::resolve_base_normal(&l.base_normal, materials)
            })
            .collect();

        // Group names for visible layers — parallel to `layers` vec
        let layer_group_names: Vec<String> = visible_layers
            .iter()
            .map(|l| l.group_name.clone())
            .collect();

        self.state.generation.start(generation::GenInput {
            layers,
            resolution,
            layer_base_colors,
            layer_base_normals,
            settings,
            mesh,
            cached_normals,
            layer_group_names,
        });
        self.state.generation_snapshot = Some(state::generation_state_hash(
            &self.state.project.layers,
            &self.state.project.settings,
            self.state.mesh_hash,
        ));
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
        self.state.textures.height = Some(textures::height_buffer_to_handle(
            ctx,
            &result.height,
            r,
            "gen_height",
        ));
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

        // Upload color and normal textures to 3D preview
        if let Some(ref rs) = self.render_state {
            if self.state.mesh_preview.gpu_ready {
                mesh_preview::upload_color_texture(rs, &result.color, r as usize);
                mesh_preview::upload_normal_texture(rs, &result.normal_map, r as usize);
            }
        }

        // Cache mesh normals computed on the worker thread
        if let Some(normals) = &result.computed_normals {
            self.state.cached_mesh_normals = Some((normals.0, std::sync::Arc::clone(&normals.1)));
        }
        self.state.status_message = format!("Generated in {:.1}s", result.elapsed.as_secs_f32());
        self.state.generated = Some(result);
        self.state.dirty = true;

        // Chain: auto-export to pre-selected path if requested via Generate & Export
        if let Some(dir) = self.state.post_gen_export_maps.take() {
            dialogs::export_maps_to(&mut self.state, &dir);
        }
        if let Some(path) = self.state.post_gen_export_glb.take() {
            dialogs::export_glb_to(&mut self.state, &path);
        }
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

        // Sync GPU textures with current generation state
        if let Some(ref gen) = self.state.generated {
            mesh_preview::upload_color_texture(rs, &gen.color, gen.resolution as usize);
            mesh_preview::upload_normal_texture(rs, &gen.normal_map, gen.resolution as usize);
        } else {
            mesh_preview::reset_textures_to_placeholder(rs);
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
        let redo_mods = egui::Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };
        let undo_mods = egui::Modifiers {
            command: true,
            ..Default::default()
        };
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
        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::G))
            && !self.state.generation.is_running()
        {
            self.state.pending_generate = true;
        }

        // ── Backtick key: cycle viewport tabs ──
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backtick)) {
            self.state.viewport_tab = self.state.viewport_tab.next();
        }

        // Number keys 1-4 are handled inside each strip function (viewport.rs)
        // so that item definitions and shortcuts stay in a single place.

        // ── Undo: capture pre-frame snapshot AFTER undo/redo ──
        // This way the restore itself is invisible to the change tracker.
        let pre_frame = self.state.take_snapshot();

        // Project-replacing actions explicitly set dirty=false; skip auto-dirty for those frames.
        let project_replacing = self.state.pending_open || self.state.pending_new;

        // Handle deferred actions (flags set by child widgets on AppState)
        if self.state.pending_open {
            self.state.pending_open = false;
            dialogs::open_project(&mut self.state, ctx);
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
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
            // State untouched — mesh held in popup until user confirms.
        }
        if self.state.pending_reload_mesh {
            self.state.pending_reload_mesh = false;
            dialogs::reload_mesh(&mut self.state);
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_replace_mesh {
            self.state.pending_replace_mesh = false;
            dialogs::replace_mesh(&mut self.state);
            // State untouched — mesh held in popup until user confirms.
        }
        // ── Path overlay: async worker pattern ──
        // Poll for completed results first
        if let Some(poll_result) = self.state.path_worker.poll() {
            match poll_result {
                Ok(result) => {
                    // Cache freshly computed mesh normals for future reuse
                    if let Some(normals) = &result.computed_normals {
                        self.state.cached_mesh_normals =
                            Some((normals.0, std::sync::Arc::clone(&normals.1)));
                    }
                    self.state.path_overlay.apply_result(&result);
                }
                Err(msg) => {
                    self.state.status_message = format!("Path overlay error: {msg}");
                }
            }
        }
        // Submit new computation if overlay is active and cache is stale
        if self.state.viewport.path_overlay_idx.is_some() {
            if let Some(selected) = self.state.selected_layer {
                if selected < self.state.project.layers.len() {
                    let layer = &self.state.project.layers[selected];
                    if layer.visible {
                        let seed = self
                            .state
                            .project
                            .settings
                            .seed
                            .wrapping_add(selected as u32);

                        let stale = self
                            .state
                            .path_overlay
                            .is_stale_for_layer(selected, layer, seed);

                        if stale {
                            let needs_normal = layer.paint.normal_break_threshold.is_some();
                            let normals_stale = needs_normal
                                && self
                                    .state
                                    .cached_mesh_normals
                                    .as_ref()
                                    .is_none_or(|(r, _)| *r != BASE_RESOLUTION);

                            let input = preview::PathOverlayInput {
                                layer: layer.clone(),
                                layer_index: selected,
                                layer_count: self.state.project.layers.len(),
                                seed,
                                resolution: BASE_RESOLUTION,
                                cached_normals: if needs_normal {
                                    self.state.cached_mesh_normals.clone()
                                } else {
                                    None
                                },
                                mesh: if normals_stale {
                                    self.state.loaded_mesh.clone()
                                } else {
                                    None
                                },
                            };
                            self.state
                                .path_overlay
                                .set_pending(selected, layer, seed);
                            self.state.path_worker.start(input);
                        }
                    }
                }
            }
        }
        // Keep repainting while path overlay worker is active
        if self.state.path_worker.is_running() {
            ctx.request_repaint();
        }

        // Poll generation results
        if let Some(poll_result) = self.state.generation.poll() {
            match poll_result {
                Ok(result) => self.apply_generation_result(ctx, result),
                Err(msg) => {
                    self.state.status_message = msg;
                    self.state.post_gen_export_maps = None;
                    self.state.post_gen_export_glb = None;
                }
            }
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
                    let has_gen = self.state.generated.is_some();
                    let stale = self.state.stale_reason();
                    let maps_label = match stale {
                        Some(reason) if has_gen => format!("Export Maps... ({reason})"),
                        _ => "Export Maps...".to_string(),
                    };
                    if ui
                        .add_enabled(has_gen, egui::Button::new(maps_label))
                        .clicked()
                    {
                        ui.close();
                        self.state.pending_export = true;
                    }
                    let glb_label = match stale {
                        Some(reason) if has_gen => format!("Export GLB... ({reason})"),
                        _ => "Export GLB...".to_string(),
                    };
                    if ui
                        .add_enabled(has_gen, egui::Button::new(glb_label))
                        .clicked()
                    {
                        ui.close();
                        self.state.pending_export_glb = true;
                    }
                    ui.separator();
                    let can_gen = !self.state.generation.is_running();
                    if ui
                        .add_enabled(can_gen, egui::Button::new("Generate & Export Maps..."))
                        .clicked()
                    {
                        ui.close();
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.state.post_gen_export_maps = Some(dir);
                            self.state.pending_generate = true;
                        }
                    }
                    if ui
                        .add_enabled(can_gen, egui::Button::new("Generate & Export GLB..."))
                        .clicked()
                    {
                        ui.close();
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("glTF Binary", &["glb"])
                            .set_file_name("preview.glb")
                            .save_file()
                        {
                            self.state.post_gen_export_glb = Some(path);
                            self.state.pending_generate = true;
                        }
                    }
                });
                ui.menu_button("Edit", |ui: &mut egui::Ui| {
                    let can_undo = self.state.undo.can_undo();
                    let can_redo = self.state.undo.can_redo();
                    if ui
                        .add_enabled(can_undo, egui::Button::new("Undo  ⌘Z"))
                        .clicked()
                    {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.undo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                    if ui
                        .add_enabled(can_redo, egui::Button::new("Redo  ⌘⇧Z"))
                        .clicked()
                    {
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
                egui::TopBottomPanel::bottom("left_bottom").show_inside(ui, |ui: &mut egui::Ui| {
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
            let has_project = self.state.loaded_mesh.is_some() || self.state.project_path.is_some();

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
                            egui::Button::new("New Project").min_size(egui::Vec2::new(200.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.state.pending_new = true;
                    }
                });
            }
        });

        // ── Mesh Load Popup ──
        let mut apply_popup = false;
        let mut dismiss_popup = false;
        if let Some(ref mut popup) = self.state.mesh_load_popup {
            use crate::gui::sidebar::fmt_thousands;

            let popup_frame = egui::Frame {
                inner_margin: egui::Margin::same(16),
                outer_margin: egui::Margin::ZERO,
                corner_radius: egui::CornerRadius::same(8),
                shadow: egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 4,
                    color: egui::Color32::from_black_alpha(80),
                },
                fill: ctx.style().visuals.window_fill,
                stroke: egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
            };

            egui::Window::new("mesh_load_popup")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .min_width(440.0)
                .max_width(440.0)
                .max_height(420.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(popup_frame)
                .show(ctx, |ui: &mut egui::Ui| {
                    let weak = ui.visuals().weak_text_color();

                    // ── Header ──
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.strong(
                            egui::RichText::new(if popup.is_replace {
                                "Replace Mesh"
                            } else {
                                "New Project"
                            })
                            .size(15.0),
                        );
                    });
                    ui.add_space(6.0);

                    // ── Mesh Info Grid ──
                    egui::Grid::new("mesh_info_grid")
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui: &mut egui::Ui| {
                            ui.colored_label(weak, "File");
                            ui.label(&popup.filename);
                            ui.end_row();

                            ui.colored_label(weak, "Vertices");
                            ui.label(fmt_thousands(popup.vertices));
                            ui.end_row();

                            ui.colored_label(weak, "Triangles");
                            ui.label(fmt_thousands(popup.triangles));
                            ui.end_row();

                            ui.colored_label(weak, "Groups");
                            ui.label(popup.groups.to_string());
                            ui.end_row();

                            if popup.n_textures > 0 || popup.n_normals > 0 {
                                ui.colored_label(weak, "Textures");
                                ui.label(format!(
                                    "{} color, {} normal",
                                    popup.n_textures, popup.n_normals,
                                ));
                                ui.end_row();
                            }
                        });

                    // MTL toggle (OBJ only)
                    if popup.has_mtl {
                        ui.add_space(6.0);
                        ui.checkbox(&mut popup.use_mtl, "Use MTL materials");
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // ── Layer Mapping ──
                    let mappings = if popup.has_mtl && !popup.use_mtl {
                        &popup.mappings_no_mtl
                    } else {
                        &popup.mappings
                    };

                    ui.strong(egui::RichText::new("Import Layers").size(12.0));
                    ui.add_space(4.0);

                    let col_color = 110.0_f32;
                    let col_normal = 110.0_f32;

                    // Header row (outside scroll area)
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // Select-all checkbox
                        let all_on = popup.layer_enabled.iter().all(|&e| e);
                        let any_on = popup.layer_enabled.iter().any(|&e| e);
                        let mut toggle = all_on;
                        let resp = ui.checkbox(&mut toggle, "");
                        if !all_on && any_on {
                            let center = resp.rect.center();
                            ui.painter().line_segment(
                                [
                                    egui::pos2(center.x - 4.0, center.y),
                                    egui::pos2(center.x + 4.0, center.y),
                                ],
                                egui::Stroke::new(2.0, ui.visuals().text_color()),
                            );
                        }
                        if resp.changed() {
                            for e in &mut popup.layer_enabled {
                                *e = toggle;
                            }
                        }
                        ui.colored_label(weak, "Layer");

                        // Right-pinned Color / Normal headers (centered in slot)
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui: &mut egui::Ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_normal, ui.available_height()),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui: &mut egui::Ui| {
                                        ui.colored_label(weak, "Normal");
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_color, ui.available_height()),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui: &mut egui::Ui| {
                                        ui.colored_label(weak, "Color");
                                    },
                                );
                            },
                        );
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .auto_shrink(false)
                        .show(ui, |ui: &mut egui::Ui| {
                            for (i, (name, color, normal, _has_tex, _has_nrm)) in
                                mappings.iter().enumerate()
                            {
                                let row_rect = egui::Rect::from_min_size(
                                    ui.cursor().min,
                                    egui::vec2(ui.available_width(), 22.0),
                                );
                                if i % 2 == 0 {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        0.0,
                                        ui.visuals().faint_bg_color,
                                    );
                                }

                                let enabled = popup
                                    .layer_enabled
                                    .get(i)
                                    .copied()
                                    .unwrap_or(true);
                                let text_color = if enabled {
                                    ui.visuals().text_color()
                                } else {
                                    weak
                                };

                                ui.horizontal(|ui: &mut egui::Ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;

                                    // Checkbox + layer name
                                    if i < popup.layer_enabled.len() {
                                        ui.checkbox(&mut popup.layer_enabled[i], "");
                                    }
                                    if enabled {
                                        ui.strong(name);
                                    } else {
                                        ui.colored_label(weak, name);
                                    }

                                    // Color + Normal pinned to right
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui: &mut egui::Ui| {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_normal, ui.available_height()),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui: &mut egui::Ui| {
                                                    source_label_with_chip(
                                                        ui, normal, text_color, weak,
                                                    );
                                                },
                                            );
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_color, ui.available_height()),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui: &mut egui::Ui| {
                                                    source_label_with_chip(
                                                        ui, color, text_color, weak,
                                                    );
                                                },
                                            );
                                        },
                                    );
                                });
                            }
                        });

                    ui.add_space(12.0);

                    // ── Buttons (centered) ──
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(
                                egui::Button::new("  OK  ")
                                    .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            apply_popup = true;
                        }
                        if ui
                            .add(
                                egui::Button::new("Cancel")
                                    .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            dismiss_popup = true;
                        }
                    });
                });
        }
        if apply_popup {
            dialogs::apply_mesh_load_popup(&mut self.state);
            self.state.mesh_load_popup = None;
            // Post-load cleanup (deferred until confirmation)
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if dismiss_popup {
            self.state.mesh_load_popup = None;
        }

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
        if pre_frame != post_frame && !project_replacing {
            self.state.dirty = true;
        }
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.state
            .undo
            .track_frame(&pre_frame, &post_frame, pointer_down);
    }
}

/// Show a TextureSource as a compact visual in the popup.
/// Solid → chip + hex, File → icon + name, MeshMaterial → icon + index.
fn source_label_with_chip(
    ui: &mut egui::Ui,
    src: &TextureSource,
    text_color: egui::Color32,
    weak: egui::Color32,
) {
    use egui_phosphor::fill::{CUBE, FOLDER_OPEN};
    match src {
        TextureSource::Solid(rgb) => {
            let srgb = [
                (rgb[0].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[1].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[2].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
            ];
            ui.spacing_mut().item_spacing.x = 4.0;
            let chip_size = egui::vec2(14.0, 14.0);
            let (rect, _) =
                ui.allocate_exact_size(chip_size, egui::Sense::hover());
            ui.painter().rect_filled(
                rect,
                2.0,
                egui::Color32::from_rgb(srgb[0], srgb[1], srgb[2]),
            );
            ui.painter().rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0, weak),
                egui::StrokeKind::Outside,
            );
            ui.colored_label(
                text_color,
                format!("#{:02X}{:02X}{:02X}", srgb[0], srgb[1], srgb[2]),
            );
        }
        TextureSource::MeshMaterial(idx) => {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(text_color, CUBE);
            ui.colored_label(text_color, format!("[{idx}]"));
        }
        TextureSource::File(Some(tex)) => {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(text_color, FOLDER_OPEN);
            ui.colored_label(text_color, &tex.label);
        }
        TextureSource::File(None) => {
            ui.colored_label(weak, "(no file)");
        }
        TextureSource::None => {
            use egui_phosphor::fill::PROHIBIT;
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(weak, PROHIBIT);
            ui.colored_label(weak, "None");
        }
    }
}
