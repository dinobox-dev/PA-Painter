use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};

use practical_arcana_painter::asset_io::LoadedMesh;
use practical_arcana_painter::compositing::{composite_layer, GlobalMaps};
use practical_arcana_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use practical_arcana_painter::output::{
    blend_normals_udn, generate_normal_map, generate_normal_map_depicted_form,
};
use practical_arcana_painter::path_placement::generate_paths_cancellable;
use practical_arcana_painter::stroke_color::ColorTextureRef;
use practical_arcana_painter::types::{
    BaseColorSource, Color, NormalMode, OutputSettings, PaintLayer, StrokePath,
};
use practical_arcana_painter::uv_mask::UvMask;

/// All data needed for a generation run. Fully owned, Send + 'static.
pub struct GenInput {
    pub layers: Vec<PaintLayer>,
    pub resolution: u32,
    pub base_colors: Option<Arc<Vec<Color>>>,
    pub base_w: u32,
    pub base_h: u32,
    pub solid_color: Color,
    pub settings: OutputSettings,
    /// Mesh for on-thread computation of normal data and UV masks.
    pub mesh: Option<Arc<LoadedMesh>>,
    /// Cached mesh normals — reused if resolution matches, otherwise recomputed.
    pub cached_normals: Option<(u32, Arc<MeshNormalData>)>,
    /// Group names for visible layers (parallel to `layers`), used to build UV masks.
    pub layer_group_names: Vec<String>,
    /// Base normal map pixels (linear RGBA [0,1]), if loaded.
    pub base_normal_pixels: Option<Vec<[f32; 4]>>,
    pub base_normal_w: u32,
    pub base_normal_h: u32,
}

/// Output from a completed generation.
pub struct GenResult {
    pub color: Vec<Color>,
    pub height: Vec<f32>,
    pub normal_map: Vec<[f32; 3]>,
    pub stroke_id: Vec<u32>,
    pub resolution: u32,
    pub elapsed: Duration,
    /// Freshly computed mesh normals — returned so the main thread can cache them.
    pub computed_normals: Option<(u32, Arc<MeshNormalData>)>,
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
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self {
            handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(AtomicU32::new(0f32.to_bits())),
            stage: Arc::new(AtomicU8::new(0)),
            start_time: None,
        }
    }
}

impl GenerationManager {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Current generation progress (0.0–1.0).
    pub fn progress(&self) -> f32 {
        f32::from_bits(self.progress.load(Ordering::Relaxed))
    }

    /// Current pipeline stage identifier.
    pub fn stage(&self) -> u8 {
        self.stage.load(Ordering::Relaxed)
    }

    /// Returns the result if the worker has finished.
    /// Returns `Err` if the worker thread panicked.
    /// The elapsed time is overridden to include main-thread pre-computation.
    pub fn poll(&mut self) -> Option<Result<GenResult, String>> {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let total_elapsed = self.start_time.take().map(|t| t.elapsed());
            match self.handle.take().unwrap().join() {
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

    // ── Stage 3: Path generation — parallel per layer ──

    stage.store(STAGE_PATHS, Ordering::Relaxed);
    set_progress(progress, p_paths);

    let base_color = match &input.base_colors {
        Some(colors) => {
            BaseColorSource::textured(colors, input.base_w, input.base_h, input.solid_color)
        }
        None => BaseColorSource::solid(input.solid_color),
    };

    let mask_refs: Vec<Option<&UvMask>> = masks.iter().map(|m| m.as_ref()).collect();

    let mut sorted: Vec<(usize, &PaintLayer)> = input.layers.iter().enumerate().collect();
    sorted.sort_by(|a, b| a.1.order.cmp(&b.1.order));

    let completed_paths = AtomicUsize::new(0);

    let mut all_paths: Vec<Vec<StrokePath>> = sorted
        .par_iter()
        .map(|&(layer_index, layer)| {
            let tex_ref = base_color.texture.map(|data| ColorTextureRef {
                data,
                width: base_color.tex_width,
                height: base_color.tex_height,
            });
            let mask = mask_refs.get(layer_index).and_then(|m| *m);
            let result = generate_paths_cancellable(
                layer,
                layer_index as u32,
                tex_ref.as_ref(),
                normal_data,
                mask,
                Some(cancel),
            );
            let done = completed_paths.fetch_add(1, Ordering::Relaxed) + 1;
            set_progress(progress, p_paths + path_span * done as f32 / n);
            result
        })
        .collect();

    // Assign globally unique stroke IDs (1-based; 0 = no stroke).
    let mut next_id = 1u32;
    for layer_paths in all_paths.iter_mut() {
        for path in layer_paths.iter_mut() {
            path.stroke_id = next_id;
            next_id = next_id.wrapping_add(1);
        }
    }

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // ── Stage 4: Compositing — sequential per layer ──

    stage.store(STAGE_COMPOSITE, Ordering::Relaxed);
    set_progress(progress, p_composite);

    let mut global = GlobalMaps::new(
        input.resolution,
        &base_color,
        input.settings.normal_mode,
        input.settings.background_mode,
    );

    for (sorted_idx, &(layer_index, layer)) in sorted.iter().enumerate() {
        let mask = mask_refs.get(layer_index).and_then(|m| *m);
        composite_layer(
            layer,
            layer_index as u32,
            &mut global,
            &input.settings,
            &base_color,
            Some(&all_paths[sorted_idx]),
            normal_data,
            mask,
        );
        set_progress(
            progress,
            p_composite + comp_span * (sorted_idx + 1) as f32 / n,
        );

        if cancel.load(Ordering::Relaxed) {
            return None;
        }
    }

    // ── Stage 5: Normal map ──

    stage.store(STAGE_NORMAL_MAP, Ordering::Relaxed);
    set_progress(progress, p_normal_map);

    let normal_map = match input.settings.normal_mode {
        NormalMode::DepictedForm => {
            if let Some(nd) = normal_data {
                generate_normal_map_depicted_form(
                    &global.gradient_x,
                    &global.gradient_y,
                    nd,
                    &global.object_normal,
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

    // ── Stage 6: Normal blending ──

    stage.store(STAGE_BLENDING, Ordering::Relaxed);
    set_progress(progress, p_blending);

    let mut normal_map = normal_map;
    if let Some(ref base_pixels) = input.base_normal_pixels {
        blend_normals_udn(
            &mut normal_map,
            base_pixels,
            input.base_normal_w,
            input.base_normal_h,
            input.resolution,
        );
    }

    set_progress(progress, 1.0);

    Some(GenResult {
        color: global.color,
        height: global.height,
        normal_map,
        stroke_id: global.stroke_id,
        resolution: input.resolution,
        elapsed: start.elapsed(),
        computed_normals: fresh_normals.map(|nd| (input.resolution, nd)),
    })
}
