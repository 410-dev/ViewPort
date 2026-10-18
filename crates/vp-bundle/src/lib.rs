//! Project bundle (`.vpbun`) layout and opening rules.
//!
//! A bundle is a directory holding the whole workspace: database, settings, thumbnails,
//! statistics and optionally the media itself. `info.json` is always plain text and holds
//! only information that is safe to disclose.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vp_core::BUNDLE_FORMAT_VERSION;

pub const EXTENSION: &str = "vpbun";
pub const INFO_FILE: &str = "info.json";
pub const DB_FILE: &str = "db/library.sqlite";
pub const SETTINGS_FILE: &str = "settings.json";
/// Encrypted forms used by locked bundles.
pub const DB_ENC_FILE: &str = "db/library.sqlite.enc";
pub const SETTINGS_ENC_FILE: &str = "settings.json.enc";
pub const THUMBS_DIR: &str = "thumbs";
pub const STATS_DIR: &str = "stats";
/// Media stored inside the bundle; referenced by paths relative to the bundle root.
pub const MEDIA_DIR: &str = "media";

#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("'{0}' is not a ViewPort bundle (no {INFO_FILE})")]
    NotABundle(PathBuf),
    #[error("'{0}' already exists")]
    AlreadyExists(PathBuf),
    #[error("bundle format v{found} is newer than this build supports (v{supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("compressed bundles are not supported yet")]
    ArchiveNotSupported,
    #[error("invalid {INFO_FILE}: {0}")]
    InvalidInfo(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, BundleError>;

/// Key derivation parameters for a locked bundle. Public by design: they are needed to
/// derive the key and reveal nothing about it.
pub use vp_crypto::KdfParams;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleInfo {
    /// Format version of the bundle. Required.
    pub bundle_version: u32,
    /// App version that last wrote the bundle.
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub created_ms: i64,
    /// Present when the workspace is encrypted.
    #[serde(default)]
    pub lock: Option<KdfParams>,
}

/// How a bundle may be used once opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpenMode {
    /// Normal: every change is saved automatically.
    Autosave,
    /// Bundle is newer than this build and the user allowed loading it anyway. Nothing is
    /// written automatically so an older build cannot corrupt newer data; saving is explicit.
    ManualSaveOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    Supported,
    /// Newer than this build; may be opened only with the user's consent.
    Newer { found: u32, supported: u32 },
}

pub fn check_compat(info: &BundleInfo) -> Compat {
    if info.bundle_version > BUNDLE_FORMAT_VERSION {
        Compat::Newer { found: info.bundle_version, supported: BUNDLE_FORMAT_VERSION }
    } else {
        Compat::Supported
    }
}

#[derive(Debug, Clone)]
pub struct Bundle {
    pub root: PathBuf,
    pub info: BundleInfo,
    pub mode: OpenMode,
}

impl Bundle {
    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn is_locked(&self) -> bool {
        self.info.lock.is_some()
    }

    /// Creates a new, empty bundle directory.
    pub fn create(root: &Path, name: &str, app_version: &str, now_ms: i64) -> Result<Bundle> {
        if root.exists() && fs::read_dir(root)?.next().is_some() {
            return Err(BundleError::AlreadyExists(root.into()));
        }
        for dir in ["db", THUMBS_DIR, STATS_DIR] {
            fs::create_dir_all(root.join(dir))?;
        }
        let info = BundleInfo {
            bundle_version: BUNDLE_FORMAT_VERSION,
            app_version: app_version.into(),
            name: name.into(),
            created_ms: now_ms,
            lock: None,
        };
        let b = Bundle { root: root.into(), info, mode: OpenMode::Autosave };
        b.write_info()?;
        Ok(b)
    }

    /// Reads `info.json` without opening anything else.
    pub fn read_info(root: &Path) -> Result<BundleInfo> {
        if root.is_file() {
            return Err(BundleError::ArchiveNotSupported);
        }
        let text = match fs::read_to_string(root.join(INFO_FILE)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(BundleError::NotABundle(root.into())),
            Err(e) => return Err(e.into()),
        };
        Ok(serde_json::from_str(&text)?)
    }

    /// Opens a bundle. A bundle newer than this build is refused unless
    /// `allow_newer` is set (after asking the user), in which case autosave is off.
    pub fn open(root: &Path, allow_newer: bool) -> Result<Bundle> {
        let info = Self::read_info(root)?;
        let mode = match check_compat(&info) {
            Compat::Supported => OpenMode::Autosave,
            Compat::Newer { .. } if allow_newer => OpenMode::ManualSaveOnly,
            Compat::Newer { found, supported } => return Err(BundleError::TooNew { found, supported }),
        };
        Ok(Bundle { root: root.into(), info, mode })
    }

    pub fn write_info(&self) -> Result<()> {
        let path = self.root.join(INFO_FILE);
        let tmp = self.root.join(format!("{INFO_FILE}.tmp"));
        fs::write(&tmp, serde_json::to_vec_pretty(&self.info)?)?;
        fs::rename(tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(format!("Library.{EXTENSION}"));
        Bundle::create(&root, "Library", "0.1.0", 1).unwrap();
        assert!(root.join("db").is_dir());
        let b = Bundle::open(&root, false).unwrap();
        assert_eq!(b.mode, OpenMode::Autosave);
        assert_eq!(b.info.name, "Library");
        assert!(matches!(Bundle::create(&root, "x", "0.1.0", 1), Err(BundleError::AlreadyExists(_))));
    }

    #[test]
    fn newer_bundle_needs_consent() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(INFO_FILE), format!(r#"{{"bundle_version": {}}}"#, BUNDLE_FORMAT_VERSION + 1))
            .unwrap();
        assert!(matches!(Bundle::open(dir.path(), false), Err(BundleError::TooNew { .. })));
        assert_eq!(Bundle::open(dir.path(), true).unwrap().mode, OpenMode::ManualSaveOnly);
    }

    #[test]
    fn missing_info_or_version() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(Bundle::open(dir.path(), false), Err(BundleError::NotABundle(_))));
        fs::write(dir.path().join(INFO_FILE), "{}").unwrap();
        assert!(matches!(Bundle::open(dir.path(), false), Err(BundleError::InvalidInfo(_))));
    }
}
