use std::sync::mpsc;
use std::thread;

/// Information about an available update.
pub struct UpdateInfo {
    /// The newer version string (e.g. "0.3.0").
    pub version: String,
    /// URL to open for downloading the update.
    pub url: String,
}

/// Handle for a background update check.
pub struct UpdateChecker {
    rx: mpsc::Receiver<Option<UpdateInfo>>,
    pub result: Option<Option<UpdateInfo>>,
}

const GITHUB_API_URL: &str = "https://api.github.com/repos/dinobox-dev/PA-Painter/releases/latest";
const ITCH_URL: &str = "https://dinoboxgamedev.itch.io/papainter";

impl UpdateChecker {
    /// Spawn a background thread that checks for a newer release.
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let result = check_latest();
            let _ = tx.send(result);
        });
        Self { rx, result: None }
    }

    /// Poll for the result. Returns `true` once the check has completed.
    pub fn poll(&mut self) -> bool {
        if self.result.is_some() {
            return true;
        }
        if let Ok(info) = self.rx.try_recv() {
            self.result = Some(info);
            true
        } else {
            false
        }
    }

    /// Get the update info if a newer version is available.
    pub fn update_available(&self) -> Option<&UpdateInfo> {
        self.result.as_ref().and_then(|r| r.as_ref())
    }
}

/// Check the GitHub latest release API and compare with the current version.
fn check_latest() -> Option<UpdateInfo> {
    let body: String = ureq::get(GITHUB_API_URL)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "pa-painter-update-check")
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;

    let parsed: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tag = parsed.get("tag_name")?.as_str()?;

    let remote = semver::Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    let current = semver::Version::parse(env!("CARGO_PKG_VERSION")).ok()?;

    if remote > current {
        Some(UpdateInfo {
            version: remote.to_string(),
            url: ITCH_URL.to_string(),
        })
    } else {
        None
    }
}
