use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use practical_arcana_painter::compositing::{composite_all_with_paths, generate_all_paths};
use practical_arcana_painter::object_normal::MeshNormalData;
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
    pub base_colors: Option<Vec<Color>>,
    pub base_w: u32,
    pub base_h: u32,
    pub solid_color: Color,
    pub settings: OutputSettings,
    pub normal_data: Option<MeshNormalData>,
    pub masks: Vec<Option<UvMask>>,
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
}

/// Manages a single background generation thread.
pub struct GenerationManager {
    handle: Option<thread::JoinHandle<Option<GenResult>>>,
    cancel: Arc<AtomicBool>,
}

impl Default for GenerationManager {
    fn default() -> Self {
        Self {
            handle: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl GenerationManager {
    pub fn is_running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }

    /// Returns the result if the worker has finished.
    /// Returns `Err` if the worker thread panicked.
    pub fn poll(&mut self) -> Option<Result<GenResult, String>> {
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

    let base_color = match &input.base_colors {
        Some(colors) => {
            BaseColorSource::textured(colors, input.base_w, input.base_h, input.solid_color)
        }
        None => BaseColorSource::solid(input.solid_color),
    };

    let mask_refs: Vec<Option<&UvMask>> = input.masks.iter().map(|m| m.as_ref()).collect();

    let paths = generate_all_paths(
        &input.layers,
        input.resolution,
        &base_color,
        input.normal_data.as_ref(),
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
        input.normal_data.as_ref(),
        &mask_refs,
    );

    if cancel.load(Ordering::Relaxed) {
        return None;
    }

    let normal_map = match input.settings.normal_mode {
        NormalMode::DepictedForm => {
            if let Some(ref nd) = input.normal_data {
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
    })
}
