//! Local file system driver.

use std::fs::{self, File, Metadata};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::{
    Capabilities, ConfigField, DriverError, DriverFactory, Entry, MediaDriver, MediaReader, Result,
    checked_components,
};

pub struct LocalFactory;

impl DriverFactory for LocalFactory {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn config_schema(&self) -> Vec<ConfigField> {
        Vec::new()
    }

    fn connect(&self, root: &str, _config: &serde_json::Value) -> Result<Arc<dyn MediaDriver>> {
        let root = PathBuf::from(root);
        if !root.is_dir() {
            return Err(DriverError::NotFound(root.display().to_string()));
        }
        Ok(Arc::new(LocalDriver { root }))
    }
}

pub struct LocalDriver {
    root: PathBuf,
}

impl LocalDriver {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let mut p = self.root.clone();
        p.extend(checked_components(path)?);
        Ok(p)
    }

    fn entry(path: String, meta: &Metadata) -> Entry {
        Entry {
            path,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            created_ms: meta.created().ok().and_then(to_ms),
            modified_ms: meta.modified().ok().and_then(to_ms),
        }
    }
}

fn to_ms(t: SystemTime) -> Option<i64> {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_millis()).ok(),
        Err(e) => i64::try_from(e.duration().as_millis()).ok().map(|ms| -ms),
    }
}

fn not_found(e: std::io::Error, path: &str) -> DriverError {
    if e.kind() == std::io::ErrorKind::NotFound { DriverError::NotFound(path.into()) } else { e.into() }
}

fn join(dir: &str, name: &str) -> String {
    let dir = dir.trim_matches('/');
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

impl MediaDriver for LocalDriver {
    fn kind(&self) -> &'static str {
        "local"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities { write: true, watch: true, cheap_seek: true }
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        for item in fs::read_dir(self.resolve(dir)?).map_err(|e| not_found(e, dir))? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else { continue }; // skip non-UTF-8 names
            // Follow symlinks so linked folders and files behave like real ones.
            let Ok(meta) = fs::metadata(item.path()) else { continue };
            out.push(Self::entry(join(dir, &name), &meta));
        }
        Ok(out)
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        let meta = fs::metadata(self.resolve(path)?).map_err(|e| not_found(e, path))?;
        Ok(Self::entry(path.trim_matches('/').to_string(), &meta))
    }

    fn open(&self, path: &str) -> Result<Box<dyn MediaReader>> {
        let full = self.resolve(path)?;
        let file = File::open(&full).map_err(|e| not_found(e, path))?;
        let len = file.metadata()?.len();
        Ok(Box::new(LocalReader { file, len }))
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.resolve(path).ok()
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<()> {
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&full, data)?;
        Ok(())
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        fs::rename(self.resolve(from)?, self.resolve(to)?).map_err(|e| not_found(e, from))
    }

    fn delete(&self, path: &str) -> Result<()> {
        fs::remove_file(self.resolve(path)?).map_err(|e| not_found(e, path))
    }
}

/// Writes via a temp file + rename so readers never see a partial file.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or_default()
    ));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

/// A positional reader over a local file outside any source (e.g. a converted copy).
pub fn open_file(path: &Path) -> std::io::Result<Box<dyn MediaReader>> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    Ok(Box::new(LocalReader { file, len }))
}

struct LocalReader {
    file: File,
    len: u64,
}

impl MediaReader for LocalReader {
    fn len(&self) -> u64 {
        self.len
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(&self.file, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(&self.file, buf, offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_stat_read() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/a.jpg"), b"0123456789").unwrap();
        let d = LocalFactory.connect(dir.path().to_str().unwrap(), &serde_json::json!({})).unwrap();

        let root = d.list("").unwrap();
        assert_eq!(root.len(), 1);
        assert!(root[0].is_dir);
        let sub = d.list("sub").unwrap();
        assert_eq!(sub[0].path, "sub/a.jpg");
        assert_eq!(sub[0].name(), "a.jpg");
        assert_eq!(d.stat("sub/a.jpg").unwrap().size, 10);

        let r = d.open("sub/a.jpg").unwrap();
        let mut buf = [0u8; 4];
        assert_eq!(r.read_full_at(6, &mut buf).unwrap(), 4);
        assert_eq!(&buf, b"6789");
        assert_eq!(r.read_full_at(8, &mut buf).unwrap(), 2);

        assert!(matches!(d.open("../x"), Err(DriverError::PathEscapesRoot(_))));
        assert!(matches!(d.stat("nope.jpg"), Err(DriverError::NotFound(_))));

        d.write("new/b.txt", b"hi").unwrap();
        assert_eq!(fs::read(dir.path().join("new/b.txt")).unwrap(), b"hi");
    }
}
