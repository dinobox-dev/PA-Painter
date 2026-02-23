use eframe::egui;

use practical_arcana_painter::brush_profile;
use practical_arcana_painter::path_placement;
use practical_arcana_painter::stroke_height;
use practical_arcana_painter::types::{PaintSlot, PatternValues, StrokeParams, StrokeValues};

// ── Caches ──────────────────────────────────────────────────────

/// Cached stroke density texture and the parameters that produced it.
pub struct StrokePreviewCache {
    entry: Option<(StrokeValues, u32, egui::TextureHandle)>,
}

impl Default for StrokePreviewCache {
    fn default() -> Self {
        Self { entry: None }
    }
}

/// Cached pattern path data and the parameters that produced it.
pub struct PatternPreviewCache {
    entry: Option<(PatternValues, u32, Vec<Vec<[f32; 2]>>)>,
}

impl Default for PatternPreviewCache {
    fn default() -> Self {
        Self { entry: None }
    }
}

/// Combined preview caches stored in AppState.
#[derive(Default)]
pub struct PreviewCache {
    pub stroke: StrokePreviewCache,
    pub pattern: PatternPreviewCache,
}

// ── Stroke Preview ──────────────────────────────────────────────

/// Show a stroke density preview inside the sidebar.
///
/// Renders a single stroke's density map as a warm-tinted texture,
/// showing the effect of brush width, load, wiggle, and pressure curve.
pub fn show_stroke_preview(
    ui: &mut egui::Ui,
    stroke: &StrokeValues,
    seed: u32,
    cache: &mut StrokePreviewCache,
) {
    let stale = match &cache.entry {
        Some((s, sd, _)) => *s != *stroke || *sd != seed,
        None => true,
    };

    if stale {
        let brush_w = (stroke.brush_width.round() as usize).max(4);
        let profile = brush_profile::generate_brush_profile(brush_w, seed);
        let params = StrokeParams {
            brush_width: stroke.brush_width,
            load: stroke.load,
            body_wiggle: stroke.body_wiggle,
            pressure_preset: stroke.pressure_preset,
            seed,
            ..StrokeParams::default()
        };
        let result = stroke_height::generate_stroke_height(&profile, 200, &params, seed);

        // Density → warm-tinted pixels (cream on black)
        let pixels: Vec<egui::Color32> = result
            .data
            .iter()
            .map(|&d| {
                let v = d.clamp(0.0, 1.0);
                egui::Color32::from_rgba_unmultiplied(
                    (v * 230.0) as u8,
                    (v * 215.0) as u8,
                    (v * 180.0) as u8,
                    255,
                )
            })
            .collect();

        let texture = ui.ctx().load_texture(
            "stroke_preview",
            egui::ColorImage {
                size: [result.width, result.height],
                pixels,
            },
            egui::TextureOptions::LINEAR,
        );

        cache.entry = Some((stroke.clone(), seed, texture));
    }

    if let Some((_, _, ref tex)) = cache.entry {
        let available_w = ui.available_width();
        let tex_w = tex.size()[0] as f32;
        let tex_h = tex.size()[1] as f32;
        if tex_w > 0.0 && tex_h > 0.0 {
            let display_h = (available_w * tex_h / tex_w).clamp(24.0, 80.0);
            let (resp, painter) = ui.allocate_painter(
                egui::Vec2::new(available_w, display_h),
                egui::Sense::hover(),
            );
            let rect = resp.rect;
            painter.rect_filled(rect, 4.0, egui::Color32::from_gray(32));
            painter.image(
                tex.id(),
                rect.shrink(2.0),
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
}

// ── Pattern Preview ─────────────────────────────────────────────

/// Show a pattern layout preview inside the sidebar.
///
/// Draws stroke centerlines generated at 128px resolution,
/// showing the effect of spacing, length, angle variation, and guides.
pub fn show_pattern_preview(
    ui: &mut egui::Ui,
    slot: &PaintSlot,
    cache: &mut PatternPreviewCache,
) {
    let stale = match &cache.entry {
        Some((p, sd, _)) => *p != slot.pattern || *sd != slot.seed,
        None => true,
    };

    if stale {
        let layer = slot.to_paint_layer();
        let paths = path_placement::generate_paths(&layer, 0, 128, None, None, None);
        let extracted: Vec<Vec<[f32; 2]>> = paths
            .iter()
            .map(|p| p.points.iter().map(|v| [v.x, v.y]).collect())
            .collect();
        cache.entry = Some((slot.pattern.clone(), slot.seed, extracted));
    }

    let side = ui.available_width().min(256.0);
    let (resp, painter) = ui.allocate_painter(egui::Vec2::splat(side), egui::Sense::hover());
    let rect = resp.rect;

    // Dark background
    painter.rect_filled(rect, 4.0, egui::Color32::from_gray(32));

    if let Some((_, _, ref paths)) = cache.entry {
        let line = egui::Stroke::new(
            1.5,
            egui::Color32::from_rgba_unmultiplied(200, 180, 140, 200),
        );
        for path in paths {
            let pts: Vec<egui::Pos2> = path
                .iter()
                .map(|p| {
                    egui::Pos2::new(
                        rect.left() + p[0] * rect.width(),
                        rect.top() + p[1] * rect.height(),
                    )
                })
                .collect();
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], line);
            }
        }

        painter.text(
            egui::Pos2::new(rect.right() - 4.0, rect.bottom() - 4.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("{} strokes", paths.len()),
            egui::FontId::proportional(10.0),
            egui::Color32::from_gray(120),
        );
    }
}
