//! Storage driver abstraction.
//!
//! Every place media can live (local folder, SMB share, later WebDAV/FTP, VeraCrypt
//! volumes and bundle shards) implements [`MediaDriver`]. A [`DriverFactory`] describes
//! its settings with [`ConfigField`]s so the UI can build the connection form, and
//! creates connected drivers from a root plus a JSON config.
//!
//! The API is blocking; the app calls it from a worker pool. Drivers backed by async
//! protocols block on their own runtime internally.

pub mod local;

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("path '{0}' escapes the source root")]
    PathEscapesRoot(String),
    #[error("operation not supported by the '{0}' driver")]
    Unsupported(&'static str),
    #[error("unknown driver '{0}'")]
    UnknownDriver(String),
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("authentication failed")]
    Auth,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, DriverError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// '/'-separated path relative to the source root.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
}

impl Entry {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    pub write: bool,
    /// Can report changes without rescanning.
    pub watch: bool,
    /// Reads at arbitrary offsets are cheap (no full download needed to seek).
    pub cheap_seek: bool,
}

/// Positional reader over one file. `read_at` takes `&self` so one handle can serve
/// concurrent range requests (video seeking while the cache prefetches).
pub trait MediaReader: Send + Sync {
    fn len(&self) -> u64;
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Fills `buf` completely unless EOF is reached; returns bytes read.
    fn read_full_at(&self, mut offset: u64, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let mut total = 0;
        while !buf.is_empty() {
            let n = self.read_at(offset, buf)?;
            if n == 0 {
                break;
            }
            total += n;
            offset += n as u64;
            buf = &mut buf[n..];
        }
        Ok(total)
    }
}

pub trait MediaDriver: Send + Sync {
    /// The factory kind that created this driver (`local`, `smb`, ...).
    fn kind(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    /// Direct children of `dir` (`""` = root).
    fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    fn stat(&self, path: &str) -> Result<Entry>;
    fn open(&self, path: &str) -> Result<Box<dyn MediaReader>>;

    fn write(&self, _path: &str, _data: &[u8]) -> Result<()> {
        Err(DriverError::Unsupported(self.kind()))
    }
    fn rename(&self, _from: &str, _to: &str) -> Result<()> {
        Err(DriverError::Unsupported(self.kind()))
    }
    fn delete(&self, _path: &str) -> Result<()> {
        Err(DriverError::Unsupported(self.kind()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConfigKind {
    Text,
    /// Stored in the OS keychain or the encrypted bundle, never in plain settings.
    Secret,
    Integer { min: Option<i64>, max: Option<i64> },
    Bool,
    /// A local file chosen with a picker (key files, volume images).
    FilePath,
    Choice { options: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigField {
    pub key: String,
    /// i18n key for the label; the UI resolves it.
    pub label: String,
    pub kind: ConfigKind,
    pub required: bool,
    pub default: Option<serde_json::Value>,
}

impl ConfigField {
    pub fn new(key: &str, kind: ConfigKind, required: bool) -> Self {
        Self { key: key.into(), label: format!("driver.field.{key}"), kind, required, default: None }
    }

    pub fn default_value(mut self, v: serde_json::Value) -> Self {
        self.default = Some(v);
        self
    }
}

pub trait DriverFactory: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Settings this driver accepts, beyond the root.
    fn config_schema(&self) -> Vec<ConfigField>;
    fn connect(&self, root: &str, config: &serde_json::Value) -> Result<Arc<dyn MediaDriver>>;
}

#[derive(Clone, Default)]
pub struct DriverRegistry {
    factories: HashMap<&'static str, Arc<dyn DriverFactory>>,
}

impl DriverRegistry {
    pub fn with_builtin() -> Self {
        let mut r = Self::default();
        r.register(Arc::new(local::LocalFactory));
        r
    }

    pub fn register(&mut self, f: Arc<dyn DriverFactory>) {
        self.factories.insert(f.kind(), f);
    }

    pub fn get(&self, kind: &str) -> Result<&Arc<dyn DriverFactory>> {
        self.factories.get(kind).ok_or_else(|| DriverError::UnknownDriver(kind.into()))
    }

    pub fn kinds(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.factories.keys().copied()
    }
}

/// Validates a root-relative path: '/'-separated, no `..`, no absolute or drive prefixes.
/// Returns the normalized components.
pub fn checked_components(path: &str) -> Result<Vec<&str>> {
    let mut out = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return Err(DriverError::PathEscapesRoot(path.into())),
            p if p.contains(':') => return Err(DriverError::PathEscapesRoot(path.into())),
            p => out.push(p),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        assert_eq!(checked_components("a/./b\\c/").unwrap(), ["a", "b", "c"]);
        assert!(checked_components("a/../../etc").is_err());
        assert!(checked_components("C:/Windows").is_err());
        assert_eq!(checked_components("/abs").unwrap(), ["abs"]);
    }
}
