//! Shared icon/widget helpers for the GUI.
//!
//! Centralises repeated patterns: icon painting, small icon buttons,
//! toolbar buttons with badges, and truncated-text rendering.

use eframe::egui;

// ── Constants ───────────────────────────────────────────────────────

/// Opacity multiplier applied to disabled/inactive icons.
pub const DISABLED_OPACITY: f32 = 0.4;

// ── Icon Coloring ───────────────────────────────────────────────────

/// Resolve icon color from a 3-state palette:
/// - `enabled = false` → `weak_text_color × DISABLED_OPACITY`
/// - `enabled = true, hovered = false` → `weak_text_color`
/// - `enabled = true, hovered = true` → `text_color`
pub fn icon_color(ui: &egui::Ui, enabled: bool, hovered: bool) -> egui::Color32 {
    if !enabled {
        ui.visuals().weak_text_color().gamma_multiply(DISABLED_OPACITY)
    } else if hovered {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    }
}

// ── Paint-Only Icon ─────────────────────────────────────────────────

/// Paint an icon glyph centred in `rect` with optional hover highlight.
///
/// Does **not** allocate space or interact — the caller provides the
/// pre-allocated `Rect` and the hover/enabled state.
pub fn paint_icon(
    painter: &egui::Painter,
    ui: &egui::Ui,
    rect: egui::Rect,
    icon: &str,
    font_size: f32,
    enabled: bool,
    hovered: bool,
) {
    if hovered && enabled {
        painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.bg_fill);
    }
    let color = icon_color(ui, enabled, hovered);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::proportional(font_size),
        color,
    );
}

// ── Self-Allocating Icon Button ─────────────────────────────────────

/// Small icon button that allocates its own rect and returns the `Response`.
///
/// Paints the icon centred with 3-state colouring and hover background.
pub fn small_icon_button(
    ui: &mut egui::Ui,
    icon: &str,
    font_size: f32,
    side: f32,
    enabled: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(side), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = response.hovered() && enabled;
        paint_icon(ui.painter(), ui, rect, icon, font_size, enabled, hovered);
    }
    response
}

// ── Toolbar Icon Button ─────────────────────────────────────────────

/// 32×32 toolbar icon button with a number badge (top-left, 9 pt).
///
/// Uses `selection.bg_fill` for the selected state.
/// Returns `true` if clicked.
pub fn toolbar_icon_button(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    badge: &str,
    tooltip: &str,
) -> bool {
    let size = egui::Vec2::splat(32.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cr = 4.0;

        if selected {
            painter.rect_filled(rect, cr, ui.visuals().selection.bg_fill);
        } else if response.hovered() {
            painter.rect_filled(rect, cr, ui.visuals().widgets.hovered.bg_fill);
        }

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

        painter.text(
            rect.left_top() + egui::Vec2::new(3.0, 1.0),
            egui::Align2::LEFT_TOP,
            badge,
            egui::FontId::proportional(9.0),
            ui.visuals().weak_text_color(),
        );
    }

    response.on_hover_text(tooltip).clicked()
}

// ── Truncated Text ──────────────────────────────────────────────────

/// Paint single-line text at (`x`, vertical-centre of `rect`) with
/// trailing `…` when it exceeds `max_width`.
pub fn paint_truncated_text(
    painter: &egui::Painter,
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    x: f32,
    rect: egui::Rect,
    max_width: f32,
) {
    let galley = painter.layout_no_wrap(text.to_string(), font_id.clone(), color);
    let text_y = rect.center().y - galley.size().y * 0.5;
    if galley.size().x > max_width {
        let ell = painter.layout_no_wrap("\u{2026}".to_string(), font_id, color);
        let ell_w = ell.size().x;
        let clip = egui::Rect::from_min_size(
            egui::Pos2::new(x, rect.min.y),
            egui::Vec2::new(max_width - ell_w, rect.height()),
        );
        painter
            .with_clip_rect(clip)
            .galley(egui::Pos2::new(x, text_y), galley, color);
        painter.galley(
            egui::Pos2::new(x + max_width - ell_w, text_y),
            ell,
            color,
        );
    } else {
        painter.galley(egui::Pos2::new(x, text_y), galley, color);
    }
}
