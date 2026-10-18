//! Append-only pack files: `000000.pack`, `000001.pack`, ...

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Writer {
    pack: u32,
    file: File,
    len: u64,
}

pub struct PackStore {
    dir: PathBuf,
    pack_bytes: u64,
    writer: Mutex<Option<Writer>>,
    readers: Mutex<HashMap<u32, Arc<File>>>,
}

fn pack_name(pack: u32) -> String {
    format!("{pack:06}.pack")
}

impl PackStore {
    /// Opens the store in `dir`, continuing the newest pack. `pack_bytes` is the size at
    /// which a new pack is started.
    pub fn open(dir: &Path, pack_bytes: u64) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.into(), pack_bytes: pack_bytes.max(1), writer: Mutex::new(None), readers: Mutex::default() })
    }

    fn newest_pack(&self) -> std::io::Result<Option<u32>> {
        let mut newest = None;
        for e in fs::read_dir(&self.dir)? {
            let name = e?.file_name();
            let Some(n) = name.to_str().and_then(|n| n.strip_suffix(".pack")).and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            newest = newest.max(Some(n));
        }
        Ok(newest)
    }

    fn open_writer(&self, pack: u32) -> std::io::Result<Writer> {
        let file = OpenOptions::new().create(true).append(true).open(self.dir.join(pack_name(pack)))?;
        let len = file.metadata()?.len();
        Ok(Writer { pack, file, len })
    }

    /// Appends `data` and returns `(pack, offset)`.
    pub fn append(&self, data: &[u8]) -> std::io::Result<(u32, u64)> {
        let mut guard = self.writer.lock().unwrap();
        if guard.is_none() {
            let pack = self.newest_pack()?.unwrap_or(0);
            *guard = Some(self.open_writer(pack)?);
        }
        let w = guard.as_mut().unwrap();
        if w.len > 0 && w.len + data.len() as u64 > self.pack_bytes {
            *w = self.open_writer(w.pack + 1)?;
        }
        let offset = w.len;
        w.file.write_all(data)?;
        w.len += data.len() as u64;
        Ok((w.pack, offset))
    }

    pub fn read(&self, pack: u32, offset: u64, len: u32) -> std::io::Result<Vec<u8>> {
        let file = {
            let mut readers = self.readers.lock().unwrap();
            match readers.get(&pack) {
                Some(f) => f.clone(),
                None => {
                    let f = Arc::new(File::open(self.dir.join(pack_name(pack)))?);
                    readers.insert(pack, f.clone());
                    f
                }
            }
        };
        let mut buf = vec![0u8; len as usize];
        let mut done = 0;
        while done < buf.len() {
            let n = read_at(&file, &mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(buf)
    }

    /// Flushes pending writes to disk.
    pub fn sync(&self) -> std::io::Result<()> {
        if let Some(w) = self.writer.lock().unwrap().as_ref() {
            w.file.sync_data()?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_rotate_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = PackStore::open(dir.path(), 10).unwrap();
        assert_eq!(store.append(b"hello").unwrap(), (0, 0));
        assert_eq!(store.append(b"abc").unwrap(), (0, 5));
        assert_eq!(store.append(b"toolong").unwrap(), (1, 0), "rotates past pack_bytes");
        assert_eq!(store.read(0, 5, 3).unwrap(), b"abc");
        assert_eq!(store.read(1, 0, 7).unwrap(), b"toolong");
        assert!(store.read(0, 6, 10).is_err());
        drop(store);

        let store = PackStore::open(dir.path(), 10).unwrap();
        assert_eq!(store.append(b"x").unwrap(), (1, 7), "continues the newest pack");
    }
}
