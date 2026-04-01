use pa_painter::project::utc_now_iso8601;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_RECENT: usize = 10;
const FILE_NAME: &str = "recent_projects.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// Last time this project was opened or saved (ISO 8601).
    pub last_opened: String,
}

impl RecentEntry {
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string())
    }

    pub fn dir_display(&self) -> String {
        self.path
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    }

    /// Human-readable relative time (e.g. "2 days ago", "3 hours ago").
    pub fn time_ago(&self) -> String {
        let Ok(then) = parse_iso8601(&self.last_opened) else {
            return self.last_opened.clone();
        };
        let now = std::time::SystemTime::now();
        let Ok(elapsed) = now.duration_since(then) else {
            return self.last_opened.clone();
        };
        let secs = elapsed.as_secs();
        if secs < 60 {
            "just now".to_string()
        } else if secs < 3600 {
            let m = secs / 60;
            format!("{m} min ago")
        } else if secs < 86400 {
            let h = secs / 3600;
            format!("{h} hour{} ago", if h == 1 { "" } else { "s" })
        } else {
            let d = secs / 86400;
            format!("{d} day{} ago", if d == 1 { "" } else { "s" })
        }
    }
}

/// Returns the path to the recent projects JSON file.
fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        dirs_next::data_dir().map(|d| d.join("PA Painter").join(FILE_NAME))
    }
    #[cfg(target_os = "windows")]
    {
        dirs_next::data_local_dir().map(|d| d.join("PA Painter").join(FILE_NAME))
    }
    #[cfg(target_os = "linux")]
    {
        dirs_next::config_dir().map(|d| d.join("pa-painter").join(FILE_NAME))
    }
}

/// Load the recent files list from disk, filtering out missing files.
pub fn load() -> Vec<RecentEntry> {
    let Some(path) = config_path() else {
        return Vec::new();
    };
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let entries: Vec<RecentEntry> = serde_json::from_str(&data).unwrap_or_default();
    entries
        .into_iter()
        .filter(|e| e.path.exists())
        .take(MAX_RECENT)
        .collect()
}

/// Remove a path from the recent files list, persist, and return the updated list.
pub fn remove(path: &Path) -> Vec<RecentEntry> {
    let mut list = load();
    list.retain(|e| e.path != path);
    save(&list);
    list
}

/// Add a path to the top of the recent files list, persist, and return the updated list.
pub fn push(path: &Path) -> Vec<RecentEntry> {
    let canonical = path.to_path_buf();
    let mut list = load();
    list.retain(|e| e.path != canonical);
    list.insert(
        0,
        RecentEntry {
            path: canonical,
            last_opened: utc_now_iso8601(),
        },
    );
    list.truncate(MAX_RECENT);
    save(&list);
    list
}

fn save(list: &[RecentEntry]) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let _ = std::fs::write(&path, json);
    }
}

fn parse_iso8601(s: &str) -> Result<std::time::SystemTime, ()> {
    // Parse "YYYY-MM-DDThh:mm:ssZ"
    if s.len() < 19 {
        return Err(());
    }
    let y: u64 = s[0..4].parse().map_err(|_| ())?;
    let mo: u64 = s[5..7].parse().map_err(|_| ())?;
    let d: u64 = s[8..10].parse().map_err(|_| ())?;
    let h: u64 = s[11..13].parse().map_err(|_| ())?;
    let mi: u64 = s[14..16].parse().map_err(|_| ())?;
    let sc: u64 = s[17..19].parse().map_err(|_| ())?;

    // Days from epoch to year
    let mut days: u64 = 0;
    for yr in 1970..y {
        days += if is_leap(yr) { 366 } else { 365 };
    }
    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    for &md in &month_days[..(mo.saturating_sub(1) as usize)] {
        days += md;
    }
    days += d.saturating_sub(1);

    let total_secs = days * 86400 + h * 3600 + mi * 60 + sc;
    Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(total_secs))
}

fn is_leap(y: u64) -> bool {
    y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400))
}
