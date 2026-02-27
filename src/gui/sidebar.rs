use eframe::egui;

use practical_arcana_painter::types::{
    BackgroundMode, Layer, NormalMode, PaintValues, ResolutionPreset,
};

use super::state::AppState;

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
        use egui_phosphor::fill::{ARROW_CLOCKWISE, FOLDER_OPEN, X};

        let label_w = 48.0;
        let icon_w = LAYER_ICON_SIZE;
        let row_h = LAYER_ICON_SIZE + 2.0;
        let gap = 2.0;
        let max_icons_w = icon_w * 3.0 + gap * 2.0;
        let content_w = ui.available_width();
        let text_w = (content_w - label_w - gap - max_icons_w - gap).max(30.0);

            // ── Mesh row ──
            let mesh_text = if let Some(ref mesh) = state.loaded_mesh {
                let name = short_filename(&state.project.mesh_ref.path);
                format!("{} ({} grp)", name, mesh.groups.len())
            } else {
                "(none)".to_string()
            };
            let has_mesh = state.loaded_mesh.is_some();
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.spacing_mut().item_spacing.x = gap;
                { let (r, _) = ui.allocate_exact_size(egui::Vec2::new(label_w, row_h), egui::Sense::hover());
                    ui.painter().text(egui::Pos2::new(r.min.x, r.center().y), egui::Align2::LEFT_CENTER, "Mesh", egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color()); }
                ui.add(
                    egui::TextEdit::singleline(&mut mesh_text.clone())
                        .desired_width(text_w)
                        .interactive(false),
                );
                let (r_reload, resp_reload) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_reload, ARROW_CLOCKWISE, has_mesh, resp_reload.hovered());
                if resp_reload.on_hover_text("Reload mesh").clicked() && has_mesh {
                    state.pending_reload_mesh = true;
                }
                let (r_replace, resp_replace) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_replace, FOLDER_OPEN, true, resp_replace.hovered());
                if resp_replace.on_hover_text("Replace mesh...").clicked() {
                    state.pending_replace_mesh = true;
                }
            });

            // ── Color row ──
            let color_text = if let Some(tex_path) = state.project.base_color.texture_path() {
                short_filename(tex_path).to_string()
            } else {
                "(solid)".to_string()
            };
            let has_texture = state.project.base_color.texture_path().is_some();
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.spacing_mut().item_spacing.x = gap;
                { let (r, _) = ui.allocate_exact_size(egui::Vec2::new(label_w, row_h), egui::Sense::hover());
                    ui.painter().text(egui::Pos2::new(r.min.x, r.center().y), egui::Align2::LEFT_CENTER, "Color", egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color()); }
                ui.add(
                    egui::TextEdit::singleline(&mut color_text.clone())
                        .desired_width(text_w)
                        .interactive(false),
                );
                let (r_reload, resp_reload) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_reload, ARROW_CLOCKWISE, has_texture, resp_reload.hovered());
                if resp_reload.on_hover_text("Reload texture").clicked() && has_texture {
                    state.pending_reload_texture = true;
                }
                let (r_load, resp_load) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_load, FOLDER_OPEN, true, resp_load.hovered());
                if resp_load.on_hover_text("Load texture...").clicked() {
                    state.pending_load_texture = true;
                }
            });

            // ── Normal row ──
            let has_normal = state.project.base_normal.is_some();
            let normal_text = if let Some(ref normal_path) = state.project.base_normal {
                short_filename(normal_path).to_string()
            } else {
                "(none)".to_string()
            };
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.spacing_mut().item_spacing.x = gap;
                { let (r, _) = ui.allocate_exact_size(egui::Vec2::new(label_w, row_h), egui::Sense::hover());
                    ui.painter().text(egui::Pos2::new(r.min.x, r.center().y), egui::Align2::LEFT_CENTER, "Normal", egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color()); }
                ui.add(
                    egui::TextEdit::singleline(&mut normal_text.clone())
                        .desired_width(text_w)
                        .interactive(false),
                );
                let (r_reload, resp_reload) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_reload, ARROW_CLOCKWISE, has_normal, resp_reload.hovered());
                if resp_reload.on_hover_text("Reload normal").clicked() && has_normal {
                    state.pending_reload_normal = true;
                }
                let tip = if has_normal { "Replace normal..." } else { "Load normal..." };
                let (r_load, resp_load) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                draw_layer_icon(ui.painter(), ui, r_load, FOLDER_OPEN, true, resp_load.hovered());
                if resp_load.on_hover_text(tip).clicked() {
                    state.pending_load_normal = true;
                }
                if has_normal {
                    let (r_clear, resp_clear) = ui.allocate_exact_size(egui::Vec2::new(icon_w, row_h), egui::Sense::click());
                    draw_layer_icon(ui.painter(), ui, r_clear, X, true, resp_clear.hovered());
                    if resp_clear.on_hover_text("Clear normal").clicked() {
                        state.project.base_normal = None;
                        state.loaded_normal = None;
                        state.dirty = true;
                    }
                }
            });
    });

    ui.separator();

    // ── Project Settings ──
    section_header(ui, "Settings");
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
        ui.add(
            egui::Slider::new(&mut state.project.settings.normal_strength, 0.0..=1.0)
                .text("Normal Strength"),
        );
    });

    ui.separator();
}

/// Draw the layers header: "Layers" label + [+] button (pinned above scroll).
pub fn show_layers_header(ui: &mut egui::Ui, state: &mut AppState) {
    use egui_phosphor::fill::PLUS;

    ui.horizontal(|ui: &mut egui::Ui| {
        let font = egui::FontId::proportional(14.0);
        ui.label(egui::RichText::new("Layers").font(font).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui: &mut egui::Ui| {
            let has_mesh = state.loaded_mesh.is_some();
            let size = egui::Vec2::splat(20.0);
            let (btn_rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
            if ui.is_rect_visible(btn_rect) {
                draw_layer_icon(ui.painter(), ui, btn_rect, PLUS, has_mesh, resp.hovered());
            }
            if resp.on_hover_text("Add Layer").clicked() && has_mesh {
                state.project.layers.push(Layer {
                    name: "__all__".to_string(),
                    visible: true,
                    group_name: "__all__".to_string(),
                    order: 0, // bottom of stack; all orders reassigned below
                    paint: PaintValues::default(),
                    guides: vec![],
                });
                // Reassign: index 0 (top of UI) = highest order (painted last = on top)
                let n = state.project.layers.len() as i32;
                for (i, layer) in state.project.layers.iter_mut().enumerate() {
                    layer.order = n - 1 - i as i32;
                }
                state.selected_layer = Some(state.project.layers.len() - 1);
                state.selected_guide = None;
            }
        });
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

            let (rect, _) = ui.allocate_exact_size(
                egui::Vec2::new(row_w, row_h),
                egui::Sense::hover(),
            );

            if !ui.is_rect_visible(rect) {
                continue;
            }

            let p = ui.painter();

            // Selected background
            if selected {
                p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
            }

            // ── Eye icon (left, full row height) ──
            let eye_rect = egui::Rect::from_min_size(
                rect.min,
                egui::Vec2::new(LAYER_ICON_SIZE, row_h),
            );
            let eye_id = ui.id().with(("layer_eye", i));
            let eye_resp = ui.interact(eye_rect, eye_id, egui::Sense::click());
            {
                let eye_icon = if visible { EYE } else { EYE_SLASH };
                if eye_resp.hovered() {
                    p.rect_filled(eye_rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
                }
                let color = if visible {
                    if eye_resp.hovered() { ui.visuals().text_color() } else { ui.visuals().weak_text_color() }
                } else {
                    ui.visuals().weak_text_color().gamma_multiply(0.5)
                };
                p.text(eye_rect.center(), egui::Align2::CENTER_CENTER, eye_icon, egui::FontId::proportional(13.0), color);
            }
            if eye_resp.on_hover_text("Toggle visibility").clicked() {
                state.project.layers[i].visible = !visible;
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
                draw_layer_icon(p, ui, down_rect, ARROW_DOWN, can_down, down_resp.hovered());
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
                draw_layer_icon(p, ui, up_rect, ARROW_UP, i > 0, up_resp.hovered());
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
                draw_layer_icon(p, ui, del_rect, TRASH_SIMPLE, true, del_resp.hovered());
                if del_resp.on_hover_text("Delete layer").clicked() {
                    delete_idx = Some(i);
                }
            }

            // ── Name label (middle, clickable) ──
            let name_left = rect.min.x + LAYER_ICON_SIZE + icon_gap;
            let name_right = if selected { actions_right - icon_gap } else { rect.max.x };
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
                let galley = p.layout_no_wrap(label, font_id.clone(), text_color);
                let text_y = rect.center().y - galley.size().y * 0.5;
                if galley.size().x > max_text_w {
                    let ell = p.layout_no_wrap("\u{2026}".to_string(), font_id, text_color);
                    let ell_w = ell.size().x;
                    let clip = egui::Rect::from_min_size(
                        egui::Pos2::new(name_left + 2.0, rect.min.y),
                        egui::Vec2::new(max_text_w - ell_w, rect.height()),
                    );
                    p.with_clip_rect(clip).galley(egui::Pos2::new(name_left + 2.0, text_y), galley, text_color);
                    p.galley(egui::Pos2::new(name_left + 2.0 + max_text_w - ell_w, text_y), ell, text_color);
                } else {
                    p.galley(egui::Pos2::new(name_left + 2.0, text_y), galley, text_color);
                }
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
        }
    }

    }); // indent
}

/// Draw the bottom-pinned section: Seed + Generate button.
pub fn show_bottom(ui: &mut egui::Ui, state: &mut AppState) {
    // ── Seed ──
    ui.add_space(4.0);
    ui.horizontal(|ui: &mut egui::Ui| {
        ui.label("Seed:");
        let seed_text = seed_to_alpha(state.project.settings.seed);
        // TextEdit fills remaining space minus Shuffle button width
        let button_width = 60.0;
        let text_width = (ui.available_width() - button_width - ui.spacing().item_spacing.x).max(40.0);
        ui.add(
            egui::TextEdit::singleline(&mut seed_text.clone())
                .desired_width(text_width)
                .font(egui::TextStyle::Monospace)
                .interactive(false),
        );
        if ui.button("Shuffle").clicked() {
            state.project.settings.seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| (d.as_millis() & 0xFFFFFFFF) as u32)
                .unwrap_or(1234);
        }
    });

    ui.add_space(4.0);

    // ── Generate ──
    let running = state.generation.is_running();
    let stale = state.stale_reason();
    let (label, text_color, fill) = if running {
        (
            "Generating...".to_string(),
            egui::Color32::from_gray(160),
            egui::Color32::from_gray(50),
        )
    } else if let Some(reason) = stale {
        (
            format!("Generate ({reason})"),
            egui::Color32::from_rgb(255, 220, 80),
            egui::Color32::from_rgb(80, 65, 20),
        )
    } else {
        (
            "Generate".to_string(),
            egui::Color32::WHITE,
            egui::Color32::from_gray(60),
        )
    };
    let btn = ui.scope(|ui: &mut egui::Ui| {
        ui.visuals_mut().widgets.hovered.expansion = 0.0;
        ui.visuals_mut().widgets.active.expansion = 0.0;
        let button = egui::Button::new(egui::RichText::new(&label).color(text_color))
            .min_size(egui::Vec2::new(ui.available_width(), 36.0))
            .fill(fill);
        ui.add(button)
    }).inner;
    if btn.on_hover_text("⌘G").clicked() && !running {
        state.pending_generate = true;
    }
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

/// Convert a u32 seed to a fixed 6-letter uppercase string (base-26).
fn seed_to_alpha(mut seed: u32) -> String {
    let mut chars = [b'A'; 6];
    for c in chars.iter_mut().rev() {
        *c = b'A' + (seed % 26) as u8;
        seed /= 26;
    }
    String::from_utf8_lossy(&chars).into_owned()
}

fn short_filename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

const LAYER_ICON_SIZE: f32 = 18.0;

/// Draw a small icon with hover highlight (used within manual layer row painting).
fn draw_layer_icon(
    p: &egui::Painter,
    ui: &egui::Ui,
    rect: egui::Rect,
    icon: &str,
    active: bool,
    hovered: bool,
) {
    if hovered && active {
        p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let color = if !active {
        ui.visuals().weak_text_color().gamma_multiply(0.4)
    } else if hovered {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(13.0),
        color,
    );
}
