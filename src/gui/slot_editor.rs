use eframe::egui;

use practical_arcana_painter::pressure::{evaluate_pressure, preset_to_custom};
use practical_arcana_painter::types::{
    CurveKnot, PaintPreset, PaintValues, PresetLibrary, PressureCurve,
};

use super::preview::StrokePreviewCache;

use super::preview;
use super::sidebar::build_group_names;
use super::state::AppState;

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

    ui.heading(format!("Layer: {}", state.project.layers[idx].name));

    // Group selector (scoped borrow of layer.group_name)
    {
        let layer = &mut state.project.layers[idx];
        egui::ComboBox::from_label("Group")
            .selected_text(&layer.group_name)
            .show_ui(ui, |ui: &mut egui::Ui| {
                for name in &group_names {
                    ui.selectable_value(&mut layer.group_name, name.clone(), name.as_str());
                }
            });
    }

    ui.add_space(4.0);

    // ── Preset Picker ──
    show_preset_picker(ui, state, idx);

    ui.add_space(4.0);

    // Derive per-layer seed from project seed + layer index
    let layer_seed = state.project.settings.seed.wrapping_add(idx as u32);

    // Reborrow layer + cache for the rest of the editor
    let layer = &mut state.project.layers[idx];
    let cache = &mut state.preview_cache;

    // ── Paint Settings (unified brush + layout) ──
    egui::CollapsingHeader::new("Brush")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            ui.add(
                egui::Slider::new(&mut layer.paint.brush_width, 5.0..=100.0)
                    .text("Brush Width"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.load, 0.0..=2.0)
                    .step_by(0.01)
                    .text("Load"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.body_wiggle, 0.0..=0.5)
                    .step_by(0.01)
                    .text("Body Wiggle"),
            );

            // Combined pressure curve + stroke density preview
            show_combined_stroke_curve(ui, &mut layer.paint, layer_seed, &mut cache.stroke);
        });

    ui.add_space(4.0);

    // ── Layout Settings ──
    egui::CollapsingHeader::new("Layout")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            ui.add(
                egui::Slider::new(&mut layer.paint.stroke_spacing, 0.1..=3.0)
                    .step_by(0.1)
                    .text("Spacing"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.max_stroke_length, 10.0..=500.0)
                    .text("Max Length"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.angle_variation, 0.0..=45.0)
                    .text("Angle Var"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.max_turn_angle, 0.0..=90.0)
                    .text("Max Turn"),
            );
            ui.add(
                egui::Slider::new(&mut layer.paint.color_variation, 0.0..=0.5)
                    .step_by(0.01)
                    .text("Color Var"),
            );

            // Optional thresholds
            let mut color_break_enabled = layer.paint.color_break_threshold.is_some();
            ui.checkbox(&mut color_break_enabled, "Color Break");
            if color_break_enabled {
                let val = layer.paint.color_break_threshold.get_or_insert(0.1);
                ui.add(egui::Slider::new(val, 0.01..=0.5).text("Threshold"));
            } else {
                layer.paint.color_break_threshold = None;
            }

            let mut normal_break_enabled = layer.paint.normal_break_threshold.is_some();
            ui.checkbox(&mut normal_break_enabled, "Normal Break");
            if normal_break_enabled {
                let val = layer.paint.normal_break_threshold.get_or_insert(0.5);
                ui.add(egui::Slider::new(val, 0.0..=1.0).text("Threshold"));
            } else {
                layer.paint.normal_break_threshold = None;
            }


        });

    ui.add_space(4.0);

    // ── Guides ──
    egui::CollapsingHeader::new(format!("Guides ({})", layer.guides.len()))
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            for (i, guide) in layer.guides.iter().enumerate() {
                ui.label(format!(
                    "#{i} {:?} pos({:.2},{:.2}) dir({:.2},{:.2}) r={:.2}",
                    guide.guide_type,
                    guide.position.x,
                    guide.position.y,
                    guide.direction.x,
                    guide.direction.y,
                    guide.influence,
                ));
            }
        });

}

// ── Preset Picker ──────────────────────────────────────────────

/// Preset combo box with stroke thumbnails + "Save as Preset" button.
///
/// Separated from `show()` for borrow-splitting: this function accesses
/// `state.project.presets` and `state.preset_thumbnails` in a scope where
/// `state.project.layers[idx]` is not mutably borrowed.
fn show_preset_picker(ui: &mut egui::Ui, state: &mut AppState, layer_idx: usize) {
    let layer_seed = state.project.settings.seed.wrapping_add(layer_idx as u32);
    let current_paint = state.project.layers[layer_idx].paint.clone();

    // Determine current preset name by checking built-in then project presets
    let built_in = PresetLibrary::built_in();
    let current_name = built_in
        .matching_preset(&current_paint)
        .or_else(|| state.project.presets.matching_preset(&current_paint))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Custom".to_string());

    // Merge all presets into an owned vec (avoids simultaneous borrow of project.presets + layers)
    let all_presets: Vec<PaintPreset> = built_in
        .presets
        .into_iter()
        .chain(state.project.presets.presets.iter().cloned())
        .collect();

    let mut selected_values: Option<PaintValues> = None;
    let thumbs = &mut state.preset_thumbnails;

    egui::ComboBox::from_label("Preset")
        .selected_text(&current_name)
        .width(ui.available_width() - 50.0)
        .show_ui(ui, |ui: &mut egui::Ui| {
            for preset in &all_presets {
                let thumb_id = thumbs.get_or_create(ui.ctx(), &preset.values, layer_seed);
                let resp = ui.horizontal(|ui: &mut egui::Ui| {
                    ui.image(egui::load::SizedTexture::new(thumb_id, [60.0, 16.0]));
                    ui.selectable_label(current_name == preset.name, &preset.name)
                });
                if resp.inner.clicked() {
                    selected_values = Some(preset.values.clone());
                }
            }
        });

    if let Some(values) = selected_values {
        state.project.layers[layer_idx].paint = values;
    }

    // "Save as Preset" inline editor using egui temp data for state
    let save_editing_id = ui.id().with("preset_save_editing");
    let save_name_id = ui.id().with("preset_save_name");
    let mut editing: bool = ui.data_mut(|d| d.get_temp(save_editing_id).unwrap_or(false));

    if editing {
        let mut name: String = ui.data_mut(|d| d.get_temp(save_name_id).unwrap_or_default());
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.label("Name:");
            ui.text_edit_singleline(&mut name);
        });

        let mut done = false;
        ui.horizontal(|ui: &mut egui::Ui| {
            if ui.button("Save").clicked() && !name.trim().is_empty() {
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
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });

        if done {
            editing = false;
            name = String::new();
        }
        ui.data_mut(|d| {
            d.insert_temp(save_name_id, name);
            d.insert_temp(save_editing_id, editing);
        });
    } else if ui.button("Save as Preset").clicked() {
        ui.data_mut(|d| d.insert_temp(save_editing_id, true));
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

    /// Curve [x,y] → screen position.  y=0 is bottom, y=Y_MAX is top.
    fn to_screen(&self, cx: f32, cy: f32) -> egui::Pos2 {
        egui::Pos2::new(
            self.rect.left() + cx * self.w,
            self.rect.bottom() - (cy / Y_MAX) * self.h,
        )
    }

    /// Screen position → curve [x,y].
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

    let canvas_w = ui.available_width().min(256.0);
    let (response, painter) =
        ui.allocate_painter(egui::Vec2::new(canvas_w, CANVAS_H), egui::Sense::click_and_drag());
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
    let grid_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(80, 80, 80, 100));
    for i in 1..4 {
        let x = rect.left() + canvas_w * i as f32 / 4.0;
        painter.line_segment(
            [egui::Pos2::new(x, rect.top()), egui::Pos2::new(x, rect.bottom())],
            grid_stroke,
        );
    }
    for i in 1..4 {
        let y = rect.top() + CANVAS_H * i as f32 / 4.0;
        painter.line_segment(
            [egui::Pos2::new(rect.left(), y), egui::Pos2::new(rect.right(), y)],
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
        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(100, 100, 100, 120)),
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
            let color = if is_endpoint { endpoint_color } else { midpoint_color };
            let center = canvas.to_screen(knot.pos[0], knot.pos[1]);
            painter.circle_filled(center, KNOT_RADIUS, color);
        }

        // ── Interactions ──

        let mut knot_dragged: Option<usize> = None;
        let mut hin_dragged: Option<usize> = None;
        let mut hout_dragged: Option<usize> = None;
        let mut remove_idx: Option<usize> = None;

        for i in 0..n {
            let center = canvas.to_screen(knots[i].pos[0], knots[i].pos[1]);
            let id = response.id.with(("knot", i));
            let hit_rect =
                egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
            let resp = ui.interact(hit_rect, id, egui::Sense::click_and_drag());

            if resp.dragged() {
                knot_dragged = Some(i);
            }
            if resp.secondary_clicked() && i > 0 && i < n - 1 && n > 2 {
                remove_idx = Some(i);
            }
        }

        for i in 0..n {
            if i > 0 {
                let center = canvas.to_screen(knots[i].handle_in[0], knots[i].handle_in[1]);
                let id = response.id.with(("hin", i));
                let hit_rect =
                    egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
                let resp = ui.interact(hit_rect, id, egui::Sense::drag());
                if resp.dragged() {
                    hin_dragged = Some(i);
                }
            }
            if i < n - 1 {
                let center = canvas.to_screen(knots[i].handle_out[0], knots[i].handle_out[1]);
                let id = response.id.with(("hout", i));
                let hit_rect =
                    egui::Rect::from_center_size(center, egui::Vec2::splat(HIT_RADIUS * 2.0));
                let resp = ui.interact(hit_rect, id, egui::Sense::drag());
                if resp.dragged() {
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

        ui.colored_label(
            egui::Color32::from_gray(100),
            "Drag points · Double-click add · Right-click remove",
        );
    }
}
