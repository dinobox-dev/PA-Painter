use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};
use eframe::egui_wgpu;

use practical_arcana_painter::types::GuideType;

use super::mesh_preview;
use super::state::{AppState, GuideTool, MapMode, ViewportTab};

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

// ── Main viewport entry point ───────────────────────────────────────

/// Draw the viewport: tabs, main content, and floating strip overlay.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    render_state: Option<&egui_wgpu::RenderState>,
) {
    // ── Top tab bar ──
    ui.horizontal(|ui: &mut egui::Ui| {
        let tabs = [
            (ViewportTab::UvView, "UV View"),
            (ViewportTab::Guide, "Guide"),
            (ViewportTab::Mesh3D, "3D"),
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
        ViewportTab::Guide => show_guide_view(ui, state),
        ViewportTab::Mesh3D => show_3d_view(ui, state, render_state),
    }
}

// ── Strip contents ──────────────────────────────────────────────────

/// Square icon button with a small shortcut number badge in the top-left corner.
fn icon_button(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    num: &str,
    tooltip: &str,
) -> bool {
    let size = Vec2::splat(32.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cr = 4.0;

        // Background
        if selected {
            painter.rect_filled(rect, cr, ui.visuals().selection.bg_fill);
        } else if response.hovered() {
            painter.rect_filled(rect, cr, ui.visuals().widgets.hovered.bg_fill);
        }

        // Icon (centered)
        let icon_color = if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            icon,
            egui::FontId::proportional(18.0),
            icon_color,
        );

        // Number badge (top-left)
        painter.text(
            rect.left_top() + Vec2::new(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            num,
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );
    }

    response.on_hover_text(tooltip).clicked()
}

fn strip_uv_view(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    use egui_phosphor::fill::*;
    let maps = [
        (MapMode::Color,    "1", PALETTE,   "Color"),
        (MapMode::Height,   "2", MOUNTAINS, "Height"),
        (MapMode::Normal,   "3", SPHERE,    "Normal"),
        (MapMode::StrokeId, "4", HASH,      "Stroke ID"),
    ];
    for (mode, num, icon, tooltip) in &maps {
        if icon_button(ui, state.map_mode == *mode, icon, num, tooltip) {
            state.map_mode = *mode;
        }
    }
    ui.separator();
    ui.checkbox(&mut state.viewport.show_wireframe, "Wireframe");
    ui.checkbox(&mut state.viewport.show_path_overlay, "Paths");
}

fn strip_guide(ui: &mut egui::Ui, state: &mut AppState) {
    ui.spacing_mut().item_spacing.x = 4.0;
    use egui_phosphor::fill::*;
    let tools = [
        (GuideTool::Select,         "1", CURSOR,  "Select"),
        (GuideTool::AddDirectional, "2", COMPASS, "Directional"),
        (GuideTool::AddRadial,      "3", TARGET,  "Radial"),
        (GuideTool::AddVortex,      "4", SPIRAL,  "Vortex"),
    ];
    for (tool, num, icon, tooltip) in &tools {
        if icon_button(ui, state.guide_tool == *tool, icon, num, tooltip) {
            if state.guide_tool != *tool {
                if *tool != GuideTool::Select {
                    state.selected_guide = None;
                }
                state.guide_tool = *tool;
            }
        }
    }
}

fn strip_3d(ui: &mut egui::Ui, state: &mut AppState) {
    if ui.small_button("Reset Camera").clicked() {
        if let Some(ref mesh) = state.loaded_mesh {
            state.mesh_preview.fit_to_mesh(mesh);
        }
    }
}

// ── UV View tab ─────────────────────────────────────────────────────

fn show_uv_view(ui: &mut egui::Ui, state: &mut AppState) {
    let (response, painter) =
        ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
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
    draw_path_overlay(&painter, state, rect);

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

// ── Guide tab ───────────────────────────────────────────────────────

fn show_guide_view(ui: &mut egui::Ui, state: &mut AppState) {
    let (response, painter) =
        ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;

    painter.rect_filled(rect, 0.0, Color32::from_gray(48));

    // Show color texture as background context
    let tex = state
        .textures
        .color
        .as_ref()
        .or(state.textures.base_texture.as_ref());
    draw_texture(&painter, tex, state, rect);
    draw_wireframe(&painter, state, rect);
    draw_path_overlay(&painter, state, rect);

    // Guides (always shown in Guide tab)
    draw_guides(&painter, state, rect);

    // Guide interaction (tool-based)
    super::guide_editor::handle_guides(&response, ui, state, rect);

    // Guide properties popup
    draw_guide_popup(ui, state, rect);

    draw_stale_badge(&painter, state, rect);

    // Floating strip overlay at bottom center
    egui::Area::new(egui::Id::new("strip_guide"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(rect.center().x, rect.bottom() - 8.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    strip_guide(ui, state);
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
    egui::Area::new(egui::Id::new("strip_3d"))
        .order(egui::Order::Foreground)
        .fixed_pos(Pos2::new(view_rect.center().x, view_rect.bottom() - 8.0))
        .pivot(egui::Align2::CENTER_BOTTOM)
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    strip_3d(ui, state);
                });
            });
        });
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
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 80)),
        egui::StrokeKind::Outside,
    );
}

fn draw_wireframe(painter: &egui::Painter, state: &AppState, rect: Rect) {
    if !state.viewport.show_wireframe {
        return;
    }
    if let Some(ref edges) = state.uv_edges {
        let wire_stroke =
            egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 200, 255, 50));
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
        let circ_alpha = if is_sel { 80 } else { 40 };
        painter.circle_stroke(
            center,
            radius_px,
            egui::Stroke::new(
                1.0,
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
    if !state.viewport.show_path_overlay {
        return;
    }

    let visible = state.path_overlay.visible_paths(&state.project.layers);

    // Color palette for different layers (cycle through)
    let layer_colors = [
        Color32::from_rgba_unmultiplied(200, 180, 140, 100), // warm cream
        Color32::from_rgba_unmultiplied(140, 180, 200, 100), // cool blue
        Color32::from_rgba_unmultiplied(180, 200, 140, 100), // sage green
        Color32::from_rgba_unmultiplied(200, 140, 180, 100), // pink
    ];

    for (layer_idx, paths) in &visible {
        let color = layer_colors[*layer_idx % layer_colors.len()];
        let stroke = egui::Stroke::new(1.0, color);

        for path in *paths {
            for w in path.windows(2) {
                let a = uv_to_screen(glam::Vec2::new(w[0][0], w[0][1]), state, rect);
                let b = uv_to_screen(glam::Vec2::new(w[1][0], w[1][1]), state, rect);
                painter.line_segment([a, b], stroke);
            }
        }
    }
}

/// Draw a small arrowhead at `tip` pointing in `dir`.
fn draw_arrowhead(painter: &egui::Painter, tip: Pos2, dir: glam::Vec2, color: Color32) {
    let perp = glam::Vec2::new(-dir.y, dir.x);
    let hs = 8.0;
    let bx = tip.x - dir.x * hs;
    let by = tip.y - dir.y * hs;
    painter.line_segment(
        [tip, Pos2::new(bx + perp.x * hs * 0.5, by + perp.y * hs * 0.5)],
        egui::Stroke::new(2.0, color),
    );
    painter.line_segment(
        [tip, Pos2::new(bx - perp.x * hs * 0.5, by - perp.y * hs * 0.5)],
        egui::Stroke::new(2.0, color),
    );
}

/// Draw a stale/not-generated badge in the top-right corner of the viewport.
fn draw_stale_badge(painter: &egui::Painter, state: &AppState, rect: Rect) {
    if let Some(reason) = state.stale_reason() {
        let font = egui::FontId::proportional(11.0);
        let badge_pos = Pos2::new(rect.right() - 8.0, rect.top() + 8.0);
        let galley = painter.layout_no_wrap(reason.to_string(), font.clone(), Color32::WHITE);
        let badge_rect = Rect::from_min_size(
            Pos2::new(
                badge_pos.x - galley.size().x - 12.0,
                badge_pos.y,
            ),
            Vec2::new(galley.size().x + 12.0, galley.size().y + 6.0),
        );
        painter.rect_filled(badge_rect, 4.0, Color32::from_rgba_unmultiplied(180, 140, 30, 200));
        painter.text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            reason,
            font,
            Color32::WHITE,
        );
    }
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
            || (response.dragged_by(egui::PointerButton::Primary)
                && ui.input(|i| i.modifiers.alt)))
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
        .interactable(interactable)
        .fixed_pos(Pos2::new(popup_x, popup_y))
        .show(ui.ctx(), |ui: &mut egui::Ui| {
            let guide = &mut state.project.layers[layer_idx].guides[guide_idx];

            // Guide type — Source/Sink shown as "Radial"
            let display_type = match guide.guide_type {
                GuideType::Source | GuideType::Sink => "Radial",
                GuideType::Directional => "Directional",
                GuideType::Vortex => "Vortex",
            };
            egui::ComboBox::from_label("Type")
                .selected_text(display_type)
                .show_ui(ui, |ui: &mut egui::Ui| {
                    ui.selectable_value(
                        &mut guide.guide_type,
                        GuideType::Directional,
                        "Directional",
                    );
                    // "Radial" maps to Source; Inward checkbox toggles to Sink
                    if ui.selectable_value(
                        &mut guide.guide_type,
                        GuideType::Source,
                        "Radial",
                    ).changed() {
                        guide.direction.x = 1.0; // reset to outward
                    }
                    ui.selectable_value(&mut guide.guide_type, GuideType::Vortex, "Vortex");
                });

            // Inward (Radial: Source/Sink toggle)
            if guide.guide_type == GuideType::Source || guide.guide_type == GuideType::Sink {
                let mut inward = guide.guide_type == GuideType::Sink;
                if ui.checkbox(&mut inward, "Inward").changed() {
                    guide.guide_type = if inward { GuideType::Sink } else { GuideType::Source };
                }
            }

            // Clockwise (Vortex only)
            if guide.guide_type == GuideType::Vortex {
                let mut cw = guide.direction.x < 0.0;
                if ui.checkbox(&mut cw, "Clockwise").changed() {
                    guide.direction.x = if cw { -1.0 } else { 1.0 };
                }
            }

            // Influence
            ui.add(
                egui::Slider::new(&mut guide.influence, 0.02..=1.0)
                    .text("Influence"),
            );

            // Strength
            ui.add(
                egui::Slider::new(&mut guide.strength, 0.0..=2.0)
                    .text("Strength"),
            );

            // Delete button
            ui.add_space(4.0);
            if ui.button("Delete").clicked() {
                state.project.layers[layer_idx].guides.remove(guide_idx);
                state.selected_guide = None;
                state.guide_drag = None;
            }
        });
}
