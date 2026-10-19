//! Services only the mobile OS provides, behind one interface for every platform:
//! storage access, hiding the system bars, a file browser for platforms without a
//! usable folder dialog, and (Android) video probing and frame grabs through the OS
//! media framework, which stands in for FFmpeg there (no conversion).
//!
//! The Android side is `ViewPortPlugin.kt` in the generated Android project. Desktop
//! builds get answers that mean "nothing to do".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::plugin::TauriPlugin;

use crate::error::{CmdError, CmdResult};

/// What the OS media framework reports about a video.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsProbe {
    pub container: String,
    pub duration_ms: Option<i64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// Whether this device can decode every stream in a container the WebView reads.
    pub playable: bool,
}

impl OsProbe {
    pub fn to_probe(&self) -> vp_media::Probe {
        vp_media::Probe {
            container: self.container.clone(),
            duration_ms: self.duration_ms,
            width: self.width,
            height: self.height,
            video_codec: self.video_codec.clone(),
            audio_codec: self.audio_codec.clone(),
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    use serde::Deserialize;
    use serde_json::json;
    use tauri::Wry;
    use tauri::plugin::PluginHandle;

    use super::OsProbe;

    static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

    pub fn set_handle(h: PluginHandle<Wry>) {
        let _ = HANDLE.set(h);
    }

    #[derive(Deserialize)]
    struct Granted {
        granted: bool,
    }

    #[derive(Deserialize)]
    struct FramePath {
        path: PathBuf,
    }

    pub fn media_available() -> bool {
        HANDLE.get().is_some()
    }

    pub fn storage_granted() -> bool {
        HANDLE.get().and_then(|h| h.run_mobile_plugin::<Granted>("storageStatus", ()).ok()).is_some_and(|g| g.granted)
    }

    pub fn request_storage() -> bool {
        HANDLE.get().and_then(|h| h.run_mobile_plugin::<Granted>("requestStorage", ()).ok()).is_some_and(|g| g.granted)
    }

    pub fn set_fullscreen(on: bool) {
        if let Some(h) = HANDLE.get() {
            let _ = h.run_mobile_plugin::<serde_json::Value>("setFullscreen", json!({ "on": on }));
        }
    }

    pub fn system_bars(dark: bool, background: &str) {
        if let Some(h) = HANDLE.get() {
            let _ = h.run_mobile_plugin::<serde_json::Value>("systemBars", json!({ "dark": dark, "background": background }));
        }
    }

    pub fn probe(path: &Path) -> Option<OsProbe> {
        HANDLE.get()?.run_mobile_plugin("probe", json!({ "path": path })).ok()
    }

    pub fn frame(path: &Path, at_ms: i64, max_edge: u32, quality: u8) -> Option<Vec<u8>> {
        let args = json!({ "path": path, "atMs": at_ms, "maxEdge": max_edge, "quality": quality });
        let out: FramePath = HANDLE.get()?.run_mobile_plugin("frame", args).ok()?;
        let data = std::fs::read(&out.path).ok();
        let _ = std::fs::remove_file(&out.path);
        data.filter(|d| !d.is_empty())
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use std::path::Path;

    use super::OsProbe;

    pub fn media_available() -> bool {
        false
    }

    pub fn storage_granted() -> bool {
        true
    }

    pub fn request_storage() -> bool {
        true
    }

    pub fn set_fullscreen(_on: bool) {}

    pub fn system_bars(_dark: bool, _background: &str) {}

    pub fn probe(_path: &Path) -> Option<OsProbe> {
        None
    }

    pub fn frame(_path: &Path, _at_ms: i64, _max_edge: u32, _quality: u8) -> Option<Vec<u8>> {
        None
    }
}

pub use imp::{frame, media_available, probe};

/// The Tauri plugin that connects to the native side.
pub fn init() -> TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("viewport")
        .setup(|_app, _api| {
            #[cfg(target_os = "android")]
            imp::set_handle(_api.register_android_plugin("com.viewport.app", "ViewPortPlugin")?);
            Ok(())
        })
        .build()
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
pub struct PlatformInfo {
    /// "android", "ios", "windows", "macos" or "linux".
    pub os: &'static str,
    pub mobile: bool,
    /// Whether the app may read and write shared storage (always true on desktop).
    pub storage_granted: bool,
    /// Video thumbnails and probing through the OS instead of FFmpeg.
    pub os_media: bool,
}

#[tauri::command]
pub async fn platform_info() -> PlatformInfo {
    tauri::async_runtime::spawn_blocking(|| PlatformInfo {
        os: std::env::consts::OS,
        mobile: cfg!(any(target_os = "android", target_os = "ios")),
        storage_granted: imp::storage_granted(),
        os_media: imp::media_available(),
    })
    .await
    .unwrap_or(PlatformInfo { os: std::env::consts::OS, mobile: false, storage_granted: false, os_media: false })
}

/// Asks for access to shared storage (Android: the "all files access" screen). Resolves
/// when the user comes back, with the result.
#[tauri::command]
pub async fn request_storage_access() -> bool {
    tauri::async_runtime::spawn_blocking(imp::request_storage).await.unwrap_or(false)
}

/// Hides or shows the system bars (mobile). The desktop viewer uses window fullscreen.
#[tauri::command]
pub async fn set_system_fullscreen(on: bool) {
    let _ = tauri::async_runtime::spawn_blocking(move || imp::set_fullscreen(on)).await;
}

/// Matches the system bars (mobile) to the UI theme: icon contrast and the color behind them.
#[tauri::command]
pub async fn set_system_bars(dark: bool, background: String) {
    let _ = tauri::async_runtime::spawn_blocking(move || imp::system_bars(dark, &background)).await;
}

#[derive(Serialize)]
pub struct StorageRoot {
    /// "internal", "external" or "home"; the UI names it.
    pub kind: &'static str,
    pub name: String,
    pub path: String,
}

/// Where the in-app file browser starts: the device's shared storage volumes.
#[tauri::command]
pub fn storage_roots() -> Vec<StorageRoot> {
    let mut roots = Vec::new();
    if cfg!(target_os = "android") {
        let internal = Path::new("/storage/emulated/0");
        if internal.is_dir() {
            roots.push(StorageRoot { kind: "internal", name: String::new(), path: internal.display().to_string() });
        }
        // Memory cards and USB drives mount as /storage/XXXX-XXXX.
        if let Ok(rd) = std::fs::read_dir("/storage") {
            let mut ext: Vec<_> = rd
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "emulated" && n != "self" && !n.starts_with('.'))
                .collect();
            ext.sort();
            for name in ext {
                let path = format!("/storage/{name}");
                if std::fs::read_dir(&path).is_ok() {
                    roots.push(StorageRoot { kind: "external", name, path });
                }
            }
        }
    } else if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        roots.push(StorageRoot { kind: "home", name: String::new(), path: PathBuf::from(home).display().to_string() });
    }
    roots
}

#[derive(Serialize)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

#[derive(Serialize)]
pub struct DirListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
}

/// One folder for the in-app file browser: subfolders first, then files (only those
/// with one of `extensions` when given; none when `files` is false). Hidden entries
/// are skipped. Folders whose name ends in `.vpbun` are bundles and listed as folders.
#[tauri::command]
pub async fn browse_dir(path: String, files: bool, extensions: Vec<String>) -> CmdResult<DirListing> {
    tauri::async_runtime::spawn_blocking(move || list_dir(Path::new(&path), files, &extensions))
        .await
        .map_err(|e| CmdError::new("io", e))?
}

fn list_dir(path: &Path, files: bool, extensions: &[String]) -> CmdResult<DirListing> {
    let rd = std::fs::read_dir(path).map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::PermissionDenied { "permission" } else { "io" };
        CmdError::new(code, e)
    })?;
    let wanted = |name: &str| {
        extensions.is_empty()
            || name.rsplit_once('.').is_some_and(|(_, ext)| extensions.iter().any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext)))
    };
    let mut entries: Vec<DirEntry> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            // Follows symlinks, so linked folders browse like folders.
            let meta = std::fs::metadata(e.path()).ok()?;
            if meta.is_dir() {
                Some(DirEntry { name, dir: true, size: 0 })
            } else if files && wanted(&name) {
                Some(DirEntry { name, dir: false, size: meta.len() })
            } else {
                None
            }
        })
        .collect();
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(DirListing {
        path: path.display().to_string(),
        parent: path.parent().map(|p| p.display().to_string()).filter(|p| !p.is_empty()),
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_first_and_filters_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("b")).unwrap();
        std::fs::create_dir(dir.path().join("A.vpbun")).unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("x.zip"), b"zip").unwrap();
        std::fs::write(dir.path().join("y.txt"), b"t").unwrap();

        let all = list_dir(dir.path(), true, &[]).unwrap();
        let names: Vec<_> = all.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A.vpbun", "b", "x.zip", "y.txt"]);
        assert!(all.parent.is_some());

        let zips = list_dir(dir.path(), true, &[".ZIP".into()]).unwrap();
        let names: Vec<_> = zips.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["A.vpbun", "b", "x.zip"]);
        assert_eq!(zips.entries[2].size, 3);

        let dirs = list_dir(dir.path(), false, &[]).unwrap();
        assert!(dirs.entries.iter().all(|e| e.dir));
    }

    #[test]
    fn missing_folder_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_dir(&dir.path().join("nope"), true, &[]).is_err());
    }
}
