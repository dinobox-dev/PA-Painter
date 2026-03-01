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
        ui.visuals()
            .weak_text_color()
            .gamma_multiply(DISABLED_OPACITY)
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

// ── Slider Row ─────────────────────────────────────────────────

/// Slider track + fixed-width text input + label on a single row.
///
/// Uses `TextEdit` with `clip_text(true)` so the value field never
/// expands when large numbers are typed — unlike the built-in
/// `DragValue` which hardcodes `clip_text(false)`.
pub fn slider_row(
    ui: &mut egui::Ui,
    id_salt: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    label: &str,
    step: Option<f64>,
    decimals: usize,
) -> egui::Response {
    const TEXT_FIELD_W: f32 = 48.0;

    ui.horizontal(|ui: &mut egui::Ui| {
        // Use the default slider_width so all tracks are the same length.
        let mut slider = egui::Slider::new(value, range.clone()).show_value(false);
        if let Some(s) = step {
            slider = slider.step_by(s);
        }
        let slider_resp = ui.add(slider);

        // Text field backed by frame-persistent temp data.
        // While focused: preserve the user's typed text across frames.
        // While not focused: show the formatted value.
        let state_id = egui::Id::new(("slider_row", id_salt));
        let stored: Option<String> = ui.data_mut(|d| d.get_temp(state_id));
        let mut text_buf = stored.unwrap_or_else(|| format!("{:.*}", decimals, *value));

        // Reserve a paint slot so the background is drawn behind the text.
        let bg_idx = ui.painter().add(egui::Shape::Noop);

        let te_resp = ui.add(
            egui::TextEdit::singleline(&mut text_buf)
                .desired_width(TEXT_FIELD_W)
                .clip_text(true)
                .frame(false),
        );

        // Fill the reserved slot with a DragValue-style background.
        if ui.is_rect_visible(te_resp.rect) {
            let bg = if te_resp.has_focus() {
                ui.visuals().widgets.active.bg_fill
            } else if te_resp.hovered() {
                ui.visuals().widgets.hovered.bg_fill
            } else {
                ui.visuals().widgets.inactive.bg_fill
            };
            let cr = ui.visuals().widgets.inactive.corner_radius;
            ui.painter()
                .set(bg_idx, egui::Shape::rect_filled(te_resp.rect, cr, bg));
        }

        // Release focus on any press outside this field (click or drag start).
        if te_resp.has_focus() && !te_resp.hovered() && ui.input(|i| i.pointer.any_pressed()) {
            te_resp.surrender_focus();
        }

        if te_resp.changed() || te_resp.lost_focus() {
            if let Ok(v) = text_buf.parse::<f32>() {
                *value = v.clamp(*range.start(), *range.end());
            }
        }

        if te_resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(state_id, text_buf));
        } else {
            ui.data_mut(|d| d.remove::<String>(state_id));
        }

        ui.label(label);

        slider_resp
    })
    .inner
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
        painter.galley(egui::Pos2::new(x + max_width - ell_w, text_y), ell, color);
    } else {
        painter.galley(egui::Pos2::new(x, text_y), galley, color);
    }
}
