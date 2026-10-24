//! Driver over a shard store (`.vpshard`, see `vp-shard`).
//!
//! The app opens shards itself (it manages their keys and write locks) and wraps them in
//! [`ShardDriver`]; [`ShardFactory`] connects from a root path plus a password or a
//! remembered master key, like any other driver.

use std::path::Path;
use std::sync::Arc;

use vp_shard::{EntryInfo, OpenOptions, Shard, ShardError, ShardReader};

use crate::{Capabilities, ConfigField, ConfigKind, DriverError, DriverFactory, Entry, MediaDriver, MediaReader, Result};

pub const KIND: &str = "shard";

impl From<ShardError> for DriverError {
    fn from(e: ShardError) -> Self {
        match e {
            ShardError::NotFound(p) => DriverError::NotFound(p),
            ShardError::InvalidPath(p) => DriverError::PathEscapesRoot(p),
            ShardError::ReadOnly => DriverError::Unsupported(KIND),
            ShardError::Locked | ShardError::WrongKey => DriverError::Auth,
            ShardError::Io(e) => DriverError::Io(e),
            ShardError::NotAShard(p) => DriverError::InvalidConfig(format!("'{}' is not a shard", p.display())),
            other => DriverError::Io(std::io::Error::other(other.to_string())),
        }
    }
}

pub struct ShardDriver {
    shard: Arc<Shard>,
}

impl ShardDriver {
    pub fn new(shard: Arc<Shard>) -> Self {
        Self { shard }
    }

    pub fn shard(&self) -> &Arc<Shard> {
        &self.shard
    }
}

fn entry(e: EntryInfo) -> Entry {
    Entry { path: e.path, is_dir: e.is_dir, size: e.size, created_ms: e.created_ms, modified_ms: e.modified_ms, added_ms: None }
}

struct Reader(ShardReader);

impl MediaReader for Reader {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_at(offset, buf)
    }
}

impl MediaDriver for ShardDriver {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { write: self.shard.is_writable(), watch: false, cheap_seek: true }
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        // The app's own files (item properties) are not part of the library.
        Ok(self.shard.list(dir)?.into_iter().filter(|e| !vp_shard::is_reserved(&e.path)).map(entry).collect())
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        Ok(entry(self.shard.stat(path)?))
    }

    fn open(&self, path: &str) -> Result<Box<dyn MediaReader>> {
        Ok(Box::new(Reader(self.shard.open_file(path)?)))
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .ok();
        Ok(self.shard.put(path, data, now, now)?)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.shard.rename(from, to)?;
        Ok(())
    }

    fn delete(&self, path: &str) -> Result<()> {
        self.shard.delete(path)?;
        Ok(())
    }
}

/// Wraps a [`ShardReader`] as a driver reader.
pub fn media_reader(r: ShardReader) -> Box<dyn MediaReader> {
    Box::new(Reader(r))
}

pub struct ShardFactory;

impl DriverFactory for ShardFactory {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        vec![ConfigField::new("password", ConfigKind::Secret, false), ConfigField::new("keyfile", ConfigKind::FilePath, false)]
    }

    fn connect(&self, root: &str, config: &serde_json::Value) -> Result<Arc<dyn MediaDriver>> {
        let root = Path::new(root);
        let info = Shard::read_info(root)?;
        let key = match &info.encryption {
            None => None,
            Some(_) => {
                if let Some(hex) = config.get("key").and_then(|v| v.as_str()) {
                    Some(vp_crypto::BundleKey::from_hex(hex).map_err(|_| DriverError::Auth)?)
                } else {
                    let password = config.get("password").and_then(|v| v.as_str()).unwrap_or_default();
                    let keyfile = match config.get("keyfile").and_then(|v| v.as_str()).filter(|k| !k.is_empty()) {
                        Some(p) => Some(std::fs::read(p)?),
                        None => None,
                    };
                    Some(Shard::unlock(&info, password, keyfile.as_deref())?)
                }
            }
        };
        Ok(Arc::new(ShardDriver::new(Arc::new(Shard::open(root, key, OpenOptions::default())?))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_over_a_shard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("s.vpshard");
        let opts = vp_shard::CreateOptions {
            password: Some("pw".into()),
            kdf_memory_kib: 64,
            kdf_iterations: 1,
            ..Default::default()
        };
        drop(Shard::create_and_open(&root, &opts, OpenOptions::default()).unwrap());
        let root_s = root.display().to_string();
        assert!(matches!(ShardFactory.connect(&root_s, &serde_json::json!({ "password": "no" })), Err(DriverError::Auth)));
        let d = ShardFactory.connect(&root_s, &serde_json::json!({ "password": "pw" })).unwrap();
        assert!(d.capabilities().write);
        d.write("a/b.jpg", b"hello").unwrap();
        let listed = d.list("").unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].is_dir);
        let r = d.open("a/b.jpg").unwrap();
        let mut buf = [0u8; 5];
        assert_eq!(r.read_full_at(0, &mut buf).unwrap(), 5);
        assert_eq!(&buf, b"hello");
        d.rename("a/b.jpg", "c.jpg").unwrap();
        assert!(matches!(d.stat("a/b.jpg"), Err(DriverError::NotFound(_))));
        d.delete("c.jpg").unwrap();
        assert!(d.list("").unwrap().is_empty());
    }
}
