//! Extract track — single-file, ad-hoc DCC-targeted output.
//!
//! Distinct from [`super::export`]: extract takes one file path from
//! `rfd::save_file` and writes a single map. Settings are stored per-user
//! (via [`crate::gui::extract_prefs`]), not in `.papr`.

use std::path::PathBuf;

use pa_painter::pipeline::output::{ExtractObjectNormalParams, UpAxis, extract_object_normal};

use crate::gui::extract_prefs;
use crate::gui::state::AppState;

/// Build the suggested output filename for the current project + selection.
fn suggest_filename(state: &AppState) -> String {
    let stem = state
        .project_path
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("untitled");
    let suffix = match state.extract_prefs.up_axis {
        UpAxis::Y => "object_normal_yup",
        UpAxis::Z => "object_normal_zup",
    };
    let ext = state.extract_prefs.encoding.extension();
    format!("{stem}_{suffix}.{ext}")
}

/// Pick a file path and run the extraction. Called from the dispatch loop
/// when `pending_extract` is set.
pub fn extract_object_normal_action(state: &mut AppState) {
    let Some(generated) = state.generated.as_ref() else {
        state.status_message = "Nothing to extract — generate first".to_string();
        return;
    };
    let Some((_, normal_data)) = state.cached_mesh_normals.as_ref() else {
        state.status_message =
            "Object-space normal extract requires DepictedForm normal mode".to_string();
        return;
    };
    if generated.object_normal.is_empty() {
        state.status_message =
            "Generate with DepictedForm normal mode before extracting".to_string();
        return;
    }

    let suggested = suggest_filename(state);
    let mut dialog = rfd::FileDialog::new().set_file_name(&suggested);
    if let Some(ref dir) = state.extract_prefs.last_dir {
        dialog = dialog.set_directory(dir);
    }

    state.modal_dialog_active = true;
    let chosen: Option<PathBuf> = dialog.save_file();
    state.modal_dialog_active = false;

    let Some(path) = chosen else {
        return;
    };

    if let Some(parent) = path.parent() {
        state.extract_prefs.last_dir = Some(parent.to_path_buf());
    }
    extract_prefs::save(&state.extract_prefs);

    let normal_strength = state.project.settings.normal_strength;
    let resolution = generated.resolution;
    let result = extract_object_normal(&ExtractObjectNormalParams {
        gradient_x: &generated.gradient_x,
        gradient_y: &generated.gradient_y,
        normal_data,
        global_object_normals: &generated.object_normal,
        paint_load: &generated.paint_load,
        resolution,
        normal_strength,
        up_axis: state.extract_prefs.up_axis,
        encoding: state.extract_prefs.encoding,
        output_path: &path,
    });

    match result {
        Ok(()) => {
            state.status_message = format!(
                "Extracted object normal ({:?}, {:?}) → {}",
                state.extract_prefs.up_axis,
                state.extract_prefs.encoding,
                path.display(),
            );
        }
        Err(e) => {
            state.status_message = format!("Extract failed: {e}");
        }
    }
}
