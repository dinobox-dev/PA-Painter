use eframe::egui;

use practical_arcana_painter::pressure::{evaluate_pressure, preset_to_custom};
use practical_arcana_painter::types::{CurveKnot, PressureCurve};

use super::preview;
use super::sidebar::build_group_names;
use super::state::AppState;

/// Draw the right-panel slot editor for the currently selected slot.
/// Returns early if no slot is selected.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(idx) = state.selected_slot else {
        return;
    };
    if idx >= state.project.slots.len() {
        return;
    }

    let group_names = build_group_names(state);
    let slot = &mut state.project.slots[idx];
    let cache = &mut state.preview_cache;

    ui.heading(format!("Slot: {}", slot.group_name));

    // Group selector
    egui::ComboBox::from_label("Group")
        .selected_text(&slot.group_name)
        .show_ui(ui, |ui: &mut egui::Ui| {
            for name in &group_names {
                ui.selectable_value(&mut slot.group_name, name.clone(), name.as_str());
            }
        });

    ui.add_space(4.0);

    // ── Stroke Settings ──
    egui::CollapsingHeader::new("Stroke")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            ui.add(
                egui::Slider::new(&mut slot.stroke.brush_width, 5.0..=100.0)
                    .text("Brush Width"),
            );
            ui.add(
                egui::Slider::new(&mut slot.stroke.load, 0.0..=2.0)
                    .step_by(0.01)
                    .text("Load"),
            );
            ui.add(
                egui::Slider::new(&mut slot.stroke.body_wiggle, 0.0..=0.5)
                    .step_by(0.01)
                    .text("Body Wiggle"),
            );

            // Pressure curve editor widget
            show_pressure_curve_editor(ui, &mut slot.stroke.pressure_curve);

            // Stroke preset selector
            let presets = practical_arcana_painter::types::PresetLibrary::built_in();
            let current_name = presets
                .matching_stroke_preset(&slot.stroke)
                .unwrap_or("(custom)");
            egui::ComboBox::from_label("Stroke Preset")
                .selected_text(current_name)
                .show_ui(ui, |ui: &mut egui::Ui| {
                    for preset in &presets.strokes {
                        if ui
                            .selectable_label(
                                current_name == preset.name,
                                &preset.name,
                            )
                            .clicked()
                        {
                            slot.stroke = preset.values.clone();
                        }
                    }
                });

            // Stroke density preview
            ui.add_space(8.0);
            preview::show_stroke_preview(ui, &slot.stroke, slot.seed, &mut cache.stroke);
        });

    ui.add_space(4.0);

    // ── Pattern Settings ──
    egui::CollapsingHeader::new("Pattern")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            ui.add(
                egui::Slider::new(&mut slot.pattern.stroke_spacing, 0.1..=3.0)
                    .step_by(0.1)
                    .text("Spacing"),
            );
            ui.add(
                egui::Slider::new(&mut slot.pattern.max_stroke_length, 10.0..=500.0)
                    .text("Max Length"),
            );
            ui.add(
                egui::Slider::new(&mut slot.pattern.angle_variation, 0.0..=45.0)
                    .text("Angle Var"),
            );
            ui.add(
                egui::Slider::new(&mut slot.pattern.max_turn_angle, 0.0..=90.0)
                    .text("Max Turn"),
            );
            ui.add(
                egui::Slider::new(&mut slot.pattern.color_variation, 0.0..=0.5)
                    .step_by(0.01)
                    .text("Color Var"),
            );

            // Optional thresholds
            let mut color_break_enabled = slot.pattern.color_break_threshold.is_some();
            ui.checkbox(&mut color_break_enabled, "Color Break");
            if color_break_enabled {
                let val = slot.pattern.color_break_threshold.get_or_insert(0.1);
                ui.add(egui::Slider::new(val, 0.01..=0.5).text("Threshold"));
            } else {
                slot.pattern.color_break_threshold = None;
            }

            let mut normal_break_enabled = slot.pattern.normal_break_threshold.is_some();
            ui.checkbox(&mut normal_break_enabled, "Normal Break");
            if normal_break_enabled {
                let val = slot.pattern.normal_break_threshold.get_or_insert(0.5);
                ui.add(egui::Slider::new(val, 0.0..=1.0).text("Threshold"));
            } else {
                slot.pattern.normal_break_threshold = None;
            }

            // Pattern preset selector
            let presets = practical_arcana_painter::types::PresetLibrary::built_in();
            let current_name = presets
                .matching_pattern_preset(&slot.pattern)
                .unwrap_or("(custom)");
            egui::ComboBox::from_label("Pattern Preset")
                .selected_text(current_name)
                .show_ui(ui, |ui: &mut egui::Ui| {
                    for preset in &presets.patterns {
                        if ui
                            .selectable_label(
                                current_name == preset.name,
                                &preset.name,
                            )
                            .clicked()
                        {
                            slot.pattern = preset.values.clone();
                        }
                    }
                });

            // Pattern layout preview
            ui.add_space(8.0);
            preview::show_pattern_preview(ui, slot, &mut cache.pattern);
        });

    ui.add_space(4.0);

    // ── Guides ──
    egui::CollapsingHeader::new(format!("Guides ({})", slot.pattern.guides.len()))
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            for (i, guide) in slot.pattern.guides.iter().enumerate() {
                ui.label(format!(
                    "#{i} pos({:.2},{:.2}) dir({:.2},{:.2}) r={:.2}",
                    guide.position.x,
                    guide.position.y,
                    guide.direction.x,
                    guide.direction.y,
                    guide.influence,
                ));
            }
        });

    ui.add_space(4.0);

    // ── Seed ──
    ui.horizontal(|ui: &mut egui::Ui| {
        ui.label("Seed:");
        ui.add(egui::DragValue::new(&mut slot.seed));
    });
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

/// Draw the pressure curve preview with interactive Bézier control point editing.
/// Automatically converts legacy Preset curves to editable Custom splines.
fn show_pressure_curve_editor(ui: &mut egui::Ui, curve: &mut PressureCurve) {
    // Auto-convert legacy Preset curves to editable Custom splines
    if let PressureCurve::Preset(p) = *curve {
        *curve = preset_to_custom(p);
    }
    let canvas_w = ui.available_width().min(256.0);
    let (response, painter) =
        ui.allocate_painter(egui::Vec2::new(canvas_w, CANVAS_H), egui::Sense::click_and_drag());
    let canvas = CurveCanvas::new(response.rect);
    let rect = response.rect;

    // Background
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(32));

    // Grid lines
    let grid_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(48));
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
        egui::Stroke::new(1.0, egui::Color32::from_gray(64)),
    );

    // Draw curve (both Preset and Custom)
    let segments = 64;
    let curve_stroke = egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 180, 140));
    for i in 0..segments {
        let t0 = i as f32 / segments as f32;
        let t1 = (i + 1) as f32 / segments as f32;
        let p0 = canvas.to_screen(t0, evaluate_pressure(curve, t0));
        let p1 = canvas.to_screen(t1, evaluate_pressure(curve, t1));
        painter.line_segment([p0, p1], curve_stroke);
    }

    // Interactive editing for Custom curves
    if let PressureCurve::Custom(ref mut knots) = curve {
        let n = knots.len();
        let endpoint_color = egui::Color32::from_rgb(100, 200, 255);
        let midpoint_color = egui::Color32::from_rgb(255, 160, 80);
        let handle_color = egui::Color32::from_rgb(180, 180, 180);
        let handle_line_stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(80));

        // ── Draw handles (lines + circles), then knots on top ──

        for (i, knot) in knots.iter().enumerate() {
            let knot_screen = canvas.to_screen(knot.pos[0], knot.pos[1]);

            // handle_in (skip first knot — no incoming segment)
            if i > 0 {
                let hin = canvas.to_screen(knot.handle_in[0], knot.handle_in[1]);
                painter.line_segment([knot_screen, hin], handle_line_stroke);
                painter.circle_filled(hin, HANDLE_RADIUS, handle_color);
            }
            // handle_out (skip last knot — no outgoing segment)
            if i < n - 1 {
                let hout = canvas.to_screen(knot.handle_out[0], knot.handle_out[1]);
                painter.line_segment([knot_screen, hout], handle_line_stroke);
                painter.circle_filled(hout, HANDLE_RADIUS, handle_color);
            }
        }

        // Draw knot circles on top
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

        // Knot interactions (registered first → higher egui priority)
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

        // Handle interactions
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

        // Apply drag — use absolute pointer position (not deltas)
        if let Some(pointer_pos) = ui.ctx().pointer_latest_pos() {
            if let Some(i) = knot_dragged {
                let [mut cx, cy] = canvas.to_curve(pointer_pos);
                let old = knots[i].pos;

                // Constrain x: endpoints locked, midpoints between neighbors
                if i == 0 {
                    cx = 0.0;
                } else if i == n - 1 {
                    cx = 1.0;
                } else {
                    cx = cx.clamp(knots[i - 1].pos[0] + 0.01, knots[i + 1].pos[0] - 0.01);
                }

                // Move knot + handles by same delta (preserves handle shape)
                let dx = cx - old[0];
                let dy = cy - old[1];
                knots[i].pos = [cx, cy];
                knots[i].handle_in[0] += dx;
                knots[i].handle_in[1] += dy;
                knots[i].handle_out[0] += dx;
                knots[i].handle_out[1] += dy;
            } else if let Some(i) = hin_dragged {
                let [cx, cy] = canvas.to_curve(pointer_pos);
                // handle_in.x must be in [prev_knot.x, this_knot.x]
                let x_min = if i > 0 { knots[i - 1].pos[0] } else { 0.0 };
                let x_max = knots[i].pos[0];
                knots[i].handle_in = [cx.clamp(x_min, x_max), cy.clamp(0.0, Y_MAX)];
            } else if let Some(i) = hout_dragged {
                let [cx, cy] = canvas.to_curve(pointer_pos);
                // handle_out.x must be in [this_knot.x, next_knot.x]
                let x_min = knots[i].pos[0];
                let x_max = if i + 1 < n { knots[i + 1].pos[0] } else { 1.0 };
                knots[i].handle_out = [cx.clamp(x_min, x_max), cy.clamp(0.0, Y_MAX)];
            }
        }

        // Remove knot
        if let Some(i) = remove_idx {
            knots.remove(i);
        }

        // Double-click on empty area to add a new knot
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

        // Hint text
        ui.colored_label(
            egui::Color32::from_gray(100),
            "Drag points · Double-click add · Right-click remove",
        );
    }
}
