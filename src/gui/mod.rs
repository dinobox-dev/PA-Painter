pub mod dialogs;
pub mod generation;
pub mod guide_editor;
pub mod mesh_preview;
pub mod preview;
pub mod sidebar;
pub mod slot_editor;
pub mod state;
pub mod textures;
pub mod undo;
pub mod update_check;
pub mod viewport;
pub mod widgets;

use eframe::egui;
use eframe::egui_wgpu;
use state::{AppState, GuideTool, UnsavedAction};

use pa_painter::compositing::{
    fill_base_color_region, resolve_base_color, resolve_base_normal, GlobalMaps,
};
use pa_painter::output::{blend_normals_udn, ExportFormat};
use pa_painter::types::{
    BaseColorSource, Color, NormalYConvention, TextureSource, BASE_RESOLUTION,
};
use pa_painter::uv_mask::UvMask;

/// Extract per-layer LayerMaps references (in compositing order).
/// Prefers `rendered_layers` from GenResult; falls back to `layer_cache` from GenerationManager
/// (full-res generation moves rendered_layers into layer_cache for reuse).
fn collect_layer_refs<'a>(
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

/// Main GUI application.
pub struct PainterApp {
    state: AppState,
    checkerboard: Option<egui::TextureHandle>,
    render_state: Option<egui_wgpu::RenderState>,
    /// Previous frame's result_mode for toggle change detection.
    prev_result_mode: state::ResultMode,
    /// Previous frame's draw_order for change detection (re-upload time texture).
    prev_draw_order: state::DrawOrder,
    prev_chunk_size: u32,
    /// Hash of base texture state for 3D preview invalidation when show_result is off.
    prev_base_tex_hash: u64,
    /// Background remerge worker.
    remerge_worker: generation::RemergeWorker,
    /// Previous show_direction_field state for toggle change detection.
    prev_show_direction_field: bool,
    /// Hash of guide state for direction field overlay invalidation.
    prev_direction_field_hash: u64,
    /// Background update checker.
    update_checker: update_check::UpdateChecker,
    /// Whether the user dismissed the update banner.
    update_dismissed: bool,
}

impl PainterApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Register Phosphor icon font
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Fill);
        cc.egui_ctx.set_fonts(fonts);

        Self {
            state: AppState::new(),
            checkerboard: None,
            render_state: cc.wgpu_render_state.clone(),
            prev_result_mode: state::ResultMode::Paint,
            prev_draw_order: state::DrawOrder::Sequential,
            prev_chunk_size: 1,
            prev_base_tex_hash: 0,
            remerge_worker: generation::RemergeWorker::default(),
            prev_show_direction_field: false,
            prev_direction_field_hash: 0,
            update_checker: update_check::UpdateChecker::spawn(),
            update_dismissed: false,
        }
    }

    /// Compute a combined hash of all layer render hashes + mesh hash.
    /// Used to detect Type C/D parameter changes between frames.
    fn combined_render_hash(&self) -> u64 {
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
    fn start_generation(&mut self) {
        self.state.auto_preview_timer = None;
        self.state.generation.progressive_queue.clear();
        self.state.generation.progressive_total = 1;
        self.start_generation_at_resolution(
            self.state.project.settings.resolution_preset.resolution(),
            false,
        );
    }

    fn start_preview_generation(&mut self) {
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

    fn apply_generation_result(&mut self, ctx: &egui::Context, mut result: generation::GenResult) {
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

    /// Hash of visible layers' base texture state (color, normal, group, order, visibility).
    fn base_texture_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for layer in &self.state.project.layers {
            layer.visible.hash(&mut h);
            if layer.visible {
                layer.order.hash(&mut h);
                layer.group_name.hash(&mut h);
                if let Ok(bytes) = serde_json::to_vec(&(&layer.base_color, &layer.base_normal)) {
                    bytes.hash(&mut h);
                }
            }
        }
        h.finish()
    }

    /// Hash of all visible layers' guides (for direction field overlay invalidation).
    fn direction_field_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for layer in &self.state.project.layers {
            if layer.visible {
                layer.guides.len().hash(&mut h);
                for g in &layer.guides {
                    g.guide_type.hash(&mut h);
                    g.position.x.to_bits().hash(&mut h);
                    g.position.y.to_bits().hash(&mut h);
                    g.direction.x.to_bits().hash(&mut h);
                    g.direction.y.to_bits().hash(&mut h);
                    g.influence.to_bits().hash(&mut h);
                    g.strength.to_bits().hash(&mut h);
                }
            }
        }
        h.finish()
    }

    /// Render and upload the direction field overlay from all visible layers' guides.
    fn upload_direction_field_overlay(&self, render_state: &egui_wgpu::RenderState) {
        let all_guides: Vec<pa_painter::types::Guide> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .flat_map(|l| l.guides.iter().cloned())
            .collect();

        let resolution = 512u32;
        let arrow_spacing = 32u32;
        let pixels = pa_painter::direction_field::render_direction_field_overlay(
            &all_guides,
            resolution,
            arrow_spacing,
        );
        mesh_preview::upload_overlay_texture(render_state, &pixels, resolution);
    }

    /// Upload base-only textures to the 3D preview (no stroke results).
    /// Composites visible layers' base color and base normal textures.
    fn upload_base_only_to_3d(&self) {
        let Some(ref rs) = self.render_state else {
            return;
        };
        if !self.state.mesh_preview.gpu_ready {
            return;
        }

        let settings = &self.state.project.settings;
        let resolution = settings.resolution_preset.resolution();
        let materials = self
            .state
            .loaded_mesh
            .as_ref()
            .map(|m| m.materials.as_slice())
            .unwrap_or(&[]);

        // Collect visible layers sorted by order
        let mut sorted_layers: Vec<&pa_painter::types::Layer> = self
            .state
            .project
            .layers
            .iter()
            .filter(|l| l.visible)
            .collect();
        sorted_layers.sort_by_key(|l| l.order);

        // Build UV masks
        let masks: Vec<Option<UvMask>> = if let Some(ref mesh) = self.state.loaded_mesh {
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

        // Fill base colors
        let default_base = BaseColorSource::solid(Color::WHITE);
        let mut global = GlobalMaps::new(
            resolution,
            &default_base,
            settings.normal_mode,
            settings.background_mode,
        );
        for (si, layer) in sorted_layers.iter().enumerate() {
            let bc = resolve_base_color(&layer.base_color, materials);
            let src = bc.as_source();
            fill_base_color_region(&mut global, &src, mask_refs[si]);
        }

        // Base normal: flat normal + UDN blending
        let mut normal_map = vec![[0.5_f32, 0.5, 1.0]; (resolution * resolution) as usize];
        for (si, layer) in sorted_layers.iter().enumerate() {
            let bn = resolve_base_normal(&layer.base_normal, materials);
            if let Some(ref pixels) = bn.pixels {
                blend_normals_udn(
                    &mut normal_map,
                    pixels,
                    bn.width,
                    bn.height,
                    resolution,
                    mask_refs[si],
                );
            }
        }

        mesh_preview::upload_color_texture(rs, &global.color, resolution as usize);
        mesh_preview::upload_normal_texture(rs, &normal_map, resolution as usize);
    }

    /// Used when only visibility, order, or dry changes — no re-rendering needed.
    /// Runs the merge → Sobel → normal map → UDN pipeline using cached per-layer
    /// render results. Skips silently if the cache is empty or incomplete.
    fn start_remerge(&mut self) {
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

    fn apply_remerge_result(&mut self, ctx: &egui::Context, mut result: generation::RemergeResult) {
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
    fn init_mesh_preview_no_fit(&mut self) {
        self.init_mesh_preview_inner(false);
    }

    fn init_mesh_preview(&mut self) {
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

    fn show_export_settings_window(ctx: &egui::Context, state: &mut AppState) {
        if !state.show_export_settings {
            return;
        }

        // Ensure draft exists (defensive — normally created when opening)
        if state.export_settings_draft.is_none() {
            state.export_settings_draft = Some(state.project.export_settings.clone());
        }

        let mut action: Option<bool> = None; // Some(true) = save, Some(false) = cancel
        let weak = ctx.style().visuals.weak_text_color();

        let frame = egui::Frame {
            inner_margin: egui::Margin::same(16),
            outer_margin: egui::Margin::ZERO,
            corner_radius: egui::CornerRadius::same(8),
            shadow: egui::Shadow {
                offset: [0, 4],
                blur: 16,
                spread: 4,
                color: egui::Color32::from_black_alpha(80),
            },
            fill: ctx.style().visuals.window_fill,
            stroke: egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
        };

        egui::Window::new("export_settings")
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .min_width(260.0)
            .max_width(260.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(frame)
            .show(ctx, |ui: &mut egui::Ui| {
                let es = state.export_settings_draft.as_mut().unwrap();
                ui.spacing_mut().item_spacing.y = 4.0;

                // ── Header ──
                ui.vertical_centered(|ui: &mut egui::Ui| {
                    ui.strong(egui::RichText::new("Export Settings").size(15.0));
                });
                ui.add_space(8.0);

                // ── Texture Maps ──
                ui.checkbox(&mut es.export_maps, "Texture Maps");
                if es.export_maps {
                    ui.indent("maps_indent", |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;

                        ui.horizontal(|ui: &mut egui::Ui| {
                            ui.colored_label(weak, "Format");
                            ui.selectable_value(&mut es.format, ExportFormat::Png, "PNG");
                            ui.selectable_value(&mut es.format, ExportFormat::Exr, "EXR");
                        });
                        ui.add_space(2.0);
                        ui.checkbox(&mut es.include_color, "Color");
                        ui.checkbox(&mut es.include_normal, "Normal");
                        ui.checkbox(&mut es.include_height, "Height");
                        ui.checkbox(&mut es.include_stroke_id, "Stroke ID");
                        ui.checkbox(&mut es.include_time_map, "Stroke Time");
                        ui.add_space(4.0);
                        ui.checkbox(&mut es.per_layer, "Per Layer");
                    });
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // ── 3D Model ──
                ui.checkbox(&mut es.export_model, "3D Model");
                if es.export_model {
                    ui.indent("model_indent", |ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.y = 3.0;
                        ui.colored_label(weak, "Format: GLB");
                        ui.add_space(2.0);
                        ui.checkbox(&mut es.embed_color, "Embed color texture");
                        ui.checkbox(&mut es.embed_normal, "Embed normal map");
                    });
                }

                // ── Normal Y Convention ──
                // Show when any normal output is enabled (texture or GLB).
                let any_normal =
                    (es.export_maps && es.include_normal) || (es.export_model && es.embed_normal);
                if any_normal {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.colored_label(weak, "Normal Y axis");
                        ui.selectable_value(&mut es.normal_y, NormalYConvention::OpenGL, "OpenGL");
                        ui.selectable_value(
                            &mut es.normal_y,
                            NormalYConvention::DirectX,
                            "DirectX",
                        );
                    });
                }

                // ── Cancel / Save ──
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);
                ui.horizontal(|ui: &mut egui::Ui| {
                    let btn_w = 80.0_f32;
                    let gap = 12.0_f32;
                    ui.spacing_mut().item_spacing.x = gap;
                    let total = btn_w * 2.0 + gap;
                    let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                    ui.add_space(pad);
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                        .clicked()
                    {
                        action = Some(false);
                    }
                    let accent = egui::Color32::from_rgb(45, 120, 220);
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Save").color(egui::Color32::WHITE),
                            )
                            .fill(accent)
                            .min_size(egui::Vec2::new(btn_w, 28.0)),
                        )
                        .clicked()
                    {
                        action = Some(true);
                    }
                });
            });

        match action {
            Some(true) => {
                if let Some(draft) = state.export_settings_draft.take() {
                    state.project.export_settings = draft;
                    state.dirty = true;
                }
                state.show_export_settings = false;
            }
            Some(false) => {
                state.export_settings_draft = None;
                state.show_export_settings = false;
            }
            None => {}
        }
    }

    /// Show the mesh-load confirmation popup and handle OK / Cancel.
    fn show_mesh_load_popup(&mut self, ctx: &egui::Context) {
        let mut apply_popup = false;
        let mut dismiss_popup = false;
        if let Some(ref mut popup) = self.state.mesh_load_popup {
            use crate::gui::sidebar::fmt_thousands;

            let popup_frame = egui::Frame {
                inner_margin: egui::Margin::same(16),
                outer_margin: egui::Margin::ZERO,
                corner_radius: egui::CornerRadius::same(8),
                shadow: egui::Shadow {
                    offset: [0, 4],
                    blur: 16,
                    spread: 4,
                    color: egui::Color32::from_black_alpha(80),
                },
                fill: ctx.style().visuals.window_fill,
                stroke: egui::Stroke::new(1.0, egui::Color32::from_gray(60)),
            };

            egui::Window::new("mesh_load_popup")
                .title_bar(false)
                .collapsible(false)
                .resizable(false)
                .min_width(440.0)
                .max_width(440.0)
                .max_height(420.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(popup_frame)
                .show(ctx, |ui: &mut egui::Ui| {
                    let weak = ui.visuals().weak_text_color();

                    // ── Header ──
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.strong(
                            egui::RichText::new(if popup.is_replace {
                                "Replace Mesh"
                            } else {
                                "New Project"
                            })
                            .size(15.0),
                        );
                    });
                    ui.add_space(6.0);

                    // ── Mesh Info Grid ──
                    egui::Grid::new("mesh_info_grid")
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui: &mut egui::Ui| {
                            ui.colored_label(weak, "File");
                            ui.label(&popup.filename);
                            ui.end_row();

                            ui.colored_label(weak, "Vertices");
                            ui.label(fmt_thousands(popup.vertices));
                            ui.end_row();

                            ui.colored_label(weak, "Triangles");
                            ui.label(fmt_thousands(popup.triangles));
                            ui.end_row();

                            ui.colored_label(weak, "Groups");
                            ui.label(popup.groups.to_string());
                            ui.end_row();

                            if popup.n_textures > 0 || popup.n_normals > 0 {
                                ui.colored_label(weak, "Textures");
                                ui.label(format!(
                                    "{} color, {} normal",
                                    popup.n_textures, popup.n_normals,
                                ));
                                ui.end_row();
                            }
                        });

                    // MTL toggle (OBJ only)
                    if popup.has_mtl {
                        ui.add_space(6.0);
                        ui.checkbox(&mut popup.use_mtl, "Use MTL materials");
                    }

                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(4.0);

                    // ── Layer Mapping ──
                    let mappings = if popup.has_mtl && !popup.use_mtl {
                        &popup.mappings_no_mtl
                    } else {
                        &popup.mappings
                    };

                    ui.strong(egui::RichText::new("Import Layers").size(12.0));
                    ui.add_space(4.0);

                    let col_color = 110.0_f32;
                    let col_normal = 110.0_f32;

                    // Header row (outside scroll area)
                    ui.horizontal(|ui: &mut egui::Ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        // Select-all checkbox
                        let all_on = popup.layer_enabled.iter().all(|&e| e);
                        let any_on = popup.layer_enabled.iter().any(|&e| e);
                        let mut toggle = all_on;
                        let resp = ui.checkbox(&mut toggle, "");
                        if !all_on && any_on {
                            let center = resp.rect.center();
                            ui.painter().line_segment(
                                [
                                    egui::pos2(center.x - 4.0, center.y),
                                    egui::pos2(center.x + 4.0, center.y),
                                ],
                                egui::Stroke::new(2.0, ui.visuals().text_color()),
                            );
                        }
                        if resp.changed() {
                            for e in &mut popup.layer_enabled {
                                *e = toggle;
                            }
                        }
                        ui.colored_label(weak, "Layer");

                        // Right-pinned Color / Normal headers (centered in slot)
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui: &mut egui::Ui| {
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_normal, ui.available_height()),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui: &mut egui::Ui| {
                                        ui.colored_label(weak, "Normal");
                                    },
                                );
                                ui.allocate_ui_with_layout(
                                    egui::vec2(col_color, ui.available_height()),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui: &mut egui::Ui| {
                                        ui.colored_label(weak, "Color");
                                    },
                                );
                            },
                        );
                    });
                    ui.separator();

                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .auto_shrink(false)
                        .show(ui, |ui: &mut egui::Ui| {
                            for (i, lm) in mappings.iter().enumerate() {
                                let row_rect = egui::Rect::from_min_size(
                                    ui.cursor().min,
                                    egui::vec2(ui.available_width(), 22.0),
                                );
                                if i % 2 == 0 {
                                    ui.painter().rect_filled(
                                        row_rect,
                                        0.0,
                                        ui.visuals().faint_bg_color,
                                    );
                                }

                                let enabled = popup.layer_enabled.get(i).copied().unwrap_or(true);
                                let text_color = if enabled {
                                    ui.visuals().text_color()
                                } else {
                                    weak
                                };

                                ui.horizontal(|ui: &mut egui::Ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;

                                    // Checkbox + layer name
                                    if i < popup.layer_enabled.len() {
                                        ui.checkbox(&mut popup.layer_enabled[i], "");
                                    }
                                    if enabled {
                                        ui.strong(&lm.name);
                                    } else {
                                        ui.colored_label(weak, &lm.name);
                                    }

                                    // Color + Normal pinned to right
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui: &mut egui::Ui| {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_normal, ui.available_height()),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui: &mut egui::Ui| {
                                                    source_label_with_chip(
                                                        ui,
                                                        &lm.base_normal,
                                                        text_color,
                                                        weak,
                                                        lm.is_default,
                                                    );
                                                },
                                            );
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(col_color, ui.available_height()),
                                                egui::Layout::left_to_right(egui::Align::Center),
                                                |ui: &mut egui::Ui| {
                                                    source_label_with_chip(
                                                        ui,
                                                        &lm.base_color,
                                                        text_color,
                                                        weak,
                                                        lm.is_default,
                                                    );
                                                },
                                            );
                                        },
                                    );
                                });
                            }
                        });

                    ui.add_space(12.0);

                    // ── Buttons (centered) ──
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            dismiss_popup = true;
                        }
                        let accent = egui::Color32::from_rgb(45, 120, 220);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("  OK  ").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            apply_popup = true;
                        }
                    });
                });
        }
        if apply_popup {
            dialogs::apply_mesh_load_popup(&mut self.state);
            self.state.mesh_load_popup = None;
            // Post-load cleanup (deferred until confirmation)
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if dismiss_popup {
            self.state.mesh_load_popup = None;
        }
    }
}

impl eframe::App for PainterApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept window close when there are unsaved changes.
        if ctx.input(|i| i.viewport().close_requested()) && self.state.dirty {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.state.unsaved_confirm = Some(UnsavedAction::Quit);
        }

        // On macOS, rfd dialogs pump the event loop and can re-enter update().
        // Absorb all input so the UI renders (keeping egui state consistent)
        // but nothing is interactive.
        if self.state.modal_dialog_active {
            egui::Area::new(egui::Id::new("modal_input_blocker"))
                .fixed_pos(egui::Pos2::ZERO)
                .order(egui::Order::Foreground)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let size = ctx.content_rect().size();
                    ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                });
        }

        // Repaint while the pointer is over the window so hover reactions are instant.
        if ctx.input(|i| i.pointer.has_pointer()) {
            ctx.request_repaint();
        }
        self.init_lazy(ctx);
        self.handle_keyboard(ctx);

        // Capture pre-frame snapshot AFTER undo/redo so the restore itself
        // is invisible to the change tracker.
        let pre_frame = self.state.take_snapshot();
        // Project-replacing actions explicitly set dirty=false; skip auto-dirty for those frames.
        let project_replacing = self.state.pending_open || self.state.pending_new;

        self.dispatch_deferred(ctx);
        self.sync_gpu_textures();
        self.poll_workers(ctx);

        // ── UI panels (order matters for egui layout) ──
        self.show_menu_bar(ctx);
        self.show_update_banner(ctx);
        self.show_status_bar(ctx);
        self.show_sidebars(ctx);
        self.show_central_panel(ctx);
        self.show_mesh_load_popup(ctx);
        self.show_auxiliary_windows(ctx);

        self.auto_preview_tick(ctx, project_replacing);

        // ── Undo: track post-frame changes ──
        let post_frame = self.state.take_snapshot();
        if pre_frame != post_frame && !project_replacing {
            self.state.dirty = true;
        }
        let pointer_down = ctx.input(|i| i.pointer.any_down());
        self.state
            .undo
            .track_frame(&pre_frame, &post_frame, pointer_down);
    }
}

// ── update() sub-methods ──

impl PainterApp {
    /// One-time lazy initialization (checkerboard texture, etc.).
    fn init_lazy(&mut self, ctx: &egui::Context) {
        ctx.style_mut(|s| s.spacing.scroll.dormant_handle_opacity = 0.4);
        if self.checkerboard.is_none() {
            self.checkerboard = Some(viewport::make_checkerboard(ctx));
        }
        if self.state.textures.base_texture.is_none() {
            self.state.textures.base_texture = self.checkerboard.clone();
        }
    }

    /// Global keyboard shortcuts (undo/redo, save, generate, tab cycling).
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let redo_mods = egui::Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };
        let undo_mods = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        // Check redo first (more specific modifier combo) to prevent Cmd+Z from consuming it.
        if ctx.input_mut(|i| i.consume_key(redo_mods, egui::Key::Z)) {
            let current = self.state.take_snapshot();
            if let Some(snap) = self.state.undo.redo(current) {
                self.state.apply_snapshot(snap);
            }
        } else if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::Z)) {
            let current = self.state.take_snapshot();
            if let Some(snap) = self.state.undo.undo(current) {
                self.state.apply_snapshot(snap);
            }
        }

        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::S)) {
            self.state.pending_save = true;
        }
        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::G))
            && !self.state.generation.is_running()
            && !self.state.modal_dialog_active
        {
            self.start_generation();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backtick)) {
            self.state.viewport_tab = self.state.viewport_tab.next();
        }

        // Duplicate selected layer
        if !ctx.wants_keyboard_input() && ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::D))
        {
            if let Some(idx) = self.state.selected_layer {
                let mut cloned = self.state.project.layers[idx].clone();
                cloned.name = format!("{} copy", cloned.name);
                cloned.seed = self.state.project.layers.len() as u32;
                self.state.project.layers.insert(idx, cloned);
                self.state.selected_layer = Some(idx);
                self.state.selected_guide = None;
                let n = self.state.project.layers.len() as i32;
                for (i, layer) in self.state.project.layers.iter_mut().enumerate() {
                    layer.order = n - 1 - i as i32;
                }
                self.state.pending_remerge = true;
            }
        }

        // Delete selected layer or guide (skip if a text field has focus)
        if !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete)
                    || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)
            })
        {
            if let Some(gi) = self.state.selected_guide {
                if let Some(li) = self.state.selected_layer {
                    if gi < self.state.project.layers[li].guides.len() {
                        self.state.project.layers[li].guides.remove(gi);
                        self.state.selected_guide = None;
                    }
                }
            } else if let Some(idx) = self.state.selected_layer {
                self.state.project.layers.remove(idx);
                self.state.selected_guide = None;
                if self.state.project.layers.is_empty() {
                    self.state.selected_layer = None;
                } else {
                    self.state.selected_layer = Some(idx.min(self.state.project.layers.len() - 1));
                }
                // Re-sync order fields
                let n = self.state.project.layers.len() as i32;
                for (i, layer) in self.state.project.layers.iter_mut().enumerate() {
                    layer.order = n - 1 - i as i32;
                }
                self.state.pending_remerge = true;
            }
        }
    }

    /// Process pending_* flags set by UI widgets in the previous frame.
    fn dispatch_deferred(&mut self, ctx: &egui::Context) {
        // On macOS, rfd dialogs pump the event loop, re-entering update().
        // Skip all deferred actions while a native dialog is open.
        if self.state.modal_dialog_active {
            return;
        }
        if self.state.pending_open {
            self.state.pending_open = false;
            if self.state.dirty {
                self.state.unsaved_confirm = Some(UnsavedAction::Open);
            } else {
                self.do_open_project(ctx);
            }
        }
        if self.state.pending_open_example {
            self.state.pending_open_example = false;
            if self.state.dirty {
                self.state.unsaved_confirm = Some(UnsavedAction::OpenExample);
            } else {
                self.do_open_example();
            }
        }
        if self.state.pending_save {
            self.state.pending_save = false;
            dialogs::save_project_action(&mut self.state);
        }
        if self.state.pending_export {
            self.state.pending_export = false;
            let es = &self.state.project.export_settings;
            let do_maps = es.export_maps;
            let do_model = es.export_model;
            match (do_maps, do_model) {
                (true, true) => dialogs::export_both(&mut self.state),
                (true, false) => dialogs::export_maps(&mut self.state),
                (false, true) => dialogs::export_glb(&mut self.state),
                (false, false) => {
                    self.state.status_message =
                        "Nothing selected — enable Texture Maps or 3D Model in export settings"
                            .to_string();
                }
            }
        }
        if self.state.pending_new {
            self.state.pending_new = false;
            if self.state.dirty {
                self.state.unsaved_confirm = Some(UnsavedAction::New);
            } else {
                self.do_new_project(ctx);
            }
        }
        if self.state.pending_reload_mesh {
            self.state.pending_reload_mesh = false;
            dialogs::reload_mesh(&mut self.state);
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_replace_mesh {
            self.state.pending_replace_mesh = false;
            dialogs::replace_mesh(&mut self.state);
        }
        if self.state.pending_remerge {
            self.state.pending_remerge = false;
            self.start_remerge(); // cancels any in-flight remerge automatically
        }
    }

    /// Synchronise GPU textures for the 3D preview (result mode, direction field overlay).
    fn sync_gpu_textures(&mut self) {
        // ── Result mode: re-upload when mode/draw_order/chunk_size changes ──
        let mode = self.state.mesh_preview.result_mode;
        let show = self.state.mesh_preview.show_result();
        if mode != self.prev_result_mode {
            self.prev_result_mode = mode;
            if show {
                if let Some(ref rs) = self.render_state {
                    if self.state.mesh_preview.gpu_ready {
                        if let Some(ref gen) = self.state.generated {
                            mesh_preview::upload_color_texture_raw(
                                rs,
                                &gen.gpu_color_pixels,
                                gen.resolution as usize,
                            );
                            mesh_preview::upload_normal_texture_raw(
                                rs,
                                &gen.gpu_normal_pixels,
                                gen.resolution as usize,
                            );
                            let lh = collect_layer_refs(
                                &gen.rendered_layers,
                                &self.state.generation.layer_cache,
                            );
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
                        }
                    }
                }
            } else {
                self.upload_base_only_to_3d();
                self.prev_base_tex_hash = self.base_texture_hash();
            }
        }

        // When show_result is off, detect base texture changes
        if !show {
            let h = self.base_texture_hash();
            if h != self.prev_base_tex_hash {
                self.prev_base_tex_hash = h;
                self.upload_base_only_to_3d();
            }
        }

        // Re-upload time texture when draw_order or chunk_size changes
        let cur_order = self.state.mesh_preview.draw_order;
        let cur_chunk = self.state.mesh_preview.chunk_size;
        if cur_order != self.prev_draw_order || cur_chunk != self.prev_chunk_size {
            self.prev_draw_order = cur_order;
            self.prev_chunk_size = cur_chunk;
            if mode == state::ResultMode::Drawing {
                if let Some(ref rs) = self.render_state {
                    if self.state.mesh_preview.gpu_ready {
                        if let Some(ref gen) = self.state.generated {
                            let lh = collect_layer_refs(
                                &gen.rendered_layers,
                                &self.state.generation.layer_cache,
                            );
                            let (sc, lc, ng) = mesh_preview::upload_time_texture(
                                rs,
                                &lh,
                                gen.resolution,
                                cur_order,
                                self.state.mesh_preview.chunk_size,
                            );
                            self.state.mesh_preview.stroke_count = sc;
                            self.state.mesh_preview.layer_count = lc;
                            self.state.mesh_preview.num_groups = ng;
                        }
                    }
                }
            }
        }

        // ── Direction field overlay: sync with toggle + guide changes ──
        // Clone render_state (cheap Arc clone) to avoid borrowing self during
        // upload_direction_field_overlay.
        if let Some(rs) = self.render_state.clone() {
            if self.state.mesh_preview.gpu_ready {
                let show_df = self.state.mesh_preview.show_direction_field;
                let df_hash = self.direction_field_hash();

                if show_df != self.prev_show_direction_field {
                    self.prev_show_direction_field = show_df;
                    if show_df {
                        self.upload_direction_field_overlay(&rs);
                        self.prev_direction_field_hash = df_hash;
                    } else {
                        mesh_preview::clear_overlay_texture(&rs);
                    }
                } else if show_df && df_hash != self.prev_direction_field_hash {
                    self.prev_direction_field_hash = df_hash;
                    self.upload_direction_field_overlay(&rs);
                }
            }
        }
    }

    /// Poll background workers (path overlay, generation, remerge) and apply results.
    fn poll_workers(&mut self, ctx: &egui::Context) {
        // Skip heavy state mutations while a native dialog is blocking the main thread.
        if self.state.modal_dialog_active {
            return;
        }
        // Path overlay worker
        if let Some(poll_result) = self.state.path_worker.poll() {
            match poll_result {
                Ok(result) => {
                    if let Some(normals) = &result.computed_normals {
                        self.state.cached_mesh_normals =
                            Some((normals.0, std::sync::Arc::clone(&normals.1)));
                    }
                    self.state.path_overlay.apply_result(&result);
                }
                Err(msg) => {
                    self.state.status_message = format!("Path overlay error: {msg}");
                }
            }
        }
        // Submit new path overlay computation if cache is stale
        if self.state.viewport.path_overlay_idx.is_some() {
            if let Some(selected) = self.state.selected_layer {
                if selected < self.state.project.layers.len() {
                    let layer = &self.state.project.layers[selected];
                    if layer.visible {
                        let seed = layer.seed;

                        let stale = self
                            .state
                            .path_overlay
                            .is_stale_for_layer(selected, layer, seed);

                        if stale {
                            let needs_normal = layer.paint.normal_break_threshold.is_some();
                            let normals_stale = needs_normal
                                && self
                                    .state
                                    .cached_mesh_normals
                                    .as_ref()
                                    .is_none_or(|(r, _)| *r != BASE_RESOLUTION);

                            let input = preview::PathOverlayInput {
                                layer: layer.clone(),
                                layer_index: selected,
                                layer_count: self.state.project.layers.len(),
                                seed,
                                resolution: BASE_RESOLUTION,
                                cached_normals: if needs_normal {
                                    self.state.cached_mesh_normals.clone()
                                } else {
                                    None
                                },
                                mesh: if normals_stale {
                                    self.state.loaded_mesh.clone()
                                } else {
                                    None
                                },
                            };
                            self.state.path_overlay.set_pending(selected, layer, seed);
                            self.state.path_worker.start(input);
                        }
                    }
                }
            }
        }
        if self.state.path_worker.is_running() {
            ctx.request_repaint();
        }

        // Generation worker
        if let Some(poll_result) = self.state.generation.poll() {
            match poll_result {
                Ok(result) => self.apply_generation_result(ctx, result),
                Err(msg) => {
                    self.state.status_message = msg;
                    self.state.auto_gen_suppressed = true;
                }
            }
        }
        if self.state.generation.is_running() {
            ctx.request_repaint();
        }

        // Remerge worker
        self.state.remerge_running = self.remerge_worker.is_running();
        self.state.remerge_progress = self.remerge_worker.progress();
        if let Some(result) = self.remerge_worker.poll() {
            self.state.remerge_running = false;
            self.apply_remerge_result(ctx, result);
            if self.state.pending_remerge {
                self.state.pending_remerge = false;
                self.start_remerge();
            }
        }
        if self.state.remerge_running {
            ctx.request_repaint();
        }

        // Export worker
        if let Some(result) = self.state.export_worker.poll() {
            match result {
                Ok((count, dir)) => {
                    self.state.status_message =
                        format!("Exported {count} file(s) to {}", dir.display());
                }
                Err(msg) => {
                    self.state.status_message = msg;
                }
            }
        }
        if self.state.export_worker.is_running() {
            ctx.request_repaint();
        }
    }

    /// Colored banner below the menu bar when a newer version is available.
    fn show_update_banner(&mut self, ctx: &egui::Context) {
        self.update_checker.poll();

        if self.update_dismissed {
            return;
        }
        let Some(info) = self.update_checker.update_available() else {
            return;
        };

        let version = info.version.clone();
        let url = info.url.clone();

        let bg = egui::Color32::from_rgb(56, 152, 220);
        let bg_hover = egui::Color32::from_rgb(70, 165, 230);
        let fg = egui::Color32::from_rgb(240, 248, 255);
        let fg_dim = egui::Color32::from_rgb(200, 225, 245);

        let banner_h = 30.0;
        let text_size = 13.0;
        let icon_size = 16.0;
        let x_size = 18.0;
        let gap = 5.0;

        egui::TopBottomPanel::top("update_banner")
            .exact_height(banner_h)
            .frame(egui::Frame::new().fill(bg))
            .show(ctx, |ui| {
                let rect = ui.max_rect();
                let cy = rect.center().y;
                let p = ui.painter();

                // ── Left: icon + text + link, all vertically centered on cy ──
                let mut x = rect.left() + 12.0;

                let icon_galley = p.layout_no_wrap(
                    egui_phosphor::fill::ARROW_CIRCLE_UP.to_string(),
                    egui::FontId::proportional(icon_size),
                    fg,
                );
                p.galley(
                    egui::pos2(x, cy - icon_galley.size().y * 0.5),
                    icon_galley.clone(),
                    fg,
                );
                x += icon_galley.size().x + gap;

                let label_galley = p.layout_no_wrap(
                    format!("v{version} available"),
                    egui::FontId::proportional(text_size),
                    fg,
                );
                p.galley(
                    egui::pos2(x, cy - label_galley.size().y * 0.5),
                    label_galley.clone(),
                    fg,
                );
                x += label_galley.size().x + gap + 2.0;

                let dl_galley = p.layout_no_wrap(
                    "Download".to_string(),
                    egui::FontId::proportional(text_size),
                    fg,
                );
                let dl_pos = egui::pos2(x, cy - dl_galley.size().y * 0.5);
                let dl_rect = egui::Rect::from_min_size(dl_pos, dl_galley.size());
                p.line_segment(
                    [
                        egui::pos2(dl_rect.left(), dl_rect.bottom() - 1.0),
                        egui::pos2(dl_rect.right(), dl_rect.bottom() - 1.0),
                    ],
                    egui::Stroke::new(1.0, fg),
                );
                p.galley(dl_pos, dl_galley, fg);
                let dl_resp = ui.interact(dl_rect, ui.id().with("dl_link"), egui::Sense::click());
                if dl_resp.clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                }
                dl_resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                // ── Right: dismiss × (square, full banner height) ──
                let h = rect.height();
                let x_hit = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - h, rect.top()),
                    egui::vec2(h, h),
                );
                let x_resp = ui.interact(x_hit, ui.id().with("dismiss"), egui::Sense::click());
                if x_resp.hovered() {
                    p.rect_filled(x_hit, 0.0, bg_hover);
                }
                let x_color = if x_resp.hovered() { fg } else { fg_dim };
                p.text(
                    x_hit.center(),
                    egui::Align2::CENTER_CENTER,
                    "\u{00D7}",
                    egui::FontId::proportional(x_size),
                    x_color,
                );
                if x_resp.clicked() {
                    self.update_dismissed = true;
                }
            });
    }

    /// Top menu bar (File / Edit / View).
    /// Menu item with optional right-aligned shortcut.
    fn menu_item(
        ui: &mut egui::Ui,
        label: &str,
        shortcut: Option<egui::KeyboardShortcut>,
        enabled: bool,
    ) -> bool {
        let mut btn = egui::Button::new(label);
        if let Some(sc) = shortcut {
            btn = btn.shortcut_text(ui.ctx().format_shortcut(&sc));
        }
        ui.add_enabled(enabled, btn).clicked()
    }

    fn show_menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui: &mut egui::Ui| {
            egui::MenuBar::new().ui(ui, |ui: &mut egui::Ui| {
                ui.menu_button("File", |ui: &mut egui::Ui| {
                    use egui::{Key, KeyboardShortcut, Modifiers};
                    ui.set_min_width(200.0);
                    if Self::menu_item(ui, "New Project...", None, true) {
                        ui.close();
                        self.state.pending_new = true;
                    }
                    if Self::menu_item(ui, "Open Project...", None, true) {
                        ui.close();
                        self.state.pending_open = true;
                    }
                    if Self::menu_item(ui, "Open Example", None, true) {
                        ui.close();
                        self.state.pending_open_example = true;
                    }
                    if Self::menu_item(
                        ui,
                        "Save",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::S)),
                        true,
                    ) {
                        ui.close();
                        self.state.pending_save = true;
                    }
                    ui.separator();
                    let can_export =
                        self.state.generated.is_some() && !self.state.export_worker.is_running();
                    if Self::menu_item(ui, "Export...", None, can_export) {
                        ui.close();
                        self.state.pending_export = true;
                    }
                    ui.separator();
                    let can_gen = !self.state.generation.is_running();
                    if Self::menu_item(
                        ui,
                        "Force Full-Res",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::G)),
                        can_gen,
                    ) {
                        ui.close();
                        self.start_generation();
                    }
                });
                ui.menu_button("Edit", |ui: &mut egui::Ui| {
                    use egui::{Key, KeyboardShortcut, Modifiers};
                    ui.set_min_width(200.0);
                    let can_undo = self.state.undo.can_undo();
                    let can_redo = self.state.undo.can_redo();
                    if Self::menu_item(
                        ui,
                        "Undo",
                        Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)),
                        can_undo,
                    ) {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.undo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                    if Self::menu_item(
                        ui,
                        "Redo",
                        Some(KeyboardShortcut::new(
                            Modifiers::COMMAND | Modifiers::SHIFT,
                            Key::Z,
                        )),
                        can_redo,
                    ) {
                        ui.close();
                        let current = self.state.take_snapshot();
                        if let Some(snap) = self.state.undo.redo(current) {
                            self.state.apply_snapshot(snap);
                        }
                    }
                });
                ui.menu_button("View", |ui: &mut egui::Ui| {
                    ui.set_min_width(200.0);
                    ui.label("Theme");
                    ui.separator();
                    let mut pref = ui.ctx().options(|o| o.theme_preference);
                    let old = pref;
                    for (value, label) in [
                        (egui::ThemePreference::System, "System"),
                        (egui::ThemePreference::Dark, "Dark"),
                        (egui::ThemePreference::Light, "Light"),
                    ] {
                        let mut checked = pref == value;
                        if ui.checkbox(&mut checked, label).clicked() && checked {
                            pref = value;
                        }
                    }
                    if pref != old {
                        ui.ctx().set_theme(pref);
                        ui.close();
                    }
                });
            });
        });
    }

    /// Bottom status bar (status message, resolution, layer count).
    fn show_status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .exact_height(24.0)
            .show(ctx, |ui: &mut egui::Ui| {
                ui.horizontal(|ui: &mut egui::Ui| {
                    ui.label(&self.state.status_message);
                    ui.separator();
                    let res = self.state.project.settings.resolution_preset.resolution();
                    ui.label(format!("{}px", res));
                    ui.separator();
                    ui.label(format!("{} layers", self.state.project.layers.len()));
                });
            });
    }

    /// Left sidebar (layers, settings) and right sidebar (layer inspector).
    fn show_sidebars(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("left_panel")
            .default_width(260.0)
            .min_width(220.0)
            .max_width(400.0)
            .show(ctx, |ui: &mut egui::Ui| {
                egui::TopBottomPanel::bottom("left_bottom")
                    .frame(egui::Frame::new().inner_margin(0.0))
                    .show_separator_line(false)
                    .show_inside(ui, |ui: &mut egui::Ui| {
                        sidebar::show_bottom(ui, &mut self.state);
                    });
                sidebar::show_top(ui, &mut self.state);
                sidebar::show_layers_header(ui, &mut self.state);
                egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                    sidebar::show_layer_rows(ui, &mut self.state);
                });
            });

        if self.state.selected_layer.is_some() {
            egui::SidePanel::right("right_panel")
                .default_width(280.0)
                .min_width(240.0)
                .max_width(420.0)
                .show(ctx, |ui: &mut egui::Ui| {
                    egui::ScrollArea::vertical().show(ui, |ui: &mut egui::Ui| {
                        slot_editor::show(ui, &mut self.state);
                    });
                });
        }
    }

    /// Central panel: viewport (UV/3D) or welcome screen.
    fn show_central_panel(&mut self, ctx: &egui::Context) {
        let title = if let Some(name) = self
            .state
            .project_path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|f| f.to_string_lossy().to_string())
            .or_else(|| {
                self.state
                    .loaded_mesh
                    .is_some()
                    .then(|| "Untitled".to_string())
            }) {
            let dirty = if self.state.dirty { " *" } else { "" };
            format!("PA Painter — {name}{dirty}")
        } else {
            "PA Painter".to_string()
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        let render_state = self.render_state.clone();
        egui::CentralPanel::default().show(ctx, |ui: &mut egui::Ui| {
            let has_project = self.state.loaded_mesh.is_some() || self.state.project_path.is_some();

            if has_project {
                viewport::show(ui, &mut self.state, render_state.as_ref());
            } else {
                ui.vertical_centered(|ui: &mut egui::Ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.heading("PA Painter");
                    ui.add_space(16.0);
                    ui.label("Generate painterly texture maps from 3D meshes.");
                    ui.add_space(24.0);
                    if ui
                        .add(
                            egui::Button::new("Open Project...")
                                .min_size(egui::Vec2::new(200.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.state.pending_open = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new("New Project").min_size(egui::Vec2::new(200.0, 36.0)),
                        )
                        .clicked()
                    {
                        self.state.pending_new = true;
                    }
                });
            }
        });
    }

    /// Reload summary, export settings, and Escape key handling.
    fn show_auxiliary_windows(&mut self, ctx: &egui::Context) {
        // Reload Summary Window
        let mut dismiss_summary = false;
        if let Some(ref summary) = self.state.reload_summary {
            egui::Window::new("Mesh Reload Summary")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    if !summary.kept.is_empty() {
                        ui.label(format!("Kept: {}", summary.kept.join(", ")));
                    }
                    if !summary.added.is_empty() {
                        ui.label(format!(
                            "Added (new layers created): {}",
                            summary.added.join(", ")
                        ));
                    }
                    if !summary.orphaned.is_empty() {
                        ui.label(format!(
                            "Orphaned (remapped to __all__): {}",
                            summary.orphaned.join(", ")
                        ));
                    }
                    ui.add_space(8.0);
                    let accent = egui::Color32::from_rgb(45, 120, 220);
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("OK").color(egui::Color32::WHITE),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        dismiss_summary = true;
                    }
                });
        }
        if dismiss_summary {
            self.state.reload_summary = None;
        }

        Self::show_export_settings_window(ctx, &mut self.state);

        // Unsaved Changes Confirmation Window (modal)
        let mut unsaved_action = None;
        if self.state.unsaved_confirm.is_some() {
            let screen = ctx.content_rect();
            egui::Area::new(egui::Id::new("unsaved_dim"))
                .fixed_pos(screen.min)
                .order(egui::Order::Middle)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let rect = egui::Rect::from_min_size(screen.min, screen.size());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(80));
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                });

            egui::Window::new("Unsaved changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .order(egui::Order::Foreground)
                .fixed_size([320.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.label("Current project has unsaved changes.");
                        ui.label("Discard changes and continue?");
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            unsaved_action = Some(false);
                        }
                        let accent = egui::Color32::from_rgb(200, 80, 60);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Discard").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            unsaved_action = Some(true);
                        }
                    });
                });
        }
        match unsaved_action {
            Some(true) => {
                let action = self.state.unsaved_confirm.take();
                match action {
                    Some(UnsavedAction::Open) => self.do_open_project(ctx),
                    Some(UnsavedAction::OpenExample) => self.do_open_example(),
                    Some(UnsavedAction::New) => self.do_new_project(ctx),
                    Some(UnsavedAction::Quit) => {
                        self.state.dirty = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    None => {}
                }
            }
            Some(false) => {
                self.state.unsaved_confirm = None;
            }
            None => {}
        }

        // Export Overwrite Confirmation Window (modal)
        let mut overwrite_action = None;
        if let Some(ref confirm) = self.state.export_overwrite_confirm {
            // Dim overlay to block interaction with background UI
            let screen = ctx.content_rect();
            egui::Area::new(egui::Id::new("overwrite_dim"))
                .fixed_pos(screen.min)
                .order(egui::Order::Middle)
                .interactable(true)
                .show(ctx, |ui: &mut egui::Ui| {
                    let rect = egui::Rect::from_min_size(screen.min, screen.size());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(80));
                    ui.allocate_exact_size(screen.size(), egui::Sense::click_and_drag());
                });

            egui::Window::new("Overwrite existing files?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .order(egui::Order::Foreground)
                .fixed_size([300.0, 0.0])
                .show(ctx, |ui: &mut egui::Ui| {
                    ui.vertical_centered(|ui: &mut egui::Ui| {
                        ui.label(format!(
                            "{} file(s) will be overwritten in \"{}\".",
                            confirm.conflict_count, confirm.folder_name,
                        ));
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui: &mut egui::Ui| {
                        let btn_w = 80.0_f32;
                        let gap = 12.0_f32;
                        ui.spacing_mut().item_spacing.x = gap;
                        let total = btn_w * 2.0 + gap;
                        let pad = ((ui.available_width() - total) / 2.0).max(0.0);
                        ui.add_space(pad);
                        if ui
                            .add(egui::Button::new("Cancel").min_size(egui::Vec2::new(btn_w, 28.0)))
                            .clicked()
                        {
                            overwrite_action = Some(false);
                        }
                        let accent = egui::Color32::from_rgb(45, 120, 220);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Overwrite").color(egui::Color32::WHITE),
                                )
                                .fill(accent)
                                .min_size(egui::Vec2::new(btn_w, 28.0)),
                            )
                            .clicked()
                        {
                            overwrite_action = Some(true);
                        }
                    });
                });
        }
        match overwrite_action {
            Some(true) => dialogs::confirm_export_overwrite(&mut self.state),
            Some(false) => {
                self.state.export_overwrite_confirm = None;
            }
            None => {}
        }

        // Escape: deselect guide + return to Select tool.
        // Runs after panels so popup consume_key takes priority.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.state.selected_guide = None;
            self.state.guide_tool = GuideTool::Select;
            self.state.show_export_settings = false;
            self.state.export_settings_draft = None;
            self.state.export_overwrite_confirm = None;
            self.state.unsaved_confirm = None;
        }
    }

    /// Execute Open Project (file dialog + load).
    fn do_open_project(&mut self, ctx: &egui::Context) {
        dialogs::open_project(&mut self.state, ctx);
        self.state.cached_mesh_normals = None;
        self.state.path_worker.discard();
        self.state.group_dim_cache.invalidate();
        // Older projects lack model_transform in editor.json — if it
        // deserialized as identity, recompute from mesh bounds (camera
        // is kept from editor.json so saved angles are preserved).
        let needs_recompute = self.state.mesh_preview.model_transform == glam::Mat4::IDENTITY;
        if needs_recompute {
            if let Some(ref mesh) = self.state.loaded_mesh {
                self.state.mesh_preview.recompute_model_transform(mesh);
            }
        }
        self.init_mesh_preview_no_fit();
    }

    /// Execute Open Example (embedded demo project).
    fn do_open_example(&mut self) {
        dialogs::open_example(&mut self.state);
        self.state.cached_mesh_normals = None;
        self.state.path_worker.discard();
        self.state.group_dim_cache.invalidate();
        let needs_recompute = self.state.mesh_preview.model_transform == glam::Mat4::IDENTITY;
        if needs_recompute {
            if let Some(ref mesh) = self.state.loaded_mesh {
                self.state.mesh_preview.recompute_model_transform(mesh);
            }
        }
        self.init_mesh_preview_no_fit();
    }

    /// Execute New Project (file dialog + mesh load).
    fn do_new_project(&mut self, ctx: &egui::Context) {
        dialogs::new_project(&mut self.state, ctx);
    }

    /// Auto-preview debounce and remerge polling.
    fn auto_preview_tick(&mut self, ctx: &egui::Context, project_replacing: bool) {
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

/// Show a TextureSource as a compact visual in the popup.
/// Solid → chip + hex, File → icon + name, MeshMaterial → icon + index.
/// When `is_default` is true, shows "Default" instead of details.
fn source_label_with_chip(
    ui: &mut egui::Ui,
    src: &TextureSource,
    text_color: egui::Color32,
    weak: egui::Color32,
    is_default: bool,
) {
    use egui_phosphor::fill::{CUBE, FOLDER_OPEN};

    if is_default {
        ui.colored_label(weak, "Default");
        return;
    }

    match src {
        TextureSource::Solid(rgb) => {
            let srgb = [
                (rgb[0].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[1].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
                (rgb[2].powf(1.0 / 2.2).clamp(0.0, 1.0) * 255.0) as u8,
            ];
            ui.spacing_mut().item_spacing.x = 4.0;
            let chip_size = egui::vec2(14.0, 14.0);
            let (rect, _) = ui.allocate_exact_size(chip_size, egui::Sense::hover());
            ui.painter().rect_filled(
                rect,
                2.0,
                egui::Color32::from_rgb(srgb[0], srgb[1], srgb[2]),
            );
            ui.painter().rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0, weak),
                egui::StrokeKind::Outside,
            );
            ui.colored_label(
                text_color,
                format!("#{:02X}{:02X}{:02X}", srgb[0], srgb[1], srgb[2]),
            );
        }
        TextureSource::MeshMaterial(idx) => {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(text_color, CUBE);
            ui.colored_label(text_color, format!("[{idx}]"));
        }
        TextureSource::File(Some(tex)) => {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(text_color, FOLDER_OPEN);
            ui.colored_label(text_color, &tex.label);
        }
        TextureSource::File(None) => {
            ui.colored_label(weak, "(no file)");
        }
        TextureSource::None => {
            use egui_phosphor::fill::PROHIBIT;
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.colored_label(weak, PROHIBIT);
            ui.colored_label(weak, "None");
        }
    }
}
