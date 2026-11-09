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

    fn entry(path: String, full: &Path, meta: &Metadata) -> Entry {
        let created_ms = meta.created().ok().and_then(to_ms);
        Entry {
            path,
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            created_ms,
            modified_ms: meta.modified().ok().and_then(to_ms),
            added_ms: if meta.is_dir() { None } else { added_ms(full).or(created_ms) },
        }
    }
}

/// macOS: the "Date Added" Finder shows, when the file was put in its folder.
#[cfg(target_os = "macos")]
fn added_ms(full: &Path) -> Option<i64> {
    use std::ffi::{CString, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    #[repr(C, packed(4))]
    struct Out {
        len: u32,
        sec: i64,
        nsec: i64,
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_ADDEDTIME: u32 = 0x1000_0000;
    unsafe extern "C" {
        fn getattrlist(path: *const c_char, list: *mut c_void, buf: *mut c_void, size: usize, options: u32) -> i32;
    }

    let path = CString::new(full.as_os_str().as_bytes()).ok()?;
    let mut list = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_ADDEDTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut out = Out { len: 0, sec: 0, nsec: 0 };
    // SAFETY: both buffers are valid for the sizes given; the call only writes into `out`.
    let rc = unsafe {
        getattrlist(
            path.as_ptr(),
            (&mut list as *mut AttrList).cast(),
            (&mut out as *mut Out).cast(),
            std::mem::size_of::<Out>(),
            0,
        )
    };
    // A volume without the attribute returns just the length word.
    let (len, sec, nsec) = (out.len, out.sec, out.nsec);
    (rc == 0 && len as usize >= std::mem::size_of::<Out>() && sec > 0).then(|| sec * 1000 + nsec / 1_000_000)
}

/// Elsewhere the birth time stands in: copying or downloading a file creates it anew.
#[cfg(not(target_os = "macos"))]
fn added_ms(_full: &Path) -> Option<i64> {
    None
}

/// macOS: a folder's entries with their size, times and "Date Added" in a few calls
/// (`getattrlistbulk`) instead of two lookups per file.
#[cfg(target_os = "macos")]
mod bulk {
    use std::ffi::c_void;
    use std::fs::{self, File};
    use std::os::fd::AsRawFd;
    use std::path::Path;

    use super::{Entry, LocalDriver, join};

    #[repr(C)]
    struct AttrList {
        bitmapcount: u16,
        reserved: u16,
        commonattr: u32,
        volattr: u32,
        dirattr: u32,
        fileattr: u32,
        forkattr: u32,
    }
    const ATTR_BIT_MAP_COUNT: u16 = 5;
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_CRTIME: u32 = 0x0000_0200;
    const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
    const ATTR_CMN_ADDEDTIME: u32 = 0x1000_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
    const ATTR_FILE_DATALENGTH: u32 = 0x0000_0200;
    const VREG: u32 = 1;
    const VDIR: u32 = 2;

    unsafe extern "C" {
        fn getattrlistbulk(dirfd: i32, list: *mut c_void, buf: *mut c_void, size: usize, options: u64) -> i32;
    }

    /// Reads a value of type `T` at `*pos` (attributes are packed, 4-byte aligned) and
    /// moves past it; `None` past the end of the record.
    fn take<T: Copy>(rec: &[u8], pos: &mut usize) -> Option<T> {
        let end = pos.checked_add(size_of::<T>())?;
        let bytes = rec.get(*pos..end)?;
        *pos = end;
        // SAFETY: `bytes` holds exactly size_of::<T>() bytes; T is plain data.
        Some(unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
    }

    fn ms(sec: i64, nsec: i64) -> i64 {
        sec * 1000 + nsec / 1_000_000
    }

    /// `None` when the bulk call is not available here (the caller lists the usual way).
    pub(super) fn list(full_dir: &Path, dir: &str) -> Option<std::io::Result<Vec<Entry>>> {
        let file = match File::open(full_dir) {
            Ok(f) => f,
            Err(e) => return Some(Err(e)),
        };
        let mut attrs = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS
                | ATTR_CMN_ERROR
                | ATTR_CMN_NAME
                | ATTR_CMN_OBJTYPE
                | ATTR_CMN_CRTIME
                | ATTR_CMN_MODTIME
                | ATTR_CMN_ADDEDTIME,
            volattr: 0,
            dirattr: 0,
            fileattr: ATTR_FILE_DATALENGTH,
            forkattr: 0,
        };
        // u64 words keep the buffer 8-byte aligned.
        let mut buf = vec![0u64; 32 * 1024];
        let mut out = Vec::new();
        let mut first = true;
        loop {
            let n = unsafe {
                getattrlistbulk(
                    file.as_raw_fd(),
                    (&mut attrs as *mut AttrList).cast(),
                    buf.as_mut_ptr().cast(),
                    buf.len() * 8,
                    0,
                )
            };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                return if first { None } else { Some(Err(e)) };
            }
            first = false;
            if n == 0 {
                return Some(Ok(out));
            }
            // SAFETY: the buffer is plain bytes, as long as the u64 words.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
            let mut at = 0usize;
            for _ in 0..n {
                let Some(len) = bytes.get(at..at + 4).map(|b| u32::from_ne_bytes(b.try_into().unwrap()) as usize) else { break };
                let Some(rec) = bytes.get(at..at + len) else { break };
                if let Some(e) = parse(rec, full_dir, dir) {
                    out.push(e);
                }
                at += len.max(4);
            }
        }
    }

    fn parse(rec: &[u8], full_dir: &Path, dir: &str) -> Option<Entry> {
        let mut pos = 4;
        let common: u32 = take(rec, &mut pos)?;
        let _vol: u32 = take(rec, &mut pos)?;
        let _dir: u32 = take(rec, &mut pos)?;
        let file_attrs: u32 = take(rec, &mut pos)?;
        let _fork: u32 = take(rec, &mut pos)?;
        if common & ATTR_CMN_ERROR != 0 {
            let _err: u32 = take(rec, &mut pos)?;
        }
        let name = if common & ATTR_CMN_NAME != 0 {
            let start = pos;
            let off: i32 = take(rec, &mut pos)?;
            let len: u32 = take(rec, &mut pos)?;
            let from = (start as i64 + off as i64) as usize;
            let raw = rec.get(from..from + len as usize)?;
            let raw = raw.split(|&b| b == 0).next().unwrap_or(raw);
            std::str::from_utf8(raw).ok()?.to_string() // skip non-UTF-8 names
        } else {
            return None;
        };
        if name == "." || name == ".." {
            return None;
        }
        let kind: Option<u32> = if common & ATTR_CMN_OBJTYPE != 0 { Some(take(rec, &mut pos)?) } else { None };
        let mut time = |bit: u32| -> Option<Option<i64>> {
            if common & bit == 0 {
                return Some(None);
            }
            let sec: i64 = take(rec, &mut pos)?;
            let nsec: i64 = take(rec, &mut pos)?;
            Some(Some(ms(sec, nsec)))
        };
        let created_ms = time(ATTR_CMN_CRTIME)?;
        let modified_ms = time(ATTR_CMN_MODTIME)?;
        let added = time(ATTR_CMN_ADDEDTIME)?.filter(|&t| t > 0);
        let size: Option<i64> = if file_attrs & ATTR_FILE_DATALENGTH != 0 { Some(take(rec, &mut pos)?) } else { None };
        let path = join(dir, &name);
        match kind {
            Some(VDIR) => Some(Entry { path, is_dir: true, size: 0, created_ms, modified_ms, added_ms: None }),
            Some(VREG) => Some(Entry {
                path,
                is_dir: false,
                size: size.unwrap_or(0).max(0) as u64,
                created_ms,
                modified_ms,
                added_ms: added.or(created_ms),
            }),
            // Links (followed, as `fs::metadata` does) and anything unexpected: look it up.
            _ => {
                let full = full_dir.join(&name);
                let meta = fs::metadata(&full).ok()?;
                Some(LocalDriver::entry(path, &full, &meta))
            }
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
        let full_dir = self.resolve(dir)?;
        #[cfg(target_os = "macos")]
        if let Some(out) = bulk::list(&full_dir, dir) {
            return out.map_err(|e| not_found(e, dir));
        }
        let mut out = Vec::new();
        for item in fs::read_dir(&full_dir).map_err(|e| not_found(e, dir))? {
            let item = item?;
            let Ok(name) = item.file_name().into_string() else { continue }; // skip non-UTF-8 names
            let full = item.path();
            // The entry's own metadata comes with the listing (Windows) or from a lookup
            // relative to the open folder (Unix): cheaper than resolving the path again.
            // A link is followed, as before.
            let meta = match item.metadata() {
                Ok(m) if !m.file_type().is_symlink() => m,
                _ => match fs::metadata(&full) {
                    Ok(m) => m,
                    Err(_) => continue,
                },
            };
            out.push(Self::entry(join(dir, &name), &full, &meta));
        }
        Ok(out)
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        let full = self.resolve(path)?;
        let meta = fs::metadata(&full).map_err(|e| not_found(e, path))?;
        Ok(Self::entry(path.trim_matches('/').to_string(), &full, &meta))
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
        let target = self.resolve(to)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(self.resolve(from)?, target).map_err(|e| not_found(e, from))
    }

    fn delete(&self, path: &str) -> Result<()> {
        fs::remove_file(self.resolve(path)?).map_err(|e| not_found(e, path))
    }
}

/// Writes via a temp file + rename so readers never see a partial file. The data reaches
/// the disk before the rename, so a power cut cannot leave an empty file in place of the
/// old one.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or_default()
    ));
    {
        let mut f = File::create(&tmp)?;
        std::io::Write::write_all(&mut f, data)?;
        f.sync_all()?;
    }
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

        // Moving into a folder that does not exist yet creates it.
        d.rename("sub/a.jpg", "new/deep/b.jpg").unwrap();
        assert_eq!(d.stat("new/deep/b.jpg").unwrap().size, 10);
        assert!(matches!(d.stat("sub/a.jpg"), Err(DriverError::NotFound(_))));

        d.write("new/b.txt", b"hi").unwrap();
        assert_eq!(fs::read(dir.path().join("new/b.txt")).unwrap(), b"hi");
    }

    /// The listing gives each entry what a lookup of its path would (links followed).
    #[test]
    fn list_matches_stat() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("folder")).unwrap();
        fs::write(root.join("a.jpg"), vec![7u8; 12345]).unwrap();
        fs::write(root.join("한글 이름.png"), b"x").unwrap();
        fs::write(root.join("folder/inner.jpg"), b"yy").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("a.jpg"), root.join("link.jpg")).unwrap();
            std::os::unix::fs::symlink(root.join("folder"), root.join("linked")).unwrap();
        }
        for i in 0..3000 {
            fs::write(root.join(format!("many_{i:05}_with_a_rather_long_name_to_fill_buffers.jpg")), b"").unwrap();
        }
        let d = LocalDriver::new(root);
        let mut listed = d.list("").unwrap();
        listed.sort_by(|a, b| a.path.cmp(&b.path));
        let mut names: Vec<String> = fs::read_dir(root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(listed.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), names);
        for e in &listed {
            assert_eq!(e, &d.stat(&e.path).unwrap(), "{}", e.path);
        }
        assert_eq!(listed.iter().find(|e| e.path == "a.jpg").unwrap().size, 12345);
        #[cfg(unix)]
        {
            let link = listed.iter().find(|e| e.path == "link.jpg").unwrap();
            assert!(!link.is_dir);
            assert_eq!(link.size, 12345);
            assert!(listed.iter().find(|e| e.path == "linked").unwrap().is_dir);
        }
        assert_eq!(d.list("folder").unwrap()[0].path, "folder/inner.jpg");
        #[cfg(target_os = "macos")]
        assert_eq!(bulk::list(root, "").expect("bulk listing works here").unwrap().len(), listed.len());
        assert!(matches!(d.list("nope"), Err(DriverError::NotFound(_))));
    }
}
