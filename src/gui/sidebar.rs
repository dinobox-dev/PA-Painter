use eframe::egui;

use practical_arcana_painter::types::{
    BackgroundMode, Layer, NormalMode, PaintValues, ResolutionPreset,
};

use super::state::AppState;

/// Draw the left sidebar: Mesh, Base, Layers, Output Settings.
pub fn show_left(ui: &mut egui::Ui, state: &mut AppState) {
    // ── Mesh ──
    egui::CollapsingHeader::new("Mesh")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            if let Some(ref mesh) = state.loaded_mesh {
                let mesh_path = &state.project.mesh_ref.path;
                if !mesh_path.is_empty() {
                    ui.label(format!("File: {}", short_filename(mesh_path)));
                }
                ui.label(format!("{} groups", mesh.groups.len()));

                if ui.small_button("Reload Mesh").clicked() {
                    state.pending_reload_mesh = true;
                }
            } else {
                ui.label("No mesh loaded.");
            }
        });

    ui.separator();

    // ── Base ──
    egui::CollapsingHeader::new("Base")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            // Base Color
            ui.label("Color:");
            if let Some(tex_path) = state.project.base_color.texture_path() {
                ui.label(format!("  {}", short_filename(tex_path)));
            } else {
                ui.label("  (solid)");
            }
            if ui.small_button("Load Texture...").clicked() {
                state.pending_load_texture = true;
            }

            ui.add_space(4.0);

            // Base Normal
            ui.label("Normal:");
            if let Some(ref normal_path) = state.project.base_normal {
                ui.label(format!("  {}", short_filename(normal_path)));
                ui.horizontal(|ui: &mut egui::Ui| {
                    if ui.small_button("Replace...").clicked() {
                        state.pending_load_normal = true;
                    }
                    if ui.small_button("Clear").clicked() {
                        state.project.base_normal = None;
                        state.loaded_normal = None;
                        state.dirty = true;
                    }
                });
            } else {
                ui.label("  (none)");
                if ui.small_button("Load Normal...").clicked() {
                    state.pending_load_normal = true;
                }
            }
        });

    ui.separator();

    // ── Layers ──
    egui::CollapsingHeader::new("Layers")
        .default_open(true)
        .show(ui, |ui: &mut egui::Ui| {
            if state.project.layers.is_empty() {
                ui.label("No layers.");
            } else {
                for i in 0..state.project.layers.len() {
                    let selected = state.selected_layer == Some(i);
                    let visible = state.project.layers[i].visible;
                    let name = state.project.layers[i].name.clone();
                    let group = state.project.layers[i].group_name.clone();

                    ui.horizontal(|ui: &mut egui::Ui| {
                        // Visibility toggle
                        let eye = if visible { "👁" } else { "  " };
                        if ui.selectable_label(false, eye).clicked() {
                            state.project.layers[i].visible = !visible;
                        }

                        // Layer name (selectable)
                        let label = format!("{} ({})", name, group);
                        if ui.selectable_label(selected, &label).clicked() {
                            if selected {
                                state.selected_layer = None;
                            } else {
                                state.selected_layer = Some(i);
                            }
                            state.selected_guide = None;
                        }
                    });
                }
            }

            // Layer management buttons
            ui.horizontal(|ui: &mut egui::Ui| {
                let has_mesh = state.loaded_mesh.is_some();
                if ui.add_enabled(has_mesh, egui::Button::new("+ Add")).clicked() {
                    let order = state.project.layers.len() as i32;
                    state.project.layers.push(Layer {
                        name: "__all__".to_string(),
                        visible: true,
                        group_name: "__all__".to_string(),
                        order,
                        paint: PaintValues::default(),
                        guides: vec![],
                    });
                    state.selected_layer = Some(state.project.layers.len() - 1);
                    state.selected_guide = None;
                }

                if let Some(idx) = state.selected_layer {
                    if ui.button("Delete").clicked() && !state.project.layers.is_empty() {
                        state.project.layers.remove(idx);
                        state.selected_guide = None;
                        if state.project.layers.is_empty() {
                            state.selected_layer = None;
                        } else {
                            state.selected_layer =
                                Some(idx.min(state.project.layers.len() - 1));
                        }
                    }

                    if idx > 0 && ui.small_button("↑").clicked() {
                        state.project.layers.swap(idx, idx - 1);
                        state.selected_layer = Some(idx - 1);
                    }
                    if idx + 1 < state.project.layers.len() && ui.small_button("↓").clicked()
                    {
                        state.project.layers.swap(idx, idx + 1);
                        state.selected_layer = Some(idx + 1);
                    }
                }
            });
        });

    ui.separator();

    // ── Project Settings ──
    egui::CollapsingHeader::new("Project Settings")
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
                egui::Slider::new(&mut state.project.settings.normal_strength, 0.0..=1.0)
                    .text("Normal Strength"),
            );
        });

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
