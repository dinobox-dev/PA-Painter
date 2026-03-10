use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};
use eframe::egui_wgpu;

use practical_arcana_painter::types::{GuideType, TextureSource};

use super::mesh_preview;
use super::state::{AppState, DrawOrder, GroupDimKey, GuideTool, MapMode, ResultMode, SetupMapMode, ViewportTab};
use super::textures::{linear_to_raw_u8, linear_to_srgb_u8};
use super::widgets::{slider_row, toolbar_icon_button};

/// Convert UV coordinate to screen position.
pub fn uv_to_screen(uv: glam::Vec2, state: &AppState, viewport_rect: Rect) -> Pos2 {
    let x = (uv.x - state.viewport.offset.x) * state.viewport.zoom + viewport_rect.left();
    let y = (uv.y - state.viewport.offset.y) * state.viewport.zoom + viewport_rect.top();
    Pos2::new(x, y)
}

/// Convert screen position to UV coordinate.
pub fn screen_to_uv(pos: Pos2, state: &AppState, viewport_rect: Rect) -> glam::Vec2 {
    let x = (pos.x - viewport_rect.left()) / state.viewport.zoom + state.viewport.offset.x;
    let y = (pos.y - viewport_rect.top()) / state.viewport.zoom + state.viewport.offset.y;
    glam::Vec2::new(x, y)
}

/// Generate a checkerboard texture as placeholder.
pub fn make_checkerboard(ctx: &egui::Context) -> egui::TextureHandle {
    let size = 256;
    let cell = 16;
    let mut pixels = vec![Color32::WHITE; size * size];
    for y in 0..size {
        for x in 0..size {
            let checker = ((x / cell) + (y / cell)) % 2 == 0;
            pixels[y * size + x] = if checker {
                Color32::from_gray(200)
            } else {
                Color32::from_gray(160)
            };
        }
    }
    ctx.load_texture(
        "checkerboard",
        egui::ColorImage::new([size, size], pixels),
        egui::TextureOptions::NEAREST,
    )
}

// ── Group dim overlay ───────────────────────────────────────────────

const GROUP_DIM_RESOLUTION: u32 = 256;

/// Update the cached dim overlay texture for the selected layer's vertex group.
/// Inside-group pixels get light dim; outside-group pixels get heavy dim.
fn update_group_dim_cache(ctx: &egui::Context, state: &mut AppState) {
    let desired_key = state.selected_layer.and_then(|idx| {
        let layer = state.project.layers.get(idx)?;
        if layer.group_name == "__all__" {
            return None;
        }
        let mesh_group_count = state
            .loaded_mesh
            .as_ref()
            .map(|m| m.groups.len())
            .unwrap_or(0);
        Some(GroupDimKey {
            layer_idx: idx,
            group_name: layer.group_name.clone(),
            mesh_group_count,
        })
    });

    if desired_key.is_none() {
        if state.group_dim_cache.key.is_some() {
            state.group_dim_cache.invalidate();
        }
        return;
    }

    if state.group_dim_cache.key == desired_key {
        return;
    }

    let Some(key) = desired_key else { return };

    let texture = state.loaded_mesh.as_ref().and_then(|mesh| {
        let group = mesh.groups.iter().find(|g| g.name == key.group_name)?;
        let mask = practical_arcana_painter::uv_mask::UvMask::from_mesh_group(
            mesh,
            group,
            GROUP_DIM_RESOLUTION,
        );

        let inside_alpha: u8 = 80;
        let outside_alpha: u8 = 160;
        let res = GROUP_DIM_RESOLUTION as usize;
        let pixels: Vec<Color32> = mask
            .data
            .iter()
            .map(|&inside| {
                let a = if inside { inside_alpha } else { outside_alpha };
                Color32::from_rgba_unmultiplied(0, 0, 0, a)
            })
            .collect();

        Some(ctx.load_texture(
            "group_dim_overlay",
            egui::ColorImage::new([res, res], pixels),
            egui::TextureOptions::LINEAR,
        ))
    });

    state.group_dim_cache.key = Some(key);
    state.group_dim_cache.texture = texture;
}

// ── Setup tab base texture cache ─────────────────────────────────────

fn texture_source_hash(src: &TextureSource) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(src).hash(&mut h);
    match src {
        TextureSource::None => {}
        TextureSource::Solid(rgb) => {
            rgb[0].to_bits().hash(&mut h);
            rgb[1].to_bits().hash(&mut h);
            rgb[2].to_bits().hash(&mut h);
        }
        TextureSource::MeshMaterial(idx) => idx.hash(&mut h),
        TextureSource::File(opt) => {
            if let Some(et) = opt {
                et.label.hash(&mut h);
                et.width.hash(&mut h);
                et.height.hash(&mut h);
            }
        }
    }
    h.finish()
}

/// Resolve a TextureSource into an egui TextureHandle for the Setup tab.
fn resolve_setup_texture(
    ctx: &egui::Context,
    source: &TextureSource,
    is_color: bool,
    mesh: Option<&practical_arcana_painter::asset_io::LoadedMesh>,
) -> Option<egui::TextureHandle> {
    let name = if is_color { "setup_base_color" } else { "setup_base_normal" };
    match source {
        TextureSource::None => {
            // Color: gray 50%; Normal: flat normal (0.5, 0.5, 1.0) = #8080FF
            let c = if is_color {
                Color32::from_gray(128)
            } else {
                Color32::from_rgb(128, 128, 255)
            };
            Some(ctx.load_texture(
                name,
                egui::ColorImage::new([1, 1], vec![c]),
                egui::TextureOptions::NEAREST,
            ))
        }
        TextureSource::Solid(rgb) => {
            let c = Color32::from_rgb(
                linear_to_srgb_u8(rgb[0]),
                linear_to_srgb_u8(rgb[1]),
                linear_to_srgb_u8(rgb[2]),
            );
            Some(ctx.load_texture(
                name,
                egui::ColorImage::new([1, 1], vec![c]),
                egui::TextureOptions::NEAREST,
            ))
        }
        TextureSource::MeshMaterial(idx) => {
            let mat = mesh?.materials.get(*idx)?;
            if is_color {
                let tex = mat.base_color_texture.as_ref()?;
                Some(super::textures::loaded_texture_to_handle(ctx, tex, name))
            } else {
                let tex = mat.normal_texture.as_ref()?;
                // Normal maps are non-color data — no sRGB conversion
                Some(super::textures::loaded_texture_raw_handle(ctx, tex, name))
            }
        }
        TextureSource::File(opt) => {
            let et = opt.as_ref()?;
            let convert = if is_color { linear_to_srgb_u8 } else { linear_to_raw_u8 };
            let pixels: Vec<Color32> = et.pixels.iter().map(|p| {
                Color32::from_rgba_unmultiplied(
                    convert(p[0]),
                    convert(p[1]),
                    convert(p[2]),
                    (p[3].clamp(0.0, 1.0) * 255.0) as u8,
                )
            }).collect();
            Some(ctx.load_texture(
                name,
                egui::ColorImage::new([et.width as usize, et.height as usize], pixels),
                egui::TextureOptions::LINEAR,
            ))
        }
    }
}

/// Update the cached base texture for the Setup tab.
fn update_setup_base_texture(ctx: &egui::Context, state: &mut AppState) {
    let is_color = state.setup_map_mode == SetupMapMode::Color;
    let desired_key = state.selected_layer.and_then(|idx| {
        let layer = state.project.layers.get(idx)?;
        let src = if is_color { &layer.base_color } else { &layer.base_normal };
        Some((idx, is_color, texture_source_hash(src)))
    });

    if state.setup_base_key == desired_key {
        return;
    }

    state.setup_base_tex = desired_key.and_then(|(idx, is_color, _)| {
        let layer = state.project.layers.get(idx)?;
        let src = if is_color { &layer.base_color } else { &layer.base_normal };
        resolve_setup_texture(ctx, src, is_color, state.loaded_mesh.as_deref())
    });
    state.setup_base_key = desired_key;
}

// ── Main viewport entry point ───────────────────────────────────────

/// Draw the viewport: tabs, main content, and floating strip overlay.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    render_state: Option<&egui_wgpu::RenderState>,
) {
    // ── Top tab bar (order must match ViewportTab::next() in state.rs) ──
    ui.horizontal(|ui: &mut egui::Ui| {
        let tabs = [
            (ViewportTab::Setup, "Setup"),
            (ViewportTab::UvView, "UV Result"),
            (ViewportTab::Mesh3D, "3D Preview"),
        ];
        for (tab, label) in &tabs {
            let selected = state.viewport_tab == *tab;
            if ui.selectable_label(selected, *label).clicked() {
                state.viewport_tab = *tab;
            }
        }
    });

    // ── Main viewport content ──
    match state.viewport_tab {
        ViewportTab::UvView => show_uv_view(ui, state),
        ViewportTab::Setup => show_setup_view(ui, state),
        ViewportTab::Mesh3D => show_3d_view(ui, state, render_state),
    }
}

// ── Strip contents ──────────────────────────────────────────────────

/// Wireframe toggle + path overlay button with popup palette, shared by UV and Guide strips.
fn strip_overlay_controls(ui: &mut egui::Ui, state: &mut AppState) {
    overlay_text_button(ui, "Wireframe", state.viewport.show_wireframe, || {
        state.viewport.show_wireframe = !state.viewport.show_wireframe;
    });
    path_overlay_button(ui, &mut state.viewport.path_overlay_idx);
}

/// 32px-tall text toggle button. Active = tinted background, inactive = plain.
fn overlay_text_button_response(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let font = egui::FontId::proportional(14.0);
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), font.clone(), ui.visuals().text_color());
    let text_size = galley.size();
    let pad_x = 8.0;
    let desired = Vec2::new(text_size.x + pad_x * 2.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let cr = 4.0;
        if active {
            p.rect_filled(rect, cr, ui.visuals().selection.bg_fill);
        } else if resp.hovered() {
            p.rect_filled(rect, cr, ui.visuals().widgets.hovered.bg_fill);
        }
        let text_color = if active {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
        let text_pos = Pos2::new(rect.left() + pad_x, rect.center().y - text_size.y * 0.5);
        p.galley(text_pos, galley, text_color);
    }
    resp
}

fn overlay_text_button(ui: &mut egui::Ui, label: &str, active: bool, on_click: impl FnOnce()) {
    let font = egui::FontId::proportional(14.0);
    let galley =
        ui.painter()
            .layout_no_wrap(label.to_string(), font.clone(), ui.visuals().text_color());
    let text_size = galley.size();
    let pad_x = 8.0;
    let desired = Vec2::new(text_size.x + pad_x * 2.0, 32.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let cr = 4.0;
        if active {
            p.rect_filled(rect, cr, ui.visuals().selection.bg_fill);
        } else if resp.hovered() {
            p.rect_filled(rect, cr, ui.visuals().widgets.hovered.bg_fill);
        }
        let text_color = if active {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
        let text_pos = Pos2::new(rect.left() + pad_x, rect.center().y - text_size.y * 0.5);
        p.galley(text_pos, galley, text_color);
    }
    if resp.clicked() {
        on_click();
    }
}

/// "Paths" + color dot as one 32px-tall button. Click opens popup palette above.
fn path_overlay_button(ui: &mut egui::Ui, selected: &mut Option<usize>) {
    use super::state::PATH_PALETTE;

    let dot_r = 6.0;
    let font = egui::FontId::proportional(14.0);
    let galley =
        ui.painter()
            .layout_no_wrap("Paths".to_string(), font.clone(), ui.visuals().text_color());
    let text_size = galley.size();

    // Layout: [pad] text [gap] dot [pad]
    let pad_x = 8.0;
    let gap = 8.0;
    let total_w = pad_x + text_size.x + gap + dot_r * 2.0 + pad_x;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(total_w, 32.0), Sense::click());

    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let cr = 4.0;

        if resp.hovered() {
            p.rect_filled(rect, cr, ui.visuals().widgets.hovered.bg_fill);
        }

        // Text
        let text_pos = Pos2::new(rect.left() + pad_x, rect.center().y - text_size.y * 0.5);
        p.galley(text_pos, galley, ui.visuals().text_color());

        // Color dot
        let dot_center = Pos2::new(rect.right() - pad_x - dot_r, rect.center().y);
        match *selected {
            Some(idx) => {
                let &[rc, gc, bc] = PATH_PALETTE.get(idx).unwrap_or(&PATH_PALETTE[0]);
                // Fill + white border
                p.circle_filled(dot_center, dot_r, Color32::from_rgb(rc, gc, bc));
                p.circle_stroke(
                    dot_center,
                    dot_r,
                    egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(255, 255, 255, 200)),
                );
            }
            None => {
                let gray: u8 = 100;
                p.circle_stroke(
                    dot_center,
                    dot_r,
                    egui::Stroke::new(1.0, Color32::from_gray(gray)),
                );
                let d = dot_r * 0.55;
                p.line_segment(
                    [
                        Pos2::new(dot_center.x - d, dot_center.y + d),
                        Pos2::new(dot_center.x + d, dot_center.y - d),
                    ],
                    egui::Stroke::new(1.0, Color32::from_gray(gray)),
                );
            }
        }
    }

    // Popup palette — opens above the button
    let popup_dot_r = 6.0;
    let popup_dot_size = Vec2::splat(popup_dot_r * 2.0 + 6.0);

    egui::Popup::from_toggle_button_response(&resp)
        .align(egui::RectAlign::TOP_START)
        .align_alternatives(&[egui::RectAlign::TOP_END])
        .gap(8.0)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
        .show(|ui: &mut egui::Ui| {
            ui.horizontal(|ui: &mut egui::Ui| {
                // "Off" option
                let is_off = selected.is_none();
                let (rect, resp) = ui.allocate_exact_size(popup_dot_size, Sense::click());
                if ui.is_rect_visible(rect) {
                    let c = rect.center();
                    let p = ui.painter();
                    if is_off {
                        p.circle_stroke(
                            c,
                            popup_dot_r + 2.0,
                            egui::Stroke::new(1.5, Color32::WHITE),
                        );
                    }
                    let gray = if resp.hovered() { 160 } else { 100 };
                    p.circle_stroke(
                        c,
                        popup_dot_r,
                        egui::Stroke::new(1.0, Color32::from_gray(gray)),
                    );
                    let d = popup_dot_r * 0.55;
                    p.line_segment(
                        [Pos2::new(c.x - d, c.y + d), Pos2::new(c.x + d, c.y - d)],
                        egui::Stroke::new(1.0, Color32::from_gray(gray)),
                    );
                }
                if resp.on_hover_text("Off").clicked() {
                    *selected = None;
                }

                // Palette colors
                for (i, &[rc, gc, bc]) in PATH_PALETTE.iter().enumerate() {
                    let is_sel = *selected == Some(i);
                    let (rect, resp) = ui.allocate_exact_size(popup_dot_size, Sense::click());
                    if ui.is_rect_visible(rect) {
                        let c = rect.center();
                        let p = ui.painter();
                        let alpha = if resp.hovered() || is_sel { 255 } else { 180 };
                        p.circle_filled(
                            c,
                            popup_dot_r,
                            Color32::from_rgba_unmultiplied(rc, gc, bc, alpha),
                        );
                        if is_sel {
                            p.circle_stroke(
                                c,
                                popup_dot_r + 2.0,
                                egui::Stroke::new(1.5, Color32::WHITE),
                            );
                        }
                    }
                    if resp.clicked() {
                        *selected = Some(i);
                    }
                }
            });
        });
}

fn strip_uv_view(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    use egui_phosphor::fill::*;
    let no_text = !ui.ctx().wants_keyboard_input();
    let maps = [
        (MapMode::Color, egui::Key::Num1, "1", PALETTE, "Color"),
        (MapMode::Height, egui::Key::Num2, "2", MOUNTAINS, "Height"),
        (MapMode::Normal, egui::Key::Num3, "3", SPHERE, "Normal"),
        (MapMode::StrokeId, egui::Key::Num4, "4", HASH, "Stroke ID"),
    ];
    for (mode, key, num, icon, tooltip) in &maps {
        let pressed = no_text
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key));
        if toolbar_icon_button(ui, state.map_mode == *mode, icon, num, tooltip) || pressed {
            state.map_mode = *mode;
        }
    }
    ui.separator();
    overlay_text_button(ui, "Wireframe", state.viewport.show_wireframe, || {
        state.viewport.show_wireframe = !state.viewport.show_wireframe;
    });
}

fn strip_setup(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    use egui_phosphor::fill::*;
    let no_text = !ui.ctx().wants_keyboard_input();
    let tools = [
        (GuideTool::Select, egui::Key::Num1, "1", CURSOR, "Select"),
        (GuideTool::AddDirectional, egui::Key::Num2, "2", COMPASS, "Directional"),
        (GuideTool::AddRadial, egui::Key::Num3, "3", TARGET, "Radial"),
        (GuideTool::AddVortex, egui::Key::Num4, "4", SPIRAL, "Vortex"),
    ];
    for (tool, key, num, icon, tooltip) in &tools {
        let pressed = no_text
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key));
        let clicked = toolbar_icon_button(ui, state.guide_tool == *tool, icon, num, tooltip);
        if (clicked || pressed) && state.guide_tool != *tool {
            if *tool != GuideTool::Select {
                state.selected_guide = None;
            }
            state.guide_tool = *tool;
        }
    }
    ui.separator();

    // Base texture mode: Color / Normal
    overlay_text_button(ui, "Col", state.setup_map_mode == SetupMapMode::Color, || {
        state.setup_map_mode = SetupMapMode::Color;
        state.setup_base_key = None; // force cache refresh
    });
    overlay_text_button(ui, "Nrm", state.setup_map_mode == SetupMapMode::Normal, || {
        state.setup_map_mode = SetupMapMode::Normal;
        state.setup_base_key = None;
    });

    ui.separator();
    strip_overlay_controls(ui, state);
}

fn strip_3d(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    use egui_phosphor::fill::*;
    use super::state::OrbitTarget;
    let no_text = !ui.ctx().wants_keyboard_input();

    let targets = [
        (OrbitTarget::Object, egui::Key::Num1, "1", CAMERA_ROTATE, "Camera"),
        (OrbitTarget::Light, egui::Key::Num2, "2", SUN, "Light"),
    ];
    // Effective target reflects temporary override (e.g. middle-click camera)
    let effective_target = state.mesh_preview.orbit_target_override
        .unwrap_or(state.mesh_preview.orbit_target);
    for (target, key, num, icon, tooltip) in &targets {
        let pressed = no_text
            && ui
                .ctx()
                .input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key));
        if toolbar_icon_button(ui, effective_target == *target, icon, num, tooltip)
            || pressed
        {
            state.mesh_preview.orbit_target = *target;
            state.mesh_preview.orbit_target_override = None;
        }
    }

    ui.separator();

    ui.add_space(4.0);
    ui.label("Ambient");
    ui.add_space(4.0);
    ui.add(
        egui::DragValue::new(&mut state.mesh_preview.ambient)
            .range(0.0..=1.0)
            .speed(0.01)
            .fixed_decimals(2),
    );

    ui.separator();

    // 3D overlay toggles: Result (generated textures) and Direction (field arrows)
    {
        let label = match state.mesh_preview.result_mode {
            ResultMode::None => "Result: None",
            ResultMode::Paint => "Result: Paint",
            ResultMode::Drawing => "Result: Drawing",
        };
        let active = state.mesh_preview.result_mode != ResultMode::None;
        let response = overlay_text_button_response(ui, label, active);
        if response.clicked() {
            // Cycle: None → Paint → Drawing → None
            state.mesh_preview.result_mode = match state.mesh_preview.result_mode {
                ResultMode::None => ResultMode::Paint,
                ResultMode::Paint => ResultMode::Drawing,
                ResultMode::Drawing => ResultMode::None,
            };
        }
    }
    overlay_text_button(ui, "Direction", state.mesh_preview.show_direction_field, || {
        state.mesh_preview.show_direction_field = !state.mesh_preview.show_direction_field;
    });
}

// ── Playback bar (Drawing mode) ─────────────────────────────────────

fn draw_playback_bar(ui: &mut egui::Ui, state: &mut AppState) {
    use egui_phosphor::fill::*;
    use super::state::PlaybackMode;

    ui.vertical(|ui: &mut egui::Ui| {
        // Row 1: Play/Pause, Stop, Loop mode, progress slider, time label
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let icon = if state.mesh_preview.playing { PAUSE } else { PLAY };
            if ui.button(egui::RichText::new(icon).size(16.0)).clicked() {
                state.mesh_preview.playing = !state.mesh_preview.playing;
                // Reset pingpong direction and re-enable play for Once mode restart
                if state.mesh_preview.playing {
                    state.mesh_preview.pingpong_forward = true;
                }
            }
            // Stop/reset button
            if ui.button(egui::RichText::new(STOP).size(16.0)).clicked() {
                state.mesh_preview.playing = false;
                state.mesh_preview.time = 0.0;
                state.mesh_preview.pingpong_forward = true;
            }

            // Loop mode cycle button
            let (mode_icon, mode_tip) = match state.mesh_preview.playback_mode {
                PlaybackMode::Loop => (REPEAT, "Loop"),
                PlaybackMode::PingPong => (ARROWS_LEFT_RIGHT, "Ping-Pong"),
                PlaybackMode::Once => (ARROW_RIGHT, "Once"),
            };
            let btn = ui.button(egui::RichText::new(mode_icon).size(16.0))
                .on_hover_text(mode_tip);
            if btn.clicked() {
                state.mesh_preview.playback_mode = match state.mesh_preview.playback_mode {
                    PlaybackMode::Loop => PlaybackMode::PingPong,
                    PlaybackMode::PingPong => PlaybackMode::Once,
                    PlaybackMode::Once => PlaybackMode::Loop,
                };
            }

            let max_time = playback_max_time(state);
            let slider = egui::Slider::new(&mut state.mesh_preview.time, 0.0..=max_time)
                .show_value(false)
                .trailing_fill(true);
            ui.add_sized(Vec2::new(200.0, 18.0), slider);

            ui.label(format!("{:.1}s", state.mesh_preview.time));
        });

        // Row 2: Speed, Draw Time, Gap, Chunk, Draw Order
        ui.horizontal(|ui: &mut egui::Ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            ui.label("Speed");
            ui.add(
                egui::DragValue::new(&mut state.mesh_preview.speed)
                    .range(0.1..=4.0)
                    .speed(0.05)
                    .suffix("×")
                    .fixed_decimals(1),
            );

            ui.separator();

            ui.label("Draw");
            ui.add(
                egui::DragValue::new(&mut state.mesh_preview.draw_time)
                    .range(0.05..=5.0)
                    .speed(0.02)
                    .suffix("s")
                    .fixed_decimals(2),
            );

            ui.separator();

            ui.label("Gap");
            ui.add(
                egui::DragValue::new(&mut state.mesh_preview.gap)
                    .range(-3.0..=5.0)
                    .speed(0.02)
                    .suffix("s")
                    .fixed_decimals(2),
            );

            ui.separator();

            ui.label("Chunk");
            ui.add(
                egui::DragValue::new(&mut state.mesh_preview.chunk_size)
                    .range(1..=200)
                    .speed(0.2),
            );

            ui.separator();

            let order_label = match state.mesh_preview.draw_order {
                DrawOrder::Sequential => "Sequential",
                DrawOrder::Random => "Random",
            };
            egui::ComboBox::from_id_salt("draw_order")
                .selected_text(order_label)
                .width(90.0)
                .show_ui(ui, |ui: &mut egui::Ui| {
                    ui.selectable_value(&mut state.mesh_preview.draw_order, DrawOrder::Sequential, "Sequential");
                    ui.selectable_value(&mut state.mesh_preview.draw_order, DrawOrder::Random, "Random");
                });
        });
    });
}

/// Compute the maximum playback time in seconds.
fn playback_max_time(state: &AppState) -> f32 {
    let draw = state.mesh_preview.draw_time;
    let gap = state.mesh_preview.gap;
    let chunk = state.mesh_preview.chunk_size.max(1) as f32;
    let num_groups = (state.mesh_preview.stroke_count as f32 / chunk).ceil().max(1.0);
    // Last group starts at (num_groups-1) * (draw+gap), finishes at + draw
    ((num_groups - 1.0) * (draw + gap) + draw + 0.05).max(0.1)
}

// ── UV View tab ─────────────────────────────────────────────────────

fn show_uv_view(ui: &mut egui::Ui, state: &mut AppState) {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;

    painter.rect_filled(rect, 0.0, Color32::from_gray(48));

    // Texture
    let tex = match state.map_mode {
        MapMode::Color => state
            .textures
            .color
            .as_ref()
            .or(state.textures.base_texture.as_ref()),
        MapMode::Height => state.textures.height.as_ref(),
        MapMode::Normal => state.textures.normal.as_ref(),
        MapMode::StrokeId => state.textures.stroke_id.as_ref(),
    };

    draw_texture(&painter, tex, state, rect);
    draw_wireframe(&painter, state, rect);

    draw_stale_badge(&painter, state, rect);

    // Floating strip overlay at bottom center
    egui::Area::new(egui::Id::new("strip_uv"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(rect.center().x, rect.bottom() - 8.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    strip_uv_view(ui, state);
                });
            });
        });

    handle_pan_zoom(&response, ui, state, rect, false);
}

// ── Setup tab (formerly Guide) ───────────────────────────────────────

fn show_setup_view(ui: &mut egui::Ui, state: &mut AppState) {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;

    painter.rect_filled(rect, 0.0, Color32::from_gray(48));

    // Show selected layer's base texture (color or normal)
    update_setup_base_texture(ui.ctx(), state);
    let tex = state
        .setup_base_tex
        .as_ref()
        .or(state.textures.base_texture.as_ref());
    draw_texture(&painter, tex, state, rect);
    draw_wireframe(&painter, state, rect);

    // Dim overlay — group-specific mask or uniform fallback
    update_group_dim_cache(ui.ctx(), state);
    let uv_min = uv_to_screen(glam::Vec2::ZERO, state, rect);
    let uv_max = uv_to_screen(glam::Vec2::ONE, state, rect);
    let uv_rect = Rect::from_min_max(uv_min, uv_max);
    if let Some(ref tex) = state.group_dim_cache.texture {
        painter.image(
            tex.id(),
            uv_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.rect_filled(uv_rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 80));
    }

    draw_path_overlay(&painter, state, rect);

    // Guides (always shown in Guide tab)
    draw_guides(&painter, state, rect);

    // Guide interaction (tool-based)
    super::guide_editor::handle_guides(&response, ui, state, rect);

    // Guide properties popup
    draw_guide_popup(ui, state, rect);

    draw_stale_badge(&painter, state, rect);

    // Floating strip overlay at bottom center
    egui::Area::new(egui::Id::new("strip_setup"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(rect.center().x, rect.bottom() - 8.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    strip_setup(ui, state);
                });
            });
        });

    handle_pan_zoom(&response, ui, state, rect, state.guide_drag.is_some());
}

// ── 3D tab ──────────────────────────────────────────────────────────

fn show_3d_view(
    ui: &mut egui::Ui,
    state: &mut AppState,
    render_state: Option<&egui_wgpu::RenderState>,
) {
    let view_rect = ui.available_rect_before_wrap();

    // Advance time map playback
    if state.mesh_preview.result_mode == ResultMode::Drawing
        && state.mesh_preview.playing
    {
        use super::state::PlaybackMode;
        let dt = ui.input(|i| i.unstable_dt).min(0.1);
        let max_t = playback_max_time(state);
        match state.mesh_preview.playback_mode {
            PlaybackMode::Loop => {
                state.mesh_preview.time += dt * state.mesh_preview.speed;
                if state.mesh_preview.time > max_t {
                    state.mesh_preview.time = 0.0;
                }
            }
            PlaybackMode::PingPong => {
                let delta = dt * state.mesh_preview.speed;
                if state.mesh_preview.pingpong_forward {
                    state.mesh_preview.time += delta;
                    if state.mesh_preview.time >= max_t {
                        state.mesh_preview.time = max_t;
                        state.mesh_preview.pingpong_forward = false;
                    }
                } else {
                    state.mesh_preview.time -= delta;
                    if state.mesh_preview.time <= 0.0 {
                        state.mesh_preview.time = 0.0;
                        state.mesh_preview.pingpong_forward = true;
                    }
                }
            }
            PlaybackMode::Once => {
                state.mesh_preview.time += dt * state.mesh_preview.speed;
                if state.mesh_preview.time >= max_t {
                    state.mesh_preview.time = max_t;
                    state.mesh_preview.playing = false;
                }
            }
        }
        ui.ctx().request_repaint();
    }

    if let Some(rs) = render_state {
        if state.mesh_preview.gpu_ready {
            mesh_preview::show(ui, state, rs);
        } else {
            ui.centered_and_justified(|ui: &mut egui::Ui| {
                ui.label("No mesh loaded for 3D preview.");
            });
        }
    } else {
        ui.centered_and_justified(|ui: &mut egui::Ui| {
            ui.label("wgpu not available.");
        });
    }

    // Floating strip overlay at bottom center
    let strip_bottom = view_rect.bottom() - 8.0;
    egui::Area::new(egui::Id::new("strip_3d"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(view_rect.center().x, strip_bottom))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    strip_3d(ui, state);
                });
            });
        });

    // Floating playback bar — visible only in Drawing mode, above the strip
    if state.mesh_preview.result_mode == ResultMode::Drawing {
        egui::Area::new(egui::Id::new("playback_bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view_rect.center().x, strip_bottom - 44.0))
            .pivot(egui::Align2::CENTER_BOTTOM)
            .show(ui.ctx(), |ui: &mut egui::Ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                    draw_playback_bar(ui, state);
                });
            });
    }
}

// ── Drawing helpers ─────────────────────────────────────────────────

fn draw_texture(
    painter: &egui::Painter,
    tex: Option<&egui::TextureHandle>,
    state: &AppState,
    rect: Rect,
) {
    let uv_min = uv_to_screen(glam::Vec2::ZERO, state, rect);
    let uv_max = uv_to_screen(glam::Vec2::ONE, state, rect);
    let tex_rect = Rect::from_min_max(uv_min, uv_max);

    if let Some(tex) = tex {
        painter.image(
            tex.id(),
            tex_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    } else {
        painter.rect_filled(tex_rect, 0.0, Color32::from_gray(80));
    }

    painter.rect_stroke(
        tex_rect,
        0.0,
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 160)),
        egui::StrokeKind::Outside,
    );
}

fn draw_wireframe(painter: &egui::Painter, state: &AppState, rect: Rect) {
    if !state.viewport.show_wireframe {
        return;
    }
    if let Some(ref edges) = state.uv_edges {
        let halo_stroke = egui::Stroke::new(2.0, Color32::from_rgba_unmultiplied(0, 0, 0, 100));
        let wire_stroke =
            egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 150));
        for &(a, b) in edges {
            let sa = uv_to_screen(a, state, rect);
            let sb = uv_to_screen(b, state, rect);
            painter.line_segment([sa, sb], halo_stroke);
        }
        for &(a, b) in edges {
            let sa = uv_to_screen(a, state, rect);
            let sb = uv_to_screen(b, state, rect);
            painter.line_segment([sa, sb], wire_stroke);
        }
    }
}

fn draw_guides(painter: &egui::Painter, state: &AppState, rect: Rect) {
    let Some(idx) = state.selected_layer else {
        return;
    };
    if idx >= state.project.layers.len() {
        return;
    }

    let guides = &state.project.layers[idx].guides;
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, rect);
        let is_sel = state.selected_guide == Some(i);
        let radius_px = guide.influence * state.viewport.zoom;

        // Influence circle
        let circ_width = if is_sel { 2.5 } else { 2.0 };
        let circ_alpha = if is_sel { 200 } else { 120 };
        painter.circle_stroke(
            center,
            radius_px,
            egui::Stroke::new(
                circ_width,
                Color32::from_rgba_unmultiplied(255, 200, 0, circ_alpha),
            ),
        );

        let arrow_alpha = if is_sel { 220 } else { 160 };
        let arrow_color = Color32::from_rgba_unmultiplied(255, 200, 0, arrow_alpha);

        // Type-specific rendering
        match guide.guide_type {
            GuideType::Directional => {
                draw_guide_directional(painter, center, guide, radius_px, arrow_color);
            }
            GuideType::Source => {
                draw_guide_radial(painter, center, radius_px, arrow_color, true);
            }
            GuideType::Sink => {
                draw_guide_radial(painter, center, radius_px, arrow_color, false);
            }
            GuideType::Vortex => {
                let cw = guide.direction.x < 0.0;
                draw_guide_vortex(painter, center, radius_px, arrow_color, cw);
            }
        }

        // Center dot
        let dot_color = if is_sel {
            Color32::from_rgba_unmultiplied(255, 255, 100, 255)
        } else {
            Color32::from_rgba_unmultiplied(255, 200, 0, 200)
        };
        painter.circle_filled(center, if is_sel { 5.0 } else { 4.0 }, dot_color);
    }
}

/// Directional guide: single arrow in guide.direction.
fn draw_guide_directional(
    painter: &egui::Painter,
    center: Pos2,
    guide: &practical_arcana_painter::types::Guide,
    radius_px: f32,
    color: Color32,
) {
    let dir = if guide.direction.length() > 0.001 {
        guide.direction.normalize()
    } else {
        glam::Vec2::X
    };
    let arrow_len = 40.0_f32.min(radius_px.max(20.0));
    let tip = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
    painter.line_segment([center, tip], egui::Stroke::new(2.0, color));
    draw_arrowhead(painter, tip, dir, color);
}

/// Source/Sink guide: 4 radial arrows (outward for Source, inward for Sink).
fn draw_guide_radial(
    painter: &egui::Painter,
    center: Pos2,
    radius_px: f32,
    color: Color32,
    outward: bool,
) {
    let arrow_len = 20.0_f32.min(radius_px.max(12.0));
    let inner_r = 6.0;

    for i in 0..4 {
        let angle = i as f32 * std::f32::consts::FRAC_PI_2;
        let dir = glam::Vec2::new(angle.cos(), angle.sin());

        let (start, tip, arrow_dir) = if outward {
            // Source: arrows point outward from center
            let s = Pos2::new(center.x + dir.x * inner_r, center.y + dir.y * inner_r);
            let t = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
            (s, t, dir)
        } else {
            // Sink: arrows point inward toward center
            let s = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
            let t = Pos2::new(center.x + dir.x * inner_r, center.y + dir.y * inner_r);
            (s, t, -dir)
        };

        painter.line_segment([start, tip], egui::Stroke::new(2.0, color));
        draw_arrowhead(painter, tip, arrow_dir, color);
    }
}

/// Vortex guide: curved arrows indicating rotation around center.
/// `clockwise` flips the arc direction.
fn draw_guide_vortex(
    painter: &egui::Painter,
    center: Pos2,
    radius_px: f32,
    color: Color32,
    clockwise: bool,
) {
    let r = 16.0_f32.min(radius_px.max(10.0));
    let sign = if clockwise { -1.0_f32 } else { 1.0 };

    // Draw 3 arc segments around the center
    for i in 0..3 {
        let start_angle = i as f32 * std::f32::consts::TAU / 3.0;
        let segments = 8;
        let arc_len = sign * std::f32::consts::FRAC_PI_2; // 90° each

        let mut points = Vec::with_capacity(segments + 1);
        for s in 0..=segments {
            let angle = start_angle + arc_len * s as f32 / segments as f32;
            points.push(Pos2::new(
                center.x + angle.cos() * r,
                center.y + angle.sin() * r,
            ));
        }

        for w in points.windows(2) {
            painter.line_segment([w[0], w[1]], egui::Stroke::new(2.0, color));
        }

        // Arrowhead at the end of the arc
        if let Some(&tip) = points.last() {
            let end_angle = start_angle + arc_len;
            let tangent = glam::Vec2::new(-end_angle.sin(), end_angle.cos()) * sign;
            draw_arrowhead(painter, tip, tangent, color);
        }
    }
}

/// Draw per-layer path overlay lines on the viewport.
fn draw_path_overlay(painter: &egui::Painter, state: &AppState, rect: Rect) {
    let Some(idx) = state.viewport.path_overlay_idx else {
        return;
    };
    let &[r, g, b] = super::state::PATH_PALETTE
        .get(idx)
        .unwrap_or(&super::state::PATH_PALETTE[0]);

    let path_stroke = egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(r, g, b, 80));

    let mut badge_info: Option<(usize, Option<u32>)> = None;
    if let Some((_layer_idx, paths, orig_segs)) =
        state.path_overlay.selected_paths(state.selected_layer)
    {
        // Safety cap: prevent wgpu buffer overflow if worker budget wasn't enough.
        const MAX_SEGMENTS: usize = 100_000;
        let total_segs: usize = paths.iter().map(|p| p.len().saturating_sub(1)).sum();
        let stride = if total_segs > MAX_SEGMENTS {
            total_segs.div_ceil(MAX_SEGMENTS)
        } else {
            1
        };

        for path in paths {
            if path.len() < 2 {
                continue;
            }
            let mut i = 0;
            while i + stride < path.len() {
                let a = uv_to_screen(glam::Vec2::new(path[i][0], path[i][1]), state, rect);
                let b = uv_to_screen(
                    glam::Vec2::new(path[i + stride][0], path[i + stride][1]),
                    state,
                    rect,
                );
                painter.line_segment([a, b], path_stroke);
                i += stride;
            }
            if i < path.len() - 1 {
                let last = path.len() - 1;
                let a = uv_to_screen(glam::Vec2::new(path[i][0], path[i][1]), state, rect);
                let b = uv_to_screen(glam::Vec2::new(path[last][0], path[last][1]), state, rect);
                painter.line_segment([a, b], path_stroke);
            }
        }

        // Remember path info for badge (drawn after, to avoid overlap with spinner)
        let simplify_pct = if orig_segs > 0 && total_segs < orig_segs {
            Some((total_segs as f64 / orig_segs as f64 * 100.0).round() as u32)
        } else {
            None
        };
        badge_info = Some((paths.len(), simplify_pct));
    }

    // Badge: spinner when computing, path count when done (mutually exclusive, top-right)
    let badge_right = rect.right() - 8.0;
    let badge_top = rect.top() + 8.0;
    if state.path_worker.is_running() {
        // Measure text to size the background
        let text = "Computing paths…";
        let font = egui::FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(text.to_string(), font.clone(), Color32::WHITE);
        let spinner_r = 5.0;
        let pad = 6.0;
        let badge_w = spinner_r * 2.0 + 8.0 + galley.size().x + pad * 2.0;
        let badge_h = galley.size().y + pad;
        let badge_rect = Rect::from_min_size(
            Pos2::new(badge_right - badge_w, badge_top),
            Vec2::new(badge_w, badge_h),
        );
        painter.rect_filled(
            badge_rect,
            4.0,
            Color32::from_rgba_unmultiplied(180, 100, 30, 200),
        );

        // Spinning arc inside badge
        let time = painter.ctx().input(|i| i.time) as f32;
        let center = Pos2::new(badge_rect.left() + pad + spinner_r, badge_rect.center().y);
        let start_angle = time * 5.0;
        let arc_len = std::f32::consts::PI * 1.5;
        let steps = 16;
        let arc_pts: Vec<Pos2> = (0..=steps)
            .map(|i| {
                let a = start_angle + (i as f32 / steps as f32) * arc_len;
                Pos2::new(
                    center.x + spinner_r * a.cos(),
                    center.y + spinner_r * a.sin(),
                )
            })
            .collect();
        painter.add(egui::Shape::line(
            arc_pts,
            egui::Stroke::new(1.5, Color32::WHITE),
        ));
        painter.text(
            Pos2::new(center.x + spinner_r + 6.0, badge_rect.center().y),
            egui::Align2::LEFT_CENTER,
            text,
            font,
            Color32::WHITE,
        );
    } else if let Some((count, simplify_pct)) = badge_info {
        let label = if let Some(pct) = simplify_pct {
            format!("{count} paths (simplified, ~{pct}% shown)")
        } else {
            format!("{count} paths")
        };
        let font = egui::FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(label.clone(), font.clone(), Color32::WHITE);
        let badge_w = galley.size().x + 12.0;
        let badge_h = galley.size().y + 6.0;
        let badge_rect = Rect::from_min_size(
            Pos2::new(badge_right - badge_w, badge_top),
            Vec2::new(badge_w, badge_h),
        );
        let bg = if simplify_pct.is_some() {
            Color32::from_rgba_unmultiplied(180, 100, 30, 200)
        } else {
            Color32::from_rgba_unmultiplied(80, 80, 80, 160)
        };
        painter.rect_filled(badge_rect, 4.0, bg);
        painter.text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            font,
            Color32::WHITE,
        );
    }
}

/// Draw a small arrowhead at `tip` pointing in `dir`.
fn draw_arrowhead(painter: &egui::Painter, tip: Pos2, dir: glam::Vec2, color: Color32) {
    let perp = glam::Vec2::new(-dir.y, dir.x);
    let hs = 8.0;
    let bx = tip.x - dir.x * hs;
    let by = tip.y - dir.y * hs;
    let left = Pos2::new(bx + perp.x * hs * 0.5, by + perp.y * hs * 0.5);
    let right = Pos2::new(bx - perp.x * hs * 0.5, by - perp.y * hs * 0.5);
    painter.line_segment([tip, left], egui::Stroke::new(2.0, color));
    painter.line_segment([tip, right], egui::Stroke::new(2.0, color));
}

/// Draw a status badge in the top-left corner of the viewport.
/// Shows "Updating..." while generation is running, or stale reason if outdated.
fn draw_stale_badge(painter: &egui::Painter, state: &AppState, rect: Rect) {
    let (label, bg_color) = if state.generation.is_running() {
        ("Updating...", Color32::from_rgba_unmultiplied(60, 120, 180, 200))
    } else if let Some(reason) = state.stale_reason() {
        (reason, Color32::from_rgba_unmultiplied(180, 140, 30, 200))
    } else {
        return;
    };

    let font = egui::FontId::proportional(11.0);
    let galley = painter.layout_no_wrap(label.to_string(), font.clone(), Color32::WHITE);
    let badge_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.top() + 8.0),
        Vec2::new(galley.size().x + 12.0, galley.size().y + 6.0),
    );
    painter.rect_filled(badge_rect, 4.0, bg_color);
    painter.text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        Color32::WHITE,
    );
}

/// Common pan/zoom handling for 2D viewport modes.
fn handle_pan_zoom(
    response: &egui::Response,
    ui: &egui::Ui,
    state: &mut AppState,
    rect: Rect,
    guide_dragging: bool,
) {
    // Pan (middle-drag or alt+left-drag, not during guide drag)
    if !guide_dragging
        && (response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary) && ui.input(|i| i.modifiers.alt)))
    {
        let delta = response.drag_delta();
        state.viewport.offset.x -= delta.x / state.viewport.zoom;
        state.viewport.offset.y -= delta.y / state.viewport.zoom;
    }

    // Zoom (scroll wheel)
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let zoom_factor = (scroll * 0.003).exp();
            let old_zoom = state.viewport.zoom;
            state.viewport.zoom = (old_zoom * zoom_factor).clamp(32.0, 8192.0);

            if let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) {
                let uv_at_cursor = glam::Vec2::new(
                    (pointer.x - rect.left()) / old_zoom + state.viewport.offset.x,
                    (pointer.y - rect.top()) / old_zoom + state.viewport.offset.y,
                );
                state.viewport.offset.x =
                    uv_at_cursor.x - (pointer.x - rect.left()) / state.viewport.zoom;
                state.viewport.offset.y =
                    uv_at_cursor.y - (pointer.y - rect.top()) / state.viewport.zoom;
            }
        }
    }
}

// ── Guide properties popup ──────────────────────────────────────────

/// Floating popup window showing the selected guide's properties.
/// Positioned on the opposite quadrant from the guide.
fn draw_guide_popup(ui: &mut egui::Ui, state: &mut AppState, viewport_rect: Rect) {
    let Some(layer_idx) = state.selected_layer else {
        return;
    };
    let Some(guide_idx) = state.selected_guide else {
        return;
    };
    if layer_idx >= state.project.layers.len() {
        return;
    }
    if guide_idx >= state.project.layers[layer_idx].guides.len() {
        return;
    }

    let guide_pos = state.project.layers[layer_idx].guides[guide_idx].position;
    let guide_screen = uv_to_screen(guide_pos, state, viewport_rect);

    // Position popup near the guide, offset to bottom-right by default.
    // Flip to the other side if it would overflow the viewport.
    let popup_w = 200.0;
    let popup_h = 160.0;
    let gap = 24.0;

    let mut popup_x = guide_screen.x + gap;
    let mut popup_y = guide_screen.y + gap;

    if popup_x + popup_w > viewport_rect.right() {
        popup_x = guide_screen.x - gap - popup_w;
    }
    if popup_y + popup_h > viewport_rect.bottom() {
        popup_y = guide_screen.y - gap - popup_h;
    }

    popup_x = popup_x.clamp(
        viewport_rect.left(),
        (viewport_rect.right() - popup_w).max(viewport_rect.left()),
    );
    popup_y = popup_y.clamp(
        viewport_rect.top(),
        (viewport_rect.bottom() - popup_h).max(viewport_rect.top()),
    );

    // Disable popup interaction during drag to prevent it from stealing input
    let interactable = state.guide_drag.is_none();

    egui::Window::new(format!("Guide #{}", guide_idx))
        .movable(false)
        .resizable(false)
        .collapsible(false)
        .title_bar(false)
        .interactable(interactable)
        .fixed_pos(Pos2::new(popup_x, popup_y))
        .default_width(popup_w)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            use egui_phosphor::fill::*;

            // Title row: guide name (left) + delete button (right)
            let mut deleted = false;
            let button_size = 24.0;
            ui.horizontal(|ui: &mut egui::Ui| {
                ui.label(format!("Guide #{}", guide_idx));
                let space = (ui.available_width() - button_size).max(0.0);
                ui.add_space(space);
                ui.scope(|ui: &mut egui::Ui| {
                    ui.visuals_mut().widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
                    ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                    let btn = egui::Button::new(egui::RichText::new(TRASH_SIMPLE).size(15.0))
                        .min_size(egui::Vec2::splat(button_size));
                    if ui.add(btn).on_hover_text("Delete").clicked() {
                        deleted = true;
                    }
                });
            });

            if deleted {
                state.project.layers[layer_idx].guides.remove(guide_idx);
                state.selected_guide = None;
                state.guide_drag = None;
                return;
            }

            ui.separator();

            let guide = &mut state.project.layers[layer_idx].guides[guide_idx];

            // Guide type — Source/Sink shown as "Radial"
            let display_type = match guide.guide_type {
                GuideType::Source | GuideType::Sink => "Radial",
                GuideType::Directional => "Directional",
                GuideType::Vortex => "Vortex",
            };
            ui.horizontal(|ui: &mut egui::Ui| {
                egui::ComboBox::from_id_salt("guide_type")
                    .selected_text(display_type)
                    .show_ui(ui, |ui: &mut egui::Ui| {
                        ui.selectable_value(
                            &mut guide.guide_type,
                            GuideType::Directional,
                            "Directional",
                        );
                        if ui
                            .selectable_value(&mut guide.guide_type, GuideType::Source, "Radial")
                            .changed()
                        {
                            guide.direction.x = 1.0;
                        }
                        ui.selectable_value(&mut guide.guide_type, GuideType::Vortex, "Vortex");
                    });

                // Sub-option toggle beside the combo
                if guide.guide_type == GuideType::Source || guide.guide_type == GuideType::Sink {
                    let outward = guide.guide_type == GuideType::Source;
                    if ui.selectable_label(outward, "Out").clicked() {
                        guide.guide_type = GuideType::Source;
                    }
                    if ui.selectable_label(!outward, "In").clicked() {
                        guide.guide_type = GuideType::Sink;
                    }
                }
                if guide.guide_type == GuideType::Vortex {
                    let cw = guide.direction.x < 0.0;
                    if ui.selectable_label(!cw, "CCW").clicked() {
                        guide.direction.x = 1.0;
                    }
                    if ui.selectable_label(cw, "CW").clicked() {
                        guide.direction.x = -1.0;
                    }
                }
            });

            // Influence
            slider_row(
                ui,
                "guide_influence",
                &mut guide.influence,
                0.02..=1.0,
                "Influence",
                None,
                2,
            );

            // Strength
            slider_row(
                ui,
                "guide_strength",
                &mut guide.strength,
                0.0..=2.0,
                "Strength",
                None,
                2,
            );
        });
}
