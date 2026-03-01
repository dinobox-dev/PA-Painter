use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use eframe::egui;

use practical_arcana_painter::asset_io::LoadedMesh;
use practical_arcana_painter::brush_profile;
use practical_arcana_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use practical_arcana_painter::path_placement;
use practical_arcana_painter::stroke_color::ColorTextureRef;
use practical_arcana_painter::stroke_height;
use practical_arcana_painter::types::{Color, Guide, Layer, PaintValues, StrokeParams};

// ── Caches ──────────────────────────────────────────────────────

/// Cached stroke density texture and the parameters that produced it.
#[derive(Default)]
pub struct StrokePreviewCache {
    entry: Option<(PaintValues, u32, egui::TextureHandle)>,
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

/// Key for invalidation: paint values + seed + guides + color texture hash.
/// Resolution is NOT included because path generation always uses BASE_RESOLUTION.
#[derive(Clone, PartialEq)]
struct LayerPathKey {
    paint: PaintValues,
    seed: u32,
    guides: Vec<Guide>,
    /// Hash of the color texture data; changes when texture content is swapped or reloaded.
    color_tex_hash: u64,
}

/// Cached path data for a single layer.
#[derive(Default)]
pub struct LayerPathCache {
    key: Option<LayerPathKey>,
    pub paths: Vec<Vec<[f32; 2]>>,
    /// Original total segments before worker-side simplification.
    pub original_total_segments: usize,
}


impl LayerPathCache {
    /// Check if cache is stale for the given layer state, seed, and color texture hash.
    pub fn is_stale(&self, layer: &Layer, seed: u32, color_tex_hash: u64) -> bool {
        match &self.key {
            Some(k) => {
                k.paint != layer.paint
                    || k.seed != seed
                    || k.guides != layer.guides
                    || k.color_tex_hash != color_tex_hash
            }
            None => true,
        }
    }

}

/// Per-layer path preview caches for viewport overlay.
/// Maintains one cache entry per layer index, growing/shrinking with the layer stack.
#[derive(Default)]
pub struct PathOverlayCache {
    caches: Vec<LayerPathCache>,
    /// Tracks the parameters of the currently in-flight worker computation.
    /// Prevents restarting the worker every frame while a correct computation is running.
    pending: Option<(usize, LayerPathKey)>,
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

    /// Get cached paths for the selected layer (if any).
    /// Returns (layer_index, paths, original_total_segments).
    #[allow(clippy::type_complexity)]
    pub fn selected_paths(&self, selected: Option<usize>) -> Option<(usize, &Vec<Vec<[f32; 2]>>, usize)> {
        let i = selected?;
        self.caches.get(i).map(|c| (i, &c.paths, c.original_total_segments))
    }

    /// Check if the cache for the given layer index is stale.
    /// Returns false if a worker is already computing with matching parameters.
    pub fn is_stale_for_layer(
        &self,
        layer_index: usize,
        layer: &Layer,
        seed: u32,
        color_tex_hash: u64,
    ) -> bool {
        // If the completed cache already matches, not stale.
        let cache_hit = self
            .caches
            .get(layer_index)
            .is_some_and(|cache| !cache.is_stale(layer, seed, color_tex_hash));
        if cache_hit {
            return false;
        }

        // If a worker is already computing exactly these params, not stale either.
        if let Some((idx, ref key)) = self.pending {
            if idx == layer_index
                && key.paint == layer.paint
                && key.seed == seed
                && key.guides == layer.guides
                && key.color_tex_hash == color_tex_hash
            {
                return false;
            }
        }

        true
    }

    /// Record that a worker has been started for the given layer params.
    pub fn set_pending(&mut self, layer_index: usize, layer: &Layer, seed: u32, color_tex_hash: u64) {
        self.pending = Some((layer_index, LayerPathKey {
            paint: layer.paint.clone(),
            seed,
            guides: layer.guides.clone(),
            color_tex_hash,
        }));
    }

    /// Write a completed worker result into the cache slot.
    pub fn apply_result(&mut self, result: &PathOverlayResult) {
        self.pending = None;
        self.sync_layer_count(result.layer_count);
        if let Some(cache) = self.caches.get_mut(result.layer_index) {
            cache.paths = result.paths.clone();
            cache.original_total_segments = result.original_total_segments;
            cache.key = Some(LayerPathKey {
                paint: result.paint.clone(),
                seed: result.seed,
                guides: result.guides.clone(),
                color_tex_hash: result.color_tex_hash,
            });
        }
        // Clear non-selected layers
        for (i, cache) in self.caches.iter_mut().enumerate() {
            if i != result.layer_index {
                cache.key = None;
                cache.paths = Vec::new();
            }
        }
    }
}

// ── Path Overlay Worker ─────────────────────────────────────────

/// Input for the background path overlay computation.
/// All data is fully owned or Arc-shared — Send + 'static.
pub struct PathOverlayInput {
    pub layer: Layer,
    pub layer_index: usize,
    pub layer_count: usize,
    pub seed: u32,
    pub resolution: u32,
    pub color_data: Option<Arc<Vec<Color>>>,
    pub color_w: u32,
    pub color_h: u32,
    pub color_tex_hash: u64,
    /// Cached mesh normals from a previous computation. Reused if resolution matches.
    pub cached_normals: Option<(u32, Arc<MeshNormalData>)>,
    /// Mesh for on-demand normal computation when cache is cold.
    pub mesh: Option<Arc<LoadedMesh>>,
}

/// Result from a completed path overlay computation.
pub struct PathOverlayResult {
    pub paths: Vec<Vec<[f32; 2]>>,
    /// Original total segments before simplification (for badge display).
    pub original_total_segments: usize,
    pub layer_index: usize,
    pub layer_count: usize,
    pub seed: u32,
    pub color_tex_hash: u64,
    /// Copy of the layer's PaintValues + guides for cache key storage.
    pub paint: PaintValues,
    pub guides: Vec<Guide>,
    /// Freshly computed mesh normals — returned so the main thread can cache them.
    pub computed_normals: Option<(u32, Arc<MeshNormalData>)>,
}

/// Background worker for path overlay computation.
/// Mirrors the GenerationManager pattern: one thread, cancel token, poll/discard.
pub struct PathOverlayWorker {
    handle: Option<thread::JoinHandle<Option<PathOverlayResult>>>,
    cancel: Arc<AtomicBool>,
}

impl Default for PathOverlayWorker {
    fn default() -> Self {
        Self {
            handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl PathOverlayWorker {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Signal cancellation and drop the handle.
    /// The old thread finishes on its own; its result is discarded.
    pub fn discard(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.handle = None;
    }

    /// Poll for a completed result. Returns `None` if still running or cancelled.
    pub fn poll(&mut self) -> Option<Result<PathOverlayResult, String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            match self.handle.take().unwrap().join() {
                Ok(Some(result)) => Some(Ok(result)),
                Ok(None) => None, // cancelled
                Err(e) => {
                    let msg = e
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown error");
                    Some(Err(format!("Path overlay thread panicked: {msg}")))
                }
            }
        } else {
            None
        }
    }

    /// Spawn a new worker thread. Cancels any in-flight computation first.
    pub fn start(&mut self, input: PathOverlayInput) {
        self.discard();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        self.handle = Some(thread::spawn(move || run_path_overlay(input, &cancel)));
    }
}

/// Background thread function for path overlay computation.
fn run_path_overlay(input: PathOverlayInput, cancel: &AtomicBool) -> Option<PathOverlayResult> {
    let needs_normal = input.layer.paint.normal_break_threshold.is_some();

    // Resolve mesh normals: reuse cached if resolution matches, else compute.
    let cached_valid = needs_normal
        && input
            .cached_normals
            .as_ref()
            .is_some_and(|(res, _)| *res == input.resolution);

    let fresh_normals: Option<Arc<MeshNormalData>> = if needs_normal && !cached_valid {
        input
            .mesh
            .as_ref()
            .map(|mesh| Arc::new(compute_mesh_normal_data(mesh, input.resolution)))
    } else {
        None
    };

    let normal_ref: Option<&MeshNormalData> = if cached_valid {
        input.cached_normals.as_ref().map(|(_, nd)| nd.as_ref())
    } else {
        fresh_normals.as_deref()
    };

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // Build color texture reference (borrows from Arc — valid for this scope)
    let color_ref = input.color_data.as_ref().map(|data| ColorTextureRef {
        data,
        width: input.color_w,
        height: input.color_h,
    });

    let paint_layer = input.layer.to_paint_layer_with_seed(input.seed);
    let paths = path_placement::generate_paths_cancellable(
        &paint_layer,
        0,
        color_ref.as_ref(),
        normal_ref,
        None,
        Some(cancel),
    );

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // Simplify paths for overlay rendering: cap total points to keep UI responsive.
    // At 100K points, 7000 paths get ~14 pts each (~70px spacing) — enough for preview.
    const OVERLAY_POINT_BUDGET: usize = 100_000;
    let original_total_points: usize = paths.iter().map(|p| p.points.len()).sum();
    let original_total_segments = original_total_points.saturating_sub(paths.len());
    let point_stride = if original_total_points > OVERLAY_POINT_BUDGET {
        original_total_points.div_ceil(OVERLAY_POINT_BUDGET)
    } else {
        1
    };

    let path_points: Vec<Vec<[f32; 2]>> = paths
        .iter()
        .map(|p| {
            if point_stride > 1 && p.points.len() > 2 {
                let mut pts: Vec<[f32; 2]> = p
                    .points
                    .iter()
                    .step_by(point_stride)
                    .map(|v| [v.x, v.y])
                    .collect();
                // Always include the last point for path continuity
                let last = p.points.last().unwrap();
                let last_pt = [last.x, last.y];
                if pts.last() != Some(&last_pt) {
                    pts.push(last_pt);
                }
                pts
            } else {
                p.points.iter().map(|v| [v.x, v.y]).collect()
            }
        })
        .collect();

    Some(PathOverlayResult {
        paths: path_points,
        original_total_segments,
        layer_index: input.layer_index,
        layer_count: input.layer_count,
        seed: input.seed,
        color_tex_hash: input.color_tex_hash,
        paint: input.layer.paint.clone(),
        guides: input.layer.guides.clone(),
        computed_normals: fresh_normals.map(|nd| (input.resolution, nd)),
    })
}

// ── Preset Thumbnail Cache ─────────────────────────────────────

/// Cache of small stroke preview textures keyed by PaintValues.
#[derive(Default)]
pub struct PresetThumbnailCache {
    entries: Vec<(PaintValues, egui::TextureHandle)>,
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
            format!("preset_thumb_{}", self.entries.len()),
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
