pub mod dialogs;
pub mod generation;
pub mod guide_editor;
pub mod mesh_preview;
pub mod preview;
pub mod sidebar;
pub mod slot_editor;
pub mod state;
pub mod textures;
pub mod viewport;

use eframe::egui;
use eframe::egui_wgpu;
use state::AppState;

use practical_arcana_painter::object_normal::compute_mesh_normal_data;
use practical_arcana_painter::types::{pixels_to_colors, Color, NormalMode};

/// Main GUI application.
pub struct PainterApp {
    state: AppState,
    checkerboard: Option<egui::TextureHandle>,
    render_state: Option<egui_wgpu::RenderState>,
}

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
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

        // Base color
        let base_colors = self
            .state
            .loaded_texture
            .as_ref()
            .map(|tex| pixels_to_colors(&tex.pixels));
        let (base_w, base_h) = self
            .state
            .loaded_texture
            .as_ref()
            .map(|t| (t.width, t.height))
            .unwrap_or((0, 0));
        let sc = self.state.project.color_ref.solid_color;
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
            (0..self.state.project.slots.len()).map(|_| None).collect()
        };

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
        });
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
        // Lazy-init checkerboard
        if self.checkerboard.is_none() {
            self.checkerboard = Some(viewport::make_checkerboard(ctx));
        }
        if self.state.textures.base_texture.is_none() {
            self.state.textures.base_texture = self.checkerboard.clone();
        }

        // Handle deferred actions (flags set by child widgets on AppState)
        if self.state.pending_open {
            self.state.pending_open = false;
            dialogs::open_project(&mut self.state, ctx);
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
        if self.state.pending_generate {
            self.state.pending_generate = false;
            self.start_generation();
        }
        if self.state.pending_new {
            self.state.pending_new = false;
            dialogs::new_project(&mut self.state, ctx);
            self.init_mesh_preview();
        }
        if self.state.pending_load_mesh {
            self.state.pending_load_mesh = false;
            dialogs::load_mesh_dialog(&mut self.state, ctx);
            self.init_mesh_preview();
        }
        if self.state.pending_load_texture {
            self.state.pending_load_texture = false;
            dialogs::load_texture_dialog(&mut self.state, ctx);
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
            egui::menu::bar(ui, |ui: &mut egui::Ui| {
                ui.menu_button("File", |ui: &mut egui::Ui| {
                    if ui.button("New Project...").clicked() {
                        ui.close_menu();
                        self.state.pending_new = true;
                    }
                    if ui.button("Open Project...").clicked() {
                        ui.close_menu();
                        self.state.pending_open = true;
                    }
                    if ui.button("Save").clicked() {
                        ui.close_menu();
                        self.state.pending_save = true;
                    }
                    ui.separator();
                    if ui.button("Export...").clicked() {
                        ui.close_menu();
                        self.state.pending_export = true;
                    }
                });
                ui.menu_button("View", |ui: &mut egui::Ui| {
                    ui.checkbox(&mut self.state.viewport.show_wireframe, "UV Wireframe");
                    ui.checkbox(&mut self.state.viewport.show_guides, "Guides");
                });

                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui: &mut egui::Ui| {
                        let running = self.state.generation.is_running();
                        let label = if running {
                            "Generating..."
                        } else {
                            "▶ Generate"
                        };
                        let btn = ui.add_enabled(!running, egui::Button::new(label));
                        if btn.clicked() {
                            self.state.pending_generate = true;
                        }
                    },
                );
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
                    ui.label(format!("{} slots", self.state.project.slots.len()));
                });
            });

        // ── Left sidebar ──
        egui::SidePanel::left("left_panel")
            .default_width(260.0)
            .min_width(220.0)
            .max_width(400.0)
            .show(ctx, |ui: &mut egui::Ui| {
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    sidebar::show_left(ui, &mut self.state);
                });
            });

        // ── Right sidebar (slot editor, only when a slot is selected) ──
        if self.state.selected_slot.is_some() {
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
    }
}
