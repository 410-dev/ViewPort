//! Positional file I/O that works the same on every platform.

use std::fs::File;
use std::io;
use std::path::Path;

#[cfg(unix)]
pub fn read_at(f: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, off)
}

#[cfg(windows)]
pub fn read_at(f: &File, off: u64, buf: &mut [u8]) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, off)
}

pub fn read_exact_at(f: &File, mut off: u64, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(f, off, buf) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                off += n as u64;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub fn write_all_at(f: &File, off: u64, buf: &[u8]) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}

#[cfg(windows)]
pub fn write_all_at(f: &File, mut off: u64, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_write(f, buf, off) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                off += n as u64;
                buf = &buf[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Writes through a temporary file and renames it over `path`, so readers see either the
/// old or the new contents.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let f = File::create(&tmp)?;
        write_all_at(&f, 0, data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}
