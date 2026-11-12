//! Bundle passwords remembered on this device, by bundle id (`info.json` `id`), so a
//! locked bundle can be opened without typing its password: after a biometric check
//! where the device has biometrics (`biometric.rs`), directly where it has none.
//!
//! The list (id, name, last path, key file path) is a plain file in the app config
//! folder and holds no secret; each password is in the OS credential store (`secrets.rs`)
//! and never goes back to the UI.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use vp_bundle::Bundle;

use crate::commands::{blocking, install};
use crate::error::{CmdError, CmdResult};
use crate::workspace::{BundleSummary, Workspace};
use crate::AppState;

const FILE: &str = "bundle_passwords.json";
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub name: String,
    /// Where the bundle was last opened or added from on this device.
    pub path: String,
    /// Key file the bundle was locked with, if any (a path on this device).
    #[serde(default)]
    pub keyfile: Option<String>,
    #[serde(default)]
    pub saved_ms: i64,
}

fn account(id: &str) -> String {
    format!("bundle-password:{id}")
}

fn list_file(app: &AppHandle) -> Option<PathBuf> {
    crate::recent::config_file(app, FILE)
}

fn read(app: &AppHandle) -> Vec<Entry> {
    list_file(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write(app: &AppHandle, entries: &[Entry]) -> CmdResult<()> {
    let p = list_file(app).ok_or_else(|| CmdError::new("io", "no config folder"))?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(p, serde_json::to_vec_pretty(entries)?)?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Id, name and lock of the bundle at `path`, giving it an id if it has none yet.
fn identify(path: &Path) -> CmdResult<(String, String, vp_bundle::KdfParams)> {
    let mut bundle = Bundle::open(path, true)?;
    let lock = bundle.info.lock.clone().ok_or_else(|| CmdError::new("not_locked", "the bundle is not locked"))?;
    let id = bundle.ensure_id()?.to_string();
    if id.is_empty() {
        return Err(CmdError::new("no_id", "this bundle cannot be identified (opened read-only)"));
    }
    let name = match bundle.info.name.as_str() {
        "" => path.file_name().map(|n| n.to_string_lossy().trim_end_matches(".vpbun").to_string()).unwrap_or_default(),
        n => n.to_string(),
    };
    Ok((id, name, lock))
}

/// Stores (or replaces) a bundle's password. The caller has checked it.
pub fn remember(app: &AppHandle, id: &str, name: &str, path: &Path, password: &str, keyfile: Option<&str>) -> CmdResult<Entry> {
    if id.is_empty() {
        return Err(CmdError::new("no_id", "this bundle cannot be identified (opened read-only)"));
    }
    let _guard = LOCK.lock().unwrap();
    if !crate::secrets::put(&account(id), password) {
        return Err(CmdError::new("secret_store", "the password could not be stored on this device"));
    }
    let entry = Entry {
        id: id.into(),
        name: name.into(),
        path: path.to_string_lossy().into_owned(),
        keyfile: keyfile.filter(|k| !k.is_empty()).map(str::to_string),
        saved_ms: now_ms(),
    };
    let mut all = read(app);
    all.retain(|e| e.id != id);
    all.insert(0, entry.clone());
    write(app, &all)?;
    Ok(entry)
}

/// Remembers the password of a bundle, after checking it against the bundle's lock.
fn check_and_remember(app: &AppHandle, path: &Path, password: &str, keyfile: Option<&str>) -> CmdResult<Entry> {
    let (id, name, lock) = identify(path)?;
    let kf = keyfile.filter(|k| !k.is_empty()).map(std::fs::read).transpose()?;
    vp_crypto::unlock(password, kf.as_deref(), &lock)?;
    remember(app, &id, &name, path, password, keyfile)
}

fn find(app: &AppHandle, id: &str) -> Option<Entry> {
    read(app).into_iter().find(|e| e.id == id)
}

// ---------------------------------------------------------------- commands

#[derive(Serialize)]
pub struct VaultStatus {
    pub biometric: crate::biometric::Status,
    pub entries: Vec<Entry>,
}

/// Remembered bundles (never their passwords) and what biometrics this device has.
#[tauri::command]
pub async fn vault_status(app: AppHandle) -> CmdResult<VaultStatus> {
    blocking(move || Ok(VaultStatus { biometric: crate::biometric::status(), entries: read(&app) })).await
}

/// Adds or changes the remembered password of the locked bundle at `path`. The
/// password (and key file) must open it.
#[tauri::command]
pub async fn vault_save(app: AppHandle, path: String, password: String, keyfile: Option<String>) -> CmdResult<Entry> {
    blocking(move || check_and_remember(&app, Path::new(&path), &password, keyfile.as_deref())).await
}

/// Changes the remembered password of a bundle already in the list, found at its last
/// path (or `path`, when given).
#[tauri::command]
pub async fn vault_change(app: AppHandle, id: String, path: Option<String>, password: String, keyfile: Option<String>) -> CmdResult<Entry> {
    blocking(move || {
        let entry = find(&app, &id).ok_or_else(|| CmdError::new("not_saved", "no password is remembered for this bundle"))?;
        let path = PathBuf::from(path.unwrap_or(entry.path));
        let (found, ..) = identify(&path).map_err(|e| e.with_path(&path))?;
        if found != id {
            return Err(CmdError::new("other_bundle", "the bundle at this path is a different bundle").with_path(&path));
        }
        let keyfile = keyfile.or(entry.keyfile);
        check_and_remember(&app, &path, &password, keyfile.as_deref())
    })
    .await
}

/// Forgets a bundle's password.
#[tauri::command]
pub async fn vault_remove(app: AppHandle, id: String) -> CmdResult<()> {
    blocking(move || {
        let _guard = LOCK.lock().unwrap();
        crate::secrets::delete(&account(&id));
        let mut all = read(&app);
        all.retain(|e| e.id != id);
        write(&app, &all)
    })
    .await
}

/// Opens a locked bundle with its remembered password. On a device with biometrics the
/// user is asked for them first (`reason` and `cancel` are the prompt's texts), and
/// only if biometric unlock is turned on. Errors: `not_saved`, `biometric_off`,
/// `biometric_cancelled`, `biometric_failed`, `saved_password_wrong`.
#[tauri::command]
pub async fn vault_unlock(
    app: AppHandle,
    path: String,
    allow_newer: bool,
    reason: String,
    cancel: String,
) -> CmdResult<BundleSummary> {
    let handle = app.clone();
    let ws = blocking(move || -> CmdResult<Workspace> {
        let dir = PathBuf::from(&path);
        let mut bundle = Bundle::open(&dir, allow_newer)?;
        let id = bundle.ensure_id()?.to_string();
        let entry = find(&handle, &id).filter(|_| !id.is_empty());
        let Some(entry) = entry else { return Err(CmdError::new("not_saved", "no password is remembered for this bundle")) };
        let bio = crate::biometric::status();
        if bio.supported {
            if !crate::recent::app_settings(&handle).security.biometric_unlock {
                return Err(CmdError::new("biometric_off", "biometric unlock is turned off"));
            }
            match crate::biometric::authenticate(&reason, &cancel) {
                crate::biometric::Outcome::Passed => {}
                crate::biometric::Outcome::Cancelled => return Err(CmdError::new("biometric_cancelled", "cancelled")),
                crate::biometric::Outcome::Failed => {
                    return Err(CmdError::new("biometric_failed", "biometric check failed"));
                }
            }
        }
        let password = crate::secrets::get(&account(&id))
            .ok_or_else(|| CmdError::new("not_saved", "the remembered password is missing"))?;
        let kf = entry.keyfile.as_deref().map(std::fs::read).transpose()?;
        let ws = Workspace::open_locked(&dir, allow_newer, &password, kf.as_deref()).map_err(|e| {
            if e.code == "wrong_key" { CmdError::new("saved_password_wrong", "the remembered password no longer opens this bundle") } else { e }
        })?;
        // Follow the bundle to where it is now.
        if entry.path != path {
            let _guard = LOCK.lock().unwrap();
            let mut all = read(&handle);
            if let Some(e) = all.iter_mut().find(|e| e.id == id) {
                e.path = path.clone();
            }
            let _ = write(&handle, &all);
        }
        Ok(ws)
    })
    .await?;
    install(&app, &app.state::<AppState>(), ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_needs_a_locked_bundle_and_gives_it_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Lib.vpbun");
        let mut b = Bundle::create(&root, "", "0", 1).unwrap();
        assert_eq!(identify(&root).unwrap_err().code, "not_locked");
        let (_, params) = vp_crypto::new_key("pw", None, 64, 1).unwrap();
        b.info.lock = Some(params.clone());
        b.info.id.clear();
        b.write_info().unwrap();
        let (id, name, lock) = identify(&root).unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(name, "Lib");
        assert!(vp_crypto::unlock("pw", None, &lock).is_ok());
        assert_eq!(identify(&root).unwrap().0, id);
    }

    /// Device-test helper: `VP_MAKE_LOCKED=<folder> VP_MAKE_LOCKED_PW=<password> cargo test
    /// -p viewport make_locked_bundle -- --ignored` makes a locked bundle to copy to a phone.
    #[test]
    #[ignore]
    fn make_locked_bundle() {
        let (Ok(path), Ok(pw)) = (std::env::var("VP_MAKE_LOCKED"), std::env::var("VP_MAKE_LOCKED_PW")) else { return };
        let root = PathBuf::from(path);
        let ws = Workspace::create(&root, &root.file_stem().unwrap().to_string_lossy()).unwrap();
        let key = ws.enable_lock(&pw, None).unwrap();
        ws.closed.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(ws);
        crate::workspace::remove_files(&root, true).unwrap();
        drop(key);
        assert_eq!(identify(&root).unwrap().0.len(), 32);
    }
}
