use eframe::egui::{self, CursorIcon, Pos2, Rect};

use practical_arcana_painter::types::{Guide, GuideType};

use super::state::{AppState, DragTarget, GuideTool};
use super::viewport::{screen_to_uv, uv_to_screen};

const HANDLE_RADIUS: f32 = 8.0;

/// Handle interactive guide manipulation in the viewport (tool-based dispatch).
pub fn handle_guides(
    response: &egui::Response,
    ui: &egui::Ui,
    state: &mut AppState,
    viewport_rect: Rect,
) {
    let Some(layer_idx) = state.selected_layer else {
        return;
    };
    if layer_idx >= state.project.layers.len() {
        return;
    }

    match state.guide_tool {
        GuideTool::Select => handle_select(response, ui, state, layer_idx, viewport_rect),
        GuideTool::AddDirectional => handle_add(
            response,
            ui,
            state,
            layer_idx,
            viewport_rect,
            GuideType::Directional,
        ),
        GuideTool::AddRadial => handle_add(
            response,
            ui,
            state,
            layer_idx,
            viewport_rect,
            GuideType::Source,
        ),
        GuideTool::AddVortex => handle_add(
            response,
            ui,
            state,
            layer_idx,
            viewport_rect,
            GuideType::Vortex,
        ),
    }
}

/// Unified Select tool: click=select/deselect, drag center=move,
/// drag direction handle=rotate, drag influence circle edge=resize.
fn handle_select(
    response: &egui::Response,
    ui: &egui::Ui,
    state: &mut AppState,
    layer_idx: usize,
    viewport_rect: Rect,
) {
    // Start drag: use press_origin for accurate hit-testing.
    // (interact_pointer_pos returns the position at drag-threshold, which may
    //  have drifted past the handle — press_origin is the true press point.)
    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
            let guides = &state.project.layers[layer_idx].guides;
            if let Some(target) = hit_test_full(pos, guides, state, viewport_rect) {
                let idx = match target {
                    DragTarget::GuidePosition(i)
                    | DragTarget::GuideDirection(i)
                    | DragTarget::GuideInfluence(i) => i,
                };
                state.guide_drag = Some(target);
                state.selected_guide = Some(idx);
            }
        }
    }

    // Ongoing drag: dispatch based on drag target
    if response.dragged_by(egui::PointerButton::Primary) {
        match state.guide_drag {
            Some(DragTarget::GuidePosition(i)) => {
                // Move the guide (delta-based — works correctly)
                let delta_uv = glam::Vec2::new(
                    response.drag_delta().x / state.viewport.zoom,
                    response.drag_delta().y / state.viewport.zoom,
                );
                state.project.layers[layer_idx].guides[i].position += delta_uv;
            }
            Some(DragTarget::GuideDirection(i)) => {
                // Adjust direction ONLY (influence is decoupled — use circle edge instead).
                // Use latest_pos (current pointer) not interact_pointer_pos (drag start).
                if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                    let pointer_uv = screen_to_uv(pos, state, viewport_rect);
                    let guide_pos = state.project.layers[layer_idx].guides[i].position;
                    let new_dir = pointer_uv - guide_pos;
                    if new_dir.length() > 0.001 {
                        state.project.layers[layer_idx].guides[i].direction = new_dir.normalize();
                    }
                }
            }
            Some(DragTarget::GuideInfluence(i)) => {
                // Resize influence: distance from guide center to pointer in UV space.
                if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                    let center = uv_to_screen(
                        state.project.layers[layer_idx].guides[i].position,
                        state,
                        viewport_rect,
                    );
                    let dist_px = pos.distance(center);
                    let influence = (dist_px / state.viewport.zoom).clamp(0.02, 2.0);
                    state.project.layers[layer_idx].guides[i].influence = influence;
                }
            }
            None => {}
        }
    }

    // End drag
    if response.drag_stopped() {
        state.guide_drag = None;
    }

    // Click (no drag): select or deselect
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let guides = &state.project.layers[layer_idx].guides;
            if let Some(idx) = hit_test_position(pos, guides, state, viewport_rect) {
                state.selected_guide = Some(idx);
            } else {
                state.selected_guide = None;
            }
        }
    }

    // ── Cursor hints ──
    if let Some(ref drag) = state.guide_drag {
        // During drag: show active-drag cursor
        let icon = match drag {
            DragTarget::GuidePosition(_) => CursorIcon::Grabbing,
            DragTarget::GuideDirection(_) => CursorIcon::Crosshair,
            DragTarget::GuideInfluence(_) => CursorIcon::ResizeHorizontal,
        };
        ui.output_mut(|o| o.cursor_icon = icon);
    } else if response.hovered() {
        // Not dragging: show hover hint based on what's under the pointer
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
            let guides = &state.project.layers[layer_idx].guides;
            if let Some(target) = hit_test_full(pos, guides, state, viewport_rect) {
                let icon = match target {
                    DragTarget::GuidePosition(_) => CursorIcon::Grab,
                    DragTarget::GuideDirection(_) => CursorIcon::Crosshair,
                    DragTarget::GuideInfluence(_) => CursorIcon::ResizeHorizontal,
                };
                ui.output_mut(|o| o.cursor_icon = icon);
            }
        }
    }
}

/// Add tool: click to place a new guide of the given type.
fn handle_add(
    response: &egui::Response,
    _ui: &egui::Ui,
    state: &mut AppState,
    layer_idx: usize,
    viewport_rect: Rect,
    guide_type: GuideType,
) {
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let uv = screen_to_uv(pos, state, viewport_rect);
            if (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y) {
                state.project.layers[layer_idx].guides.push(Guide {
                    guide_type,
                    position: uv,
                    direction: glam::Vec2::X,
                    influence: 0.2,
                    strength: 1.0,
                });
                let n = state.project.layers[layer_idx].guides.len();
                state.selected_guide = Some(n - 1);
                // Switch to Select so user can immediately interact
                state.guide_tool = GuideTool::Select;
            }
        }
    }
}

// ── Hit testing ─────────────────────────────────────────────────────

/// Hit-test pointer position against guide center dots only.
/// Returns the guide index if hit.
fn hit_test_position(
    pos: Pos2,
    guides: &[Guide],
    state: &AppState,
    viewport_rect: Rect,
) -> Option<usize> {
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, viewport_rect);
        if pos.distance(center) < HANDLE_RADIUS {
            return Some(i);
        }
    }
    None
}

/// Hit-test pointer against center dots, direction handles, and influence circle edges.
/// Priority: center dot > direction handle > influence circle edge.
fn hit_test_full(
    pos: Pos2,
    guides: &[Guide],
    state: &AppState,
    viewport_rect: Rect,
) -> Option<DragTarget> {
    // 1. Position handles (center dots) — checked first so the center area
    //    always triggers position drag, even when the arrow overlaps.
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, viewport_rect);
        if pos.distance(center) < HANDLE_RADIUS {
            return Some(DragTarget::GuidePosition(i));
        }
    }

    // 2. Direction handles (arrow shaft + tip) — only for Directional type.
    for (i, guide) in guides.iter().enumerate() {
        if guide.guide_type != GuideType::Directional {
            continue;
        }
        let center = uv_to_screen(guide.position, state, viewport_rect);
        let dir = if guide.direction.length() > 0.001 {
            guide.direction.normalize()
        } else {
            glam::Vec2::X
        };
        let radius_px = guide.influence * state.viewport.zoom;
        let arrow_len = 40.0_f32.min(radius_px.max(20.0));
        let seg_start = Pos2::new(
            center.x + dir.x * HANDLE_RADIUS,
            center.y + dir.y * HANDLE_RADIUS,
        );
        let seg_end = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
        if point_to_segment_dist(pos, seg_start, seg_end) < HANDLE_RADIUS {
            return Some(DragTarget::GuideDirection(i));
        }
    }

    // 3. Influence circle edge — grab the ring to resize
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, viewport_rect);
        let radius_px = guide.influence * state.viewport.zoom;
        let dist = pos.distance(center);
        if (dist - radius_px).abs() < HANDLE_RADIUS {
            return Some(DragTarget::GuideInfluence(i));
        }
    }

    None
}

/// Shortest distance from point `p` to the line segment `a`–`b`.
fn point_to_segment_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = egui::vec2(b.x - a.x, b.y - a.y);
    let ap = egui::vec2(p.x - a.x, p.y - a.y);
    let len_sq = ab.length_sq();
    if len_sq < 1e-6 {
        return ap.length();
    }
    let t = ap.dot(ab) / len_sq;
    let t = t.clamp(0.0, 1.0);
    let closest = Pos2::new(a.x + ab.x * t, a.y + ab.y * t);
    p.distance(closest)
}
