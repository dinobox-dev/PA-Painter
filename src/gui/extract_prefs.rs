//! App-level persistence for the Extract dialog's last-used config.
//!
//! Stored on disk per-user (NOT in `.papr`), so reproducibility of project
//! files is unaffected. Mirrors the storage pattern used by
//! [`super::recent_files`].

use std::path::PathBuf;

use pa_painter::pipeline::output::{ExtractEncoding, UpAxis};
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "extract_prefs.json";

/// User's last-used Extract dialog selections.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtractPrefs {
    #[serde(default)]
    pub up_axis: UpAxis,
    #[serde(default)]
    pub encoding: ExtractEncoding,
    /// Last directory used in the Save... dialog. Used to seed the next pick.
    #[serde(default)]
    pub last_dir: Option<PathBuf>,
}

fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir().map(|d| d.join("PA Painter").join(FILE_NAME))
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir().map(|d| d.join("PA Painter").join(FILE_NAME))
    }
    #[cfg(target_os = "linux")]
    {
        dirs::config_dir().map(|d| d.join("pa-painter").join(FILE_NAME))
    }
}

pub fn load() -> ExtractPrefs {
    let Some(path) = config_path() else {
        return ExtractPrefs::default();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return ExtractPrefs::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save(prefs: &ExtractPrefs) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(prefs) {
        let _ = std::fs::write(&path, json);
    }
}
