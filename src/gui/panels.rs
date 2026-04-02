//! Top-level UI panels: menu bar, status bar, sidebars, central panel, update banner.

use eframe::egui;

use super::recent_files;
use super::sidebar;
use super::slot_editor;
use super::viewport;
use super::widgets;
use super::PainterApp;

impl PainterApp {
    /// Colored banner below the menu bar when a newer version is available.
    pub(super) fn show_update_banner(&mut self, ctx: &egui::Context) {
        self.update_checker.poll();

        if self.update_dismissed {
            return;
        }
        let Some(info) = self.update_checker.update_available() else {
            return;
        };

        let version = info.version.clone();
        let url = info.url.clone();

        let bg = egui::Color32::from_rgb(56, 152, 220);
        let bg_hover = egui::Color32::from_rgb(70, 165, 230);
        let fg = egui::Color32::from_rgb(240, 248, 255);
        let fg_dim = egui::Color32::from_rgb(200, 225, 245);

        let banner_h = 30.0;
        let text_size = 13.0;
        let icon_size = 16.0;
        let x_size = 18.0;
        let gap = 5.0;

        egui::TopBottomPanel::top("update_banner")
            .exact_height(banner_h)
            .frame(egui::Frame::new().fill(bg))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let cy = rect.center().y;
                let p = ui.painter();

                // ── Left: icon + text + link, all vertically centered on cy ──
                let mut x = rect.left() + 12.0;

                let icon_galley = p.layout_no_wrap(
                    egui_phosphor::fill::ARROW_CIRCLE_UP.to_string(),
                    egui::FontId::proportional(icon_size),
                    fg,
                );
                p.galley(
                    egui::pos2(x, cy - icon_galley.size().y * 0.5),
                    icon_galley.clone(),
                    fg,
                );
                x += icon_galley.size().x + gap;

                let label_galley = p.layout_no_wrap(
                    format!("v{version} available"),
                    egui::FontId::proportional(text_size),
                    fg,
                );
                p.galley(
                    egui::pos2(x, cy - label_galley.size().y * 0.5),
                    label_galley.clone(),
                    fg,
                );
                x += label_galley.size().x + gap + 2.0;

                let dl_galley = p.layout_no_wrap(
                    "Download".to_string(),
                    egui::FontId::proportional(text_size),
                    fg,
                );
                let dl_pos = egui::pos2(x, cy - dl_galley.size().y * 0.5);
                let dl_rect = egui::Rect::from_min_size(dl_pos, dl_galley.size());
                p.line_segment(
                    [
                        egui::pos2(dl_rect.left(), dl_rect.bottom() - 1.0),
                        egui::pos2(dl_rect.right(), dl_rect.bottom() - 1.0),
                    ],
                    egui::Stroke::new(1.0, fg),
                );
                p.galley(dl_pos, dl_galley, fg);
                let dl_resp = ui.interact(dl_rect, ui.id().with("dl_link"), egui::Sense::click());
                if dl_resp.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                }
                dl_resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                // ── Right: dismiss × (square, full banner height) ──
                let h = rect.height();
                let x_hit = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - h, rect.top()),
                    egui::vec2(h, h),
                );
                let x_resp = ui.interact(x_hit, ui.id().with("dismiss"), egui::Sense::click());
                if x_resp.hovered() {
                    p.rect_filled(x_hit, 0.0, bg_hover);
                }
                let x_color = if x_resp.hovered() { fg } else { fg_dim };
                p.text(
                    x_hit.center(),
                    egui::Align2::CENTER_CENTER,
                    "\u{00D7}",
                    egui::FontId::proportional(x_size),
                    x_color,
                );
                if x_resp.clicked() {
                    self.update_dismissed = true;
                }
            });
    }

    /// Top menu bar (File / Edit / View).
    /// Menu item with optional right-aligned shortcut.
    fn menu_item(
        ui: &mut egui::Ui,
        label: &str,
        shortcut: Option<egui::KeyboardShortcut>,
        enabled: bool,
    ) -> bool {
        let mut btn = egui::Button::new(label);
        if let Some(sc) = shortcut {
            btn = btn.shortcut_text(ui.ctx().format_shortcut(&sc));
        }
        ui.add_enabled(enabled, btn).clicked()
    }

    pub(super) fn show_menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui: &mut egui::Ui| {
            egui::MenuBar::new().ui(ui, |ui: &mut egui::Ui| {
                ui.menu_button("File", |ui: &mut egui::Ui| {
                    use egui::{Key, KeyboardShortcut, Modifiers};
                    ui.set_min_width(200.0);
                    if Self::menu_item(ui, "New Project...", None, true) {
                        ui.close();
                        self.state.pending_new = true;
                    }
                    if Self::menu_item(ui, "New Window", None, true) {
                        ui.close();
                        if let Ok(exe) = std::env::current_exe() {
                            std::process::Command::new(exe).spawn().ok();
                        }
                    }
                    ui.separator();
                    if Self::menu_item(ui, "Open Project...", None, true) {
                        ui.close();
                        self.state.pending_open = true;
                    }
                    if Self::menu_item(ui, "Open Example", None, true) {
                        ui.close();
                        self.state.pending_open_example = true;
                    }
                    if !self.recent_files.is_empty() {
                        ui.menu_button("Recent Projects", |ui| {
                            ui.set_min_width(250.0);
                            let mut open_path = None;
                            for entry in &self.recent_files {
                                let label = format!("{}  ({})", entry.name(), entry.time_ago());
                                if ui
                                    .button(label)
                                    .on_hover_text(entry.path.display().to_string())
                                    .clicked()
                                {
                                    open_path = Some(entry.path.clone());
                                    ui.close();
                                }
                            }
                            if let Some(path) = open_path {
                                self.pending_open_recent = Some(path);
                            }
                        });
                    }
                    ui.separator();
                    if Self::menu_item(
                        ui,
                        "Save",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::S)),
                        true,
                    ) {
                        ui.close();
                        self.state.pending_save = true;
                    }
                    if Self::menu_item(
                        ui,
                        "Save As...",
                        Some(KeyboardShortcut::new(
                            Modifiers::COMMAND | Modifiers::SHIFT,
                            Key::S,
                        )),
                        true,
                    ) {
                        ui.close();
                        self.state.pending_save_as = true;
                    }
                    let can_export =
                        self.state.generated.is_some() && !self.state.export_worker.is_running();
                    if Self::menu_item(ui, "Export...", None, can_export) {
                        ui.close();
                        self.state.pending_export = true;
                    }
                });
                ui.menu_button("Edit", |ui: &mut egui::Ui| {
                    use egui::{Key, KeyboardShortcut, Modifiers};
                    ui.set_min_width(200.0);
                    let can_undo = self.state.undo.can_undo();
                    let can_redo = self.state.undo.can_redo();
                    if Self::menu_item(
                        ui,
                        "Undo",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
                        can_undo,
                    ) {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.undo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                    if Self::menu_item(
                        ui,
                        "Redo",
                        Some(KeyboardShortcut::new(
                            Modifiers::COMMAND | Modifiers::SHIFT,
                            Key::Z,
                        )),
                        can_redo,
                    ) {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.redo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                    ui.separator();
                    let can_gen =
                        !self.state.generation.is_running() && self.state.loaded_mesh.is_some();
                    if Self::menu_item(
                        ui,
                        "Force Full-Res",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::G)),
                        can_gen,
                    ) {
                        ui.close();
                        self.start_generation();
                    }
                });
                ui.menu_button("View", |ui: &mut egui::Ui| {
                    ui.set_min_width(200.0);
                    ui.label("Theme");
                    ui.separator();
                    let mut pref = ui.ctx().options(|o| o.theme_preference);
                    let old = pref;
                    for (value, label) in [
                        (egui::ThemePreference::System, "System"),
                        (egui::ThemePreference::Dark, "Dark"),
                        (egui::ThemePreference::Light, "Light"),
                    ] {
                        let mut checked = pref == value;
                        if ui.checkbox(&mut checked, label).clicked() && checked {
                            pref = value;
                        }
                    }
                    if pref != old {
                        ui.ctx().set_theme(pref);
                        ui.close();
                    }
                });
            });
        });
    }

    /// Bottom status bar (status message, resolution, layer count).
    pub(super) fn show_status_bar(&mut self, ctx: &egui::Context) {
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
    }

    /// Left sidebar (layers, settings) and right sidebar (layer inspector).
    pub(super) fn show_sidebars(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_panel")
            .default_width(260.0)
            .min_width(260.0)
            .max_width(400.0)
            .show(ctx, |ui: &mut egui::Ui| {
                egui::TopBottomPanel::bottom("left_bottom")
                    .frame(egui::Frame::new().inner_margin(0.0))
                    .show_separator_line(false)
                    .show_inside(ui, |ui: &mut egui::Ui| {
                        sidebar::show_bottom(ui, &mut self.state);
                    });
                sidebar::show_top(ui, &mut self.state);
                sidebar::show_layers_header(ui, &mut self.state);
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    sidebar::show_layer_rows(ui, &mut self.state);
                });
            });

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
    }

    /// Central panel: viewport (UV/3D) or welcome screen.
    pub(super) fn show_central_panel(&mut self, ctx: &egui::Context) {
        let title = if let Some(name) = self
            .state
            .project_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|f| f.to_string_lossy().to_string())
            .or_else(|| {
                self.state
                    .loaded_mesh
                    .is_some()
                    .then(|| "Untitled".to_string())
            }) {
            let dirty = if self.state.dirty { " *" } else { "" };
            format!("PA Painter — {name}{dirty}")
        } else {
            "PA Painter".to_string()
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        let render_state = self.render_state.clone();
        egui::CentralPanel::default().show(ctx, |ui: &mut egui::Ui| {
            let has_project = self.state.loaded_mesh.is_some() || self.state.project_path.is_some();

            if has_project {
                viewport::show(ui, &mut self.state, render_state.as_ref());
            } else {
                // Welcome screen: fixed-width centered container
                let panel_w = 460.0_f32.min(ui.available_width() - 32.0);
                let content_h = 60.0 // heading + description + spacing
                    + 56.0 + 24.0   // buttons + gap
                    + if self.recent_files.is_empty() {
                        0.0
                    } else {
                        24.0 + 23.0 + self.recent_files.len() as f32 * 22.0
                    };
                let top_space = ((ui.available_height() - content_h) / 2.5).max(16.0);
                ui.add_space(top_space);

                let margin = (ui.available_width() - panel_w).max(0.0) / 2.0;
                let container = egui::Rect::from_min_size(
                    ui.cursor().min + egui::Vec2::new(margin, 0.0),
                    egui::Vec2::new(panel_w, ui.available_height()),
                );
                let mut ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(container)
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                );

                ui.heading("PA Painter");
                ui.add_space(8.0);
                ui.label("Generate painterly texture maps from 3D meshes.");
                ui.add_space(24.0);

                let btn_h = 56.0;
                let half_w = (panel_w - ui.spacing().item_spacing.x) / 2.0;
                let btn_size = egui::Vec2::new(half_w, btn_h);

                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new("Open Project...").min_size(btn_size))
                        .clicked()
                    {
                        self.state.pending_open = true;
                    }
                    if ui
                        .add(egui::Button::new("New Project").min_size(btn_size))
                        .clicked()
                    {
                        self.state.pending_new = true;
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("or drag and drop .papr / .obj / .glb files")
                        .size(12.0)
                        .color(ui.visuals().weak_text_color()),
                );

                if !self.recent_files.is_empty() {
                    ui.add_space(24.0);

                    let mut open_path = None;
                    let weak = ui.visuals().weak_text_color();
                    let text_color = ui.visuals().text_color();
                    let name_font = egui::FontId::proportional(13.0);
                    let small_font = egui::FontId::proportional(11.0);

                    let col_name = panel_w * 0.25;
                    let col_opened = panel_w * 0.25;
                    let col_loc = panel_w - col_name - col_opened;
                    let row_h = 22.0;
                    let cols = [col_name, col_loc, col_opened];

                    let paint_cell = |ui: &egui::Ui,
                                      rect: egui::Rect,
                                      text: &str,
                                      font: egui::FontId,
                                      color: egui::Color32| {
                        widgets::paint_truncated_text(
                            ui.painter(),
                            text,
                            font,
                            color,
                            rect.left(),
                            rect,
                            rect.width(),
                        );
                    };

                    // Header
                    let (header_rect, _) = ui
                        .allocate_exact_size(egui::Vec2::new(panel_w, row_h), egui::Sense::hover());
                    let mut hx = header_rect.min.x;
                    for (&w, &label) in cols.iter().zip(["Name", "Location", "Opened"].iter()) {
                        let cell = egui::Rect::from_min_size(
                            egui::Pos2::new(hx, header_rect.min.y),
                            egui::Vec2::new(w, row_h),
                        );
                        paint_cell(&ui, cell, label, small_font.clone(), weak);
                        hx += w;
                    }

                    // Separator
                    let (sep_rect, _) =
                        ui.allocate_exact_size(egui::Vec2::new(panel_w, 1.0), egui::Sense::hover());
                    ui.painter().line_segment(
                        [sep_rect.left_center(), sep_rect.right_center()],
                        ui.visuals().widgets.noninteractive.bg_stroke,
                    );

                    // Rows (scrollable)
                    let mut remove_path = None;
                    egui::ScrollArea::vertical()
                        .auto_shrink(true)
                        .show(&mut ui, |ui| {
                            for entry in &self.recent_files {
                                let (row_rect, row_resp) = ui.allocate_exact_size(
                                    egui::Vec2::new(panel_w, row_h),
                                    egui::Sense::click(),
                                );

                                let row_hovered = row_resp.hovered();
                                let pointer_pos = ui.ctx().input(|i| i.pointer.hover_pos());

                                let x_size = row_h;
                                let x_rect = egui::Rect::from_min_size(
                                    egui::Pos2::new(row_rect.max.x - x_size, row_rect.min.y),
                                    egui::Vec2::splat(x_size),
                                );
                                let x_hovered =
                                    row_hovered && pointer_pos.is_some_and(|p| x_rect.contains(p));
                                let x_clicked = row_resp.clicked() && x_hovered;

                                if row_hovered {
                                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if row_hovered && !x_hovered {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        2.0,
                                        ui.visuals().widgets.hovered.bg_fill,
                                    );
                                }
                                if row_hovered {
                                    if x_hovered {
                                        ui.painter().rect_filled(
                                            x_rect.shrink(2.0),
                                            3.0,
                                            ui.visuals().widgets.active.bg_fill,
                                        );
                                    }
                                    ui.painter().text(
                                        x_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "\u{00d7}",
                                        egui::FontId::proportional(15.0),
                                        if x_hovered { text_color } else { weak },
                                    );
                                }

                                let texts = [entry.name(), entry.dir_display(), entry.time_ago()];
                                let fonts =
                                    [name_font.clone(), small_font.clone(), small_font.clone()];
                                let colors = [text_color, weak, weak];
                                let mut x = row_rect.min.x;
                                for i in 0..3 {
                                    let w = if row_hovered && i == 2 {
                                        (cols[i] - x_size).max(0.0)
                                    } else {
                                        cols[i]
                                    };
                                    let cell = egui::Rect::from_min_size(
                                        egui::Pos2::new(x, row_rect.min.y),
                                        egui::Vec2::new(w, row_h),
                                    );
                                    paint_cell(ui, cell, &texts[i], fonts[i].clone(), colors[i]);
                                    x += cols[i];
                                }

                                if x_clicked {
                                    remove_path = Some(entry.path.clone());
                                } else if row_resp.clicked() && !x_hovered {
                                    open_path = Some(entry.path.clone());
                                }
                            }
                            if let Some(path) = remove_path {
                                self.recent_files = recent_files::remove(&path);
                            }
                        });
                    if let Some(path) = open_path {
                        self.pending_open_recent = Some(path);
                    }
                }
            }
        });
    }
}
