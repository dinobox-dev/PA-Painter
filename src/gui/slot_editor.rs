use std::sync::Arc;

use eframe::egui;

use pa_painter::mesh::asset_io;
use pa_painter::types::{
    CurveKnot, EmbeddedTexture, PaintPreset, PaintValues, PresetLibrary, PressureCurve,
    TextureSource,
};
use pa_painter::util::pressure::{evaluate_pressure, preset_to_custom};

use super::preview::StrokePreviewCache;

use super::preview;
use super::sidebar::{build_group_names, section_header, SECTION_INDENT};
use super::state::AppState;
use super::widgets::{paint_icon, paint_truncated_text, slider_row, small_icon_button};

/// Draw the right-panel layer editor for the currently selected layer.
/// Returns early if no layer is selected.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(idx) = state.selected_layer else {
        return;
    };
    if idx >= state.project.layers.len() {
        return;
    }

    let group_names = build_group_names(state);
    ui.spacing_mut().indent = SECTION_INDENT;
    let old_dry = state.project.layers[idx].dry;

    // ── Layer ──
    section_header(ui, "Layer");
    ui.indent("layer_content", |ui: &mut egui::Ui| {
        egui::Grid::new("layer_info_grid")
            .num_columns(2)
            .spacing([6.0, 4.0])
            .show(ui, |ui: &mut egui::Ui| {
                // Name
                ui.label("Name");
                ui.add(
                    egui::TextEdit::singleline(&mut state.project.layers[idx].name)
                        .desired_width(ui.available_width()),
                );
                ui.end_row();

                // Group
                ui.label("Group");
                {
                    let layer = &mut state.project.layers[idx];
                    let combo_w = ui.available_width();
                    egui::ComboBox::from_id_salt("layer_group_combo")
                        .selected_text(&layer.group_name)
                        .width(combo_w)
                        .show_ui(ui, |ui: &mut egui::Ui| {
                            for name in &group_names {
                                ui.selectable_value(
                                    &mut layer.group_name,
                                    name.clone(),
                                    name.as_str(),
                                );
                            }
                        });
                }
                ui.end_row();

                // Color source
                ui.label("Color");
                show_color_source_controls(ui, state, idx);
                ui.end_row();

                // Normal source
                ui.label("Normal");
                show_normal_source_controls(ui, state, idx);
                ui.end_row();

                // Seed
                ui.label("Seed");
                {
                    let layer = &mut state.project.layers[idx];
                    ui.horizontal(|ui: &mut egui::Ui| {
                        use egui_phosphor::fill::SHUFFLE;
                        const ICON_SIZE: f32 = 18.0;
                        const ICON_FONT: f32 = 13.0;

                        let margin = ui.spacing().button_padding.x * 2.0;
                        let text_w = (ui.available_width()
                            - ICON_SIZE
                            - ui.spacing().item_spacing.x
                            - margin)
                            .max(20.0);

                        let state_id = egui::Id::new("seed_edit_buf");
                        let stored: Option<String> = ui.data_mut(|d| d.get_temp(state_id));
                        let mut buf = stored.unwrap_or_else(|| seed_to_alpha(layer.seed));

                        let te_resp = ui.add(
                            egui::TextEdit::singleline(&mut buf)
                                .desired_width(text_w)
                                .font(egui::TextStyle::Monospace)
                                .char_limit(6),
                        );

                        // Select all on initial focus
                        if te_resp.gained_focus() {
                            if let Some(mut state) =
                                egui::TextEdit::load_state(ui.ctx(), te_resp.id)
                            {
                                state
                                    .cursor
                                    .set_char_range(Some(egui::text::CCursorRange::two(
                                        egui::text::CCursor::new(0),
                                        egui::text::CCursor::new(buf.len()),
                                    )));
                                state.store(ui.ctx(), te_resp.id);
                            }
                        }

                        // Filter: keep only A-Z, auto-uppercase
                        if te_resp.changed() {
                            buf = buf
                                .chars()
                                .filter(|c| c.is_ascii_alphabetic())
                                .map(|c| c.to_ascii_uppercase())
                                .collect();
                        }

                        if te_resp.lost_focus() {
                            if let Some(v) = alpha_to_seed(&buf) {
                                layer.seed = v;
                            }
                            ui.data_mut(|d| d.remove::<String>(state_id));
                        } else if te_resp.has_focus() {
                            ui.data_mut(|d| d.insert_temp(state_id, buf));
                        } else {
                            ui.data_mut(|d| d.remove::<String>(state_id));
                        }

                        if small_icon_button(ui, SHUFFLE, ICON_FONT, ICON_SIZE, true)
                            .on_hover_text("Shuffle")
                            .clicked()
                        {
                            use rand::Rng;
                            layer.seed = rand::thread_rng().gen_range(0..26u32.pow(6));
                            ui.data_mut(|d| d.remove::<String>(state_id));
                        }
                    });
                }
                ui.end_row();
            });
    });

    ui.separator();

    // ── Paint ──
    // Header row: "Paint" label + preset combo + save icon inline
    ui.add_space(2.0);
    ui.horizontal(|ui: &mut egui::Ui| {
        let font = egui::FontId::proportional(14.0);
        ui.label(egui::RichText::new("Paint").font(font).strong());
        // Reserve icon on the right first, then combo fills the rest.
        let spacing = ui.spacing().item_spacing.x;
        let icon_w = 20.0;
        let combo_w = (ui.available_width() - icon_w - spacing).max(40.0);
        show_preset_combo_sized(ui, state, idx, combo_w);
        show_save_preset_icon(ui, state, idx);
    });
    ui.add_space(1.0);

    ui.indent("paint_content", |ui: &mut egui::Ui| {
        let layer = &mut state.project.layers[idx];
        let layer_seed = layer.seed;
        let cache = &mut state.preview_cache;

        // Pressure curve + stroke preview (top)
        show_combined_stroke_curve(ui, &mut layer.paint, layer_seed, &mut cache.stroke);

        ui.add_space(4.0);

        ui.label(egui::RichText::new("Brush").weak());
        slider_row(
            ui,
            "brush_width",
            &mut layer.paint.brush_width,
            5.0..=100.0,
            "Brush Width",
            None,
            1,
        );
        slider_row(
            ui,
            "load",
            &mut layer.paint.load,
            0.0..=2.0,
            "Load",
            Some(0.01),
            2,
        );
        slider_row(
            ui,
            "body_wiggle",
            &mut layer.paint.body_wiggle,
            0.0..=0.5,
            "Body Wiggle",
            Some(0.01),
            2,
        );
        slider_row(
            ui,
            "viscosity",
            &mut layer.paint.viscosity,
            0.0..=1.0,
            "Viscosity",
            Some(0.01),
            2,
        );

        ui.add_space(4.0);
        ui.label(egui::RichText::new("Layout").weak());
        slider_row(
            ui,
            "stroke_spacing",
            &mut layer.paint.stroke_spacing,
            0.1..=3.0,
            "Spacing",
            Some(0.1),
            1,
        );
        slider_row(
            ui,
            "max_stroke_length",
            &mut layer.paint.max_stroke_length,
            10.0..=500.0,
            "Max Length",
            None,
            0,
        );
        slider_row(
            ui,
            "angle_variation",
            &mut layer.paint.angle_variation,
            0.0..=45.0,
            "Angle Var",
            None,
            1,
        );
        slider_row(
            ui,
            "max_turn_angle",
            &mut layer.paint.max_turn_angle,
            0.0..=90.0,
            "Max Turn",
            None,
            1,
        );
        slider_row(
            ui,
            "color_variation",
            &mut layer.paint.color_variation,
            0.0..=0.5,
            "Color Var",
            Some(0.01),
            2,
        );
        slider_row(
            ui,
            "dry",
            &mut layer.dry,
            0.0..=1.0,
            "Paint after Dry",
            Some(0.01),
            2,
        );

        let mut color_break_enabled = layer.paint.color_break_threshold.is_some();
        ui.checkbox(&mut color_break_enabled, "Color Break");
        if color_break_enabled {
            let val = layer.paint.color_break_threshold.get_or_insert(0.1);
            slider_row(ui, "color_break_thr", val, 0.01..=0.5, "Threshold", None, 2);
        } else {
            layer.paint.color_break_threshold = None;
        }

        let mut normal_break_enabled = layer.paint.normal_break_threshold.is_some();
        ui.checkbox(&mut normal_break_enabled, "Normal Break");
        if normal_break_enabled {
            let val = layer.paint.normal_break_threshold.get_or_insert(0.5);
            slider_row(ui, "normal_break_thr", val, 0.0..=1.0, "Threshold", None, 2);
        } else {
            layer.paint.normal_break_threshold = None;
        }
    });
    if state.project.layers[idx].dry != old_dry {
        state.pending_remerge = true;
    }

    ui.separator();

    // ── Guides ──
    section_header(
        ui,
        &format!("Guides ({})", state.project.layers[idx].guides.len()),
    );
    ui.indent("guides_content", |ui: &mut egui::Ui| {
        if state.project.layers[idx].guides.is_empty() {
            ui.label(egui::RichText::new("No guides").color(egui::Color32::from_gray(120)));
        }
        for i in 0..state.project.layers[idx].guides.len() {
            let guide = &state.project.layers[idx].guides[i];
            let display_type = match guide.guide_type {
                pa_painter::types::GuideType::Source | pa_painter::types::GuideType::Sink => {
                    "Radial"
                }
                pa_painter::types::GuideType::Directional => "Directional",
                pa_painter::types::GuideType::Vortex => "Vortex",
            };
            let label = format!("#{i} {display_type}");
            let selected = state.selected_guide == Some(i);
            if ui.selectable_label(selected, &label).clicked() {
                state.selected_guide = if selected { None } else { Some(i) };
            }
        }
    });
}

// ── Preset Picker ──────────────────────────────────────────────

/// Preset combo box only (no label, no save button).
/// Width is caller-specified. Applies selection immediately.
fn show_preset_combo_sized(
    ui: &mut egui::Ui,
    state: &mut AppState,
    layer_idx: usize,
    combo_w: f32,
) {
    let layer_seed = state.project.layers[layer_idx].seed;
    let current_paint = state.project.layers[layer_idx].paint.clone();

    // Determine current preset name by checking built-in then project presets
    let built_in = PresetLibrary::built_in();
    let current_name = built_in
        .matching_preset(&current_paint)
        .or_else(|| state.project.presets.matching_preset(&current_paint))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Custom".to_string());

    // Separate built-in and user presets (clone user presets to avoid borrow conflict)
    let builtin_presets = &built_in.presets;
    let user_presets: Vec<PaintPreset> = state.project.presets.presets.clone();

    let mut selected_values: Option<PaintValues> = None;
    let mut delete_user_idx: Option<usize> = None;
    let thumbs = &mut state.preset_thumbnails;

    // Wrap ComboBox in a fixed-width sub-UI so its internal
    // available_width() matches combo_w and .truncate() works correctly.
    ui.allocate_ui_with_layout(
        egui::Vec2::new(combo_w, ui.available_height()),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui: &mut egui::Ui| {
            egui::ComboBox::from_id_salt("preset_combo")
                .selected_text(&current_name)
                .width(combo_w)
                .truncate()
                .show_ui(ui, |ui: &mut egui::Ui| {
                    // Fix popup content width to match combo button
                    ui.set_max_width(combo_w);

                    let delete_btn_w = 18.0;
                    let thumb_w = 60.0;
                    let spacing = ui.spacing().item_spacing.x;
                    let scrollbar_margin =
                        ui.spacing().scroll.bar_width + ui.spacing().scroll.bar_outer_margin;

                    // User presets first (if any)
                    if !user_presets.is_empty() {
                        ui.label(egui::RichText::new("Custom").weak().size(11.0));
                        for (i, preset) in user_presets.iter().enumerate() {
                            let thumb_id =
                                thumbs.get_or_create(ui.ctx(), &preset.values, layer_seed);
                            let selected = current_name == preset.name;
                            let row_w = ui.available_width() - scrollbar_margin;

                            // Row: [thumb] [name...] [delete]
                            let row_h = 20.0;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::Vec2::new(row_w, row_h),
                                egui::Sense::click(),
                            );
                            if ui.is_rect_visible(rect) {
                                let p = ui.painter();
                                if selected {
                                    p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                                } else if resp.hovered() {
                                    p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
                                }
                                // Thumbnail (vertically centered)
                                let img_y = rect.center().y - 8.0;
                                let img_rect = egui::Rect::from_min_size(
                                    egui::Pos2::new(rect.min.x + 2.0, img_y),
                                    egui::Vec2::new(thumb_w, 16.0),
                                );
                                p.image(
                                    thumb_id,
                                    img_rect,
                                    egui::Rect::from_min_max(
                                        egui::Pos2::ZERO,
                                        egui::Pos2::new(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                                // Name text (truncated)
                                let text_left = rect.min.x + thumb_w + spacing + 2.0;
                                let text_right = rect.max.x - delete_btn_w - spacing;
                                let text_color = if selected {
                                    ui.visuals().selection.stroke.color
                                } else {
                                    ui.visuals().text_color()
                                };
                                let max_text_w = (text_right - text_left).max(10.0);
                                let font_id = egui::TextStyle::Body.resolve(ui.style());
                                paint_truncated_text(
                                    p,
                                    &preset.name,
                                    font_id,
                                    text_color,
                                    text_left,
                                    rect,
                                    max_text_w,
                                );
                            }
                            if resp.clicked() {
                                selected_values = Some(preset.values.clone());
                            }

                            // Delete button (overlaid on the row, right side)
                            let del_rect = egui::Rect::from_min_size(
                                egui::Pos2::new(rect.max.x - delete_btn_w, rect.min.y),
                                egui::Vec2::new(delete_btn_w, row_h),
                            );
                            let del_id = ui.id().with(("del_preset", i));
                            let del_resp = ui.interact(del_rect, del_id, egui::Sense::click());
                            if ui.is_rect_visible(del_rect) {
                                use egui_phosphor::fill::TRASH_SIMPLE;
                                paint_icon(
                                    ui.painter(),
                                    ui,
                                    del_rect,
                                    TRASH_SIMPLE,
                                    13.0,
                                    true,
                                    del_resp.hovered(),
                                );
                            }
                            if del_resp.on_hover_text("Delete").clicked() {
                                delete_user_idx = Some(i);
                            }
                        }
                        ui.separator();
                    }

                    // Built-in presets
                    ui.label(egui::RichText::new("Built-in").weak().size(11.0));
                    for preset in builtin_presets {
                        let thumb_id = thumbs.get_or_create(ui.ctx(), &preset.values, layer_seed);
                        let selected = current_name == preset.name;
                        let row_w = ui.available_width() - scrollbar_margin;
                        let row_h = 20.0;
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::Vec2::new(row_w, row_h),
                            egui::Sense::click(),
                        );
                        if ui.is_rect_visible(rect) {
                            let p = ui.painter();
                            if selected {
                                p.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
                            } else if resp.hovered() {
                                p.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
                            }
                            let img_y = rect.center().y - 8.0;
                            let img_rect = egui::Rect::from_min_size(
                                egui::Pos2::new(rect.min.x + 2.0, img_y),
                                egui::Vec2::new(thumb_w, 16.0),
                            );
                            p.image(
                                thumb_id,
                                img_rect,
                                egui::Rect::from_min_max(
                                    egui::Pos2::ZERO,
                                    egui::Pos2::new(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                            let text_left = rect.min.x + thumb_w + spacing + 2.0;
                            let text_color = if selected {
                                ui.visuals().selection.stroke.color
                            } else {
                                ui.visuals().text_color()
                            };
                            let max_text_w = (rect.max.x - text_left).max(10.0);
                            let font_id = egui::TextStyle::Body.resolve(ui.style());
                            paint_truncated_text(
                                p,
                                &preset.name,
                                font_id,
                                text_color,
                                text_left,
                                rect,
                                max_text_w,
                            );
                        }
                        if resp.clicked() {
                            selected_values = Some(preset.values.clone());
                        }
                    }
                });
        },
    ); // allocate_ui_with_layout

    if let Some(values) = selected_values {
        state.project.layers[layer_idx].paint = values;
    }
    if let Some(idx) = delete_user_idx {
        state.project.presets.presets.remove(idx);
        state.dirty = true;
        state.status_message = "Preset deleted".to_string();

        // Clean up thumbnail cache: keep only entries for existing presets
        let built_in = PresetLibrary::built_in();
        let active: Vec<&PaintValues> = built_in
            .presets
            .iter()
            .chain(state.project.presets.presets.iter())
            .map(|p| &p.values)
            .collect();
        state.preset_thumbnails.retain_active(&active);
    }
}

/// "Save as Preset" button with popup.
fn show_save_preset_icon(ui: &mut egui::Ui, state: &mut AppState, layer_idx: usize) {
    use egui_phosphor::fill::FLOPPY_DISK;

    let current_paint = state.project.layers[layer_idx].paint.clone();
    let built_in = PresetLibrary::built_in();
    let is_custom = built_in
        .matching_preset(&current_paint)
        .or_else(|| state.project.presets.matching_preset(&current_paint))
        .is_none();
    let save_open_id = ui.id().with("preset_save_open");
    let save_name_id = ui.id().with("preset_save_name");
    let mut save_open: bool = ui.data_mut(|d| d.get_temp(save_open_id).unwrap_or(false));

    let enabled = is_custom && !save_open;
    let btn_resp = small_icon_button(ui, FLOPPY_DISK, 14.0, 20.0, enabled);
    let btn_clicked = btn_resp.clicked();
    let btn_rect = btn_resp.rect;
    if enabled {
        btn_resp.on_hover_text("Save as Preset");
    } else {
        btn_resp.on_hover_text("Matches existing preset");
    }

    let just_opened = enabled && btn_clicked;
    if just_opened {
        save_open = true;
        ui.data_mut(|d| d.insert_temp::<String>(save_name_id, String::new()));
    }

    if save_open {
        let mut name: String = ui.data_mut(|d| d.get_temp(save_name_id).unwrap_or_default());
        let mut close = false;

        // Check keys at context level so TextEdit focus doesn't swallow them
        let enter = ui.ctx().input(|i| i.key_pressed(egui::Key::Enter));
        let esc = ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
        if esc {
            close = true;
        }

        let area_resp = egui::Area::new(egui::Id::new("save_preset_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::new(btn_rect.left(), btn_rect.bottom() + 4.0))
            .show(ui.ctx(), |ui: &mut egui::Ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                    use egui_phosphor::fill::{CHECK, X};
                    let btn_side = 20.0;

                    ui.horizontal(|ui: &mut egui::Ui| {
                        let text_w = 140.0_f32;
                        let text_resp = ui.add(
                            egui::TextEdit::singleline(&mut name)
                                .desired_width(text_w)
                                .hint_text("Preset name"),
                        );
                        if name.is_empty() {
                            text_resp.request_focus();
                        }

                        let can_save = !name.trim().is_empty();

                        // Save (check) icon
                        let save_resp = small_icon_button(ui, CHECK, 14.0, btn_side, can_save);
                        let save_clicked = save_resp.clicked();
                        if can_save {
                            save_resp.on_hover_text("Save (Enter)");
                        }
                        if (save_clicked || enter) && can_save {
                            let preset = PaintPreset {
                                name: name.trim().to_string(),
                                values: state.project.layers[layer_idx].paint.clone(),
                            };
                            match state.project.presets.try_add_preset(preset) {
                                Ok(()) => {
                                    state.status_message = format!("Saved preset: {}", name.trim());
                                    state.dirty = true;
                                }
                                Err(existing) => {
                                    state.status_message =
                                        format!("Duplicate values (existing: {})", existing);
                                }
                            }
                            close = true;
                        }

                        // Cancel (X) icon
                        let cancel_resp = small_icon_button(ui, X, 14.0, btn_side, true);
                        let cancel_clicked = cancel_resp.clicked();
                        cancel_resp.on_hover_text("Cancel (Esc)");
                        if cancel_clicked {
                            close = true;
                        }
                    });
                });
            });

        // Close on click outside — skip on the frame the popup just opened
        // (the button click itself would register as "elsewhere")
        if !just_opened && area_resp.response.clicked_elsewhere() {
            close = true;
        }

        if close {
            save_open = false;
        }
        ui.data_mut(|d| {
            d.insert_temp(save_name_id, name);
            d.insert_temp(save_open_id, save_open);
        });
    } else {
        ui.data_mut(|d| d.insert_temp(save_open_id, false));
    }
}

// ── Pressure Curve Editor ───────────────────────────────────────

const CANVAS_H: f32 = 100.0;
const Y_MAX: f32 = 1.5;
const KNOT_RADIUS: f32 = 5.0;
const HANDLE_RADIUS: f32 = 3.5;
const HIT_RADIUS: f32 = 10.0;

/// Coordinate helpers for the curve canvas.
struct CurveCanvas {
    rect: egui::Rect,
    w: f32,
    h: f32,
}

impl CurveCanvas {
    fn new(rect: egui::Rect) -> Self {
        Self {
            rect,
            w: rect.width(),
            h: rect.height(),
        }
    }

    /// Curve \[x,y\] → screen position.  y=0 is bottom, y=Y_MAX is top.
    fn to_screen(&self, cx: f32, cy: f32) -> egui::Pos2 {
        egui::Pos2::new(
            self.rect.left() + cx * self.w,
            self.rect.bottom() - (cy / Y_MAX) * self.h,
        )
    }

    /// Screen position → curve \[x,y\].
    fn to_curve(&self, pos: egui::Pos2) -> [f32; 2] {
        let cx = ((pos.x - self.rect.left()) / self.w).clamp(0.0, 1.0);
        let cy = ((self.rect.bottom() - pos.y) / self.h * Y_MAX).clamp(0.0, Y_MAX);
        [cx, cy]
    }
}

/// Combined stroke preview with pressure curve overlay.
/// The stroke density texture fills the background (semi-transparent),
/// and the interactive pressure curve is drawn on top.
/// Both share the same horizontal axis `t=0..1`.
fn show_combined_stroke_curve(
    ui: &mut egui::Ui,
    paint: &mut PaintValues,
    seed: u32,
    cache: &mut StrokePreviewCache,
) {
    // Auto-convert legacy Preset curves
    if let PressureCurve::Preset(p) = paint.pressure_curve {
        paint.pressure_curve = preset_to_custom(p);
    }

    // Update stroke preview cache if stale
    preview::update_stroke_cache(ui.ctx(), paint, seed, cache);

    let canvas_w = ui.available_width();
    let (response, painter) = ui.allocate_painter(
        egui::Vec2::new(canvas_w, CANVAS_H),
        egui::Sense::click_and_drag(),
    );
    let canvas = CurveCanvas::new(response.rect);
    let rect = response.rect;

    // 1. Dark background
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(32));

    // 2. Stroke density texture — preserve aspect ratio, center vertically
    if let Some(tex) = cache.texture() {
        let tex_w = tex.size()[0] as f32;
        let tex_h = tex.size()[1] as f32;
        if tex_w > 0.0 && tex_h > 0.0 {
            let scale = (rect.width() - 2.0) / tex_w;
            let display_h = (tex_h * scale).min(rect.height() - 2.0);
            let img_rect = egui::Rect::from_center_size(
                rect.center(),
                egui::Vec2::new(rect.width() - 2.0, display_h),
            );
            painter.image(
                tex.id(),
                img_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140),
            );
        }
    }

    // 3. Grid lines (semi-transparent so stroke texture shows through)
    let grid_stroke =
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 80, 80, 100));
    for i in 1..4 {
        let x = rect.left() + canvas_w * i as f32 / 4.0;
        painter.line_segment(
            [
                egui::Pos2::new(x, rect.top()),
                egui::Pos2::new(x, rect.bottom()),
            ],
            grid_stroke,
        );
    }
    for i in 1..4 {
        let y = rect.top() + CANVAS_H * i as f32 / 4.0;
        painter.line_segment(
            [
                egui::Pos2::new(rect.left(), y),
                egui::Pos2::new(rect.right(), y),
            ],
            grid_stroke,
        );
    }
    // y=1.0 reference line
    let y1_screen = rect.bottom() - (1.0 / Y_MAX) * CANVAS_H;
    painter.line_segment(
        [
            egui::Pos2::new(rect.left(), y1_screen),
            egui::Pos2::new(rect.right(), y1_screen),
        ],
        egui::Stroke::new(
            1.0,
            egui::Color32::from_rgba_unmultiplied(100, 100, 100, 120),
        ),
    );

    // 4. Pressure curve on top
    let curve = &mut paint.pressure_curve;
    let segments = 64;
    let curve_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 180, 140));
    for i in 0..segments {
        let t0 = i as f32 / segments as f32;
        let t1 = (i + 1) as f32 / segments as f32;
        let p0 = canvas.to_screen(t0, evaluate_pressure(curve, t0));
        let p1 = canvas.to_screen(t1, evaluate_pressure(curve, t1));
        painter.line_segment([p0, p1], curve_stroke);
    }

    // 5. Interactive editing for Custom curves
    draw_curve_knots_and_handles(ui, &painter, &response, &canvas, curve);
}

/// Draw interactive Bézier knots and handles for the pressure curve.
/// Extracted so it can be used as a layer on top of any background
/// (e.g., the combined stroke+curve widget).
fn draw_curve_knots_and_handles(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    response: &egui::Response,
    canvas: &CurveCanvas,
    curve: &mut PressureCurve,
) {
    if let PressureCurve::Custom(ref mut knots) = curve {
        let n = knots.len();
        let endpoint_color = egui::Color32::from_rgb(100, 200, 255);
        let midpoint_color = egui::Color32::from_rgb(255, 160, 80);
        let handle_color = egui::Color32::from_rgb(180, 180, 180);
        let handle_line_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(80));

        // ── Draw handles (lines + circles), then knots on top ──

        for (i, knot) in knots.iter().enumerate() {
            let knot_screen = canvas.to_screen(knot.pos[0], knot.pos[1]);

            if i > 0 {
                let hin = canvas.to_screen(knot.handle_in[0], knot.handle_in[1]);
                painter.line_segment([knot_screen, hin], handle_line_stroke);
                painter.circle_filled(hin, HANDLE_RADIUS, handle_color);
            }
            if i < n - 1 {
                let hout = canvas.to_screen(knot.handle_out[0], knot.handle_out[1]);
                painter.line_segment([knot_screen, hout], handle_line_stroke);
                painter.circle_filled(hout, HANDLE_RADIUS, handle_color);
            }
        }

        for (i, knot) in knots.iter().enumerate() {
            let is_endpoint = i == 0 || i == n - 1;
            let color = if is_endpoint {
                endpoint_color
            } else {
                midpoint_color
            };
            let center = canvas.to_screen(knot.pos[0], knot.pos[1]);
            painter.circle_filled(center, KNOT_RADIUS, color);
        }

        // ── Interactions ──

        let mut knot_dragged: Option<usize> = None;
        let mut hin_dragged: Option<usize> = None;
        let mut hout_dragged: Option<usize> = None;
        let mut remove_idx: Option<usize> = None;

        for (i, knot) in knots.iter().enumerate() {
            let center = canvas.to_screen(knot.pos[0], knot.pos[1]);
            let id = response.id.with(("knot", i));
            let hit_rect =
                egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
            let resp = ui.interact(hit_rect, id, egui::Sense::click_and_drag());

            if resp.dragged_by(egui::PointerButton::Primary) {
                knot_dragged = Some(i);
            }
            if resp.secondary_clicked() && i > 0 && i < n - 1 && n > 2 {
                remove_idx = Some(i);
            }
        }

        for (i, knot) in knots.iter().enumerate() {
            if i > 0 {
                let center = canvas.to_screen(knot.handle_in[0], knot.handle_in[1]);
                let id = response.id.with(("hin", i));
                let hit_rect =
                    egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
                let resp = ui.interact(hit_rect, id, egui::Sense::drag());
                if resp.dragged_by(egui::PointerButton::Primary) {
                    hin_dragged = Some(i);
                }
            }
            if i < n - 1 {
                let center = canvas.to_screen(knot.handle_out[0], knot.handle_out[1]);
                let id = response.id.with(("hout", i));
                let hit_rect =
                    egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
                let resp = ui.interact(hit_rect, id, egui::Sense::drag());
                if resp.dragged_by(egui::PointerButton::Primary) {
                    hout_dragged = Some(i);
                }
            }
        }

        if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
            if let Some(i) = knot_dragged {
                let [mut cx, cy] = canvas.to_curve(pointer_pos);
                let old = knots[i].pos;

                if i == 0 {
                    cx = 0.0;
                } else if i == n - 1 {
                    cx = 1.0;
                } else {
                    cx = cx.clamp(knots[i - 1].pos[0] + 0.01, knots[i + 1].pos[0] - 0.01);
                }

                let dx = cx - old[0];
                let dy = cy - old[1];
                knots[i].pos = [cx, cy];
                knots[i].handle_in[0] += dx;
                knots[i].handle_in[1] += dy;
                knots[i].handle_out[0] += dx;
                knots[i].handle_out[1] += dy;
            } else if let Some(i) = hin_dragged {
                let [cx, cy] = canvas.to_curve(pointer_pos);
                let x_min = if i > 0 { knots[i - 1].pos[0] } else { 0.0 };
                let x_max = knots[i].pos[0];
                knots[i].handle_in = [cx.clamp(x_min, x_max), cy.clamp(0.0, Y_MAX)];
            } else if let Some(i) = hout_dragged {
                let [cx, cy] = canvas.to_curve(pointer_pos);
                let x_min = knots[i].pos[0];
                let x_max = if i + 1 < n { knots[i + 1].pos[0] } else { 1.0 };
                knots[i].handle_out = [cx.clamp(x_min, x_max), cy.clamp(0.0, Y_MAX)];
            }
        }

        if let Some(i) = remove_idx {
            knots.remove(i);
        }

        if response.double_clicked() {
            if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
                let near_existing = knots.iter().any(|k| {
                    canvas.to_screen(k.pos[0], k.pos[1]).distance(pointer_pos) < HIT_RADIUS
                });
                if !near_existing {
                    let [cx, cy] = canvas.to_curve(pointer_pos);
                    if cx > 0.01 && cx < 0.99 {
                        let insert_at = knots
                            .iter()
                            .position(|k| k.pos[0] > cx)
                            .unwrap_or(knots.len());
                        let prev = if insert_at > 0 {
                            Some(knots[insert_at - 1].pos)
                        } else {
                            None
                        };
                        let next = if insert_at < knots.len() {
                            Some(knots[insert_at].pos)
                        } else {
                            None
                        };
                        let new_knot = CurveKnot::smooth([cx, cy], prev, next);
                        knots.insert(insert_at, new_knot);
                    }
                }
            }
        }

        let hint_color = egui::Color32::from_gray(100);
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            ui.colored_label(hint_color, "Dbl-click");
            ui.colored_label(egui::Color32::from_gray(150), "add");
            ui.colored_label(hint_color, " / Right-click");
            ui.colored_label(egui::Color32::from_gray(150), "remove");
        });
    }
}

// ── Texture Source Picker ───────────────────────────────────────

/// Which mode a source picker button represents.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceMode {
    Mesh,
    File,
    SolidOrNone, // Solid for color, None for normal
}

/// Draw a radio-style icon button. Returns true if clicked.
fn source_button(
    ui: &mut egui::Ui,
    icon: &str,
    tooltip: &str,
    selected: bool,
    enabled: bool,
) -> bool {
    let size = egui::Vec2::splat(20.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill);
        } else if resp.hovered() && enabled {
            painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
        }
        let color = if !enabled {
            ui.visuals()
                .weak_text_color()
                .gamma_multiply(super::widgets::DISABLED_OPACITY)
        } else if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().weak_text_color()
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(13.0),
            color,
        );
    }
    let clicked = resp.clicked() && enabled;
    resp.on_hover_text(tooltip);
    clicked
}

/// Determine current SourceMode from a TextureSource value.
fn current_mode(src: &TextureSource) -> SourceMode {
    match src {
        TextureSource::MeshMaterial(_) => SourceMode::Mesh,
        TextureSource::File(_) => SourceMode::File,
        TextureSource::Solid(_) => SourceMode::SolidOrNone,
        TextureSource::None => SourceMode::SolidOrNone,
    }
}

/// Find the material index for a layer's group_name.
fn layer_material_index(state: &AppState, group_name: &str) -> Option<usize> {
    let mesh = state.loaded_mesh.as_ref()?;
    mesh.groups
        .iter()
        .position(|g| g.name == group_name)
        .filter(|&i| i < mesh.materials.len())
}

/// Get the MeshMaterialInfo for a layer's group, if it exists.
fn layer_material<'a>(
    state: &'a AppState,
    group_name: &str,
) -> Option<&'a pa_painter::mesh::asset_io::MeshMaterialInfo> {
    let mesh = state.loaded_mesh.as_ref()?;
    let idx = mesh.groups.iter().position(|g| g.name == group_name)?;
    mesh.materials.get(idx)
}

/// Color source controls for Grid cell (no label).
/// Draws icon buttons + context widget in a horizontal, plus file buttons if needed.
fn show_color_source_controls(ui: &mut egui::Ui, state: &mut AppState, layer_idx: usize) {
    let group_name = state.project.layers[layer_idx].group_name.clone();
    let has_color_textures =
        layer_material(state, &group_name).is_some_and(|mat| mat.base_color_texture.is_some());
    let mode = current_mode(&state.project.layers[layer_idx].base_color);

    ui.horizontal(|ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing.x = 1.0;

        use egui_phosphor::fill::{CUBE, FOLDER_OPEN, PALETTE};
        if source_button(
            ui,
            CUBE,
            "Use mesh texture",
            mode == SourceMode::Mesh,
            has_color_textures,
        ) {
            let mat_idx = layer_material_index(state, &group_name).unwrap_or(0);
            state.project.layers[layer_idx].base_color = TextureSource::MeshMaterial(mat_idx);
        }
        if source_button(
            ui,
            FOLDER_OPEN,
            "Load from file",
            mode == SourceMode::File,
            true,
        ) && !matches!(
            state.project.layers[layer_idx].base_color,
            TextureSource::File(_)
        ) {
            state.project.layers[layer_idx].base_color = TextureSource::File(None);
        }
        if source_button(
            ui,
            PALETTE,
            "Solid color",
            mode == SourceMode::SolidOrNone,
            true,
        ) && !matches!(
            state.project.layers[layer_idx].base_color,
            TextureSource::Solid(_) | TextureSource::None
        ) {
            state.project.layers[layer_idx].base_color = TextureSource::Solid([1.0, 1.0, 1.0]);
        }

        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(4.0);

        let mesh_ref = &state.loaded_mesh;
        match &mut state.project.layers[layer_idx].base_color {
            TextureSource::MeshMaterial(ref mut mat_idx) => {
                show_material_combo(ui, mesh_ref, mat_idx, "color_mat_combo", true);
            }
            TextureSource::File(ref mut tex_opt) => {
                show_file_with_actions(ui, tex_opt);
            }
            TextureSource::Solid(ref mut rgb) => {
                show_solid_color_fill(ui, rgb);
            }
            TextureSource::None => {
                state.project.layers[layer_idx].base_color = TextureSource::Solid([1.0, 1.0, 1.0]);
            }
        }
    });
}

/// Normal source controls for Grid cell (no label).
fn show_normal_source_controls(ui: &mut egui::Ui, state: &mut AppState, layer_idx: usize) {
    let group_name = state.project.layers[layer_idx].group_name.clone();
    let has_normal_textures =
        layer_material(state, &group_name).is_some_and(|mat| mat.normal_texture.is_some());
    let old_normal = state.project.layers[layer_idx].base_normal.clone();
    let mode = current_mode(&state.project.layers[layer_idx].base_normal);

    ui.horizontal(|ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing.x = 1.0;

        use egui_phosphor::fill::{CUBE, FOLDER_OPEN, PROHIBIT};
        if source_button(
            ui,
            CUBE,
            "Use mesh normal map",
            mode == SourceMode::Mesh,
            has_normal_textures,
        ) {
            let mat_idx = layer_material_index(state, &group_name).unwrap_or(0);
            state.project.layers[layer_idx].base_normal = TextureSource::MeshMaterial(mat_idx);
        }
        if source_button(
            ui,
            FOLDER_OPEN,
            "Load from file",
            mode == SourceMode::File,
            true,
        ) && !matches!(
            state.project.layers[layer_idx].base_normal,
            TextureSource::File(_)
        ) {
            state.project.layers[layer_idx].base_normal = TextureSource::File(None);
        }
        if source_button(
            ui,
            PROHIBIT,
            "No normal map",
            mode == SourceMode::SolidOrNone,
            true,
        ) {
            state.project.layers[layer_idx].base_normal = TextureSource::None;
        }

        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add_space(4.0);

        let mesh_ref = &state.loaded_mesh;
        match &mut state.project.layers[layer_idx].base_normal {
            TextureSource::MeshMaterial(ref mut mat_idx) => {
                show_material_combo(ui, mesh_ref, mat_idx, "normal_mat_combo", false);
            }
            TextureSource::File(ref mut tex_opt) => {
                show_file_with_actions(ui, tex_opt);
            }
            TextureSource::None | TextureSource::Solid(_) => {
                let weak = ui.visuals().weak_text_color();
                ui.colored_label(weak, "(none)");
            }
        }
    });

    if state.project.layers[layer_idx].base_normal != old_normal {
        state.pending_remerge = true;
    }
}

/// Material texture dropdown (ComboBox).
fn show_material_combo(
    ui: &mut egui::Ui,
    loaded_mesh: &Option<Arc<pa_painter::mesh::asset_io::LoadedMesh>>,
    mat_idx: &mut usize,
    id_salt: &str,
    is_color: bool,
) {
    let materials = loaded_mesh
        .as_ref()
        .map(|m| &m.materials[..])
        .unwrap_or(&[]);

    if materials.is_empty() {
        let weak = ui.visuals().weak_text_color();
        ui.colored_label(weak, "(no materials)");
        return;
    }

    // Build display names
    let names: Vec<String> = materials
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            if is_color {
                m.base_color_texture.is_some()
            } else {
                m.normal_texture.is_some()
            }
        })
        .map(|(i, m)| {
            if m.name.is_empty() {
                format!("Material {}", i)
            } else {
                m.name.clone()
            }
        })
        .collect();

    let valid_indices: Vec<usize> = materials
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            if is_color {
                m.base_color_texture.is_some()
            } else {
                m.normal_texture.is_some()
            }
        })
        .map(|(i, _)| i)
        .collect();

    if names.is_empty() {
        let weak = ui.visuals().weak_text_color();
        ui.colored_label(weak, "(no textures)");
        return;
    }

    // Current selection text
    let current_text = if *mat_idx < materials.len() {
        let m = &materials[*mat_idx];
        if m.name.is_empty() {
            format!("Material {}", *mat_idx)
        } else {
            m.name.clone()
        }
    } else {
        format!("Material {}", *mat_idx)
    };

    let combo_w = ui.available_width();
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(&current_text)
        .width(combo_w)
        .truncate()
        .show_ui(ui, |ui: &mut egui::Ui| {
            for (display_idx, &real_idx) in valid_indices.iter().enumerate() {
                ui.selectable_value(mat_idx, real_idx, &names[display_idx]);
            }
        });
}

/// File label + action icons on the same line.
/// Has file: \[filename…\] \[swap\] \[x\]  —  No file: \[folder_open\]
/// Solid color picker that fills remaining width.
fn show_solid_color_fill(ui: &mut egui::Ui, rgb: &mut [f32; 3]) {
    let w = ui.available_width();
    ui.spacing_mut().interact_size.x = w;
    egui::color_picker::color_edit_button_rgb(ui, rgb);
}

fn show_file_with_actions(ui: &mut egui::Ui, tex_opt: &mut Option<EmbeddedTexture>) {
    use egui_phosphor::fill::{SWAP, X_CIRCLE};
    const ICON_SIZE: f32 = 18.0;
    const ICON_FONT: f32 = 13.0;

    if let Some(ref tex) = tex_opt {
        let label_text = tex.label.clone();
        // Pre-subtract icon space (2 icons + 2 spacings between 3 widgets)
        let icons_w = ICON_SIZE * 2.0 + ui.spacing().item_spacing.x * 2.0;
        let label_w = (ui.available_width() - icons_w).max(20.0);
        let weak = ui.visuals().weak_text_color();
        ui.add_sized(
            [label_w, ui.spacing().interact_size.y],
            egui::Label::new(egui::RichText::new(&label_text).color(weak)).truncate(),
        )
        .on_hover_text(&label_text);
        if small_icon_button(ui, SWAP, ICON_FONT, ICON_SIZE, true)
            .on_hover_text("Replace")
            .clicked()
        {
            if let Some(new_tex) = pick_and_load_texture() {
                *tex_opt = Some(new_tex);
            }
        }
        if small_icon_button(ui, X_CIRCLE, ICON_FONT, ICON_SIZE, true)
            .on_hover_text("Clear")
            .clicked()
        {
            *tex_opt = None;
        }
    } else {
        let btn_w = ui.available_width();
        if ui
            .add_sized(
                [btn_w, ui.spacing().interact_size.y],
                egui::Button::new("Open file…"),
            )
            .clicked()
        {
            if let Some(new_tex) = pick_and_load_texture() {
                *tex_opt = Some(new_tex);
            }
        }
    }
}

/// Open a file dialog and load a texture, returning an EmbeddedTexture.
/// Convert a u32 seed to a fixed 6-letter uppercase string (base-26).
fn seed_to_alpha(mut seed: u32) -> String {
    let mut chars = [b'A'; 6];
    for c in chars.iter_mut().rev() {
        *c = b'A' + (seed % 26) as u8;
        seed /= 26;
    }
    String::from_utf8_lossy(&chars).into_owned()
}

fn alpha_to_seed(s: &str) -> Option<u32> {
    let mut result: u32 = 0;
    for &b in s.as_bytes() {
        if !b.is_ascii_uppercase() {
            return None;
        }
        result = result.checked_mul(26)?.checked_add((b - b'A') as u32)?;
    }
    Some(result)
}

fn pick_and_load_texture() -> Option<EmbeddedTexture> {
    let path = rfd::FileDialog::new()
        .add_filter("Image", &["png", "tga", "exr", "jpg", "jpeg"])
        .pick_file()?;

    match asset_io::load_texture(&path) {
        Ok(tex) => {
            let label = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "texture".to_string());
            let content_hash = EmbeddedTexture::compute_content_hash(&tex.pixels);
            Some(EmbeddedTexture {
                label,
                pixels: Arc::new(tex.pixels),
                width: tex.width,
                height: tex.height,
                content_hash,
            })
        }
        Err(e) => {
            eprintln!("Failed to load texture: {e}");
            None
        }
    }
}
