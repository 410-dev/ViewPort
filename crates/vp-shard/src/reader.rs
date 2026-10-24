//! Random-access reads of a stored item.

use std::fs::File;
use std::io;
use std::sync::{Arc, Mutex};

use vp_crypto::{SegmentCipher, TAG_LEN};

use crate::format::{Id, Stream, segment_aad, segment_nonce};
use crate::index::Item;
use crate::io::{read_at, read_exact_at};

/// Reads one item. Positional and `&self`, so one reader can serve concurrent range
/// requests. It keeps its chunk files open, so it stays valid while compaction moves
/// the item elsewhere.
pub struct ShardReader {
    pub(crate) item: Item,
    pub(crate) files: Vec<Arc<File>>,
    pub(crate) stream: Stream,
    pub(crate) shard: Id,
    pub(crate) cipher: Option<Arc<SegmentCipher>>,
    pub(crate) seg: u64,
    /// The last decrypted segment, for sequential small reads.
    pub(crate) cache: Mutex<Option<(u64, Arc<Vec<u8>>)>>,
}

impl ShardReader {
    pub fn len(&self) -> u64 {
        self.item.size
    }

    pub fn is_empty(&self) -> bool {
        self.item.size == 0
    }

    pub fn item(&self) -> &Item {
        &self.item
    }

    pub fn attrs(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.item.attrs
    }

    fn part_at(&self, off: u64) -> Option<usize> {
        let i = self.item.parts.partition_point(|p| p.off + p.len <= off);
        (i < self.item.parts.len()).then_some(i)
    }

    /// Decrypts segment `seg` (global number) of part `pi`.
    fn segment(&self, pi: usize, seg: u64) -> io::Result<Arc<Vec<u8>>> {
        if let Some((n, data)) = &*self.cache.lock().unwrap()
            && *n == seg
        {
            return Ok(data.clone());
        }
        let cipher = self.cipher.as_ref().expect("encrypted reader");
        let part = &self.item.parts[pi];
        let local = seg - part.off / self.seg;
        let plain_len = self.seg.min(part.off + part.len - seg * self.seg);
        let mut buf = vec![0u8; (plain_len + TAG_LEN as u64) as usize];
        read_exact_at(&self.files[pi], part.at + local * (self.seg + TAG_LEN as u64), &mut buf)?;
        let last_seg = if self.item.size == 0 { 0 } else { (self.item.size - 1) / self.seg };
        let nonce = self.item.nonce.ok_or_else(|| io::Error::other("missing nonce"))?;
        let plain = cipher
            .open(
                &segment_nonce(nonce, seg),
                &buf,
                &segment_aad(self.shard, self.stream, self.item.body_id(), seg, seg == last_seg),
            )
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "shard data is damaged or was tampered with"))?;
        let plain = Arc::new(plain);
        *self.cache.lock().unwrap() = Some((seg, plain.clone()));
        Ok(plain)
    }

    /// Reads at `off`; returns 0 at the end. May return fewer bytes than asked.
    pub fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        if off >= self.item.size || buf.is_empty() {
            return Ok(0);
        }
        let Some(pi) = self.part_at(off) else { return Ok(0) };
        let part = &self.item.parts[pi];
        if self.cipher.is_none() {
            let want = buf.len().min((part.off + part.len - off) as usize);
            return read_at(&self.files[pi], part.at + (off - part.off), &mut buf[..want]);
        }
        let seg = off / self.seg;
        let data = self.segment(pi, seg)?;
        let start = (off - seg * self.seg) as usize;
        let n = buf.len().min(data.len() - start);
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    pub fn read_full_at(&self, mut off: u64, mut buf: &mut [u8]) -> io::Result<usize> {
        let mut total = 0;
        while !buf.is_empty() {
            let n = self.read_at(off, buf)?;
            if n == 0 {
                break;
            }
            total += n;
            off += n as u64;
            buf = &mut buf[n..];
        }
        Ok(total)
    }

    pub fn read_all(&self) -> io::Result<Vec<u8>> {
        let mut out = vec![0u8; self.item.size as usize];
        let n = self.read_full_at(0, &mut out)?;
        if n < out.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        Ok(out)
    }
}

/// Sequential `Read` over a [`ShardReader`].
pub struct ShardStream {
    reader: ShardReader,
    pos: u64,
}

impl ShardStream {
    pub fn new(reader: ShardReader) -> Self {
        Self { reader, pos: 0 }
    }
}

impl io::Read for ShardStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read_at(self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}
