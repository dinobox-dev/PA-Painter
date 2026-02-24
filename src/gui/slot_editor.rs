use eframe::egui;

use practical_arcana_painter::types::PressurePreset;

use super::preview;
use super::sidebar::{build_group_names, pressure_name};
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

            // Pressure preset
            egui::ComboBox::from_label("Pressure")
                .selected_text(pressure_name(slot.stroke.pressure_preset))
                .show_ui(ui, |ui: &mut egui::Ui| {
                    for &preset in &[
                        PressurePreset::Uniform,
                        PressurePreset::FadeOut,
                        PressurePreset::FadeIn,
                        PressurePreset::Bell,
                        PressurePreset::Taper,
                    ] {
                        ui.selectable_value(
                            &mut slot.stroke.pressure_preset,
                            preset,
                            pressure_name(preset),
                        );
                    }
                });

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
