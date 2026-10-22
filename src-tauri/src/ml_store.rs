//! Model downloads: a catalog of known model packs (`models/catalog.json`, built in) the
//! user can fetch from their publishers after reading and accepting each license in the
//! UI. Packs land in the config folder's `models/<id>/`; the manifest is written last, so a
//! half-downloaded pack is never picked up. The accepted license is kept in `consent.json`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use vp_ml::PackKind;

use crate::error::{CmdError, CmdResult};

const CATALOG: &str = include_str!("../../models/catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogFile {
    pub url: String,
    /// Path inside the pack folder.
    pub path: String,
    /// Expected size (0 = unknown), for progress before the server answers.
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogLicense {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub kind: PackKind,
    /// By locale ("ko", "en").
    pub summary: HashMap<String, String>,
    pub license: CatalogLicense,
    pub homepage: String,
    pub files: Vec<CatalogFile>,
    /// Written as the pack's `manifest.json`.
    pub manifest: serde_json::Value,
}

fn catalog() -> Vec<CatalogEntry> {
    serde_json::from_str(CATALOG).expect("built-in model catalog is valid JSON")
}

/// Packs being downloaded now, by catalog id.
static DOWNLOADING: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn downloading(id: &str) -> bool {
    DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.contains(id))
}

fn pack_dir(app: &AppHandle, id: &str) -> CmdResult<PathBuf> {
    let base = app.path().app_config_dir().map_err(|e| CmdError::new("io", e))?;
    Ok(base.join("models").join(crate::shards::safe_name(id, "pack")))
}

#[derive(Serialize)]
pub struct StoreItem {
    #[serde(flatten)]
    pub entry: CatalogEntry,
    /// Total download size in bytes (0 when not known).
    pub size: u64,
    /// A pack with this id is installed somewhere the app looks.
    pub installed: bool,
    pub downloading: bool,
    /// Where it would be (or was) downloaded to.
    pub path: String,
}

/// The catalog, with what is installed and being downloaded.
#[tauri::command]
pub async fn ml_catalog(app: AppHandle) -> CmdResult<Vec<StoreItem>> {
    let installed: HashSet<String> = {
        let (packs, _) = vp_ml::pack::find_packs(&crate::ml::model_dirs(&app));
        packs.into_iter().map(|p| p.manifest.id).collect()
    };
    catalog()
        .into_iter()
        .map(|entry| {
            Ok(StoreItem {
                size: entry.files.iter().map(|f| f.size).sum(),
                installed: installed.contains(&entry.id),
                downloading: downloading(&entry.id),
                path: pack_dir(&app, &entry.id)?.display().to_string(),
                entry,
            })
        })
        .collect()
}

/// Downloads catalog pack `id` in the background. `license` must be the name of the license
/// the user was shown and accepted.
#[tauri::command]
pub async fn ml_download(app: AppHandle, id: String, license: String) -> CmdResult<()> {
    let entry = catalog().into_iter().find(|e| e.id == id).ok_or_else(|| CmdError::new("not_found", "unknown model"))?;
    if license != entry.license.name {
        return Err(CmdError::new("license_not_accepted", "the model's license was not accepted"));
    }
    if !cfg!(any(target_os = "android", target_os = "ios")) {
        let dir = pack_dir(&app, &id)?;
        {
            let mut set = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner());
            if !set.get_or_insert_with(HashSet::new).insert(id.clone()) {
                return Ok(());
            }
        }
        std::thread::spawn(move || {
            imp::download(&entry, &dir);
            if let Some(set) = DOWNLOADING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                set.remove(&entry.id);
            }
        });
        return Ok(());
    }
    Err(CmdError::new("unsupported", "models are not downloaded on this device"))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod imp {
    use std::io::{Read, Write};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::CatalogEntry;
    use crate::error::{CmdError, CmdResult};
    use crate::tasks::{self, Task, Unit};

    pub fn download(entry: &CatalogEntry, dir: &Path) {
        let cancel = Arc::new(AtomicBool::new(false));
        let total: u64 = entry.files.iter().map(|f| f.size).sum();
        let task = tasks::begin_with_cancel("download_model", &entry.name, Unit::Bytes, (total > 0).then_some(total), Some(cancel.clone()));
        match fetch_all(entry, dir, &task, &cancel) {
            Ok(()) => task.finish(),
            Err(e) if e.code == "cancelled" => task.cancelled(),
            Err(e) => task.fail(e.message),
        }
    }

    fn fetch_all(entry: &CatalogEntry, dir: &Path, task: &Task, cancel: &AtomicBool) -> CmdResult<()> {
        std::fs::create_dir_all(dir)?;
        // A previous pack of this id stops counting until the new one is complete.
        let _ = std::fs::remove_file(dir.join(vp_ml::pack::MANIFEST));
        for f in &entry.files {
            let parts = vp_driver::checked_components(&f.path)?;
            let target = parts.iter().fold(dir.to_path_buf(), |p, c| p.join(c));
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            task.detail(&f.path);
            fetch(&f.url, &target, task, cancel)?;
        }
        let consent = serde_json::json!({
            "license": entry.license.name,
            "url": entry.license.url,
            "accepted_ms": crate::workspace::now_ms(),
        });
        std::fs::write(dir.join("consent.json"), serde_json::to_vec_pretty(&consent)?)?;
        std::fs::write(dir.join(vp_ml::pack::MANIFEST), serde_json::to_vec_pretty(&entry.manifest)?)?;
        // Only a pack that loads counts as installed.
        vp_ml::Pack::load(dir).map_err(|e| CmdError::new("ml_pack", e))?;
        Ok(())
    }

    /// Streams `url` to `target` through `target.part`.
    fn fetch(url: &str, target: &Path, task: &Task, cancel: &AtomicBool) -> CmdResult<()> {
        let part = target.with_extension(format!("{}.part", target.extension().and_then(|e| e.to_str()).unwrap_or("")));
        let result = (|| -> CmdResult<()> {
            let mut resp = ureq::get(url).call().map_err(|e| CmdError::new("download", e))?;
            let mut body = resp.body_mut().with_config().limit(u64::MAX).reader();
            let mut out = std::fs::File::create(&part)?;
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(CmdError::new("cancelled", "cancelled"));
                }
                let n = body.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                task.add(n as u64);
            }
            out.sync_all()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                std::fs::rename(&part, target)?;
                Ok(())
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                Err(e)
            }
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
mod imp {
    pub fn download(_entry: &super::CatalogEntry, _dir: &std::path::Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_are_complete_packs() {
        let entries = catalog();
        assert!(entries.len() >= 5);
        let mut ids = HashSet::new();
        for e in &entries {
            assert!(ids.insert(e.id.clone()), "duplicate id {}", e.id);
            assert!(!e.files.is_empty() && !e.license.name.is_empty() && e.summary.contains_key("ko") && e.summary.contains_key("en"));
            // The manifest it writes is a valid pack of the same id and kind, naming a file it downloads.
            let m: vp_ml::pack::Manifest = serde_json::from_value(e.manifest.clone()).unwrap();
            assert_eq!((m.id.as_str(), m.kind), (e.id.as_str(), e.kind));
            assert!(e.files.iter().any(|f| f.path == m.model));
            if let Some(l) = &m.labels {
                assert!(e.files.iter().any(|f| &f.path == l));
            }
            assert!(e.files.iter().all(|f| f.url.starts_with("https://")));
        }
    }

    /// Serves `body` for every request on a local port.
    fn serve(body: &'static [u8]) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(body);
            }
        });
        format!("http://{addr}")
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn download_writes_the_files_then_the_manifest() {
        let base = serve(b"onnx bytes");
        let dir = tempfile::tempdir().unwrap();
        let pack = dir.path().join("x");
        let mut entry = catalog().into_iter().find(|e| e.id == "yunet").unwrap();
        entry.files[0].url = format!("{base}/model");
        imp::download(&entry, &pack);
        assert_eq!(std::fs::read(pack.join("face_detection_yunet_2023mar.onnx")).unwrap(), b"onnx bytes");
        assert!(pack.join("consent.json").is_file());
        let loaded = vp_ml::Pack::load(&pack).unwrap();
        assert_eq!(loaded.manifest.key(), "yunet@2023mar");
        assert!(std::fs::read_dir(&pack).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));

        // A failed download leaves no manifest (so no half pack) and no partial file.
        let broken = dir.path().join("y");
        entry.files[0].url = "http://127.0.0.1:9/unreachable".into();
        imp::download(&entry, &broken);
        assert!(!broken.join(vp_ml::pack::MANIFEST).exists());
        assert!(vp_ml::Pack::load(&broken).is_err());
    }
}
