use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use pa_painter::asset_io::LoadedMesh;
use pa_painter::compositing::{finalize_layers, render_layer, LayerMaps};
use pa_painter::compositing::{resolve_base_color, resolve_base_normal};
use pa_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use pa_painter::output::{
    blend_normals_udn, generate_normal_map, generate_normal_map_depicted_form,
};
use pa_painter::path_placement::{generate_paths, PathContext};
use pa_painter::stretch_map::{compute_stretch_map, StretchMap};
use pa_painter::stroke_color::ColorTextureRef;
use pa_painter::types::{
    BackgroundMode, BaseColorSource, Color, Layer, LayerBaseColor, LayerBaseNormal, NormalMode,
    OutputSettings, PaintLayer, StrokePath,
};
use pa_painter::uv_mask::UvMask;

/// All data needed for a generation run. Fully owned, Send + 'static.
pub struct GenInput {
    pub layers: Vec<PaintLayer>,
    pub resolution: u32,
    /// Per-layer base color data (parallel to `layers`).
    pub layer_base_colors: Vec<LayerBaseColor>,
    /// Per-layer base normal data for UDN blending (parallel to `layers`).
    pub layer_base_normals: Vec<LayerBaseNormal>,
    pub settings: OutputSettings,
    /// Mesh for on-thread computation of normal data and UV masks.
    pub mesh: Option<Arc<LoadedMesh>>,
    /// Cached mesh normals — reused if resolution matches, otherwise recomputed.
    pub cached_normals: Option<(u32, Arc<MeshNormalData>)>,
    /// Group names for visible layers (parallel to `layers`), used to build UV masks.
    pub layer_group_names: Vec<String>,
    /// Per-layer dryness of surface below (parallel to `layers`).
    pub layer_dry: Vec<f32>,
    /// Per-layer render hashes (parallel to `layers`), computed from render-relevant fields.
    pub layer_hashes: Vec<u64>,
    /// Per-layer path hashes (parallel to `layers`), covering only path-affecting fields.
    pub layer_path_hashes: Vec<u64>,
    /// Cached layer renders from a previous run, keyed by render hash.
    pub cached_layers: Vec<(u64, Arc<LayerMaps>)>,
    /// Cached paths from a previous run, keyed by path hash.
    pub cached_paths: Vec<(u64, Arc<Vec<StrokePath>>)>,
}

/// Output from a completed generation.
pub struct GenResult {
    pub color: Vec<Color>,
    pub height: Vec<f32>,
    pub normal_map: Vec<[f32; 3]>,
    pub stroke_id: Vec<u32>,
    pub stroke_time_order: Vec<f32>,
    pub stroke_time_arc: Vec<f32>,
    pub resolution: u32,
    pub elapsed: Duration,
    /// Freshly computed mesh normals — returned so the main thread can cache them.
    pub computed_normals: Option<(u32, Arc<MeshNormalData>)>,
    /// Rendered layer maps for caching, keyed by render hash.
    pub rendered_layers: Vec<(u64, Arc<LayerMaps>)>,
    /// Generated paths for caching, keyed by path hash.
    pub rendered_paths: Vec<(u64, Arc<Vec<StrokePath>>)>,
    /// Type A settings snapshot at generation start — used to detect stale output stage.
    pub gen_normal_strength: f32,
    pub gen_normal_mode: NormalMode,
    pub gen_background_mode: BackgroundMode,
    /// Pre-converted display images (built on worker thread to avoid main-thread stalls).
    pub display_color: egui::ColorImage,
    pub display_height: egui::ColorImage,
    pub display_normal: egui::ColorImage,
    pub display_stroke_id: egui::ColorImage,
    /// Pre-converted GPU pixel bytes for 3D preview (built on worker thread).
    pub gpu_color_pixels: Vec<u8>,
    pub gpu_normal_pixels: Vec<u8>,
}

/// Manages a single background generation thread.
/// Pipeline stage identifiers for UI display.
pub const STAGE_NORMALS: u8 = 1;
pub const STAGE_MASKS: u8 = 2;
pub const STAGE_PATHS: u8 = 3;
pub const STAGE_COMPOSITE: u8 = 4;
pub const STAGE_NORMAL_MAP: u8 = 5;
pub const STAGE_BLENDING: u8 = 6;

pub struct GenerationManager {
    handle: Option<thread::JoinHandle<Option<GenResult>>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU32>,
    stage: Arc<AtomicU8>,
    /// Wall-clock start time recorded before pre-computation, so the displayed
    /// elapsed duration includes main-thread prep work the user actually waits for.
    pub start_time: Option<Instant>,
    /// Cached per-layer render results from the last completed generation.
    pub layer_cache: Vec<(u64, Arc<LayerMaps>)>,
    /// Cached per-layer paths from the last completed generation, keyed by path hash.
    /// Path geometry is resolution-independent so these survive resolution changes.
    pub path_cache: Vec<(u64, Arc<Vec<StrokePath>>)>,
    /// Global hash (resolution + mesh) for the cached layers.
    /// When this changes, all cached layers are invalidated.
    pub cache_global_hash: u64,
    /// Whether the current run is a low-res preview (skip cache update on completion).
    pub is_preview: bool,
    /// Queue of remaining progressive resolution steps (ascending).
    /// Each completed preview pops the front and starts the next.
    /// Empty means the current run is the final (full-res) step.
    pub progressive_queue: Vec<u32>,
    /// Total number of progressive steps (initial + queue) for overall progress display.
    pub progressive_total: u32,
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self {
            handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU32::new(0f32.to_bits())),
            stage: Arc::new(AtomicU8::new(0)),
            start_time: None,
            layer_cache: Vec::new(),
            path_cache: Vec::new(),
            cache_global_hash: 0,
            is_preview: false,
            progressive_queue: Vec::new(),
            progressive_total: 1,
        }
    }
}

impl GenerationManager {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Current generation progress (0.0–1.0) for the active step only.
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Overall progress across all progressive steps (0.0–1.0).
    pub fn overall_progress(&self) -> f32 {
        let total = self.progressive_total.max(1) as f32;
        let remaining = self.progressive_queue.len() as f32;
        let completed_steps = total - remaining - 1.0; // steps already done
        ((completed_steps + self.progress()) / total).clamp(0.0, 1.0)
    }

    /// Current pipeline stage identifier.
    #[allow(dead_code)]
    pub fn stage(&self) -> u8 {
        self.stage.load(Ordering::Relaxed)
    }

    /// Returns the result if the worker has finished.
    /// Returns `Err` if the worker thread panicked.
    /// The elapsed time is overridden to include main-thread pre-computation.
    pub fn poll(&mut self) -> Option<Result<GenResult, String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let total_elapsed = self.start_time.map(|t| t.elapsed());
            match self
                .handle
                .take()
                .expect("worker handle was checked above")
                .join()
            {
                Ok(Some(mut result)) => {
                    if let Some(elapsed) = total_elapsed {
                        result.elapsed = elapsed;
                    }
                    Some(Ok(result))
                }
                Ok(None) => None, // cancelled
                Err(e) => {
                    let msg = e
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| e.downcast_ref::<String>().map(|s| s.as_str()))
                        .unwrap_or("unknown error");
                    Some(Err(format!("Generation thread panicked: {msg}")))
                }
            }
        } else {
            None
        }
    }

    /// Signal cancellation and drop the handle so the thread exits at the next checkpoint.
    pub fn discard(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.handle = None;
    }

    /// Spawn a worker thread to run the full generation pipeline.
    pub fn start(&mut self, input: GenInput) {
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        self.stage = Arc::new(AtomicU8::new(STAGE_NORMALS));
        let cancel = Arc::clone(&self.cancel);
        let progress = Arc::clone(&self.progress);
        let stage = Arc::clone(&self.stage);
        self.handle = Some(thread::spawn(move || {
            run_pipeline(input, &cancel, &progress, &stage)
        }));
    }
}

/// Helper to store progress as f32 bits in an AtomicU32.
fn set_progress(progress: &AtomicU32, value: f32) {
    progress.store(value.to_bits(), Ordering::Relaxed);
}

fn run_pipeline(
    input: GenInput,
    cancel: &AtomicBool,
    progress: &AtomicU32,
    stage: &AtomicU8,
) -> Option<GenResult> {
    let start = Instant::now();

    // ── Progress weight model ──
    // Fixed stages cost 1 unit each (normals, masks, normal_map, blending = 4).
    // Per-layer stages cost 2 units each (paths, compositing = 4n).
    // Total = 4 + 4n.  This ensures per-layer stages dominate when n is large,
    // while fixed stages get meaningful weight when n is small.
    let n = input.layers.len().max(1) as f32;
    let total = 4.0 + 4.0 * n;
    let p_normals = 0.0; // start of normals
    let p_masks = 1.0 / total; // start of masks
    let p_paths = 2.0 / total; // start of paths
    let p_composite = (2.0 + 2.0 * n) / total; // start of compositing
    let p_normal_map = (2.0 + 4.0 * n) / total; // start of normal map
    let p_blending = (3.0 + 4.0 * n) / total; // start of blending
    let path_span = p_composite - p_paths; // width of paths stage
    let comp_span = p_normal_map - p_composite; // width of compositing stage

    // ── Stage 1: Mesh normals ──

    stage.store(STAGE_NORMALS, Ordering::Relaxed);
    set_progress(progress, p_normals);

    let fresh_normals: Option<Arc<MeshNormalData>> =
        if input.settings.normal_mode == NormalMode::DepictedForm {
            match &input.cached_normals {
                Some((res, _)) if *res == input.resolution => None,
                _ => input
                    .mesh
                    .as_ref()
                    .map(|mesh| Arc::new(compute_mesh_normal_data(mesh, input.resolution))),
            }
        } else {
            None
        };

    let normal_data: Option<&MeshNormalData> = if let Some(ref nd) = fresh_normals {
        Some(nd)
    } else if input.settings.normal_mode == NormalMode::DepictedForm {
        input.cached_normals.as_ref().map(|(_, nd)| nd.as_ref())
    } else {
        None
    };

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // ── Stage 2: UV masks ──

    stage.store(STAGE_MASKS, Ordering::Relaxed);
    set_progress(progress, p_masks);

    let masks: Vec<Option<UvMask>> = if let Some(ref mesh) = input.mesh {
        input
            .layer_group_names
            .iter()
            .map(|group_name| {
                if group_name == "__all__" {
                    None
                } else {
                    mesh.groups
                        .iter()
                        .find(|g| g.name == *group_name)
                        .map(|group| {
                            let mut mask = UvMask::from_mesh_group(mesh, group, input.resolution);
                            mask.dilate(2);
                            mask
                        })
                }
            })
            .collect()
    } else {
        input.layer_group_names.iter().map(|_| None).collect()
    };

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // Compute stretch map for UV distortion compensation
    let stretch_data: Option<StretchMap> = input
        .mesh
        .as_ref()
        .map(|mesh| compute_stretch_map(mesh, input.resolution));
    let stretch_ref = stretch_data.as_ref();

    let mask_refs: Vec<Option<&UvMask>> = masks.iter().map(|m| m.as_ref()).collect();

    let mut sorted: Vec<(usize, &PaintLayer)> = input.layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    // ── Check per-layer caches (render + path) ──
    //
    // Three cases per layer:
    //   1. render_hash match → full LayerMaps cache hit (skip paths + render)
    //   2. path_hash match  → path cache hit (skip path generation, re-render)
    //   3. neither          → generate paths from scratch, then render
    #[derive(Clone)]
    enum LayerCacheState {
        RenderHit(Arc<LayerMaps>),
        PathHit(Arc<Vec<StrokePath>>),
        Miss,
    }

    let cache_states: Vec<LayerCacheState> = sorted
        .iter()
        .map(|&(layer_index, _)| {
            let render_h = input.layer_hashes.get(layer_index).copied().unwrap_or(0);
            if let Some((_, maps)) = input.cached_layers.iter().find(|(h, _)| *h == render_h) {
                return LayerCacheState::RenderHit(Arc::clone(maps));
            }
            let path_h = input
                .layer_path_hashes
                .get(layer_index)
                .copied()
                .unwrap_or(0);
            if let Some((_, paths)) = input.cached_paths.iter().find(|(h, _)| *h == path_h) {
                return LayerCacheState::PathHit(Arc::clone(paths));
            }
            LayerCacheState::Miss
        })
        .collect();

    // Indices of layers that need fresh path generation (Miss only, not PathHit)
    let path_miss_indices: Vec<(usize, usize)> = sorted
        .iter()
        .enumerate()
        .filter(|(si, _)| matches!(cache_states[*si], LayerCacheState::Miss))
        .map(|(si, &(li, _))| (si, li))
        .collect();

    // ── Stage 3: Path generation — only for layers with no path cache ──

    stage.store(STAGE_PATHS, Ordering::Relaxed);
    set_progress(progress, p_paths);

    let completed_paths = AtomicUsize::new(0);
    let n_path_miss = path_miss_indices.len().max(1) as f32;

    let mut fresh_paths: Vec<Vec<StrokePath>> = path_miss_indices
        .par_iter()
        .map(|&(_, layer_index)| {
            let layer = &input.layers[layer_index];
            let base = input
                .layer_base_colors
                .get(layer_index)
                .map(|bc| bc.as_source())
                .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
            let tex_ref = base.texture.map(|data| ColorTextureRef {
                data,
                width: base.tex_width,
                height: base.tex_height,
            });
            let mask = mask_refs.get(layer_index).and_then(|m| *m);
            let result = generate_paths(
                layer,
                layer_index as u32,
                &PathContext {
                    color_tex: tex_ref.as_ref(),
                    normal_data,
                    mask,
                    stretch_map: stretch_ref,
                    cancel: Some(cancel),
                },
            );
            let done = completed_paths.fetch_add(1, Ordering::Relaxed) + 1;
            set_progress(progress, p_paths + path_span * done as f32 / n_path_miss);
            result
        })
        .collect();

    // Assign globally unique stroke IDs (1-based; 0 = no stroke).
    let mut next_id = 1u32;
    for layer_paths in fresh_paths.iter_mut() {
        for path in layer_paths.iter_mut() {
            path.stroke_id = next_id;
            next_id = next_id.wrapping_add(1);
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // ── Stage 4: Per-layer independent rendering → merge ──

    stage.store(STAGE_COMPOSITE, Ordering::Relaxed);
    set_progress(progress, p_composite);

    // Assemble paths for each layer (from cache or fresh), then render
    let mut result_maps: Vec<Arc<LayerMaps>> = Vec::with_capacity(sorted.len());
    let mut all_paths: Vec<(u64, Arc<Vec<StrokePath>>)> = Vec::with_capacity(sorted.len());
    let mut fresh_path_idx = 0;
    for (sorted_idx, &(layer_index, _)) in sorted.iter().enumerate() {
        let path_hash = input
            .layer_path_hashes
            .get(layer_index)
            .copied()
            .unwrap_or(0);
        match &cache_states[sorted_idx] {
            LayerCacheState::RenderHit(maps) => {
                result_maps.push(Arc::clone(maps));
                // Preserve existing path cache entry if available
                if let Some((_, paths)) = input.cached_paths.iter().find(|(h, _)| *h == path_hash) {
                    all_paths.push((path_hash, Arc::clone(paths)));
                }
            }
            LayerCacheState::PathHit(paths) => {
                let layer = &input.layers[layer_index];
                let base = input
                    .layer_base_colors
                    .get(layer_index)
                    .map(|bc| bc.as_source())
                    .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
                let mask = mask_refs.get(layer_index).and_then(|m| *m);
                result_maps.push(Arc::new(render_layer(
                    layer,
                    layer_index as u32,
                    &base,
                    Some(paths),
                    normal_data,
                    mask,
                    stretch_ref,
                    input.resolution,
                )));
                all_paths.push((path_hash, Arc::clone(paths)));
            }
            LayerCacheState::Miss => {
                let layer = &input.layers[layer_index];
                let base = input
                    .layer_base_colors
                    .get(layer_index)
                    .map(|bc| bc.as_source())
                    .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE));
                let mask = mask_refs.get(layer_index).and_then(|m| *m);
                let paths_arc = Arc::new(std::mem::take(&mut fresh_paths[fresh_path_idx]));
                fresh_path_idx += 1;
                result_maps.push(Arc::new(render_layer(
                    layer,
                    layer_index as u32,
                    &base,
                    Some(&paths_arc),
                    normal_data,
                    mask,
                    stretch_ref,
                    input.resolution,
                )));
                all_paths.push((path_hash, paths_arc));
            }
        }
        set_progress(
            progress,
            p_composite + comp_span * (sorted_idx + 1) as f32 / n,
        );

        if cancel.load(Ordering::Relaxed) {
            return None;
        }
    }

    // Build rendered_layers for caching (hash → Arc<LayerMaps>)
    let rendered_layers: Vec<(u64, Arc<LayerMaps>)> = sorted
        .iter()
        .enumerate()
        .map(|(si, &(li, _))| {
            let hash = input.layer_hashes.get(li).copied().unwrap_or(0);
            (hash, Arc::clone(&result_maps[si]))
        })
        .collect();

    // 4b: Finalize layers (fill base colors → merge → gradients)
    let layer_refs: Vec<&LayerMaps> = result_maps.iter().map(|a| a.as_ref()).collect();
    let sorted_base_colors: Vec<BaseColorSource<'_>> = sorted
        .iter()
        .map(|&(layer_index, _)| {
            input
                .layer_base_colors
                .get(layer_index)
                .map(|bc| bc.as_source())
                .unwrap_or_else(|| BaseColorSource::solid(Color::WHITE))
        })
        .collect();
    let sorted_masks: Vec<Option<&UvMask>> = sorted
        .iter()
        .map(|&(layer_index, _)| mask_refs.get(layer_index).and_then(|m| *m))
        .collect();
    let layer_dry: Vec<f32> = sorted
        .iter()
        .map(|&(layer_index, _)| input.layer_dry.get(layer_index).copied().unwrap_or(1.0))
        .collect();
    let sorted_groups: Vec<&str> = sorted
        .iter()
        .map(|&(layer_index, _)| {
            input
                .layer_group_names
                .get(layer_index)
                .map(|s| s.as_str())
                .unwrap_or("__all__")
        })
        .collect();
    let global = finalize_layers(
        &layer_refs,
        &sorted_base_colors,
        &sorted_masks,
        &layer_dry,
        &sorted_groups,
        input.resolution,
        &input.settings,
    );

    // ── Stage 5: Normal map ──

    stage.store(STAGE_NORMAL_MAP, Ordering::Relaxed);
    set_progress(progress, p_normal_map);

    let mut normal_map = match input.settings.normal_mode {
        NormalMode::DepictedForm => {
            if let Some(nd) = normal_data {
                generate_normal_map_depicted_form(
                    &global.gradient_x,
                    &global.gradient_y,
                    nd,
                    &global.object_normal,
                    &global.paint_load,
                    input.resolution,
                    input.settings.normal_strength,
                )
            } else {
                generate_normal_map(
                    &global.gradient_x,
                    &global.gradient_y,
                    input.resolution,
                    input.settings.normal_strength,
                )
            }
        }
        NormalMode::SurfacePaint => generate_normal_map(
            &global.gradient_x,
            &global.gradient_y,
            input.resolution,
            input.settings.normal_strength,
        ),
    };

    // ── Stage 6: Per-layer UDN normal blending ──

    stage.store(STAGE_BLENDING, Ordering::Relaxed);
    set_progress(progress, p_blending);

    for &(layer_index, _) in &sorted {
        if let Some(bn) = input.layer_base_normals.get(layer_index) {
            if let Some(ref pixels) = bn.pixels {
                let mask = mask_refs.get(layer_index).and_then(|m| *m);
                blend_normals_udn(
                    &mut normal_map,
                    pixels,
                    bn.width,
                    bn.height,
                    input.resolution,
                    mask,
                );
            }
        }
    }

    set_progress(progress, 1.0);

    // Pre-convert display images on the worker thread to avoid main-thread stalls.
    let display_color =
        super::textures::color_buffer_to_image(&global.color, input.resolution, input.resolution);
    let display_height = super::textures::height_buffer_to_image(&global.height, input.resolution);
    let display_normal = super::textures::normal_map_to_image(&normal_map, input.resolution);
    let display_stroke_id =
        super::textures::stroke_id_to_image(&global.stroke_id, input.resolution);

    // Pre-convert GPU pixel bytes for 3D preview textures.
    let gpu_color_pixels = super::mesh_preview::convert_color_pixels(&global.color);
    let gpu_normal_pixels = super::mesh_preview::convert_normal_pixels(&normal_map);

    Some(GenResult {
        color: global.color,
        height: global.height,
        normal_map,
        stroke_id: global.stroke_id,
        stroke_time_order: global.stroke_time_order,
        stroke_time_arc: global.stroke_time_arc,
        resolution: input.resolution,
        elapsed: start.elapsed(),
        computed_normals: fresh_normals.map(|nd| (input.resolution, nd)),
        rendered_layers,
        rendered_paths: all_paths,
        gen_normal_strength: input.settings.normal_strength,
        gen_normal_mode: input.settings.normal_mode,
        gen_background_mode: input.settings.background_mode,
        display_color,
        display_height,
        display_normal,
        display_stroke_id,
        gpu_color_pixels,
        gpu_normal_pixels,
    })
}

// ── Async Remerge ──────────────────────────────────────────────────

/// All data needed for an async remerge. Fully owned, Send + 'static.
pub struct RemergeInput {
    pub layer_cache: Vec<(u64, Arc<LayerMaps>)>,
    pub settings: OutputSettings,
    pub layers: Vec<Layer>,
    pub mesh: Option<Arc<LoadedMesh>>,
    pub cached_normals: Option<(u32, Arc<MeshNormalData>)>,
    pub rendered_paths: Vec<(u64, Arc<Vec<StrokePath>>)>,
}

/// Output from a completed async remerge.
pub struct RemergeResult {
    pub color: Vec<Color>,
    pub height: Vec<f32>,
    pub normal_map: Vec<[f32; 3]>,
    pub stroke_id: Vec<u32>,
    pub stroke_time_order: Vec<f32>,
    pub stroke_time_arc: Vec<f32>,
    pub resolution: u32,
    pub gen_normal_strength: f32,
    pub gen_normal_mode: NormalMode,
    pub gen_background_mode: BackgroundMode,
    pub rendered_layers: Vec<(u64, Arc<LayerMaps>)>,
    pub rendered_paths: Vec<(u64, Arc<Vec<StrokePath>>)>,
    /// Pre-converted display images (built on worker thread).
    pub display_color: egui::ColorImage,
    pub display_height: egui::ColorImage,
    pub display_normal: egui::ColorImage,
    pub display_stroke_id: egui::ColorImage,
    /// Pre-converted GPU pixel bytes for 3D preview (built on worker thread).
    pub gpu_color_pixels: Vec<u8>,
    pub gpu_normal_pixels: Vec<u8>,
}

/// Lightweight async worker for re-merge operations.
pub struct RemergeWorker {
    handle: Option<thread::JoinHandle<Option<RemergeResult>>>,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
}

impl Default for RemergeWorker {
    fn default() -> Self {
        Self {
            handle: None,
            progress: Arc::new(AtomicU32::new(0f32.to_bits())),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl RemergeWorker {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Current remerge progress (0.0–1.0).
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    pub fn poll(&mut self) -> Option<RemergeResult> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            match self
                .handle
                .take()
                .expect("worker handle was checked above")
                .join()
            {
                Ok(Some(result)) => Some(result),
                Ok(None) => None,
                Err(_) => None,
            }
        } else {
            None
        }
    }

    pub fn start(&mut self, input: RemergeInput) {
        // Cancel previous work (old thread will see the old cancel flag)
        self.cancel.store(true, Ordering::Relaxed);
        // Create fresh atomics for the new run
        self.cancel = Arc::new(AtomicBool::new(false));
        self.progress = Arc::new(AtomicU32::new(0f32.to_bits()));
        let progress = Arc::clone(&self.progress);
        let cancel = Arc::clone(&self.cancel);
        self.handle = Some(thread::spawn(move || {
            run_remerge(input, &progress, &cancel)
        }));
    }
}

fn run_remerge(
    input: RemergeInput,
    progress: &AtomicU32,
    cancel: &AtomicBool,
) -> Option<RemergeResult> {
    let settings = &input.settings;
    let resolution = settings.resolution_preset.resolution();
    set_progress(progress, 0.0);

    // Collect visible layers sorted by order
    let mut sorted_layers: Vec<&Layer> = input.layers.iter().filter(|l| l.visible).collect();
    sorted_layers.sort_by_key(|l| l.order);

    // Match each visible layer to its cached LayerMaps by render hash
    let mut layer_maps: Vec<&LayerMaps> = Vec::with_capacity(sorted_layers.len());
    for layer in &sorted_layers {
        let hash = layer.render_hash();
        if let Some((_, maps)) = input.layer_cache.iter().find(|(h, _)| *h == hash) {
            if maps.resolution != resolution {
                return None; // resolution mismatch — need full regeneration
            }
            layer_maps.push(maps.as_ref());
        } else {
            return None; // cache miss
        }
    }

    // Build UV masks
    let materials: &[_] = input
        .mesh
        .as_ref()
        .map(|m| m.materials.as_slice())
        .unwrap_or(&[]);
    let masks: Vec<Option<UvMask>> = if let Some(ref mesh) = input.mesh {
        sorted_layers
            .iter()
            .map(|layer| {
                if layer.group_name == "__all__" {
                    None
                } else {
                    mesh.groups
                        .iter()
                        .find(|g| g.name == layer.group_name)
                        .map(|group| {
                            let mut mask = UvMask::from_mesh_group(mesh, group, resolution);
                            mask.dilate(2);
                            mask
                        })
                }
            })
            .collect()
    } else {
        sorted_layers.iter().map(|_| None).collect()
    };
    let mask_refs: Vec<Option<&UvMask>> = masks.iter().map(|m| m.as_ref()).collect();

    // Initialize GlobalMaps
    // Finalize layers (fill base colors → merge → gradients)
    set_progress(progress, 0.1);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let base_color_data: Vec<_> = sorted_layers
        .iter()
        .map(|layer| resolve_base_color(&layer.base_color, materials))
        .collect();
    let base_colors: Vec<BaseColorSource<'_>> =
        base_color_data.iter().map(|bc| bc.as_source()).collect();
    let layer_dry: Vec<f32> = sorted_layers.iter().map(|l| l.dry).collect();
    let group_names: Vec<&str> = sorted_layers
        .iter()
        .map(|l| l.group_name.as_str())
        .collect();
    let global = finalize_layers(
        &layer_maps,
        &base_colors,
        &mask_refs,
        &layer_dry,
        &group_names,
        resolution,
        settings,
    );
    set_progress(progress, 0.4);

    // Normal map
    set_progress(progress, 0.5);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let normal_data = if settings.normal_mode == NormalMode::DepictedForm {
        input.cached_normals.as_ref().map(|(_, nd)| nd.as_ref())
    } else {
        None
    };

    let mut normal_map = match settings.normal_mode {
        NormalMode::DepictedForm => {
            if let Some(nd) = normal_data {
                generate_normal_map_depicted_form(
                    &global.gradient_x,
                    &global.gradient_y,
                    nd,
                    &global.object_normal,
                    &global.paint_load,
                    resolution,
                    settings.normal_strength,
                )
            } else {
                generate_normal_map(
                    &global.gradient_x,
                    &global.gradient_y,
                    resolution,
                    settings.normal_strength,
                )
            }
        }
        NormalMode::SurfacePaint => generate_normal_map(
            &global.gradient_x,
            &global.gradient_y,
            resolution,
            settings.normal_strength,
        ),
    };

    // UDN normal blending
    set_progress(progress, 0.7);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    for (si, layer) in sorted_layers.iter().enumerate() {
        let bn = resolve_base_normal(&layer.base_normal, materials);
        if let Some(ref pixels) = bn.pixels {
            let mask = mask_refs[si];
            blend_normals_udn(
                &mut normal_map,
                pixels,
                bn.width,
                bn.height,
                resolution,
                mask,
            );
        }
    }

    set_progress(progress, 0.8);
    if cancel.load(Ordering::Relaxed) {
        return None;
    }
    let display_color =
        super::textures::color_buffer_to_image(&global.color, resolution, resolution);
    let display_height = super::textures::height_buffer_to_image(&global.height, resolution);
    let display_normal = super::textures::normal_map_to_image(&normal_map, resolution);
    let display_stroke_id = super::textures::stroke_id_to_image(&global.stroke_id, resolution);

    let gpu_color_pixels = super::mesh_preview::convert_color_pixels(&global.color);
    let gpu_normal_pixels = super::mesh_preview::convert_normal_pixels(&normal_map);

    set_progress(progress, 1.0);
    Some(RemergeResult {
        color: global.color,
        height: global.height,
        normal_map,
        stroke_id: global.stroke_id,
        stroke_time_order: global.stroke_time_order,
        stroke_time_arc: global.stroke_time_arc,
        resolution,
        gen_normal_strength: settings.normal_strength,
        gen_normal_mode: settings.normal_mode,
        gen_background_mode: settings.background_mode,
        rendered_layers: input.layer_cache,
        rendered_paths: input.rendered_paths,
        display_color,
        display_height,
        display_normal,
        display_stroke_id,
        gpu_color_pixels,
        gpu_normal_pixels,
    })
}
