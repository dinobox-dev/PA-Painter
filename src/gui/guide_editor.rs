use eframe::egui::{self, Pos2, Rect};

use practical_arcana_painter::types::GuideVertex;

use super::state::{AppState, DragTarget};
use super::viewport::{screen_to_uv, uv_to_screen};

const HANDLE_RADIUS: f32 = 8.0;

/// Handle interactive guide manipulation in the viewport.
///
/// Supports: click-select, drag-move position, drag-adjust direction/influence,
/// double-click to add, Delete/Backspace to remove.
pub fn handle_guides(
    response: &egui::Response,
    ui: &egui::Ui,
    state: &mut AppState,
    viewport_rect: Rect,
) {
    let Some(slot_idx) = state.selected_slot else {
        return;
    };
    if slot_idx >= state.project.slots.len() {
        return;
    }
    if !state.viewport.show_guides {
        return;
    }

    // Delete selected guide
    if response.hovered()
        && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
    {
        if let Some(guide_idx) = state.selected_guide {
            if guide_idx < state.project.slots[slot_idx].pattern.guides.len() {
                state.project.slots[slot_idx]
                    .pattern
                    .guides
                    .remove(guide_idx);
                state.selected_guide = None;
                state.guide_drag = None;
                return;
            }
        }
    }

    // Double-click on empty space to add a new guide
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let uv = screen_to_uv(pos, state, viewport_rect);
            if (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y) {
                let guides = &state.project.slots[slot_idx].pattern.guides;
                if hit_test(pos, guides, state, viewport_rect).is_none() {
                    state.project.slots[slot_idx]
                        .pattern
                        .guides
                        .push(GuideVertex {
                            position: uv,
                            direction: glam::Vec2::X,
                            influence: 0.2,
                        });
                    let n = state.project.slots[slot_idx].pattern.guides.len();
                    state.selected_guide = Some(n - 1);
                }
            }
        }
        return;
    }

    // Start drag on a guide handle (left-click, no Alt)
    if response.drag_started_by(egui::PointerButton::Primary)
        && !ui.input(|i| i.modifiers.alt)
    {
        if let Some(pos) = response.interact_pointer_pos() {
            let guides = &state.project.slots[slot_idx].pattern.guides;
            if let Some(target) = hit_test(pos, guides, state, viewport_rect) {
                state.guide_drag = Some(target);
                let idx = match target {
                    DragTarget::GuidePosition(i) | DragTarget::GuideDirection(i) => i,
                };
                state.selected_guide = Some(idx);
            }
        }
    }

    // Ongoing drag
    if response.dragged_by(egui::PointerButton::Primary) {
        if let Some(target) = state.guide_drag {
            match target {
                DragTarget::GuidePosition(i) => {
                    let delta_uv = glam::Vec2::new(
                        response.drag_delta().x / state.viewport.zoom,
                        response.drag_delta().y / state.viewport.zoom,
                    );
                    state.project.slots[slot_idx].pattern.guides[i].position += delta_uv;
                }
                DragTarget::GuideDirection(i) => {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let pointer_uv = screen_to_uv(pos, state, viewport_rect);
                        let guide_pos =
                            state.project.slots[slot_idx].pattern.guides[i].position;
                        let new_dir = pointer_uv - guide_pos;
                        if new_dir.length() > 0.001 {
                            state.project.slots[slot_idx].pattern.guides[i].direction =
                                new_dir.normalize();
                            state.project.slots[slot_idx].pattern.guides[i].influence =
                                new_dir.length().max(0.02);
                        }
                    }
                }
            }
        }
    }

    // End drag
    if response.drag_stopped() {
        state.guide_drag = None;
    }

    // Click to select/deselect (not a drag, not a double-click)
    if response.clicked() && !ui.input(|i| i.modifiers.alt) {
        if let Some(pos) = response.interact_pointer_pos() {
            let guides = &state.project.slots[slot_idx].pattern.guides;
            if let Some(target) = hit_test(pos, guides, state, viewport_rect) {
                let idx = match target {
                    DragTarget::GuidePosition(i) | DragTarget::GuideDirection(i) => i,
                };
                state.selected_guide = Some(idx);
            } else {
                state.selected_guide = None;
            }
        }
    }
}

/// Hit-test pointer position against guide handles.
/// Checks direction handles (arrow tips) first, then position handles (center dots).
fn hit_test(
    pos: Pos2,
    guides: &[GuideVertex],
    state: &AppState,
    viewport_rect: Rect,
) -> Option<DragTarget> {
    // Direction handles (arrow tips) — check first since they overlap position handles
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, viewport_rect);
        let dir = if guide.direction.length() > 0.001 {
            guide.direction.normalize()
        } else {
            glam::Vec2::X
        };
        let radius_px = guide.influence * state.viewport.zoom;
        let arrow_len = 40.0_f32.min(radius_px.max(20.0));
        let tip = Pos2::new(center.x + dir.x * arrow_len, center.y + dir.y * arrow_len);
        if pos.distance(tip) < HANDLE_RADIUS {
            return Some(DragTarget::GuideDirection(i));
        }
    }

    // Position handles (center dots)
    for (i, guide) in guides.iter().enumerate() {
        let center = uv_to_screen(guide.position, state, viewport_rect);
        if pos.distance(center) < HANDLE_RADIUS {
            return Some(DragTarget::GuidePosition(i));
        }
    }

    None
}
