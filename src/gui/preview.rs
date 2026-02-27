use eframe::egui;

use practical_arcana_painter::brush_profile;
use practical_arcana_painter::object_normal::MeshNormalData;
use practical_arcana_painter::path_placement;
use practical_arcana_painter::stroke_color::ColorTextureRef;
use practical_arcana_painter::stroke_height;
use practical_arcana_painter::types::{Guide, Layer, PaintValues, StrokeParams};

// ── Caches ──────────────────────────────────────────────────────

/// Cached stroke density texture and the parameters that produced it.
pub struct StrokePreviewCache {
    entry: Option<(PaintValues, u32, egui::TextureHandle)>,
}

impl Default for StrokePreviewCache {
    fn default() -> Self {
        Self { entry: None }
    }
}

impl StrokePreviewCache {
    /// Access the cached texture handle (if any).
    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        self.entry.as_ref().map(|(_, _, tex)| tex)
    }
}

/// Combined preview caches stored in AppState.
#[derive(Default)]
pub struct PreviewCache {
    pub stroke: StrokePreviewCache,
}

// ── Stroke Preview ──────────────────────────────────────────────

/// Update the stroke density cache without drawing anything.
///
/// Use this when you need the cached texture for custom rendering
/// (e.g., as a background layer in a composite widget).
pub fn update_stroke_cache(
    ctx: &egui::Context,
    paint: &PaintValues,
    seed: u32,
    cache: &mut StrokePreviewCache,
) {
    let stale = match &cache.entry {
        Some((p, sd, _)) => *p != *paint || *sd != seed,
        None => true,
    };

    if stale {
        let brush_w = (paint.brush_width.round() as usize).max(4);
        let profile = brush_profile::generate_brush_profile(brush_w, seed);
        let params = StrokeParams {
            brush_width: paint.brush_width,
            load: paint.load,
            body_wiggle: paint.body_wiggle,
            pressure_curve: paint.pressure_curve.clone(),
            seed,
            ..StrokeParams::default()
        };
        let result = stroke_height::generate_stroke_height(&profile, 200, &params, seed);

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

        let texture = ctx.load_texture(
            "stroke_preview",
            egui::ColorImage::new([result.width, result.height], pixels),
            egui::TextureOptions::LINEAR,
        );

        cache.entry = Some((paint.clone(), seed, texture));
    }
}



// ── Per-Layer Path Overlay Cache ────────────────────────────────

/// Key for invalidation: paint values + seed + guides + resolution + color texture hash.
#[derive(Clone, PartialEq)]
struct LayerPathKey {
    paint: PaintValues,
    seed: u32,
    guides: Vec<Guide>,
    resolution: u32,
    /// Hash of the color texture data; changes when texture content is swapped or reloaded.
    color_tex_hash: u64,
}

/// Cached path data for a single layer.
pub struct LayerPathCache {
    key: Option<LayerPathKey>,
    pub paths: Vec<Vec<[f32; 2]>>,
}

impl Default for LayerPathCache {
    fn default() -> Self {
        Self {
            key: None,
            paths: Vec::new(),
        }
    }
}

impl LayerPathCache {
    /// Check if cache is stale for the given layer state, seed, resolution, and color texture hash.
    pub fn is_stale(&self, layer: &Layer, seed: u32, resolution: u32, color_tex_hash: u64) -> bool {
        match &self.key {
            Some(k) => {
                k.paint != layer.paint
                    || k.seed != seed
                    || k.guides != layer.guides
                    || k.resolution != resolution
                    || k.color_tex_hash != color_tex_hash
            }
            None => true,
        }
    }

    /// Recompute paths for this layer at the given resolution.
    pub fn recompute(
        &mut self,
        layer: &Layer,
        seed: u32,
        resolution: u32,
        color_tex: Option<&ColorTextureRef<'_>>,
        normal_data: Option<&MeshNormalData>,
        color_tex_hash: u64,
    ) {
        let paint_layer = layer.to_paint_layer_with_seed(seed);
        let paths =
            path_placement::generate_paths(&paint_layer, 0, resolution, color_tex, normal_data, None);
        self.paths = paths
            .iter()
            .map(|p| p.points.iter().map(|v| [v.x, v.y]).collect())
            .collect();
        self.key = Some(LayerPathKey {
            paint: layer.paint.clone(),
            seed,
            guides: layer.guides.clone(),
            resolution,
            color_tex_hash,
        });
    }
}

/// Per-layer path preview caches for viewport overlay.
/// Maintains one cache entry per layer index, growing/shrinking with the layer stack.
pub struct PathOverlayCache {
    caches: Vec<LayerPathCache>,
}

impl Default for PathOverlayCache {
    fn default() -> Self {
        Self {
            caches: Vec::new(),
        }
    }
}

impl PathOverlayCache {
    /// Sync cache vec length to match layer count.
    fn sync_layer_count(&mut self, count: usize) {
        if self.caches.len() > count {
            self.caches.truncate(count);
        }
        while self.caches.len() < count {
            self.caches.push(LayerPathCache::default());
        }
    }

    /// Update cache for the selected layer only.
    /// Non-selected caches are cleared to free memory.
    pub fn update(
        &mut self,
        layers: &[Layer],
        base_seed: u32,
        resolution: u32,
        selected: Option<usize>,
        color_tex: Option<&ColorTextureRef<'_>>,
        normal_data: Option<&MeshNormalData>,
        color_tex_hash: u64,
    ) {
        self.sync_layer_count(layers.len());
        for (i, cache) in self.caches.iter_mut().enumerate() {
            if Some(i) == selected {
                let layer = &layers[i];
                let seed = base_seed.wrapping_add(i as u32);
                if layer.visible && cache.is_stale(layer, seed, resolution, color_tex_hash) {
                    cache.recompute(layer, seed, resolution, color_tex, normal_data, color_tex_hash);
                }
            } else {
                // Free memory for non-selected layers
                cache.key = None;
                cache.paths = Vec::new();
            }
        }
    }

    /// Get cached paths for the selected layer (if any).
    pub fn selected_paths(&self, selected: Option<usize>) -> Option<(usize, &Vec<Vec<[f32; 2]>>)> {
        let i = selected?;
        self.caches.get(i).map(|c| (i, &c.paths))
    }
}

// ── Preset Thumbnail Cache ─────────────────────────────────────

/// Cache of small stroke preview textures keyed by PaintValues.
pub struct PresetThumbnailCache {
    entries: Vec<(PaintValues, egui::TextureHandle)>,
}

impl Default for PresetThumbnailCache {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl PresetThumbnailCache {
    /// Get or generate a thumbnail for the given PaintValues.
    pub fn get_or_create(
        &mut self,
        ctx: &egui::Context,
        values: &PaintValues,
        seed: u32,
    ) -> egui::TextureId {
        // Check if already cached
        if let Some(pos) = self.entries.iter().position(|(v, _)| v == values) {
            return self.entries[pos].1.id();
        }

        // Generate a small (100px wide) stroke density texture
        let brush_w = (values.brush_width.round() as usize).max(4);
        let profile = brush_profile::generate_brush_profile(brush_w, seed);
        let params = StrokeParams::from_paint_values(values, seed);
        let result = stroke_height::generate_stroke_height(&profile, 100, &params, seed);

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

        let handle = ctx.load_texture(
            &format!("preset_thumb_{}", self.entries.len()),
            egui::ColorImage::new([result.width, result.height], pixels),
            egui::TextureOptions::LINEAR,
        );

        let id = handle.id();
        self.entries.push((values.clone(), handle));
        id
    }

    /// Remove entries whose PaintValues are not in `active_values`.
    pub fn retain_active(&mut self, active_values: &[&PaintValues]) {
        self.entries.retain(|(v, _)| active_values.contains(&v));
    }
}
