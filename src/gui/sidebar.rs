use eframe::egui;

use practical_arcana_painter::types::{
    BackgroundMode, NormalMode, PaintSlot, PatternValues, PressurePreset, ResolutionPreset,
    StrokeValues,
};

use super::state::AppState;

/// Draw the left sidebar: Project info, Output Settings, Paint Slots list.
pub fn show_left(ui: &mut egui::Ui, state: &mut AppState) {
    // ── Project info ──
    egui::CollapsingHeader::new("Project")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            // Mesh
            if let Some(ref mesh) = state.loaded_mesh {
                let mesh_path = &state.project.mesh_ref.path;
                if !mesh_path.is_empty() {
                    ui.label(format!("Mesh: {}", short_filename(mesh_path)));
                }
                ui.label(format!("{} mesh groups", mesh.groups.len()));
            } else {
                ui.label("No mesh loaded.");
            }
            if ui.small_button("Load Mesh...").clicked() {
                state.pending_load_mesh = true;
            }

            ui.add_space(4.0);

            // Texture
            if let Some(ref tex_path) = state.project.color_ref.path {
                ui.label(format!("Texture: {}", short_filename(tex_path)));
            } else {
                ui.label("No texture loaded.");
            }
            if ui.small_button("Load Texture...").clicked() {
                state.pending_load_texture = true;
            }
        });

    ui.separator();

    // ── Output Settings ──
    egui::CollapsingHeader::new("Output Settings")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            // Resolution preset
            let current_res = state.project.settings.resolution_preset;
            egui::ComboBox::from_label("Resolution")
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
                egui::Slider::new(&mut state.project.settings.normal_strength, 0.1..=5.0)
                    .text("Normal Strength"),
            );
        });

    ui.separator();

    // ── Paint Slots ──
    egui::CollapsingHeader::new("Paint Slots")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            if state.project.slots.is_empty() {
                ui.label("No paint slots.");
            } else {
                for i in 0..state.project.slots.len() {
                    let slot = &state.project.slots[i];
                    let selected = state.selected_slot == Some(i);
                    let label = format!("{} (order: {})", slot.group_name, slot.order);
                    if ui.selectable_label(selected, &label).clicked() {
                        if selected {
                            // Deselect on re-click
                            state.selected_slot = None;
                        } else {
                            state.selected_slot = Some(i);
                        }
                        state.selected_guide = None;
                    }
                }
            }

            // Slot management buttons
            ui.horizontal(|ui: &mut egui::Ui| {
                if ui.button("+ Add").clicked() {
                    let order = state.project.slots.len() as i32;
                    state.project.slots.push(PaintSlot {
                        group_name: "__full_uv__".to_string(),
                        order,
                        stroke: StrokeValues::default(),
                        pattern: PatternValues::default(),
                        seed: 42,
                    });
                    state.selected_slot = Some(state.project.slots.len() - 1);
                    state.selected_guide = None;
                }

                if let Some(idx) = state.selected_slot {
                    if ui.button("Delete").clicked() && !state.project.slots.is_empty() {
                        state.project.slots.remove(idx);
                        state.selected_guide = None;
                        if state.project.slots.is_empty() {
                            state.selected_slot = None;
                        } else {
                            state.selected_slot =
                                Some(idx.min(state.project.slots.len() - 1));
                        }
                    }

                    if idx > 0 && ui.small_button("↑").clicked() {
                        state.project.slots.swap(idx, idx - 1);
                        state.selected_slot = Some(idx - 1);
                    }
                    if idx + 1 < state.project.slots.len() && ui.small_button("↓").clicked()
                    {
                        state.project.slots.swap(idx, idx + 1);
                        state.selected_slot = Some(idx + 1);
                    }
                }
            });
        });
}

pub fn pressure_name(p: PressurePreset) -> &'static str {
    match p {
        PressurePreset::Uniform => "Uniform",
        PressurePreset::FadeOut => "FadeOut",
        PressurePreset::FadeIn => "FadeIn",
        PressurePreset::Bell => "Bell",
        PressurePreset::Taper => "Taper",
    }
}

pub fn build_group_names(state: &AppState) -> Vec<String> {
    let mut names = vec!["__full_uv__".to_string()];
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
    path.rsplit('/').next().unwrap_or(path)
}
