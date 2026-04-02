//! Per-frame update helpers: input handling, deferred action dispatch, worker polling.

use eframe::egui;

use super::file_actions;
use super::preview;
use super::recent_files;
use super::state::{ProjectLoadSource, UnsavedAction};
use super::PainterApp;

use pa_painter::types::BASE_RESOLUTION;

impl PainterApp {
    /// Global keyboard shortcuts (undo/redo, save, generate, tab cycling).
    pub(super) fn handle_keyboard(&mut self, ctx: &egui::Context) {
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

        let save_as_mods = egui::Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };
        if ctx.input_mut(|i| i.consume_key(save_as_mods, egui::Key::S)) {
            self.state.pending_save_as = true;
        } else if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::S)) {
            self.state.pending_save = true;
        }
        if ctx.input_mut(|i| i.consume_key(undo_mods, egui::Key::G))
            && !self.state.generation.is_running()
            && !self.state.modal_dialog_active
            && self.state.loaded_mesh.is_some()
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

    /// Handle files dropped onto the window.
    pub(super) fn handle_file_drop(&mut self, ctx: &egui::Context) {
        let dropped: Vec<_> = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped {
            let Some(path) = file.path else { continue };
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.to_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "papr" => {
                    self.pending_open_recent = Some(path);
                }
                "obj" | "glb" | "gltf" => {
                    self.state.pending_new = true;
                    // Store the mesh path so new_project can skip the file dialog
                    self.state.pending_drop_mesh = Some(path);
                }
                _ => {
                    self.alert_message = Some(format!("Unsupported file type: .{ext}"));
                }
            }
            break; // handle only the first file
        }
    }

    /// Process pending_* flags set by UI widgets in the previous frame.
    pub(super) fn dispatch_deferred(&mut self, ctx: &egui::Context) {
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
        if let Some(path) = self.pending_open_recent.take() {
            if self.state.dirty {
                // Stash path back and show unsaved dialog
                self.pending_open_recent = Some(path);
                self.state.unsaved_confirm = Some(UnsavedAction::Open);
            } else if path.exists() {
                self.state.status_message = format!("Opening {}…", path.display());
                self.state
                    .project_load_worker
                    .start(ProjectLoadSource::Recent(path));
            } else {
                self.alert_message = Some(format!("File not found:\n{}", path.display()));
                self.recent_files = recent_files::remove(&path);
            }
        }
        if self.state.pending_open_example {
            self.state.pending_open_example = false;
            if self.state.dirty {
                self.state.unsaved_confirm = Some(UnsavedAction::OpenExample);
            } else {
                self.state.status_message = "Opening example project…".to_string();
                self.state
                    .project_load_worker
                    .start(ProjectLoadSource::Example);
            }
        }
        if self.state.pending_save {
            self.state.pending_save = false;
            file_actions::save_project_action(&mut self.state);
            if let Some(ref path) = self.state.project_path {
                self.recent_files = recent_files::push(path);
            }
        }
        if self.state.pending_save_as {
            self.state.pending_save_as = false;
            file_actions::save_project_as_action(&mut self.state);
            if let Some(ref path) = self.state.project_path {
                self.recent_files = recent_files::push(path);
            }
        }
        if self.state.pending_export {
            self.state.pending_export = false;
            let es = &self.state.project.export_settings;
            let do_maps = es.export_maps;
            let do_model = es.export_model;
            match (do_maps, do_model) {
                (true, true) => file_actions::export_both(&mut self.state),
                (true, false) => file_actions::export_maps(&mut self.state),
                (false, true) => file_actions::export_glb(&mut self.state),
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
            file_actions::reload_mesh(&mut self.state);
            self.state.cached_mesh_normals = None;
            self.state.path_worker.discard();
            self.state.group_dim_cache.invalidate();
            self.init_mesh_preview();
        }
        if self.state.pending_replace_mesh {
            self.state.pending_replace_mesh = false;
            file_actions::replace_mesh(&mut self.state);
        }
        if self.state.pending_remerge {
            self.state.pending_remerge = false;
            self.start_remerge(); // cancels any in-flight remerge automatically
        }
    }

    /// Poll background workers (path overlay, generation, remerge) and apply results.
    pub(super) fn poll_workers(&mut self, ctx: &egui::Context) {
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

        // Project load worker
        if let Some(result) = self.state.project_load_worker.poll() {
            match result {
                Ok((load_result, source)) => {
                    let project_path = match &source {
                        ProjectLoadSource::Open(p) | ProjectLoadSource::Recent(p) => {
                            Some(p.clone())
                        }
                        ProjectLoadSource::Example => None,
                    };
                    file_actions::apply_load_result(&mut self.state, load_result, project_path);
                    self.after_project_loaded();
                }
                Err(msg) => {
                    self.state.status_message = msg;
                }
            }
        }
        if self.state.project_load_worker.is_active() {
            ctx.request_repaint();
        }
    }
}
