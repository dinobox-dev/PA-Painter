use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use practical_arcana_painter::asset_io::LoadedMesh;
use practical_arcana_painter::compositing::{composite_all_with_paths, generate_all_paths};
use practical_arcana_painter::object_normal::{compute_mesh_normal_data, MeshNormalData};
use practical_arcana_painter::output::{
    blend_normals_udn, generate_normal_map, generate_normal_map_depicted_form,
};
use practical_arcana_painter::types::{
    BaseColorSource, Color, NormalMode, OutputSettings, PaintLayer,
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
pub struct GenerationManager {
    handle: Option<thread::JoinHandle<Option<GenResult>>>,
    cancel: Arc<AtomicBool>,
    /// Wall-clock start time recorded before pre-computation, so the displayed
    /// elapsed duration includes main-thread prep work the user actually waits for.
    pub start_time: Option<Instant>,
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self {
            handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
            start_time: None,
        }
    }
}

impl GenerationManager {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
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
        let cancel = Arc::clone(&self.cancel);
        self.handle = Some(thread::spawn(move || run_pipeline(input, &cancel)));
    }
}

fn run_pipeline(input: GenInput, cancel: &AtomicBool) -> Option<GenResult> {
    let start = Instant::now();

    // ── Pre-computation (previously on main thread) ──

    // Compute mesh normals: reuse cached if resolution matches, otherwise compute.
    let fresh_normals: Option<Arc<MeshNormalData>> =
        if input.settings.normal_mode == NormalMode::DepictedForm {
            match &input.cached_normals {
                Some((res, _)) if *res == input.resolution => None, // will use cached
                _ => input
                    .mesh
                    .as_ref()
                    .map(|mesh| Arc::new(compute_mesh_normal_data(mesh, input.resolution))),
            }
        } else {
            None
        };

    let normal_data: Option<&MeshNormalData> =
        if let Some(ref nd) = fresh_normals {
            Some(nd)
        } else if input.settings.normal_mode == NormalMode::DepictedForm {
            input.cached_normals.as_ref().map(|(_, nd)| nd.as_ref())
        } else {
            None
        };

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    // Build UV masks from mesh groups.
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
                            let mut mask =
                                UvMask::from_mesh_group(mesh, group, input.resolution);
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

    // ── Main pipeline ──

    let base_color = match &input.base_colors {
        Some(colors) => {
            BaseColorSource::textured(colors, input.base_w, input.base_h, input.solid_color)
        }
        None => BaseColorSource::solid(input.solid_color),
    };

    let mask_refs: Vec<Option<&UvMask>> = masks.iter().map(|m| m.as_ref()).collect();

    let paths = generate_all_paths(
        &input.layers,
        input.resolution,
        &base_color,
        normal_data,
        &mask_refs,
    );

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let global = composite_all_with_paths(
        &input.layers,
        input.resolution,
        &base_color,
        &input.settings,
        Some(&paths),
        normal_data,
        &mask_refs,
    );

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

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

    // Apply base normal blending (UDN) if a base normal texture is provided
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
