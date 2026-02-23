use eframe::egui::{self, Color32, Pos2, Rect, Sense, Vec2};

use super::state::{AppState, ViewMode};

/// Convert UV coordinate to screen position.
pub fn uv_to_screen(uv: glam::Vec2, state: &AppState, viewport_rect: Rect) -> Pos2 {
    let x = (uv.x - state.viewport.offset.x) * state.viewport.zoom + viewport_rect.left();
    let y = (uv.y - state.viewport.offset.y) * state.viewport.zoom + viewport_rect.top();
    Pos2::new(x, y)
}

/// Convert screen position to UV coordinate.
#[allow(dead_code)]
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
        egui::ColorImage {
            size: [size, size],
            pixels,
        },
        egui::TextureOptions::NEAREST,
    )
}

/// Draw the viewport with texture display, pan, and zoom.
pub fn show(ui: &mut egui::Ui, state: &mut AppState) {
    let (response, painter) =
        ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;

    // Background
    painter.rect_filled(rect, 0.0, Color32::from_gray(48));

    // Determine which texture to show
    let tex = match state.view_mode {
        ViewMode::Color => state
            .textures
            .color
            .as_ref()
            .or(state.textures.base_texture.as_ref()),
        ViewMode::Height => state.textures.height.as_ref(),
        ViewMode::Normal => state.textures.normal.as_ref(),
        ViewMode::StrokeId => state.textures.stroke_id.as_ref(),
    };

    // Draw texture (or tinted rect)
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

    // UV boundary outline
    painter.rect_stroke(
        tex_rect,
        0.0,
        egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 80)),
    );

    // ── UV Wireframe overlay ──
    if state.viewport.show_wireframe {
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

    // ── Guide overlay ──
    if state.viewport.show_guides {
        if let Some(idx) = state.selected_slot {
            if idx < state.project.slots.len() {
                let guides = &state.project.slots[idx].pattern.guides;
                for (i, guide) in guides.iter().enumerate() {
                    let center = uv_to_screen(guide.position, state, rect);
                    let is_sel = state.selected_guide == Some(i);

                    // Influence circle (dashed appearance via lower alpha)
                    let radius_px = guide.influence * state.viewport.zoom;
                    let circ_alpha = if is_sel { 80 } else { 40 };
                    painter.circle_stroke(
                        center,
                        radius_px,
                        egui::Stroke::new(
                            1.0,
                            Color32::from_rgba_unmultiplied(255, 200, 0, circ_alpha),
                        ),
                    );

                    // Direction arrow
                    let dir = if guide.direction.length() > 0.001 {
                        guide.direction.normalize()
                    } else {
                        glam::Vec2::X
                    };
                    let arrow_len = 40.0_f32.min(radius_px.max(20.0));
                    let tip = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
                    let arrow_alpha = if is_sel { 220 } else { 160 };
                    let arrow_color = Color32::from_rgba_unmultiplied(255, 200, 0, arrow_alpha);
                    painter.line_segment([center, tip], egui::Stroke::new(2.0, arrow_color));

                    // Arrowhead
                    let perp = glam::Vec2::new(-dir.y, dir.x);
                    let hs = 8.0; // head size
                    let bx = tip.x - dir.x * hs;
                    let by = tip.y - dir.y * hs;
                    painter.line_segment(
                        [tip, Pos2::new(bx + perp.x * hs * 0.5, by + perp.y * hs * 0.5)],
                        egui::Stroke::new(2.0, arrow_color),
                    );
                    painter.line_segment(
                        [tip, Pos2::new(bx - perp.x * hs * 0.5, by - perp.y * hs * 0.5)],
                        egui::Stroke::new(2.0, arrow_color),
                    );

                    // Center dot
                    let dot_color = if is_sel {
                        Color32::from_rgba_unmultiplied(255, 255, 100, 255)
                    } else {
                        Color32::from_rgba_unmultiplied(255, 200, 0, 200)
                    };
                    painter.circle_filled(center, if is_sel { 5.0 } else { 4.0 }, dot_color);
                }
            }
        }
    }

    // ── Guide interaction ──
    super::guide_editor::handle_guides(&response, ui, state, rect);

    // ── Pan (middle-drag or alt+left-drag, not during guide drag) ──
    let guide_dragging = state.guide_drag.is_some();
    if !guide_dragging
        && (response.dragged_by(egui::PointerButton::Middle)
            || (response.dragged_by(egui::PointerButton::Primary)
                && ui.input(|i| i.modifiers.alt)))
    {
        let delta = response.drag_delta();
        state.viewport.offset.x -= delta.x / state.viewport.zoom;
        state.viewport.offset.y -= delta.y / state.viewport.zoom;
    }

    // ── Zoom (scroll wheel) ──
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let zoom_factor = (scroll * 0.003).exp();
            let old_zoom = state.viewport.zoom;
            state.viewport.zoom = (old_zoom * zoom_factor).clamp(32.0, 8192.0);

            // Zoom toward cursor
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

    // ── View mode tabs at bottom of viewport ──
    let modes = [
        (ViewMode::Color, "Color"),
        (ViewMode::Height, "Height"),
        (ViewMode::Normal, "Normal"),
        (ViewMode::StrokeId, "StrokeID"),
    ];
    let tab_w = 72.0;
    let tab_h = 24.0;
    let tab_y = rect.bottom() - tab_h - 6.0;
    let tab_x_start = rect.left() + 8.0;

    if tab_y > rect.top() + 40.0 {
        for (i, (mode, label)) in modes.iter().enumerate() {
            let tab = Rect::from_min_size(
                Pos2::new(tab_x_start + i as f32 * (tab_w + 4.0), tab_y),
                Vec2::new(tab_w, tab_h),
            );
            let is_active = state.view_mode == *mode;
            let bg = if is_active {
                Color32::from_rgba_unmultiplied(60, 60, 60, 220)
            } else {
                Color32::from_rgba_unmultiplied(40, 40, 40, 180)
            };
            painter.rect_filled(tab, 3.0, bg);
            if is_active {
                painter.rect_stroke(tab, 3.0, egui::Stroke::new(1.0, Color32::from_gray(140)));
            }

            let text_color = if is_active {
                Color32::WHITE
            } else {
                Color32::from_gray(160)
            };
            painter.text(
                tab.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                text_color,
            );

            let tab_response = ui.interact(tab, ui.id().with(("view_tab", i)), Sense::click());
            if tab_response.clicked() {
                state.view_mode = *mode;
            }
        }
    }
}
