//! Per-device state stored in the app config directory (not in any bundle):
//! recently opened bundles and [`AppSettings`].

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};
use vp_core::settings::AppSettings;

const FILE: &str = "recent_bundles.json";
const MAX: usize = 10;

const APP_SETTINGS: &str = "app_settings.json";

fn config_file<R: Runtime>(app: &AppHandle<R>, name: &str) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join(name))
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    config_file(app, FILE)
}

static APP_SETTINGS_CACHE: std::sync::RwLock<Option<AppSettings>> = std::sync::RwLock::new(None);

pub fn app_settings<R: Runtime>(app: &AppHandle<R>) -> AppSettings {
    let s: AppSettings = config_file(app, APP_SETTINGS)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    *APP_SETTINGS_CACHE.write().unwrap() = Some(s.clone());
    s
}

/// The device settings last loaded or saved, for code without an `AppHandle`.
pub fn cached_app_settings() -> AppSettings {
    APP_SETTINGS_CACHE.read().unwrap().clone().unwrap_or_default()
}

pub fn save_app_settings(app: &AppHandle, s: &AppSettings) -> std::io::Result<()> {
    let p = config_file(app, APP_SETTINGS).ok_or_else(|| std::io::Error::other("no config dir"))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(s).map_err(std::io::Error::other)?)?;
    *APP_SETTINGS_CACHE.write().unwrap() = Some(s.clone());
    Ok(())
}

pub fn list(app: &AppHandle) -> Vec<String> {
    let Some(p) = path(app) else { return Vec::new() };
    std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn push(app: &AppHandle, bundle: &str) {
    let mut items = list(app);
    items.retain(|b| b != bundle);
    items.insert(0, bundle.to_string());
    items.truncate(MAX);
    write(app, &items);
}

/// Drops one bundle from the list.
pub fn remove(app: &AppHandle, bundle: &str) {
    let mut items = list(app);
    items.retain(|b| b != bundle);
    write(app, &items);
}

fn write(app: &AppHandle, items: &[String]) {
    let Some(p) = path(app) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(items) {
        let _ = std::fs::write(p, json);
    }
}
