//! Generation pipeline orchestration and auto-preview.

use eframe::egui;

use super::generation;
use super::mesh_preview;
use super::state;
use super::PainterApp;

/// Extract per-layer LayerMaps references (in compositing order).
/// Prefers `rendered_layers` from GenResult; falls back to `layer_cache` from GenerationManager
/// (full-res generation moves rendered_layers into layer_cache for reuse).
pub(super) fn collect_layer_refs<'a>(
    rendered_layers: &'a [(u64, std::sync::Arc<pa_painter::compositing::LayerMaps>)],
    layer_cache: &'a [(u64, std::sync::Arc<pa_painter::compositing::LayerMaps>)],
) -> Vec<&'a pa_painter::compositing::LayerMaps> {
    let source = if rendered_layers.is_empty() {
        layer_cache
    } else {
        rendered_layers
    };
    source.iter().map(|(_, lm)| lm.as_ref()).collect()
}

impl PainterApp {
    /// Compute a combined hash of all layer render hashes + mesh hash.
    /// Used to detect Type C/D parameter changes between frames.
    pub(super) fn combined_render_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for layer in &self.state.project.layers {
            layer.render_hash().hash(&mut hasher);
        }
        self.state.mesh_hash.hash(&mut hasher);
        self.state
            .project
            .settings
            .resolution_preset
            .resolution()
            .hash(&mut hasher);
        hasher.finish()
    }

    /// Preview resolution for auto-preview (256px).
    const PREVIEW_RESOLUTION: u32 = 256;
    /// Debounce delay before starting auto-preview.
    const PREVIEW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);
    pub(super) fn start_generation(&mut self) {
        self.state.auto_preview_timer = None;
        self.state.generation.progressive_queue.clear();
        self.state.generation.progressive_total = 1;
        self.start_generation_at_resolution(
            self.state.project.settings.resolution_preset.resolution(),
            false,
        );
    }

    pub(super) fn start_preview_generation(&mut self) {
        // Build progressive resolution queue: 256 → 512 → 1024 → … → full-res.
        // Each step doubles until reaching the target. Steps at or below PREVIEW_RESOLUTION
        // are skipped (the initial preview covers those).
        let full = self.state.project.settings.resolution_preset.resolution();
        let mut queue = Vec::new();
        let mut r = Self::PREVIEW_RESOLUTION * 2; // first step after initial preview
        while r < full {
            queue.push(r);
            r *= 2;
        }
        // Final full-res is always added (handled as is_preview=false when dequeued).
        queue.push(full);
        self.state.generation.progressive_total = queue.len() as u32 + 1; // +1 for initial preview
        self.state.generation.progressive_queue = queue;

        self.start_generation_at_resolution(Self::PREVIEW_RESOLUTION, true);
    }

    fn start_generation_at_resolution(&mut self, resolution: u32, is_preview: bool) {
        if self.state.generation.is_running() {
            // Cancel in-flight generation to start the new one
            self.state.generation.discard();
        }

        let full_res = self.state.project.settings.resolution_preset.resolution();
        self.state.status_message = format!("Generating {}px…", full_res);
        // Preserve start_time across progressive steps; only set if not already running.
        if self.state.generation.start_time.is_none() {
            self.state.generation.start_time = Some(std::time::Instant::now());
        }
        self.state.generation.is_preview = is_preview;

        let layers = self.state.project.paint_layers();
        let settings = self.state.project.settings.clone();

        let mesh = self.state.loaded_mesh.clone(); // Arc clone
        let cached_normals = self.state.cached_mesh_normals.clone(); // (u32, Arc) clone

        // Resolve per-layer base color and normal from TextureSource.
        let materials: &[_] = mesh.as_ref().map(|m| m.materials.as_slice()).unwrap_or(&[]);
        let visible_layers: Vec<&pa_painter::types::Layer> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .collect();
        let layer_base_colors: Vec<_> = visible_layers
            .iter()
            .map(|l| pa_painter::compositing::resolve_base_color(&l.base_color, materials))
            .collect();
        let layer_base_normals: Vec<_> = visible_layers
            .iter()
            .map(|l| pa_painter::compositing::resolve_base_normal(&l.base_normal, materials))
            .collect();

        // Group names for visible layers — parallel to `layers` vec
        let layer_group_names: Vec<String> = visible_layers
            .iter()
            .map(|l| l.group_name.clone())
            .collect();

        // Per-layer render hashes (parallel to visible layers / `layers` vec)
        let layer_hashes: Vec<u64> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.render_hash())
            .collect();
        let layer_path_hashes: Vec<u64> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .map(|l| l.path_hash())
            .collect();

        // Pass layer cache if global inputs (resolution, mesh) haven't changed
        let global_hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            resolution.hash(&mut h);
            self.state.mesh_hash.hash(&mut h);
            h.finish()
        };
        let cached_layers = if global_hash == self.state.generation.cache_global_hash {
            self.state.generation.layer_cache.clone()
        } else {
            Vec::new()
        };
        // Path cache is resolution-independent — always pass it
        let cached_paths = self.state.generation.path_cache.clone();

        self.state.generation.start(generation::GenInput {
            layers,
            resolution,
            layer_base_colors,
            layer_base_normals,
            settings,
            mesh,
            cached_normals,
            layer_group_names,
            layer_dry: visible_layers.iter().map(|l| l.dry).collect(),
            layer_hashes,
            layer_path_hashes,
            cached_layers,
            cached_paths,
        });
        if !is_preview {
            self.state.generation_snapshot = Some(state::generation_state_hash(
                &self.state.project.layers,
                &self.state.project.settings,
                self.state.mesh_hash,
            ));
        }
    }

    pub(super) fn apply_generation_result(
        &mut self,
        ctx: &egui::Context,
        mut result: generation::GenResult,
    ) {
        let is_preview = self.state.generation.is_preview;
        let r = result.resolution;
        // Upload pre-converted images — take ownership to avoid 64MB clone.
        let empty_img = egui::ColorImage::new([0, 0], vec![]);
        self.state.textures.color = Some(ctx.load_texture(
            "gen_color",
            std::mem::replace(&mut result.display_color, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.height = Some(ctx.load_texture(
            "gen_height",
            std::mem::replace(&mut result.display_height, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.normal = Some(ctx.load_texture(
            "gen_normal",
            std::mem::replace(&mut result.display_normal, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.stroke_id = Some(ctx.load_texture(
            "gen_stroke_id",
            std::mem::replace(&mut result.display_stroke_id, empty_img),
            egui::TextureOptions::LINEAR,
        ));

        // Upload color, normal, and time textures to 3D preview
        if let Some(ref rs) = self.render_state {
            if self.state.mesh_preview.gpu_ready && self.state.mesh_preview.show_result() {
                mesh_preview::upload_color_texture_raw(rs, &result.gpu_color_pixels, r as usize);
                mesh_preview::upload_normal_texture_raw(rs, &result.gpu_normal_pixels, r as usize);
                let lh =
                    collect_layer_refs(&result.rendered_layers, &self.state.generation.layer_cache);
                let (sc, lc, ng) = mesh_preview::upload_time_texture(
                    rs,
                    &lh,
                    result.resolution,
                    self.state.mesh_preview.draw_order,
                    self.state.mesh_preview.chunk_size,
                );
                self.state.mesh_preview.stroke_count = sc;
                self.state.mesh_preview.layer_count = lc;
                self.state.mesh_preview.num_groups = ng;
            }
        }

        // Cache mesh normals computed on the worker thread
        if let Some(normals) = &result.computed_normals {
            self.state.cached_mesh_normals = Some((normals.0, std::sync::Arc::clone(&normals.1)));
        }

        // Path cache is resolution-independent — always update (move, not clone)
        if !result.rendered_paths.is_empty() {
            self.state.generation.path_cache = std::mem::take(&mut result.rendered_paths);
        }

        if is_preview {
            // Preview: display result but don't update layer cache (preserve full-res cache).
            // Advance to the next progressive resolution step.
            self.state.generated = Some(result);

            // Pop the next step from the progressive queue.
            if let Some(next_res) = self.state.generation.progressive_queue.first().copied() {
                self.state.generation.progressive_queue.remove(0);
                let full = self.state.project.settings.resolution_preset.resolution();
                let is_final = next_res >= full;
                self.start_generation_at_resolution(next_res, !is_final);
            } else {
                // Queue empty (e.g. manual Cmd+G or direct call) — fall back to full-res.
                self.start_generation();
            }
        } else {
            // Full-res: update layer cache for future reuse (move, not clone).
            self.state.generation.layer_cache = std::mem::take(&mut result.rendered_layers);
            self.state.generation.cache_global_hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                result.resolution.hash(&mut h);
                self.state.mesh_hash.hash(&mut h);
                h.finish()
            };
            let total_elapsed = self
                .state
                .generation
                .start_time
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(result.elapsed.as_secs_f32());
            self.state.status_message = format!("Generated in {:.1}s", total_elapsed);
            self.state.generation.start_time = None;
            self.state.generated = Some(result);
        }

        // If Type A settings changed while generation was running, remerge with current values
        let s = &self.state.project.settings;
        if let Some(ref gen) = self.state.generated {
            if gen.gen_normal_strength != s.normal_strength
                || gen.gen_normal_mode != s.normal_mode
                || gen.gen_background_mode != s.background_mode
            {
                self.state.pending_remerge = true;
            }
        }
    }

    /// Used when only visibility, order, or dry changes — no re-rendering needed.
    /// Runs the merge → Sobel → normal map → UDN pipeline using cached per-layer
    /// render results. Skips silently if the cache is empty or incomplete.
    pub(super) fn start_remerge(&mut self) {
        let cache = &self.state.generation.layer_cache;
        if cache.is_empty() {
            return;
        }

        self.remerge_worker.start(generation::RemergeInput {
            layer_cache: cache.clone(),
            settings: self.state.project.settings.clone(),
            layers: self.state.project.layers.clone(),
            mesh: self.state.loaded_mesh.clone(),
            cached_normals: self.state.cached_mesh_normals.clone(),
            rendered_paths: self.state.generation.path_cache.clone(),
        });
    }

    pub(super) fn apply_remerge_result(
        &mut self,
        ctx: &egui::Context,
        mut result: generation::RemergeResult,
    ) {
        let r = result.resolution;

        // Update textures — take ownership to avoid large clones.
        let empty_img = egui::ColorImage::new([0, 0], vec![]);
        self.state.textures.color = Some(ctx.load_texture(
            "remerge_color",
            std::mem::replace(&mut result.display_color, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.height = Some(ctx.load_texture(
            "remerge_height",
            std::mem::replace(&mut result.display_height, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.normal = Some(ctx.load_texture(
            "remerge_normal",
            std::mem::replace(&mut result.display_normal, empty_img.clone()),
            egui::TextureOptions::LINEAR,
        ));
        self.state.textures.stroke_id = Some(ctx.load_texture(
            "remerge_stroke_id",
            std::mem::replace(&mut result.display_stroke_id, empty_img),
            egui::TextureOptions::LINEAR,
        ));

        // Upload to 3D preview
        if let Some(ref rs) = self.render_state {
            if self.state.mesh_preview.gpu_ready && self.state.mesh_preview.show_result() {
                mesh_preview::upload_color_texture_raw(rs, &result.gpu_color_pixels, r as usize);
                mesh_preview::upload_normal_texture_raw(rs, &result.gpu_normal_pixels, r as usize);
                let lh =
                    collect_layer_refs(&result.rendered_layers, &self.state.generation.layer_cache);
                let (sc, lc, ng) = mesh_preview::upload_time_texture(
                    rs,
                    &lh,
                    r,
                    self.state.mesh_preview.draw_order,
                    self.state.mesh_preview.chunk_size,
                );
                self.state.mesh_preview.stroke_count = sc;
                self.state.mesh_preview.layer_count = lc;
                self.state.mesh_preview.num_groups = ng;
            }
        }

        // Update stored result
        self.state.generated = Some(generation::GenResult {
            color: result.color,
            height: result.height,
            normal_map: result.normal_map,
            stroke_id: result.stroke_id,
            stroke_time_order: result.stroke_time_order,
            stroke_time_arc: result.stroke_time_arc,
            resolution: r,
            elapsed: std::time::Duration::ZERO,
            computed_normals: None,
            rendered_layers: result.rendered_layers,
            rendered_paths: result.rendered_paths,
            gen_normal_strength: result.gen_normal_strength,
            gen_normal_mode: result.gen_normal_mode,
            gen_background_mode: result.gen_background_mode,
            display_color: result.display_color,
            display_height: result.display_height,
            display_normal: result.display_normal,
            display_stroke_id: result.display_stroke_id,
            gpu_color_pixels: result.gpu_color_pixels,
            gpu_normal_pixels: result.gpu_normal_pixels,
        });

        // Output now matches current project state
        self.state.generation_snapshot = Some(state::generation_state_hash(
            &self.state.project.layers,
            &self.state.project.settings,
            self.state.mesh_hash,
        ));

        // Ensure the updated textures are displayed in the next frame
        ctx.request_repaint();
    }

    /// Initialize or update 3D preview GPU resources after mesh load.
    pub(super) fn init_mesh_preview_no_fit(&mut self) {
        self.init_mesh_preview_inner(false);
    }

    pub(super) fn init_mesh_preview(&mut self) {
        self.init_mesh_preview_inner(true);
    }

    fn init_mesh_preview_inner(&mut self, fit_camera: bool) {
        let Some(ref rs) = self.render_state else {
            return;
        };
        let Some(ref mesh) = self.state.loaded_mesh else {
            return;
        };

        if !self.state.mesh_preview.gpu_ready {
            mesh_preview::init_gpu_resources(rs, mesh);
            self.state.mesh_preview.gpu_ready = true;
        } else {
            mesh_preview::upload_mesh(rs, mesh);
        }
        if fit_camera {
            self.state.mesh_preview.fit_to_mesh(mesh);
        }

        // Sync GPU textures with current generation state
        if self.state.mesh_preview.show_result() {
            if let Some(ref gen) = self.state.generated {
                mesh_preview::upload_color_texture(rs, &gen.color, gen.resolution as usize);
                mesh_preview::upload_normal_texture(rs, &gen.normal_map, gen.resolution as usize);
                let lh =
                    collect_layer_refs(&gen.rendered_layers, &self.state.generation.layer_cache);
                let (sc, lc, ng) = mesh_preview::upload_time_texture(
                    rs,
                    &lh,
                    gen.resolution,
                    self.state.mesh_preview.draw_order,
                    self.state.mesh_preview.chunk_size,
                );
                self.state.mesh_preview.stroke_count = sc;
                self.state.mesh_preview.layer_count = lc;
                self.state.mesh_preview.num_groups = ng;
            } else {
                self.upload_base_only_to_3d();
            }
        } else {
            self.upload_base_only_to_3d();
        }
    }

    /// Auto-preview debounce and remerge polling.
    pub(super) fn auto_preview_tick(&mut self, ctx: &egui::Context, project_replacing: bool) {
        if self.state.modal_dialog_active {
            return;
        }
        if !project_replacing && self.state.loaded_mesh.is_some() {
            let current_render_hash = self.combined_render_hash();
            if current_render_hash != self.state.prev_render_hash
                && self.state.prev_render_hash != 0
            {
                self.state.auto_preview_timer = Some(std::time::Instant::now());
                self.state.auto_gen_suppressed = false;
            }
            self.state.prev_render_hash = current_render_hash;

            // Auto-trigger first generation when mesh is loaded but nothing generated yet
            if self.state.generated.is_none()
                && !self.state.generation.is_running()
                && !self.state.auto_gen_suppressed
                && self.state.auto_preview_timer.is_none()
            {
                self.start_preview_generation();
            }

            // Debounce → start preview
            if let Some(timer) = self.state.auto_preview_timer {
                let elapsed = timer.elapsed();
                if elapsed >= Self::PREVIEW_DEBOUNCE {
                    self.start_preview_generation();
                    self.state.auto_preview_timer = None;
                } else {
                    ctx.request_repaint();
                }
            }
        }
    }
}
