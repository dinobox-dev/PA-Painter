//! Modal dialog windows: export settings, mesh load popup, auxiliary windows
//! (unsaved changes, alert, export overwrite).

use eframe::egui;

use super::file_actions;
use super::recent_files;
use super::state::{AppState, GuideTool, ProjectLoadSource, UnsavedAction};
use super::PainterApp;

use pa_painter::pipeline::output::ExportFormat;
use pa_painter::types::{NormalYConvention, TextureSource};

impl PainterApp {
    pub(super) fn show_export_settings_window(ctx: &egui::Context, state: &mut AppState) {
        if !state.show_export_settings {
            return;
        }

        // Ensure draft exists (defensive — normally created when opening)
        if state.export_settings_draft.is_none() {
            state.export_settings_draft = Some(state.project.export_settings.clone());
        }

        let mut action: Option<bool> = None; // Some(true) = save, Some(false) = cancel
        let weak = ctx.style().visuals.weak_text_color();

        let frame = egui::Frame {
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

        egui::Window::new("export_settings")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .min_width(260.0)
            .max_width(260.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(frame)
            .show(ctx, |ui: &mut egui::Ui| {
                let es = state.export_settings_draft.as_mut().unwrap();
                ui.spacing_mut().item_spacing.y = 4.0;

                // ── Header ──
                ui.vertical_centered(|ui: &mut egui::Ui| {
                    ui.strong(egui::RichText::new("Export Settings").size(15.0));
                });
                ui.add_space(8.0);

                // ── Texture Maps ──
                ui.checkbox(&mut es.export_maps, "Texture Maps");
                if es.export_maps {
                    ui.indent("maps_indent", |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;

                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(weak, "Format");
                            ui.selectable_value(&mut es.format, ExportFormat::Png, "PNG");
                            ui.selectable_value(&mut es.format, ExportFormat::Exr, "EXR");
                        });
                        ui.add_space(2.0);
                        ui.checkbox(&mut es.include_color, "Color");
                        ui.checkbox(&mut es.include_normal, "Normal");
                        ui.checkbox(&mut es.include_height, "Height");
                        ui.checkbox(&mut es.include_stroke_id, "Stroke ID");
                        ui.checkbox(&mut es.include_time_map, "Stroke Time");
                        ui.add_space(4.0);
                        ui.checkbox(&mut es.per_layer, "Per Layer");
                    });
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── 3D Model ──
                ui.checkbox(&mut es.export_model, "3D Model");
                if es.export_model {
                    ui.indent("model_indent", |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.colored_label(weak, "Format: GLB");
                        ui.add_space(2.0);
                        ui.checkbox(&mut es.embed_color, "Embed color texture");
                        ui.checkbox(&mut es.embed_normal, "Embed normal map");
                    });
                }

                // ── Normal Y Convention ──
                // Show when any normal output is enabled (texture or GLB).
                let any_normal =
                    (es.export_maps && es.include_normal) || (es.export_model && es.embed_normal);
                if any_normal {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.colored_label(weak, "Normal Y axis");
                        ui.selectable_value(&mut es.normal_y, NormalYConvention::OpenGL, "OpenGL");
                        ui.selectable_value(
                            &mut es.normal_y,
                            NormalYConvention::DirectX,
                            "DirectX",
                        );
                    });
                }

                // ── Cancel / Save ──
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    let btn_w = 80.0_f32;
                    let gap = 12.0_f32;
                    ui.spacing_mut().item_spacing.x = gap;
                    let total = btn_w * 2.0 + gap;
                    let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                    ui.add_space(pad);
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                        .clicked()
                    {
                        action = Some(false);
                    }
                    let accent = egui::Color32::from_rgb(45, 120, 220);
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Save").color(egui::Color32::WHITE),
                            )
                            .fill(accent)
                            .min_size(egui::Vec2::new(btn_w, 28.0)),
                        )
                        .clicked()
                    {
                        action = Some(true);
                    }
                });
            });

        match action {
            Some(true) => {
                if let Some(draft) = state.export_settings_draft.take() {
                    state.project.export_settings = draft;
                    state.dirty = true;
                }
                state.show_export_settings = false;
            }
            Some(false) => {
                state.export_settings_draft = None;
                state.show_export_settings = false;
            }
            None => {}
        }
    }

    /// Show the mesh-load confirmation popup and handle OK / Cancel.
    pub(super) fn show_mesh_load_popup(&mut self, ctx: &egui::Context) {
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
                            for (i, lm) in mappings.iter().enumerate() {
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

                                let enabled = popup.layer_enabled.get(i).copied().unwrap_or(true);
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
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&lm.name).strong(),
                                            )
                                            .truncate(),
                                        );
                                    } else {
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&lm.name).color(weak),
                                            )
                                            .truncate(),
                                        );
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
                                                        ui,
                                                        &lm.base_normal,
                                                        text_color,
                                                        weak,
                                                        lm.is_default,
                                                    );
                                                },
                                            );
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_color, ui.available_height()),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui: &mut egui::Ui| {
                                                    source_label_with_chip(
                                                        ui,
                                                        &lm.base_color,
                                                        text_color,
                                                        weak,
                                                        lm.is_default,
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
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            dismiss_popup = true;
                        }
                        let accent = egui::Color32::from_rgb(45, 120, 220);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("  OK  ").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            apply_popup = true;
                        }
                    });
                });
        }
        if apply_popup {
            file_actions::apply_mesh_load_popup(&mut self.state);
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
    }

    /// Reload summary, export settings, and Escape key handling.
    pub(super) fn show_auxiliary_windows(&mut self, ctx: &egui::Context) {
        // Reload Summary Window
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
                    let accent = egui::Color32::from_rgb(45, 120, 220);
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("OK").color(egui::Color32::WHITE),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        dismiss_summary = true;
                    }
                });
        }
        if dismiss_summary {
            self.state.reload_summary = None;
        }

        Self::show_export_settings_window(ctx, &mut self.state);

        // Alert dialog (modal)
        let mut dismiss_alert = false;
        if let Some(ref msg) = self.alert_message {
            if ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            }) {
                dismiss_alert = true;
            }
            egui::Window::new("Alert")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(msg);
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        dismiss_alert = true;
                    }
                });
        }
        if dismiss_alert {
            self.alert_message = None;
        }

        // Unsaved Changes Confirmation Window (modal)
        let mut unsaved_action = None;
        if self.state.unsaved_confirm.is_some() {
            let screen = ctx.content_rect();
            egui::Area::new(egui::Id::new("unsaved_dim"))
                .fixed_pos(screen.min)
                .order(egui::Order::Middle)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let rect = egui::Rect::from_min_size(screen.min, screen.size());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(80));
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                });

            egui::Window::new("Unsaved changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .order(egui::Order::Foreground)
                .fixed_size([320.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.label("Current project has unsaved changes.");
                        ui.label("Discard changes and continue?");
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            unsaved_action = Some(false);
                        }
                        let accent = egui::Color32::from_rgb(200, 80, 60);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Discard").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            unsaved_action = Some(true);
                        }
                    });
                });
        }
        match unsaved_action {
            Some(true) => {
                let action = self.state.unsaved_confirm.take();
                match action {
                    Some(UnsavedAction::Open) => {
                        if let Some(path) = self.pending_open_recent.take() {
                            if path.exists() {
                                self.state.status_message = format!("Opening {}…", path.display());
                                self.state
                                    .project_load_worker
                                    .start(ProjectLoadSource::Recent(path));
                            } else {
                                self.alert_message =
                                    Some(format!("File not found:\n{}", path.display()));
                                self.recent_files = recent_files::remove(&path);
                            }
                        } else {
                            self.do_open_project(ctx);
                        }
                    }
                    Some(UnsavedAction::OpenExample) => {
                        self.state.status_message = "Opening example project…".to_string();
                        self.state
                            .project_load_worker
                            .start(ProjectLoadSource::Example);
                    }
                    Some(UnsavedAction::New) => self.do_new_project(ctx),
                    Some(UnsavedAction::Quit) => {
                        self.state.dirty = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    None => {}
                }
            }
            Some(false) => {
                self.state.unsaved_confirm = None;
                self.pending_open_recent = None;
            }
            None => {}
        }

        // Export Overwrite Confirmation Window (modal)
        let mut overwrite_action = None;
        if let Some(ref confirm) = self.state.export_overwrite_confirm {
            // Dim overlay to block interaction with background UI
            let screen = ctx.content_rect();
            egui::Area::new(egui::Id::new("overwrite_dim"))
                .fixed_pos(screen.min)
                .order(egui::Order::Middle)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let rect = egui::Rect::from_min_size(screen.min, screen.size());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(80));
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                });

            egui::Window::new("Overwrite existing files?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .order(egui::Order::Foreground)
                .fixed_size([300.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.label(format!(
                            "{} file(s) will be overwritten in \"{}\".",
                            confirm.conflict_count, confirm.folder_name,
                        ));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            overwrite_action = Some(false);
                        }
                        let accent = egui::Color32::from_rgb(45, 120, 220);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Overwrite").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            overwrite_action = Some(true);
                        }
                    });
                });
        }
        match overwrite_action {
            Some(true) => file_actions::confirm_export_overwrite(&mut self.state),
            Some(false) => {
                self.state.export_overwrite_confirm = None;
            }
            None => {}
        }

        // Escape: deselect guide + return to Select tool.
        // Runs after panels so popup consume_key takes priority.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.state.selected_guide = None;
            self.state.guide_tool = GuideTool::Select;
            self.state.show_export_settings = false;
            self.state.export_settings_draft = None;
            self.state.export_overwrite_confirm = None;
            self.state.unsaved_confirm = None;
            self.pending_open_recent = None;
        }
    }
}

/// Show a TextureSource as a compact visual in the popup.
/// Solid → chip + hex, File → icon + name, MeshMaterial → icon + index.
/// When `is_default` is true, shows "Default" instead of details.
fn source_label_with_chip(
    ui: &mut egui::Ui,
    src: &TextureSource,
    text_color: egui::Color32,
    weak: egui::Color32,
    is_default: bool,
) {
    use egui_phosphor::fill::{CUBE, FOLDER_OPEN};

    if is_default {
        ui.colored_label(weak, "Default");
        return;
    }

    match src {
        TextureSource::Solid(rgb) => {
            let srgb = [
                (rgb[0].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[1].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[2].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
            ];
            ui.spacing_mut().item_spacing.x = 4.0;
            let chip_size = egui::vec2(14.0, 14.0);
            let (rect, _) = ui.allocate_exact_size(chip_size, egui::Sense::hover());
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
            ui.add(egui::Label::new(egui::RichText::new(&tex.label).color(text_color)).truncate());
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
