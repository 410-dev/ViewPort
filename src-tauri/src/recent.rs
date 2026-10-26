//! Per-device state stored in the app config directory (not in any bundle):
//! recently opened bundles, where each was last browsed, and [`AppSettings`].

use std::path::PathBuf;

use tauri::{AppHandle, Manager, Runtime};
use vp_core::settings::AppSettings;

const FILE: &str = "recent_bundles.json";
const MAX: usize = 10;

const APP_SETTINGS: &str = "app_settings.json";

/// Last browsing state (source, folder, query, sort) of each recent bundle, as the UI
/// stores it. Device state: source ids belong to the bundle, the path to this device.
const LAST_VIEW: &str = "last_view.json";

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
    crate::sched::configure(&s.hardware);
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
    crate::sched::configure(&s.hardware);
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

fn last_views(app: &AppHandle) -> serde_json::Map<String, serde_json::Value> {
    config_file(app, LAST_VIEW)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn last_view(app: &AppHandle, bundle: &str) -> Option<serde_json::Value> {
    last_views(app).remove(bundle)
}

/// Remembers a bundle's browsing state; bundles no longer in the recent list are dropped.
pub fn save_last_view(app: &AppHandle, bundle: &str, view: serde_json::Value) {
    let recent = list(app);
    let mut views = last_views(app);
    views.retain(|k, _| recent.contains(k));
    views.insert(bundle.to_string(), view);
    let Some(p) = config_file(app, LAST_VIEW) else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&views) {
        let _ = std::fs::write(p, json);
    }
}
