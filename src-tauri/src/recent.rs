//! Recently opened bundles, stored in the app config directory (not in any bundle).

use std::path::PathBuf;

use tauri::{AppHandle, Manager};

const FILE: &str = "recent_bundles.json";
const MAX: usize = 10;

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(FILE))
}

pub fn list(app: &AppHandle) -> Vec<String> {
    let Some(p) = path(app) else { return Vec::new() };
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn push(app: &AppHandle, bundle: &str) {
    let Some(p) = path(app) else { return };
    let mut items = list(app);
    items.retain(|b| b != bundle);
    items.insert(0, bundle.to_string());
    items.truncate(MAX);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&items) {
        let _ = std::fs::write(p, json);
    }
}
