//! GPU texture upload and synchronisation for the 3D preview.

use eframe::egui_wgpu;

use super::PainterApp;
use super::mesh_preview;
use super::pipeline::collect_layer_refs;
use super::state;

use pa_painter::mesh::uv_mask::{DistanceField, UvMask};
use pa_painter::pipeline::compositing::{
    GlobalMaps, fill_base_color_region, resolve_base_color, resolve_base_normal,
};
use pa_painter::pipeline::output::blend_normals_udn;
use pa_painter::types::{BaseColorSource, Color, EmbeddedTexture, TextureSource};

impl PainterApp {
    /// Hash of visible layers' base texture state (color, normal, group, order, visibility)
    /// plus the current resolution preset. Resolution is included so that resizing
    /// invalidates the cached Base slot, preventing a size mismatch on the next upload.
    ///
    /// Runs every frame, so it must avoid heap allocation. We hash `TextureSource`
    /// directly (descending into `EmbeddedTexture::content_hash` for file textures)
    /// instead of round-tripping through JSON.
    fn base_texture_hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.state
            .project
            .settings
            .resolution_preset
            .resolution()
            .hash(&mut h);
        for layer in &self.state.project.layers {
            layer.visible.hash(&mut h);
            if layer.visible {
                layer.order.hash(&mut h);
                layer.group_name.hash(&mut h);
                hash_texture_source(&layer.base_color, &mut h);
                hash_texture_source(&layer.base_normal, &mut h);
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

    /// Recompose visible layers' base color/normal and upload into the **Base** slot pair.
    ///
    /// This always targets the Base slot regardless of current `result_mode`, so the
    /// data is ready for an instant bind-group swap when the user toggles to None.
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

        // Build distance fields for base color/normal fill
        let dist_fields: Vec<Option<DistanceField>> = if let Some(ref mesh) = self.state.loaded_mesh
        {
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
                                let mask = UvMask::from_mesh_group(mesh, group, resolution);
                                mask.distance_field()
                            })
                    }
                })
                .collect()
        } else {
            sorted_layers.iter().map(|_| None).collect()
        };
        let mask_refs: Vec<Option<&DistanceField>> =
            dist_fields.iter().map(|m| m.as_ref()).collect();

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

        mesh_preview::upload_color_texture(
            rs,
            &global.color,
            resolution as usize,
            mesh_preview::TextureSlot::Base,
        );
        mesh_preview::upload_normal_texture(
            rs,
            &normal_map,
            resolution as usize,
            mesh_preview::TextureSlot::Base,
        );
    }

    /// Synchronise GPU textures for the 3D preview.
    ///
    /// Two slot pairs (Base, Generated) stay GPU-resident; mode toggles only flip
    /// the bind group. Heavy uploads (base recompose, generated pixels, time-
    /// texture array) follow the same **"upload on the first stable frame"**
    /// pattern: when an input key (hash / counter / time key) changes, mark dirty
    /// but skip the upload. The next frame, if the key did not change again,
    /// perform the upload. This gives:
    ///
    ///   * Continuous edits (slider drags) → dirty stays set every frame, no
    ///     upload until the user pauses.
    ///   * One-shot transitions (mode toggle, generation finishing) → first
    ///     frame is just bind-group / state housekeeping; the heavy work runs
    ///     on the very next frame so the click feels instant.
    ///
    /// Base recompose is additionally gated on `result_mode == None` because
    /// the Base slot isn't visible in Paint/Drawing modes.
    pub(super) fn sync_gpu_textures(&mut self) {
        let mode = self.state.mesh_preview.result_mode;
        let mode_changed_this_frame = mode != self.prev_seen_mode;
        self.prev_seen_mode = mode;

        // ── Base slot ──
        // Hash every frame; mark dirty on change, recompose only on a stable
        // frame in None mode (where the Base slot is actually visible). We also
        // skip on the frame where the mode just changed so toggling into None
        // with a pre-existing `base_dirty` doesn't block the click.
        let cur_base_hash = self.base_texture_hash();
        let base_changed_this_frame = cur_base_hash != self.prev_base_tex_hash;
        self.prev_base_tex_hash = cur_base_hash;
        if base_changed_this_frame {
            self.base_dirty = true;
        }
        if self.base_dirty
            && !base_changed_this_frame
            && !mode_changed_this_frame
            && mode == state::ResultMode::None
        {
            self.upload_base_only_to_3d();
            self.base_dirty = false;
        }

        // ── Generated color/normal slot ──
        // Defer one frame after `gen_counter` bumps so the upload doesn't run on
        // the same frame as `apply_generation_result` (which already did CPU work).
        let cur_gen = self.gen_counter;
        let gen_changed_this_frame = cur_gen != self.prev_seen_gen;
        self.prev_seen_gen = cur_gen;
        if !gen_changed_this_frame
            && cur_gen != self.prev_uploaded_gen
            && let Some(ref rs) = self.render_state
            && self.state.mesh_preview.gpu_ready
            && let Some(ref generated) = self.state.generated
        {
            mesh_preview::upload_color_texture_raw(
                rs,
                &generated.gpu_color_pixels,
                generated.resolution as usize,
                mesh_preview::TextureSlot::Generated,
            );
            mesh_preview::upload_normal_texture_raw(
                rs,
                &generated.gpu_normal_pixels,
                generated.resolution as usize,
                mesh_preview::TextureSlot::Generated,
            );
            self.prev_uploaded_gen = cur_gen;
        }

        // ── Time texture: cache by (gen, draw_order, chunk_size, resolution) ──
        // Build only in Drawing mode; defer one frame after the key changes so
        // toggling into Drawing or changing draw_order/chunk_size feels instant.
        if let Some(ref generated) = self.state.generated {
            let cur_time_key = Some((
                self.gen_counter,
                self.state.mesh_preview.draw_order,
                self.state.mesh_preview.chunk_size,
                generated.resolution,
            ));
            let time_changed_this_frame = cur_time_key != self.prev_seen_time_key;
            self.prev_seen_time_key = cur_time_key;
            if mode == state::ResultMode::Drawing
                && !time_changed_this_frame
                && !mode_changed_this_frame
                && cur_time_key != self.prev_time_key
                && let Some(ref rs) = self.render_state
                && self.state.mesh_preview.gpu_ready
            {
                let lh = collect_layer_refs(
                    &generated.rendered_layers,
                    &self.state.generation.layer_cache,
                );
                let (sc, lc, ng) = mesh_preview::upload_time_texture(
                    rs,
                    &lh,
                    generated.resolution,
                    self.state.mesh_preview.draw_order,
                    self.state.mesh_preview.chunk_size,
                );
                self.state.mesh_preview.stroke_count = sc;
                self.state.mesh_preview.layer_count = lc;
                self.state.mesh_preview.num_groups = ng;
                self.prev_time_key = cur_time_key;
            }
        } else {
            self.prev_seen_time_key = None;
        }

        // ── Bind group selection: cheap rebind only, no texture work. ──
        // App-side cache (`prev_bound_to_generated`) avoids acquiring the wgpu
        // renderer write lock every frame when the desired binding hasn't changed.
        // Bind to Generated only when we have something to show — handles the case
        // where generation completes after the user already toggled to Paint/Drawing.
        if let Some(ref rs) = self.render_state
            && self.state.mesh_preview.gpu_ready
        {
            let show_generated = mode != state::ResultMode::None && self.state.generated.is_some();
            if self.prev_bound_to_generated != Some(show_generated) {
                mesh_preview::bind_textures_for_mode(rs, show_generated);
                self.prev_bound_to_generated = Some(show_generated);
            }
        }

        // ── Direction field overlay: sync with toggle + guide changes ──
        // Clone render_state (cheap Arc clone) to avoid borrowing self during
        // upload_direction_field_overlay.
        if let Some(rs) = self.render_state.clone()
            && self.state.mesh_preview.gpu_ready
        {
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

/// Allocation-free hash of a `TextureSource` for use in `base_texture_hash`.
/// `EmbeddedTexture::pixels` is heavy and not directly hashable; we descend
/// only into the cheap `content_hash` summary.
fn hash_texture_source<H: std::hash::Hasher>(src: &TextureSource, h: &mut H) {
    use std::hash::Hash;
    match src {
        TextureSource::None => 0u8.hash(h),
        TextureSource::Solid(rgb) => {
            1u8.hash(h);
            for c in rgb {
                c.to_bits().hash(h);
            }
        }
        TextureSource::MeshMaterial(idx) => {
            2u8.hash(h);
            idx.hash(h);
        }
        TextureSource::File(opt) => {
            3u8.hash(h);
            match opt {
                None => 0u8.hash(h),
                Some(tex) => {
                    1u8.hash(h);
                    hash_embedded_texture(tex, h);
                }
            }
        }
    }
}

fn hash_embedded_texture<H: std::hash::Hasher>(tex: &EmbeddedTexture, h: &mut H) {
    use std::hash::Hash;
    tex.label.hash(h);
    tex.width.hash(h);
    tex.height.hash(h);
    tex.content_hash.hash(h);
}
