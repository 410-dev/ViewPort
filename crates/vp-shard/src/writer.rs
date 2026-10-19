//! Streaming writes of one item into a stream's active chunk.

use std::sync::MutexGuard;

use sha2::{Digest as _, Sha256};

use crate::format::{Digest, Id, META_RESERVE, Meta, PartMeta, RECORD_HEADER_LEN, RecordHeader, Stream, segment_aad, segment_nonce};
use crate::index::{Item, Part};
use crate::io::write_all_at;
use crate::{Result, Shard, StreamWriter};

pub(crate) enum Target {
    File { path: String, created_ms: Option<i64>, modified_ms: Option<i64> },
    Blob { owner_path: String, owner: Id, attrs: serde_json::Map<String, serde_json::Value> },
}

struct OpenPart {
    chunk: u32,
    rec: u64,
    plain_start: u64,
    body: u64,
    plain: u64,
}

/// Writes one item. Data is pushed with [`ItemWriter::write_all`] (or `std::io::Write`)
/// and becomes visible on [`ItemWriter::finish`]. Dropping it unfinished discards what
/// was written. Holds the stream's write lock for its lifetime.
pub struct ItemWriter<'a> {
    pub(crate) shard: &'a Shard,
    pub(crate) stream: Stream,
    pub(crate) w: MutexGuard<'a, Option<StreamWriter>>,
    pub(crate) target: Target,
    pub(crate) id: Id,
    pub(crate) nonce: Option<Id>,
    pub(crate) seq: u64,
    pub(crate) seg: usize,
    pub(crate) pending: Vec<u8>,
    pub(crate) seg_index: u64,
    pub(crate) plain_done: u64,
    pub(crate) part_idx: u32,
    part: Option<OpenPart>,
    pub(crate) parts: Vec<Part>,
    pub(crate) hasher: Sha256,
    pub(crate) finished: bool,
}

impl<'a> ItemWriter<'a> {
    pub(crate) fn new(
        shard: &'a Shard,
        stream: Stream,
        w: MutexGuard<'a, Option<StreamWriter>>,
        target: Target,
        seq: u64,
    ) -> Result<Self> {
        let seg = shard.segment_bytes() as usize;
        Ok(Self {
            shard,
            stream,
            w,
            target,
            id: Id::random()?,
            nonce: if shard.cipher.is_some() { Some(Id::random()?) } else { None },
            seq,
            seg,
            pending: Vec::with_capacity(seg),
            seg_index: 0,
            plain_done: 0,
            part_idx: 0,
            part: None,
            parts: Vec::new(),
            hasher: Sha256::new(),
            finished: false,
        })
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<()> {
        while !data.is_empty() {
            // A full segment is sealed only once more data follows, so the final one can
            // be marked as such.
            if self.pending.len() == self.seg {
                let full = std::mem::replace(&mut self.pending, Vec::with_capacity(self.seg));
                self.emit(&full, false)?;
            }
            let n = (self.seg - self.pending.len()).min(data.len());
            self.pending.extend_from_slice(&data[..n]);
            self.hasher.update(&data[..n]);
            data = &data[n..];
        }
        Ok(())
    }

    fn writer(&mut self) -> &mut StreamWriter {
        self.w.as_mut().expect("stream writer is open")
    }

    fn emit(&mut self, plain: &[u8], last: bool) -> Result<()> {
        let stored: std::borrow::Cow<[u8]> = match &self.shard.cipher {
            Some(c) => {
                let nonce = segment_nonce(self.nonce.expect("nonce"), self.seg_index);
                let aad = segment_aad(self.shard.id(), self.stream, self.id, self.seg_index, last);
                std::borrow::Cow::Owned(c.seal(&nonce, plain, &aad)?)
            }
            None => std::borrow::Cow::Borrowed(plain),
        };
        let chunk_bytes = self.shard.chunk_bytes();
        if self.part.is_some() {
            let len = self.writer().len;
            if len + stored.len() as u64 + META_RESERVE > chunk_bytes {
                self.finish_part(false)?;
                self.shard.new_chunk(self.stream, &mut self.w)?;
            }
        }
        if self.part.is_none() {
            self.start_part()?;
        }
        let w = self.writer();
        write_all_at(&w.file, w.len, &stored)?;
        w.len += stored.len() as u64;
        let part = self.part.as_mut().unwrap();
        part.body += stored.len() as u64;
        part.plain += plain.len() as u64;
        self.plain_done += plain.len() as u64;
        self.seg_index += 1;
        Ok(())
    }

    fn start_part(&mut self) -> Result<()> {
        let plain_start = self.plain_done;
        let w = self.writer();
        // The header is written last; zeros mark an unfinished record.
        write_all_at(&w.file, w.len, &[0u8; RECORD_HEADER_LEN as usize])?;
        let (chunk, rec) = (w.chunk, w.len);
        w.len += RECORD_HEADER_LEN;
        self.part = Some(OpenPart { chunk, rec, plain_start, body: 0, plain: 0 });
        Ok(())
    }

    fn part_meta(&self, p: &OpenPart, last: bool, sha256: Option<Digest>) -> PartMeta {
        let (path, owner, created_ms, modified_ms, attrs) = match &self.target {
            Target::File { path, created_ms, modified_ms } => (path.clone(), None, *created_ms, *modified_ms, Default::default()),
            Target::Blob { owner, attrs, .. } => {
                (String::new(), Some(*owner), None, None, if last { attrs.clone() } else { Default::default() })
            }
        };
        PartMeta {
            seq: self.seq,
            id: self.id,
            path,
            owner,
            nonce: self.nonce,
            part: self.part_idx,
            off: p.plain_start,
            len: p.plain,
            last,
            size: last.then_some(self.plain_done),
            sha256,
            created_ms,
            modified_ms,
            attrs,
        }
    }

    fn finish_part(&mut self, last: bool) -> Result<()> {
        let p = self.part.take().expect("open part");
        let sha = last.then(|| Digest::from(self.hasher.clone().finalize()));
        let meta = Meta::Part(self.part_meta(&p, last, sha));
        let bytes = self.shard.encode_meta(self.stream, &meta)?;
        let w = self.writer();
        write_all_at(&w.file, w.len, &bytes)?;
        w.len += bytes.len() as u64;
        let header = RecordHeader { body_len: p.body, meta_len: bytes.len() as u32 }.encode();
        write_all_at(&w.file, p.rec, &header)?;
        let end = w.len;
        self.parts.push(Part {
            chunk: p.chunk,
            rec: p.rec,
            rec_len: end - p.rec,
            at: p.rec + RECORD_HEADER_LEN,
            stored: p.body,
            off: p.plain_start,
            len: p.plain,
        });
        self.part_idx += 1;
        Ok(())
    }

    /// Completes the item and makes it visible.
    pub fn finish(mut self) -> Result<()> {
        let pending = std::mem::take(&mut self.pending);
        self.emit(&pending, true)?;
        self.finish_part(true)?;
        let (chunk, end) = {
            let w = self.writer();
            (w.chunk, w.len)
        };
        let sha = Some(Digest::from(self.hasher.clone().finalize()));
        let mut item = Item {
            id: self.id,
            seq: self.seq,
            nonce: self.nonce,
            size: self.plain_done,
            created_ms: None,
            modified_ms: None,
            sha256: sha,
            parts: std::mem::take(&mut self.parts),
            attrs: Default::default(),
        };
        {
            let mut ix = self.shard.index.write().unwrap();
            match &self.target {
                Target::File { path, created_ms, modified_ms } => {
                    item.created_ms = *created_ms;
                    item.modified_ms = *modified_ms;
                    ix.install_file(path, item);
                }
                Target::Blob { owner_path, owner, attrs } => {
                    item.attrs = attrs.clone();
                    // The file may have been deleted or replaced meanwhile; then this is garbage.
                    if ix.files.get(owner_path).is_some_and(|f| f.id == *owner) {
                        ix.install_blob(self.stream, *owner, item);
                    }
                }
            }
            ix.commit_pos(self.stream, chunk, end);
        }
        self.finished = true;
        self.shard.after_commit()
    }
}

impl std::io::Write for ItemWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_all(buf).map_err(std::io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for ItemWriter<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Cut off the unfinished record. Parts already completed in earlier chunks stay
        // behind as garbage: without their final part they never become an item.
        let Some(p) = self.part.take() else { return };
        let cut = match self.w.as_mut() {
            Some(w) if w.chunk == p.chunk => {
                let ok = w.file.set_len(p.rec).is_ok();
                if ok {
                    w.len = p.rec;
                }
                ok
            }
            _ => true,
        };
        if !cut {
            // Nothing may follow an unfinished record: continue in a fresh chunk.
            let _ = self.shard.new_chunk(self.stream, &mut self.w);
        }
    }
}

impl From<sha2::digest::Output<Sha256>> for Digest {
    fn from(o: sha2::digest::Output<Sha256>) -> Self {
        let mut b = [0u8; 32];
        b.copy_from_slice(&o);
        crate::format::Hex(b)
    }
}
