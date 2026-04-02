//! GPU texture upload and synchronisation for the 3D preview.

use eframe::egui_wgpu;

use super::mesh_preview;
use super::pipeline::collect_layer_refs;
use super::state;
use super::PainterApp;

use pa_painter::mesh::uv_mask::UvMask;
use pa_painter::pipeline::compositing::{
    fill_base_color_region, resolve_base_color, resolve_base_normal, GlobalMaps,
};
use pa_painter::pipeline::output::blend_normals_udn;
use pa_painter::types::{BaseColorSource, Color};

impl PainterApp {
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
        let pixels = pa_painter::pipeline::direction_field::render_direction_field_overlay(
            &all_guides,
            resolution,
            arrow_spacing,
        );
        mesh_preview::upload_overlay_texture(render_state, &pixels, resolution);
    }

    /// Upload base-only textures to the 3D preview (no stroke results).
    /// Composites visible layers' base color and base normal textures.
    pub(super) fn upload_base_only_to_3d(&self) {
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

    /// Synchronise GPU textures for the 3D preview (result mode, direction field overlay).
    pub(super) fn sync_gpu_textures(&mut self) {
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
}
