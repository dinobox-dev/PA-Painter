use eframe::egui;

use pa_painter::types::{
    BackgroundMode, Layer, NormalMode, PaintValues, ResolutionPreset, TextureSource,
};

use super::state::AppState;
use super::widgets::{paint_icon, paint_truncated_text, slider_row};

pub const SECTION_INDENT: f32 = 8.0;

/// Draw a section header label with a slightly larger font.
pub fn section_header(ui: &mut egui::Ui, label: &str) {
    let font = egui::FontId::proportional(14.0);
    ui.add_space(2.0);
    ui.label(egui::RichText::new(label).font(font).strong());
    ui.add_space(1.0);
}

/// Draw the fixed top section: Base + Project Settings.
pub fn show_top(ui: &mut egui::Ui, state: &mut AppState) {
    ui.set_max_width(ui.available_width());

    ui.spacing_mut().indent = SECTION_INDENT;

    // ── Base (Mesh, Color, Normal — one row each) ──
    section_header(ui, "Base");
    ui.indent("base_content", |ui: &mut egui::Ui| {
        let label_w = 48.0;
        let icon_w = LAYER_ICON_SIZE;
        let row_h = LAYER_ICON_SIZE + 2.0;
        let icon_gap = 2.0;
        let row_w = ui.available_width();
        let font_id = egui::TextStyle::Body.resolve(ui.style());

        // Helper: draw a base-row with fixed width (same pattern as layer rows).
        // Icons are positioned from the right edge inward; the text label
        // fills the remaining space, shrinking when extra icons appear.
        struct BaseRowIcons {
            reload: bool, // only drawn when `true`
            open: bool,   // always present; `true` = enabled
            clear: bool,  // only drawn when `true`
        }

        let draw_base_row = |ui: &mut egui::Ui,
                             label: &str,
                             display_text: &str,
                             icons: BaseRowIcons,
                             reload_tip: &str,
                             open_tip: &str,
                             clear_tip: &str|
         -> (bool, bool, bool) {
            let (rect, _) =
                ui.allocate_exact_size(egui::Vec2::new(row_w, row_h), egui::Sense::hover());

            let mut clicked_reload = false;
            let mut clicked_open = false;
            let mut clicked_clear = false;

            if !ui.is_rect_visible(rect) {
                return (false, false, false);
            }

            let p = ui.painter();

            // ── Icons from right edge ──
            let mut icons_left = rect.max.x;

            // Clear icon (rightmost, only when present)
            if icons.clear {
                icons_left -= icon_w;
                let r = egui::Rect::from_min_size(
                    egui::Pos2::new(icons_left, rect.min.y),
                    egui::Vec2::new(icon_w, row_h),
                );
                let id = ui.id().with((label, "clear"));
                let resp = ui.interact(r, id, egui::Sense::click());
                paint_icon(p, ui, r, egui_phosphor::fill::X, 13.0, true, resp.hovered());
                if resp.on_hover_text(clear_tip).clicked() {
                    clicked_clear = true;
                }
                icons_left -= icon_gap;
            }

            // Open / Replace icon
            icons_left -= icon_w;
            {
                let r = egui::Rect::from_min_size(
                    egui::Pos2::new(icons_left, rect.min.y),
                    egui::Vec2::new(icon_w, row_h),
                );
                let id = ui.id().with((label, "open"));
                let resp = ui.interact(r, id, egui::Sense::click());
                paint_icon(
                    p,
                    ui,
                    r,
                    egui_phosphor::fill::FOLDER_OPEN,
                    13.0,
                    icons.open,
                    resp.hovered(),
                );
                if resp.on_hover_text(open_tip).clicked() && icons.open {
                    clicked_open = true;
                }
            }

            // Reload icon (only when resource is loaded)
            if icons.reload {
                icons_left -= icon_gap + icon_w;
                let r = egui::Rect::from_min_size(
                    egui::Pos2::new(icons_left, rect.min.y),
                    egui::Vec2::new(icon_w, row_h),
                );
                let id = ui.id().with((label, "reload"));
                let resp = ui.interact(r, id, egui::Sense::click());
                paint_icon(
                    p,
                    ui,
                    r,
                    egui_phosphor::fill::ARROW_CLOCKWISE,
                    13.0,
                    true,
                    resp.hovered(),
                );
                if resp.on_hover_text(reload_tip).clicked() {
                    clicked_reload = true;
                }
            }

            // ── Label (left) ──
            p.text(
                egui::Pos2::new(rect.min.x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                font_id.clone(),
                ui.visuals().text_color(),
            );

            // ── Display text (middle, clipped) ──
            let text_left = rect.min.x + label_w + icon_gap;
            let text_right = icons_left - icon_gap;
            let max_text_w = (text_right - text_left).max(10.0);
            let text_color = ui.visuals().weak_text_color();
            paint_truncated_text(
                p,
                display_text,
                font_id.clone(),
                text_color,
                text_left,
                rect,
                max_text_w,
            );

            (clicked_reload, clicked_open, clicked_clear)
        };

        // ── Mesh row ──
        let mesh_text = if state.loaded_mesh.is_some() {
            let from_path = short_filename(&state.project.mesh_ref.path);
            if from_path.is_empty() {
                // .papr load: path is runtime-only, fall back to persisted filename
                if state.project.mesh_ref.filename.is_empty() {
                    "(embedded)".to_string()
                } else {
                    state.project.mesh_ref.filename.clone()
                }
            } else {
                from_path.to_string()
            }
        } else {
            "(none)".to_string()
        };
        let has_mesh = state.loaded_mesh.is_some();
        let (reload, open, _) = draw_base_row(
            ui,
            "Mesh",
            &mesh_text,
            BaseRowIcons {
                reload: has_mesh,
                open: true,
                clear: false,
            },
            "Reload mesh",
            "Replace mesh...",
            "",
        );
        if reload {
            state.pending_reload_mesh = true;
        }
        if open {
            state.pending_replace_mesh = true;
        }

        // ── Mesh info (read-only) ──
        if let Some(ref mesh) = state.loaded_mesh {
            let info_color = ui.visuals().weak_text_color();
            let info_font = egui::FontId::proportional(11.0);
            let info_label_w = 64.0;

            let info_row = |ui: &mut egui::Ui, label: &str, value: &str| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::new(row_w, 14.0), egui::Sense::hover());
                if ui.is_rect_visible(rect) {
                    let p = ui.painter();
                    p.text(
                        egui::Pos2::new(rect.min.x, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        info_font.clone(),
                        info_color,
                    );
                    p.text(
                        egui::Pos2::new(rect.min.x + info_label_w, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        value,
                        info_font.clone(),
                        info_color,
                    );
                }
            };

            info_row(ui, "Vertices", &fmt_thousands(mesh.positions.len()));
            info_row(ui, "Triangles", &fmt_thousands(mesh.indices.len() / 3));
            info_row(ui, "Groups", &mesh.groups.len().to_string());

            let n_textures = mesh
                .materials
                .iter()
                .filter(|m| m.base_color_texture.is_some())
                .count();
            let n_normals = mesh
                .materials
                .iter()
                .filter(|m| m.normal_texture.is_some())
                .count();
            if n_textures > 0 || n_normals > 0 {
                info_row(ui, "Textures", &n_textures.to_string());
                info_row(ui, "Normals", &n_normals.to_string());
            }
        }
    });

    ui.separator();

    // ── Project Settings ──
    section_header(ui, "Settings");
    let old_settings = state.project.settings.clone();
    ui.indent("settings_content", |ui: &mut egui::Ui| {
        let combo_w = 110.0;

        // Resolution preset
        let current_res = state.project.settings.resolution_preset;
        egui::ComboBox::from_label("Resolution")
            .width(combo_w)
            .selected_text(format!("{}px", current_res.resolution()))
            .show_ui(ui, |ui: &mut egui::Ui| {
                for &preset in &[
                    ResolutionPreset::Preview,
                    ResolutionPreset::Standard,
                    ResolutionPreset::High,
                    ResolutionPreset::Ultra,
                ] {
                    ui.selectable_value(
                        &mut state.project.settings.resolution_preset,
                        preset,
                        format!("{}px", preset.resolution()),
                    );
                }
            });

        // Normal mode
        egui::ComboBox::from_label("Normal Mode")
            .width(combo_w)
            .selected_text(match state.project.settings.normal_mode {
                NormalMode::SurfacePaint => "SurfacePaint",
                NormalMode::DepictedForm => "DepictedForm",
            })
            .show_ui(ui, |ui: &mut egui::Ui| {
                ui.selectable_value(
                    &mut state.project.settings.normal_mode,
                    NormalMode::SurfacePaint,
                    "SurfacePaint",
                );
                ui.selectable_value(
                    &mut state.project.settings.normal_mode,
                    NormalMode::DepictedForm,
                    "DepictedForm",
                );
            });

        // Background mode
        egui::ComboBox::from_label("Background")
            .width(combo_w)
            .selected_text(match state.project.settings.background_mode {
                BackgroundMode::Opaque => "Opaque",
                BackgroundMode::Transparent => "Transparent",
            })
            .show_ui(ui, |ui: &mut egui::Ui| {
                ui.selectable_value(
                    &mut state.project.settings.background_mode,
                    BackgroundMode::Opaque,
                    "Opaque",
                );
                ui.selectable_value(
                    &mut state.project.settings.background_mode,
                    BackgroundMode::Transparent,
                    "Transparent",
                );
            });

        // Normal strength
        slider_row(
            ui,
            "normal_strength",
            &mut state.project.settings.normal_strength,
            0.0..=1.0,
            "Normal Strength",
            None,
            2,
        );
    });

    // Type A remerge: normal_mode, background_mode, normal_strength changed
    // Resolution changes require full re-render (different global hash).
    {
        let s = &state.project.settings;
        if s.normal_mode != old_settings.normal_mode
            || s.background_mode != old_settings.background_mode
            || s.normal_strength != old_settings.normal_strength
        {
            state.pending_remerge = true;
        }
    }

    ui.separator();
}

/// Draw the layers header: "Layers" label + [+] button (pinned above scroll).
pub fn show_layers_header(ui: &mut egui::Ui, state: &mut AppState) {
    use egui_phosphor::fill::PLUS;

    ui.horizontal(|ui: &mut egui::Ui| {
        let font = egui::FontId::proportional(14.0);
        ui.label(egui::RichText::new("Layers").font(font).strong());
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui: &mut egui::Ui| {
                let has_mesh = state.loaded_mesh.is_some();
                let size = egui::Vec2::splat(20.0);
                let (btn_rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
                if ui.is_rect_visible(btn_rect) {
                    paint_icon(
                        ui.painter(),
                        ui,
                        btn_rect,
                        PLUS,
                        13.0,
                        has_mesh,
                        resp.hovered(),
                    );
                }
                if resp.on_hover_text("Add Layer").clicked() && has_mesh {
                    let next_seed = state.project.layers.len() as u32;
                    state.project.layers.insert(
                        0,
                        Layer {
                            name: "__all__".to_string(),
                            visible: true,
                            group_name: "__all__".to_string(),
                            order: 0, // reassigned below
                            paint: PaintValues::default(),
                            guides: vec![],
                            base_color: TextureSource::Solid([1.0, 1.0, 1.0]),
                            base_normal: TextureSource::None,
                            dry: 1.0,
                            seed: next_seed,
                        },
                    );
                    // Reassign: index 0 (top of UI) = highest order (painted last = on top)
                    let n = state.project.layers.len() as i32;
                    for (i, layer) in state.project.layers.iter_mut().enumerate() {
                        layer.order = n - 1 - i as i32;
                    }
                    state.selected_layer = Some(0);
                    state.selected_guide = None;
                }
            },
        );
    });
}

/// Draw the scrollable layer rows.
pub fn show_layer_rows(ui: &mut egui::Ui, state: &mut AppState) {
    ui.set_max_width(ui.available_width());
    ui.spacing_mut().indent = SECTION_INDENT;

    ui.indent("layers_rows", |ui: &mut egui::Ui| {
        use egui_phosphor::fill::{ARROW_DOWN, ARROW_UP, EYE, EYE_SLASH, TRASH_SIMPLE};

        // Layer rows
        if state.project.layers.is_empty() {
            ui.label("No layers.");
        } else {
            let mut delete_idx: Option<usize> = None;
            let mut swap: Option<(usize, usize)> = None;
            let row_w = ui.available_width();
            let row_h = LAYER_ICON_SIZE + 2.0;
            let icon_gap = 2.0;
            let n_layers = state.project.layers.len();

            for i in 0..n_layers {
                let selected = state.selected_layer == Some(i);
                let visible = state.project.layers[i].visible;
                let name = state.project.layers[i].name.clone();
                let group = state.project.layers[i].group_name.clone();

                let (rect, _) =
                    ui.allocate_exact_size(egui::Vec2::new(row_w, row_h), egui::Sense::hover());

                if !ui.is_rect_visible(rect) {
                    continue;
                }

                let p = ui.painter();

                // Selected background
                if selected {
                    p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                }

                // ── Eye icon (left, full row height) ──
                let eye_rect =
                    egui::Rect::from_min_size(rect.min, egui::Vec2::new(LAYER_ICON_SIZE, row_h));
                let eye_id = ui.id().with(("layer_eye", i));
                let eye_resp = ui.interact(eye_rect, eye_id, egui::Sense::click());
                {
                    let eye_icon = if visible { EYE } else { EYE_SLASH };
                    if eye_resp.hovered() {
                        p.rect_filled(eye_rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
                    }
                    let color = if visible {
                        if eye_resp.hovered() {
                            ui.visuals().text_color()
                        } else {
                            ui.visuals().weak_text_color()
                        }
                    } else {
                        ui.visuals().weak_text_color().gamma_multiply(0.5)
                    };
                    p.text(
                        eye_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        eye_icon,
                        egui::FontId::proportional(13.0),
                        color,
                    );
                }
                if eye_resp.on_hover_text("Toggle visibility").clicked() {
                    state.project.layers[i].visible = !visible;
                    state.pending_remerge = true;
                }

                // ── Action icons (right, selected only) ──
                let mut actions_right = rect.max.x;

                if selected {
                    // Down arrow (rightmost)
                    let can_down = i + 1 < n_layers;
                    actions_right -= LAYER_ICON_SIZE;
                    let down_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(actions_right, rect.min.y),
                        egui::Vec2::new(LAYER_ICON_SIZE, row_h),
                    );
                    let down_id = ui.id().with(("layer_down", i));
                    let down_resp = ui.interact(down_rect, down_id, egui::Sense::click());
                    paint_icon(
                        p,
                        ui,
                        down_rect,
                        ARROW_DOWN,
                        13.0,
                        can_down,
                        down_resp.hovered(),
                    );
                    if down_resp.on_hover_text("Move down").clicked() && can_down {
                        swap = Some((i, i + 1));
                    }

                    // Up arrow
                    actions_right -= icon_gap + LAYER_ICON_SIZE;
                    let up_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(actions_right, rect.min.y),
                        egui::Vec2::new(LAYER_ICON_SIZE, row_h),
                    );
                    let up_id = ui.id().with(("layer_up", i));
                    let up_resp = ui.interact(up_rect, up_id, egui::Sense::click());
                    paint_icon(p, ui, up_rect, ARROW_UP, 13.0, i > 0, up_resp.hovered());
                    if up_resp.on_hover_text("Move up").clicked() && i > 0 {
                        swap = Some((i, i - 1));
                    }

                    // Delete
                    actions_right -= icon_gap + LAYER_ICON_SIZE;
                    let del_rect = egui::Rect::from_min_size(
                        egui::Pos2::new(actions_right, rect.min.y),
                        egui::Vec2::new(LAYER_ICON_SIZE, row_h),
                    );
                    let del_id = ui.id().with(("layer_del", i));
                    let del_resp = ui.interact(del_rect, del_id, egui::Sense::click());
                    paint_icon(
                        p,
                        ui,
                        del_rect,
                        TRASH_SIMPLE,
                        13.0,
                        true,
                        del_resp.hovered(),
                    );
                    if del_resp.on_hover_text("Delete layer").clicked() {
                        delete_idx = Some(i);
                    }
                }

                // ── Name label (middle, clickable) ──
                let name_left = rect.min.x + LAYER_ICON_SIZE + icon_gap;
                let name_right = if selected {
                    actions_right - icon_gap
                } else {
                    rect.max.x
                };
                let name_rect = egui::Rect::from_min_max(
                    egui::Pos2::new(name_left, rect.min.y),
                    egui::Pos2::new(name_right, rect.max.y),
                );
                let name_id = ui.id().with(("layer_name", i));
                let name_resp = ui.interact(name_rect, name_id, egui::Sense::click());
                {
                    if !selected && name_resp.hovered() {
                        p.rect_filled(name_rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
                    }
                    let text_color = if selected {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().text_color()
                    };
                    let label = format!("{} ({})", name, group);
                    let font_id = egui::TextStyle::Body.resolve(ui.style());
                    let max_text_w = (name_right - name_left - 4.0).max(10.0);
                    paint_truncated_text(
                        p,
                        &label,
                        font_id,
                        text_color,
                        name_left + 2.0,
                        rect,
                        max_text_w,
                    );
                }
                if name_resp.clicked() {
                    if selected {
                        state.selected_layer = None;
                    } else {
                        state.selected_layer = Some(i);
                    }
                    state.selected_guide = None;
                }
            }

            // Apply deferred actions
            let mut structure_changed = false;
            if let Some(idx) = delete_idx {
                state.project.layers.remove(idx);
                state.selected_guide = None;
                if state.project.layers.is_empty() {
                    state.selected_layer = None;
                } else {
                    state.selected_layer = Some(idx.min(state.project.layers.len() - 1));
                }
                structure_changed = true;
            }
            if let Some((a, b)) = swap {
                state.project.layers.swap(a, b);
                state.selected_layer = Some(b);
                structure_changed = true;
            }
            // Keep order fields in sync: index 0 (top of UI) = highest order (painted last = on top)
            if structure_changed {
                let n = state.project.layers.len() as i32;
                for (i, layer) in state.project.layers.iter_mut().enumerate() {
                    layer.order = n - 1 - i as i32;
                }
                state.pending_remerge = true;
            }
        }
    }); // indent
}

/// Draw the bottom-pinned section: Export buttons + thin progress bar underneath.
pub fn show_bottom(ui: &mut egui::Ui, state: &mut AppState) {
    ui.add_space(8.0);
    // ── Export buttons ──
    let has_result = state.generated.is_some();
    let stale = has_result && state.stale_reason().is_some();

    let total_width = ui.available_width();
    let btn_h = 40.0;
    let gear_w = btn_h;
    let btn_size = egui::Vec2::new(total_width - gear_w, btn_h);

    ui.horizontal(|ui| {
        ui.set_width(total_width);
        ui.set_height(btn_h);
        ui.spacing_mut().item_spacing.x = 0.0;

        // Disable hover expansion for buttons in this bar
        ui.visuals_mut().widgets.hovered.expansion = 0.0;
        ui.visuals_mut().widgets.active.expansion = 0.0;

        // Progress bar background (gray — must contrast with the filled portion)
        let bar_bg = ui.visuals().widgets.inactive.bg_fill;
        // Button fill when not busy (same hue as the progress bar filled portion)
        let btn_fill = ui.visuals().widgets.inactive.weak_bg_fill;
        let r: u8 = 3; // match egui default button rounding
        let left_rounding = egui::CornerRadius {
            nw: r,
            ne: 0,
            sw: r,
            se: 0,
        };
        let right_rounding = egui::CornerRadius {
            nw: 0,
            ne: r,
            sw: 0,
            se: r,
        };

        // ── Export (left zone) — doubles as progress bar during generation/remerge/export ──
        let generating = state.generation.is_running();
        let remerging = state.remerge_running;
        let exporting = state.export_worker.is_running();
        let busy = generating || remerging || exporting;
        let (export_rect, export_resp) =
            ui.allocate_exact_size(btn_size, egui::Sense::click() | egui::Sense::hover());

        let accent = egui::Color32::from_rgb(45, 120, 220);
        let accent_hover = egui::Color32::from_rgb(60, 140, 240);

        if busy {
            // Progress bar mode
            let in_path_stage =
                generating && state.generation.stage() == super::generation::STAGE_PATHS;
            let (progress, label) = if generating {
                let p = state.generation.overall_progress();
                if in_path_stage {
                    (p, "Placing strokes…".to_string())
                } else {
                    (p, format!("Generating… {:.0}%", p * 100.0))
                }
            } else if exporting {
                let p = state.export_worker.progress();
                let (cur, tot) = state.export_worker.steps();
                (p, format!("Exporting… {cur}/{tot}"))
            } else {
                let p = state.remerge_progress;
                (p, format!("Applying… {:.0}%", p * 100.0))
            };
            ui.painter().rect_filled(export_rect, left_rounding, bar_bg);

            // Filled portion
            if progress > 0.0 {
                let fill_width = export_rect.width() * progress;
                let fill_rect = egui::Rect::from_min_size(
                    export_rect.min,
                    egui::Vec2::new(fill_width, export_rect.height()),
                );
                let fill_rounding = if progress > 0.99 {
                    left_rounding
                } else {
                    egui::CornerRadius {
                        ne: 0,
                        se: 0,
                        ..left_rounding
                    }
                };
                let accent = ui.visuals().selection.bg_fill;
                ui.painter().rect_filled(fill_rect, fill_rounding, accent);
            }

            // Label
            ui.painter().text(
                export_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::TextStyle::Button.resolve(ui.style()),
                ui.visuals().widgets.inactive.fg_stroke.color,
            );
        } else {
            // Normal export button
            let fill = if !has_result {
                btn_fill
            } else if export_resp.hovered() {
                accent_hover
            } else {
                accent
            };
            ui.painter().rect_filled(export_rect, left_rounding, fill);
            let text_color = if !has_result {
                ui.visuals().widgets.noninteractive.fg_stroke.color
            } else {
                egui::Color32::WHITE
            };
            ui.painter().text(
                export_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Export",
                egui::TextStyle::Button.resolve(ui.style()),
                text_color,
            );
            if has_result && export_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if has_result && !state.export_worker.is_running() && export_resp.clicked() {
                state.pending_export = true;
            }
            if stale && export_resp.hovered() {
                export_resp
                    .on_hover_text("Result is outdated — parameters changed since last generation");
            }
        }

        // ── ⚙ gear (right zone, always clickable) ──
        let (gear_rect, gear_resp) = ui.allocate_exact_size(
            egui::Vec2::new(gear_w, btn_h),
            egui::Sense::click() | egui::Sense::hover(),
        );
        if gear_resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        {
            let fill = if has_result && !busy {
                if gear_resp.hovered() {
                    accent_hover
                } else {
                    accent
                }
            } else if gear_resp.hovered() {
                ui.visuals().widgets.hovered.bg_fill
            } else {
                btn_fill
            };
            ui.painter().rect_filled(gear_rect, right_rounding, fill);
            let text_color = if has_result && !busy {
                egui::Color32::WHITE
            } else if gear_resp.hovered() {
                ui.visuals().widgets.hovered.fg_stroke.color
            } else {
                ui.visuals().widgets.inactive.fg_stroke.color
            };
            ui.painter().text(
                gear_rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2699}",
                egui::TextStyle::Body.resolve(ui.style()),
                text_color,
            );
        }
        if gear_resp.clicked() {
            if state.show_export_settings {
                state.show_export_settings = false;
                state.export_settings_draft = None;
            } else {
                state.export_settings_draft = Some(state.project.export_settings.clone());
                state.show_export_settings = true;
            }
        }
    });
    ui.add_space(2.0);
}

pub fn build_group_names(state: &AppState) -> Vec<String> {
    let mut names = vec!["__all__".to_string()];
    if let Some(ref mesh) = state.loaded_mesh {
        for group in &mesh.groups {
            if !names.contains(&group.name) {
                names.push(group.name.clone());
            }
        }
    }
    names
}

fn short_filename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Format a number with thousands separators (e.g. 12450 → "12,450").
pub fn fmt_thousands(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(c);
    }
    result
}

const LAYER_ICON_SIZE: f32 = 18.0;
