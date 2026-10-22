//! Shard store: many small files packed into a few large chunk files.
//!
//! A shard is a folder (`<name>.vpshard`) that moves as a handful of big files instead of
//! hundreds of thousands of small ones, fits on FAT32 (chunks stay below 4 GiB), and can be
//! encrypted so it is readable only with this program and its key:
//!
//! ```text
//! name.vpshard/
//!   info.json                 plain: format version, id, chunk size, key slot
//!   index.vpidx               index snapshot (sealed when encrypted)
//!   shard.lock                held by the instance that writes
//!   data/000000.vpchnk ...    stored files
//!   thumbs/000000.vpchnk ...  thumbnails of stored files
//!   converted/000000.vpchnk   converted video copies
//! ```
//!
//! Chunks are append-only logs of records (`header | body | meta`). Each record's metadata
//! names what it holds, so the index can always be rebuilt from the chunks alone. Deletes
//! and renames append small records; compaction rewrites the live records of chunks that
//! are mostly garbage. With encryption, a random master key (wrapped by the password-
//! derived key in `info.json`) seals every record's metadata and the body in fixed-size
//! segments (XChaCha20-Poly1305), so any byte range is readable without decrypting the
//! rest and names, sizes and times are hidden.

mod format;
mod index;
mod io;
mod reader;
mod writer;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use vp_crypto::{BundleKey, CryptoError, KdfParams, SegmentCipher, TAG_LEN};

pub use format::{Digest, Hex, Id, Stream, normalize_path};
use format::{
    CHUNK_HEADER_LEN, META_RESERVE, Meta, PartMeta, RECORD_HEADER_LEN, RecordHeader, check_chunk_header, chunk_header,
    chunk_name, index_label, meta_label, parse_chunk_name,
};
pub use index::Item;
use index::{ChunkStat, Found, Index, Part, Tomb};
use io::{read_exact_at, write_all_at, write_atomic};
pub use reader::{ShardReader, ShardStream};
use writer::Target;
pub use writer::ItemWriter;

pub const EXTENSION: &str = "vpshard";
pub const INFO_FILE: &str = "info.json";
pub const INDEX_FILE: &str = "index.vpidx";
pub const LOCK_FILE: &str = "shard.lock";
pub const SHARD_FORMAT_VERSION: u32 = 1;

/// Folder for the app's own files inside a shard (item properties). Ordinary files, so
/// older apps read the shard as before; the shard driver leaves the folder out of listings.
pub const RESERVED_DIR: &str = ".viewport";

/// Whether `path` is [`RESERVED_DIR`] or inside it.
pub fn is_reserved(path: &str) -> bool {
    path == RESERVED_DIR || path.strip_prefix(RESERVED_DIR).is_some_and(|r| r.starts_with('/'))
}
/// FAT32 cannot hold files of 4 GiB or more.
pub const MAX_CHUNK_BYTES: u64 = u32::MAX as u64;
pub const MIN_CHUNK_BYTES: u64 = 64 * 1024;
pub const MIN_SEGMENT_BYTES: u32 = 1024;
pub const MAX_SEGMENT_BYTES: u32 = 1024 * 1024;
const INDEX_MAGIC: &[u8; 8] = b"VPIDX001";
const KEY_CHECK: &[u8] = b"ViewPort shard key check";
const COPY_PIECE: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ShardError {
    #[error("'{0}' is not a ViewPort shard (no {INFO_FILE})")]
    NotAShard(PathBuf),
    #[error("'{0}' already exists")]
    AlreadyExists(PathBuf),
    #[error("shard format v{found} is newer than this build supports (v{supported})")]
    TooNew { found: u32, supported: u32 },
    #[error("the shard is encrypted and needs its password")]
    Locked,
    #[error("wrong password or key file")]
    WrongKey,
    #[error("the shard is open read-only")]
    ReadOnly,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid path '{0}'")]
    InvalidPath(String),
    #[error("invalid option: {0}")]
    Invalid(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Crypto(CryptoError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<CryptoError> for ShardError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::WrongKey => ShardError::WrongKey,
            e => ShardError::Crypto(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, ShardError>;

/// The key slot of an encrypted shard. The master key never changes; changing the
/// password only re-wraps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Encryption {
    /// Always `xchacha20poly1305`.
    pub cipher: String,
    pub kdf: KdfParams,
    /// The master key sealed with the password-derived key (base64).
    pub wrapped_key: String,
    /// A known value sealed with the master key (hex), to check a stored master key.
    pub key_check: String,
}

/// `info.json`: always plain text, only what is safe to disclose.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShardInfo {
    pub shard_version: u32,
    /// Oldest format a reader must understand; newer builds may add things older ones can
    /// still read (then they open read-only).
    #[serde(default = "one")]
    pub min_reader_version: u32,
    pub id: Id,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub created_ms: i64,
    pub chunk_bytes: u64,
    pub segment_bytes: u32,
    #[serde(default)]
    pub encryption: Option<Encryption>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub name: String,
    pub app_version: String,
    pub created_ms: i64,
    pub chunk_bytes: u64,
    pub segment_bytes: u32,
    /// Encrypts the shard with this password (and key file, if given).
    pub password: Option<String>,
    pub keyfile: Option<Vec<u8>>,
    pub kdf_memory_kib: u32,
    pub kdf_iterations: u32,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            app_version: String::new(),
            created_ms: 0,
            chunk_bytes: 1 << 30,
            segment_bytes: 64 * 1024,
            password: None,
            keyfile: None,
            kdf_memory_kib: vp_crypto::DEFAULT_MEMORY_KIB,
            kdf_iterations: vp_crypto::DEFAULT_ITERATIONS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpenOptions {
    /// Take the write lock. Falls back to read-only if another instance holds it.
    pub writable: bool,
    /// Save an index snapshot after this many new records...
    pub snapshot_min_records: u64,
    /// ...once they are also this fraction of the stored files (bounds rewrite cost).
    pub snapshot_ratio: f64,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self { writable: true, snapshot_min_records: 2000, snapshot_ratio: 0.25 }
    }
}

/// A file or folder in a shard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntryInfo {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub created_ms: Option<i64>,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ShardStats {
    pub files: u64,
    pub bytes: u64,
    pub thumbs: u64,
    pub converted: u64,
    pub chunks: u64,
    /// Total size of the chunk files.
    pub disk_bytes: u64,
    /// Space compaction would reclaim.
    pub garbage_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CompactReport {
    pub chunks_removed: u64,
    pub bytes_freed: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VerifyReport {
    pub items: u64,
    /// Paths of damaged files (`<path>#thumb` / `<path>#converted` for those).
    pub damaged: Vec<String>,
}

pub(crate) struct StreamWriter {
    pub(crate) chunk: u32,
    pub(crate) file: Arc<File>,
    pub(crate) len: u64,
}

pub struct Shard {
    root: PathBuf,
    info: RwLock<ShardInfo>,
    key: Option<BundleKey>,
    pub(crate) cipher: Option<Arc<SegmentCipher>>,
    writable: bool,
    busy: bool,
    _lock: Option<File>,
    pub(crate) index: RwLock<Index>,
    writers: [Mutex<Option<StreamWriter>>; 3],
    /// Write handles of the active chunks, for syncing without the stream locks.
    active: Mutex<[Option<Arc<File>>; 3]>,
    handles: Mutex<HashMap<(Stream, u32), Arc<File>>>,
    since_snapshot: AtomicU64,
    opts: OpenOptions,
    snapshot_lock: Mutex<()>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len().is_multiple_of(2))
        .then(|| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect())
        .flatten()
}

fn norm(path: &str) -> Result<String> {
    normalize_path(path).ok_or_else(|| ShardError::InvalidPath(path.into()))
}

impl Shard {
    // ------------------------------------------------------------ create / open

    /// Creates an empty shard folder. Returns its master key if it is encrypted.
    pub fn create(root: &Path, opts: &CreateOptions) -> Result<Option<BundleKey>> {
        if root.exists() && fs::read_dir(root)?.next().is_some() {
            return Err(ShardError::AlreadyExists(root.into()));
        }
        if !(MIN_CHUNK_BYTES..=MAX_CHUNK_BYTES).contains(&opts.chunk_bytes) {
            return Err(ShardError::Invalid(format!("chunk size {} out of range", opts.chunk_bytes)));
        }
        if !(MIN_SEGMENT_BYTES..=MAX_SEGMENT_BYTES).contains(&opts.segment_bytes)
            || (opts.segment_bytes as u64 + TAG_LEN as u64) * 4 + META_RESERVE + CHUNK_HEADER_LEN > opts.chunk_bytes
        {
            return Err(ShardError::Invalid(format!("segment size {} out of range", opts.segment_bytes)));
        }
        let id = Id::random()?;
        let (encryption, key) = match &opts.password {
            Some(pw) => {
                if pw.is_empty() {
                    return Err(ShardError::Invalid("password is empty".into()));
                }
                let master = BundleKey::random()?;
                let (kek, kdf) = vp_crypto::new_key(pw, opts.keyfile.as_deref(), opts.kdf_memory_kib, opts.kdf_iterations)?;
                let enc = Encryption {
                    cipher: "xchacha20poly1305".into(),
                    kdf,
                    wrapped_key: vp_crypto::wrap_key(&kek, &master)?,
                    key_check: hex(&vp_crypto::seal(&master, KEY_CHECK, &index_label(id))?),
                };
                (Some(enc), Some(master))
            }
            None => (None, None),
        };
        let info = ShardInfo {
            shard_version: SHARD_FORMAT_VERSION,
            min_reader_version: SHARD_FORMAT_VERSION,
            id,
            name: opts.name.clone(),
            app_version: opts.app_version.clone(),
            created_ms: opts.created_ms,
            chunk_bytes: opts.chunk_bytes,
            segment_bytes: opts.segment_bytes,
            encryption,
        };
        fs::create_dir_all(root)?;
        for s in Stream::ALL {
            fs::create_dir_all(root.join(s.dir()))?;
        }
        write_atomic(&root.join(INFO_FILE), &serde_json::to_vec_pretty(&info)?)?;
        Ok(key)
    }

    pub fn create_and_open(root: &Path, opts: &CreateOptions, open: OpenOptions) -> Result<Shard> {
        let key = Self::create(root, opts)?;
        Self::open(root, key, open)
    }

    /// Reads `info.json` without opening anything else.
    pub fn read_info(root: &Path) -> Result<ShardInfo> {
        match fs::read(root.join(INFO_FILE)) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ShardError::NotAShard(root.into())),
            Err(e) => Err(e.into()),
        }
    }

    /// Derives the master key of an encrypted shard from its password.
    pub fn unlock(info: &ShardInfo, password: &str, keyfile: Option<&[u8]>) -> Result<BundleKey> {
        let enc = info.encryption.as_ref().ok_or_else(|| ShardError::Invalid("the shard is not encrypted".into()))?;
        let kek = vp_crypto::unlock(password, keyfile, &enc.kdf)?;
        let key = vp_crypto::unwrap_key(&kek, &enc.wrapped_key)?;
        Self::check_key(info, &key)?;
        Ok(key)
    }

    /// Checks a master key (e.g. one remembered by a locked bundle) against the shard.
    pub fn check_key(info: &ShardInfo, key: &BundleKey) -> Result<()> {
        let enc = info.encryption.as_ref().ok_or_else(|| ShardError::Invalid("the shard is not encrypted".into()))?;
        let sealed = unhex(&enc.key_check).ok_or_else(|| ShardError::Invalid("bad key check".into()))?;
        match vp_crypto::open(key, &sealed, &index_label(info.id)) {
            Ok(v) if v == KEY_CHECK => Ok(()),
            _ => Err(ShardError::WrongKey),
        }
    }

    /// Opens a shard. An encrypted one needs its master key ([`Shard::unlock`]).
    pub fn open(root: &Path, key: Option<BundleKey>, opts: OpenOptions) -> Result<Shard> {
        let info = Self::read_info(root)?;
        let mut writable = opts.writable;
        if info.shard_version > SHARD_FORMAT_VERSION {
            if info.min_reader_version > SHARD_FORMAT_VERSION {
                return Err(ShardError::TooNew { found: info.shard_version, supported: SHARD_FORMAT_VERSION });
            }
            writable = false;
        }
        let key = match (&info.encryption, key) {
            (Some(_), Some(k)) => {
                Self::check_key(&info, &k)?;
                Some(k)
            }
            (Some(_), None) => return Err(ShardError::Locked),
            (None, _) => None,
        };
        let (mut lock, mut busy) = (None, false);
        if writable {
            let f = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(root.join(LOCK_FILE));
            match f {
                Ok(f) => match f.try_lock() {
                    Ok(()) => lock = Some(f),
                    Err(fs::TryLockError::WouldBlock) => {
                        writable = false;
                        busy = true;
                    }
                    // Some file systems cannot lock at all; go on without.
                    Err(fs::TryLockError::Error(_)) => lock = Some(f),
                },
                // A read-only medium.
                Err(_) => writable = false,
            }
        }
        let cipher = key.as_ref().map(|k| Arc::new(SegmentCipher::new(k)));
        let shard = Shard {
            root: root.into(),
            info: RwLock::new(info),
            key,
            cipher,
            writable,
            busy,
            _lock: lock,
            index: RwLock::new(Index::default()),
            writers: Default::default(),
            active: Mutex::default(),
            handles: Mutex::default(),
            since_snapshot: AtomicU64::new(0),
            opts,
            snapshot_lock: Mutex::new(()),
        };
        let (index, replayed) = shard.load_index()?;
        *shard.index.write().unwrap() = index;
        if replayed && shard.writable {
            shard.since_snapshot.store(1, Ordering::Relaxed);
            shard.flush()?;
        }
        Ok(shard)
    }

    /// Re-reads the index of a shard opened read-only, to see what another instance wrote.
    pub fn refresh(&self) -> Result<()> {
        if self.writable {
            return Ok(());
        }
        self.handles.lock().unwrap().clear();
        let (index, _) = self.load_index()?;
        *self.index.write().unwrap() = index;
        Ok(())
    }

    // ------------------------------------------------------------ accessors

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn info(&self) -> ShardInfo {
        self.info.read().unwrap().clone()
    }

    pub fn id(&self) -> Id {
        self.info.read().unwrap().id
    }

    pub fn is_encrypted(&self) -> bool {
        self.key.is_some()
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Opened read-only because another instance is writing to it.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub(crate) fn chunk_bytes(&self) -> u64 {
        self.info.read().unwrap().chunk_bytes
    }

    pub(crate) fn segment_bytes(&self) -> u32 {
        self.info.read().unwrap().segment_bytes
    }

    fn chunk_path(&self, stream: Stream, chunk: u32) -> PathBuf {
        self.root.join(stream.dir()).join(chunk_name(chunk))
    }

    fn check_writable(&self) -> Result<()> {
        if self.writable { Ok(()) } else { Err(ShardError::ReadOnly) }
    }

    // ------------------------------------------------------------ index loading

    fn chunk_numbers(&self, stream: Stream) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        match fs::read_dir(self.root.join(stream.dir())) {
            Ok(entries) => {
                for e in entries {
                    if let Some(n) = e?.file_name().to_str().and_then(parse_chunk_name) {
                        out.push(n);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        out.sort_unstable();
        Ok(out)
    }

    fn read_snapshot(&self) -> Option<Index> {
        let data = fs::read(self.root.join(INDEX_FILE)).ok()?;
        let body = data.strip_prefix(INDEX_MAGIC)?;
        let json = match &self.key {
            Some(k) => vp_crypto::open(k, body, &index_label(self.id())).ok()?,
            None => body.to_vec(),
        };
        serde_json::from_slice(&json).ok()
    }

    /// Loads the snapshot and replays what was written after it. Without a usable
    /// snapshot, every chunk is replayed. Returns whether anything was replayed.
    fn load_index(&self) -> Result<(Index, bool)> {
        let snapshot = self.read_snapshot();
        let have_snapshot = snapshot.is_some();
        let mut index = snapshot.unwrap_or_default();
        let mut found = Vec::new();
        for s in Stream::ALL {
            let on_disk = self.chunk_numbers(s)?;
            let applied = index.applied[s.idx()];
            for &c in &on_disk {
                let start = if !have_snapshot || c > applied.chunk {
                    0
                } else if c == applied.chunk {
                    applied.off
                } else {
                    if !index.chunks[s.idx()].contains_key(&c) && self.writable {
                        // Left over from a compaction interrupted after its snapshot.
                        let _ = fs::remove_file(self.chunk_path(s, c));
                    }
                    continue;
                };
                let file = self.handle(s, c)?;
                let len = file.metadata()?.len();
                let (records, end) = self.scan_chunk(&file, len, s, c, start)?;
                if end < len && self.writable {
                    drop(file);
                    self.handles.lock().unwrap().remove(&(s, c));
                    let path = self.chunk_path(s, c);
                    if end == 0 {
                        fs::remove_file(&path)?;
                        index.chunks[s.idx()].remove(&c);
                        continue;
                    }
                    fs::OpenOptions::new().write(true).open(&path)?.set_len(end)?;
                }
                if end == 0 {
                    continue;
                }
                index.chunks[s.idx()].entry(c).or_default().len = end;
                index.applied[s.idx()] = index.applied[s.idx()].max_with(c, end);
                found.extend(records);
            }
            if let Some(&max) = on_disk.last() {
                index.next_chunk[s.idx()] = index.next_chunk[s.idx()].max(max + 1);
            }
        }
        let replayed = !found.is_empty();
        index.replay(found);
        Ok((index, replayed))
    }

    /// Reads records from `start` until the first incomplete or invalid one. Returns
    /// them and the offset where valid data ends (0 if the chunk header is bad).
    fn scan_chunk(&self, file: &File, len: u64, stream: Stream, chunk: u32, start: u64) -> Result<(Vec<Found>, u64)> {
        let mut pos = start;
        if pos < CHUNK_HEADER_LEN {
            let mut h = [0u8; CHUNK_HEADER_LEN as usize];
            if len < CHUNK_HEADER_LEN || read_exact_at(file, 0, &mut h).is_err() || !check_chunk_header(&h, self.id(), stream, chunk) {
                return Ok((Vec::new(), 0));
            }
            pos = CHUNK_HEADER_LEN;
        }
        let mut out = Vec::new();
        while pos + RECORD_HEADER_LEN <= len {
            let mut h = [0u8; RECORD_HEADER_LEN as usize];
            read_exact_at(file, pos, &mut h)?;
            let Some(rh) = RecordHeader::decode(&h) else { break };
            let at = pos + RECORD_HEADER_LEN;
            let Some(end) = at.checked_add(rh.body_len).and_then(|m| m.checked_add(rh.meta_len as u64)) else { break };
            if end > len {
                break;
            }
            let mut m = vec![0u8; rh.meta_len as usize];
            read_exact_at(file, at + rh.body_len, &mut m)?;
            let Ok(meta) = self.decode_meta(stream, &m) else { break };
            out.push(Found { stream, chunk, rec: pos, rec_len: end - pos, at, stored: rh.body_len, meta });
            pos = end;
        }
        Ok((out, pos))
    }

    pub(crate) fn encode_meta(&self, stream: Stream, meta: &Meta) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(meta)?;
        Ok(match &self.key {
            Some(k) => vp_crypto::seal(k, &json, &meta_label(self.id(), stream))?,
            None => json,
        })
    }

    fn decode_meta(&self, stream: Stream, bytes: &[u8]) -> Result<Meta> {
        let json = match &self.key {
            Some(k) => vp_crypto::open(k, bytes, &meta_label(self.id(), stream))?,
            None => bytes.to_vec(),
        };
        Ok(serde_json::from_slice(&json)?)
    }

    // ------------------------------------------------------------ snapshots

    /// Makes everything written so far durable and saves an index snapshot.
    pub fn flush(&self) -> Result<()> {
        if !self.writable || self.since_snapshot.load(Ordering::Relaxed) == 0 {
            return Ok(());
        }
        let _one = self.snapshot_lock.lock().unwrap();
        self.since_snapshot.store(0, Ordering::Relaxed);
        let json = serde_json::to_vec(&*self.index.read().unwrap())?;
        // Records the snapshot points at must reach the disk before it does.
        for f in self.active.lock().unwrap().iter().flatten() {
            f.sync_data()?;
        }
        let mut data = INDEX_MAGIC.to_vec();
        match &self.key {
            Some(k) => data.extend(vp_crypto::seal(k, &json, &index_label(self.id()))?),
            None => data.extend(json),
        }
        write_atomic(&self.root.join(INDEX_FILE), &data)?;
        Ok(())
    }

    pub(crate) fn after_commit(&self) -> Result<()> {
        let n = self.since_snapshot.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= self.opts.snapshot_min_records {
            let files = self.index.read().unwrap().files.len() as f64;
            if n as f64 >= files * self.opts.snapshot_ratio {
                self.flush()?;
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ chunk files

    fn handle(&self, stream: Stream, chunk: u32) -> Result<Arc<File>> {
        let mut handles = self.handles.lock().unwrap();
        if let Some(f) = handles.get(&(stream, chunk)) {
            return Ok(f.clone());
        }
        let f = Arc::new(File::open(self.chunk_path(stream, chunk))?);
        handles.insert((stream, chunk), f.clone());
        Ok(f)
    }

    fn lock_writer(&self, stream: Stream) -> Result<MutexGuard<'_, Option<StreamWriter>>> {
        self.check_writable()?;
        let mut w = self.writers[stream.idx()].lock().unwrap();
        if w.is_none() {
            let last = self.index.read().unwrap().chunks[stream.idx()].iter().next_back().map(|(c, st)| (*c, st.len));
            match last {
                Some((c, len)) if len + 2 * META_RESERVE < self.chunk_bytes() => {
                    let file = Arc::new(fs::OpenOptions::new().read(true).write(true).open(self.chunk_path(stream, c))?);
                    let len = file.metadata()?.len();
                    self.active.lock().unwrap()[stream.idx()] = Some(file.clone());
                    *w = Some(StreamWriter { chunk: c, file, len });
                }
                _ => self.new_chunk(stream, &mut w)?,
            }
        }
        Ok(w)
    }

    /// Seals the current chunk of `stream` and starts a new one.
    pub(crate) fn new_chunk(&self, stream: Stream, w: &mut Option<StreamWriter>) -> Result<()> {
        if let Some(old) = w.take() {
            old.file.sync_data()?;
        }
        let c = {
            let mut ix = self.index.write().unwrap();
            let c = ix.next_chunk[stream.idx()];
            ix.next_chunk[stream.idx()] += 1;
            ix.chunks[stream.idx()].insert(c, ChunkStat { len: CHUNK_HEADER_LEN, live: 0 });
            c
        };
        let path = self.chunk_path(stream, c);
        fs::create_dir_all(path.parent().unwrap())?;
        let file = fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path)?;
        write_all_at(&file, 0, &chunk_header(self.id(), stream, c, self.key.is_some()))?;
        let file = Arc::new(file);
        self.active.lock().unwrap()[stream.idx()] = Some(file.clone());
        *w = Some(StreamWriter { chunk: c, file, len: CHUNK_HEADER_LEN });
        self.since_snapshot.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn stored_len(&self, size: u64) -> u64 {
        if self.key.is_none() {
            return size;
        }
        let seg = self.segment_bytes() as u64;
        size + size.div_ceil(seg).max(1) * TAG_LEN as u64
    }

    fn item_writer(&self, stream: Stream, target: Target, size_hint: Option<u64>) -> Result<ItemWriter<'_>> {
        let mut w = self.lock_writer(stream)?;
        // A small item that does not fit in what is left of the chunk starts a new one
        // rather than being split.
        if let Some(n) = size_hint {
            let need = self.stored_len(n) + RECORD_HEADER_LEN + META_RESERVE;
            let len = w.as_ref().unwrap().len;
            if len > CHUNK_HEADER_LEN && len + need > self.chunk_bytes() && CHUNK_HEADER_LEN + need <= self.chunk_bytes() {
                self.new_chunk(stream, &mut w)?;
            }
        }
        let seq = self.index.write().unwrap().alloc_seq();
        ItemWriter::new(self, stream, w, target, seq)
    }

    // ------------------------------------------------------------ files

    /// Starts writing a file at `path`, replacing any file there when finished.
    pub fn writer(&self, path: &str, size_hint: Option<u64>, created_ms: Option<i64>, modified_ms: Option<i64>) -> Result<ItemWriter<'_>> {
        let path = norm(path)?;
        if path.is_empty() {
            return Err(ShardError::InvalidPath(path));
        }
        self.item_writer(Stream::Data, Target::File { path, created_ms, modified_ms }, size_hint)
    }

    pub fn put(&self, path: &str, data: &[u8], created_ms: Option<i64>, modified_ms: Option<i64>) -> Result<()> {
        let mut w = self.writer(path, Some(data.len() as u64), created_ms, modified_ms)?;
        w.write_all(data)?;
        w.finish()
    }

    pub fn put_reader(
        &self,
        path: &str,
        r: &mut dyn std::io::Read,
        size_hint: Option<u64>,
        created_ms: Option<i64>,
        modified_ms: Option<i64>,
    ) -> Result<()> {
        let mut w = self.writer(path, size_hint, created_ms, modified_ms)?;
        let mut buf = vec![0u8; COPY_PIECE];
        loop {
            let n = r.read(&mut buf)?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n])?;
        }
        w.finish()
    }

    fn reader(&self, stream: Stream, item: Item) -> Result<ShardReader> {
        let files = item.parts.iter().map(|p| self.handle(stream, p.chunk)).collect::<Result<Vec<_>>>()?;
        Ok(ShardReader {
            item,
            files,
            stream,
            shard: self.id(),
            cipher: self.cipher.clone(),
            seg: self.segment_bytes() as u64,
            cache: Mutex::new(None),
        })
    }

    pub fn open_file(&self, path: &str) -> Result<ShardReader> {
        let path = norm(path)?;
        let item = self.index.read().unwrap().files.get(&path).cloned().ok_or(ShardError::NotFound(path))?;
        self.reader(Stream::Data, item)
    }

    /// The stored item's id (hex) at `path`: it stays with the file when the file is
    /// renamed or moved, and changes when the file is replaced.
    pub fn file_id(&self, path: &str) -> Option<String> {
        let path = norm(path).ok()?;
        self.index.read().unwrap().files.get(&path).map(|f| f.id.to_string())
    }

    pub fn exists(&self, path: &str) -> bool {
        norm(path).is_ok_and(|p| self.index.read().unwrap().files.contains_key(&p))
    }

    /// A file or folder.
    pub fn stat(&self, path: &str) -> Result<EntryInfo> {
        let path = norm(path)?;
        let ix = self.index.read().unwrap();
        if let Some(f) = ix.files.get(&path) {
            return Ok(EntryInfo { path, is_dir: false, size: f.size, created_ms: f.created_ms, modified_ms: f.modified_ms });
        }
        if ix.is_dir(&path) {
            return Ok(EntryInfo { path, is_dir: true, size: 0, created_ms: None, modified_ms: None });
        }
        Err(ShardError::NotFound(path))
    }

    /// Direct children of folder `dir` (`""` = root): folders first, then files.
    pub fn list(&self, dir: &str) -> Result<Vec<EntryInfo>> {
        let dir = norm(dir)?;
        let ix = self.index.read().unwrap();
        if !ix.is_dir(&dir) {
            return Err(ShardError::NotFound(dir));
        }
        let (files, mut dirs) = ix.children(&dir);
        dirs.sort();
        dirs.dedup();
        let join = |name: &str| if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") };
        let mut out: Vec<EntryInfo> = dirs
            .iter()
            .map(|d| EntryInfo { path: join(d), is_dir: true, size: 0, created_ms: None, modified_ms: None })
            .collect();
        out.extend(files.into_iter().map(|(p, f)| EntryInfo {
            path: p.clone(),
            is_dir: false,
            size: f.size,
            created_ms: f.created_ms,
            modified_ms: f.modified_ms,
        }));
        Ok(out)
    }

    /// Paths and sizes of the files at or under `path` (`""` = all), in path order.
    pub fn files_under(&self, path: &str) -> Result<Vec<(String, u64)>> {
        let path = norm(path)?;
        Ok(self.index.read().unwrap().under(&path).into_iter().map(|(p, f)| (p.clone(), f.size)).collect())
    }

    fn write_tomb(&self, make: impl FnOnce(u64) -> Meta) -> Result<()> {
        let mut w = self.lock_writer(Stream::Data)?;
        let seq = self.index.write().unwrap().alloc_seq();
        let meta = make(seq);
        let bytes = self.encode_meta(Stream::Data, &meta)?;
        let need = RECORD_HEADER_LEN + bytes.len() as u64;
        if w.as_ref().unwrap().len + need > self.chunk_bytes() {
            self.new_chunk(Stream::Data, &mut w)?;
        }
        let sw = w.as_mut().unwrap();
        let rec = sw.len;
        write_all_at(&sw.file, rec + RECORD_HEADER_LEN, &bytes)?;
        write_all_at(&sw.file, rec, &RecordHeader { body_len: 0, meta_len: bytes.len() as u32 }.encode())?;
        sw.len += need;
        let (chunk, end) = (sw.chunk, sw.len);
        {
            let mut ix = self.index.write().unwrap();
            ix.apply_tomb(Tomb { chunk, rec, rec_len: need, meta });
            ix.commit_pos(Stream::Data, chunk, end);
        }
        drop(w);
        self.after_commit()
    }

    /// Deletes a file, or every file under a folder. Returns how many were deleted.
    pub fn delete(&self, path: &str) -> Result<usize> {
        let path = norm(path)?;
        let targets: Vec<String> = self.index.read().unwrap().under(&path).into_iter().map(|(p, _)| p.clone()).collect();
        if targets.is_empty() {
            return Err(ShardError::NotFound(path));
        }
        for p in &targets {
            self.write_tomb(|seq| Meta::Delete { seq, path: p.clone() })?;
        }
        Ok(targets.len())
    }

    /// Renames a file or folder, replacing files at the destination. Returns how many
    /// files moved. Thumbnails and converted copies follow their files.
    pub fn rename(&self, from: &str, to: &str) -> Result<usize> {
        let (from, to) = (norm(from)?, norm(to)?);
        if from.is_empty() || to.is_empty() || to == from || to.starts_with(&format!("{from}/")) {
            return Err(ShardError::InvalidPath(to));
        }
        let moves: Vec<(String, String)> = {
            let ix = self.index.read().unwrap();
            if ix.files.contains_key(&from) {
                vec![(from.clone(), to.clone())]
            } else {
                ix.under(&from).into_iter().map(|(p, _)| (p.clone(), format!("{to}{}", &p[from.len()..]))).collect()
            }
        };
        if moves.is_empty() {
            return Err(ShardError::NotFound(from));
        }
        for (a, b) in &moves {
            self.write_tomb(|seq| Meta::Move { seq, from: a.clone(), to: b.clone() })?;
        }
        Ok(moves.len())
    }

    // ------------------------------------------------------------ thumbnails and converted copies

    fn owner(&self, path: &str) -> Result<(String, Id)> {
        let path = norm(path)?;
        let id = self.index.read().unwrap().files.get(&path).map(|f| f.id).ok_or_else(|| ShardError::NotFound(path.clone()))?;
        Ok((path, id))
    }

    /// Starts writing the thumbnail or converted copy of the file at `owner_path`.
    pub fn blob_writer(
        &self,
        stream: Stream,
        owner_path: &str,
        size_hint: Option<u64>,
        attrs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ItemWriter<'_>> {
        if stream == Stream::Data {
            return Err(ShardError::Invalid("blobs go to the thumbs or converted stream".into()));
        }
        let (owner_path, owner) = self.owner(owner_path)?;
        self.item_writer(stream, Target::Blob { owner_path, owner, attrs }, size_hint)
    }

    pub fn put_blob(
        &self,
        stream: Stream,
        owner_path: &str,
        data: &[u8],
        attrs: serde_json::Map<String, serde_json::Value>,
    ) -> Result<()> {
        let mut w = self.blob_writer(stream, owner_path, Some(data.len() as u64), attrs)?;
        w.write_all(data)?;
        w.finish()
    }

    pub fn blob(&self, stream: Stream, owner_path: &str) -> Result<Option<ShardReader>> {
        let Ok((_, owner)) = self.owner(owner_path) else { return Ok(None) };
        let item = self.index.read().unwrap().blobs(stream).get(&owner).cloned();
        item.map(|i| self.reader(stream, i)).transpose()
    }

    pub fn has_blob(&self, stream: Stream, owner_path: &str) -> bool {
        self.owner(owner_path).is_ok_and(|(_, o)| self.index.read().unwrap().blobs(stream).contains_key(&o))
    }

    // ------------------------------------------------------------ maintenance

    pub fn stats(&self) -> ShardStats {
        let ix = self.index.read().unwrap();
        let mut s = ShardStats {
            files: ix.files.len() as u64,
            bytes: ix.files.values().map(|f| f.size).sum(),
            thumbs: ix.thumbs.len() as u64,
            converted: ix.converted.len() as u64,
            ..Default::default()
        };
        for chunks in &ix.chunks {
            for st in chunks.values() {
                s.chunks += 1;
                s.disk_bytes += st.len;
                s.garbage_bytes += st.len.saturating_sub(CHUNK_HEADER_LEN + st.live);
            }
        }
        s
    }

    /// Share of the chunk files that compaction would reclaim.
    pub fn garbage_ratio(&self) -> f64 {
        let s = self.stats();
        if s.disk_bytes == 0 { 0.0 } else { s.garbage_bytes as f64 / s.disk_bytes as f64 }
    }

    /// Rewrites the live records of chunks that are at least `min_ratio` garbage (all
    /// chunks with any garbage when `full`) and deletes those chunks. `progress` gets
    /// bytes copied and the total to copy.
    pub fn compact(&self, full: bool, min_ratio: f64, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<CompactReport> {
        self.check_writable()?;
        let mut report = CompactReport::default();
        let mut doomed: Vec<(Stream, u32)> = Vec::new();
        let garbage = |st: &ChunkStat| st.len.saturating_sub(CHUNK_HEADER_LEN + st.live);
        let mut result = Ok(());
        'streams: for s in Stream::ALL {
            if self.index.read().unwrap().chunks[s.idx()].is_empty() {
                continue;
            }
            let mut w = self.lock_writer(s)?;
            let active = w.as_ref().unwrap().chunk;
            if full && self.index.read().unwrap().chunks[s.idx()].get(&active).is_some_and(|st| garbage(st) > 0) {
                self.new_chunk(s, &mut w)?;
            }
            let active = w.as_ref().unwrap().chunk;
            let candidates: Vec<(u32, ChunkStat)> = self.index.read().unwrap().chunks[s.idx()]
                .iter()
                .filter(|(c, st)| {
                    **c != active
                        && (st.live == 0 || (full && garbage(st) > 0) || garbage(st) as f64 >= min_ratio * st.len as f64)
                })
                .map(|(c, st)| (*c, *st))
                .collect();
            let total: u64 = candidates.iter().map(|(_, st)| st.live).sum();
            let mut done = 0u64;
            for (c, st) in candidates {
                if cancel.load(Ordering::Relaxed) {
                    result = Err(ShardError::Cancelled);
                    break 'streams;
                }
                self.compact_chunk(s, c, full, &mut w, &mut |n| {
                    done += n;
                    progress(done, total);
                })?;
                self.index.write().unwrap().chunks[s.idx()].remove(&c);
                report.chunks_removed += 1;
                report.bytes_freed += garbage(&st);
                doomed.push((s, c));
            }
            if let Some(sw) = w.as_ref() {
                sw.file.sync_data()?;
            }
        }
        if !doomed.is_empty() {
            // The snapshot must stop naming the old chunks before they go.
            self.since_snapshot.fetch_add(1, Ordering::Relaxed);
            self.flush()?;
            let mut handles = self.handles.lock().unwrap();
            for (s, c) in doomed {
                handles.remove(&(s, c));
                let _ = fs::remove_file(self.chunk_path(s, c));
            }
        }
        result.map(|_| report)
    }

    fn compact_chunk(
        &self,
        stream: Stream,
        chunk: u32,
        full: bool,
        w: &mut MutexGuard<'_, Option<StreamWriter>>,
        progress: &mut dyn FnMut(u64),
    ) -> Result<()> {
        enum Job {
            Part { key: String, owner: Option<Id>, id: Id, idx: usize },
            Tomb(Tomb),
        }
        let jobs: Vec<Job> = {
            let ix = self.index.read().unwrap();
            let mut jobs = Vec::new();
            match stream {
                Stream::Data => {
                    for (path, f) in &ix.files {
                        for (i, p) in f.parts.iter().enumerate() {
                            if p.chunk == chunk {
                                jobs.push(Job::Part { key: path.clone(), owner: None, id: f.id, idx: i });
                            }
                        }
                    }
                    jobs.extend(ix.tombs.iter().filter(|t| t.chunk == chunk).cloned().map(Job::Tomb));
                }
                s => {
                    for (owner, b) in ix.blobs(s) {
                        for (i, p) in b.parts.iter().enumerate() {
                            if p.chunk == chunk {
                                jobs.push(Job::Part { key: String::new(), owner: Some(*owner), id: b.id, idx: i });
                            }
                        }
                    }
                }
            }
            jobs
        };
        let src = self.handle(stream, chunk)?;
        for job in jobs {
            match job {
                Job::Tomb(t) => {
                    if full {
                        // Every live record is rewritten, so nothing older is left to undo.
                        self.index.write().unwrap().tombs.retain(|x| x != &t);
                        continue;
                    }
                    let bytes = self.encode_meta(stream, &t.meta)?;
                    let need = RECORD_HEADER_LEN + bytes.len() as u64;
                    if w.as_ref().unwrap().len + need > self.chunk_bytes() {
                        self.new_chunk(stream, w)?;
                    }
                    let sw = w.as_mut().unwrap();
                    let rec = sw.len;
                    write_all_at(&sw.file, rec + RECORD_HEADER_LEN, &bytes)?;
                    write_all_at(&sw.file, rec, &RecordHeader { body_len: 0, meta_len: bytes.len() as u32 }.encode())?;
                    sw.len += need;
                    let (nc, end) = (sw.chunk, sw.len);
                    let mut ix = self.index.write().unwrap();
                    if let Some(x) = ix.tombs.iter_mut().find(|x| **x == t) {
                        *x = Tomb { chunk: nc, rec, rec_len: need, meta: t.meta.clone() };
                        ix.chunks[stream.idx()].entry(nc).or_default().live += need;
                    }
                    ix.commit_pos(stream, nc, end);
                }
                Job::Part { key, owner, id, idx } => {
                    let found = {
                        let ix = self.index.read().unwrap();
                        let item = match owner {
                            None => ix.files.get(&key),
                            Some(o) => ix.blobs(stream).get(&o),
                        };
                        item.filter(|i| i.id == id).cloned()
                    };
                    let Some(item) = found else { continue };
                    let old = item.parts[idx].clone();
                    let last = idx + 1 == item.parts.len();
                    let meta = Meta::Part(PartMeta {
                        seq: item.seq,
                        id,
                        path: key.clone(),
                        owner,
                        nonce: item.nonce,
                        part: idx as u32,
                        off: old.off,
                        len: old.len,
                        last,
                        size: last.then_some(item.size),
                        sha256: if last { item.sha256 } else { None },
                        created_ms: item.created_ms,
                        modified_ms: item.modified_ms,
                        attrs: if last { item.attrs.clone() } else { Default::default() },
                    });
                    let bytes = self.encode_meta(stream, &meta)?;
                    let need = RECORD_HEADER_LEN + old.stored + bytes.len() as u64;
                    if w.as_ref().unwrap().len + need > self.chunk_bytes() {
                        self.new_chunk(stream, w)?;
                    }
                    let sw = w.as_mut().unwrap();
                    let rec = sw.len;
                    let at = rec + RECORD_HEADER_LEN;
                    write_all_at(&sw.file, rec, &[0u8; RECORD_HEADER_LEN as usize])?;
                    let mut buf = vec![0u8; COPY_PIECE.min(old.stored as usize)];
                    let mut copied = 0u64;
                    while copied < old.stored {
                        let n = (old.stored - copied).min(buf.len() as u64) as usize;
                        read_exact_at(&src, old.at + copied, &mut buf[..n])?;
                        write_all_at(&sw.file, at + copied, &buf[..n])?;
                        copied += n as u64;
                    }
                    write_all_at(&sw.file, at + old.stored, &bytes)?;
                    write_all_at(&sw.file, rec, &RecordHeader { body_len: old.stored, meta_len: bytes.len() as u32 }.encode())?;
                    sw.len += need;
                    let (nc, end) = (sw.chunk, sw.len);
                    let part = Part { chunk: nc, rec, rec_len: need, at, stored: old.stored, off: old.off, len: old.len };
                    let mut ix = self.index.write().unwrap();
                    let item = match owner {
                        None => ix.files.get_mut(&key),
                        Some(o) => match stream {
                            Stream::Thumbs => ix.thumbs.get_mut(&o),
                            _ => ix.converted.get_mut(&o),
                        },
                    };
                    if let Some(item) = item.filter(|i| i.id == id) {
                        item.parts[idx] = part;
                        ix.chunks[stream.idx()].entry(nc).or_default().live += need;
                    }
                    ix.commit_pos(stream, nc, end);
                    drop(ix);
                    progress(old.rec_len);
                }
            }
        }
        Ok(())
    }

    /// Reads every stored item back and checks it. Encrypted items are authenticated
    /// segment by segment; all items are checked against their SHA-256.
    pub fn verify(&self, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) -> Result<VerifyReport> {
        let items: Vec<(String, Stream, Item)> = {
            let ix = self.index.read().unwrap();
            let names: HashMap<Id, &String> = ix.files.iter().map(|(p, f)| (f.id, p)).collect();
            let mut v: Vec<(String, Stream, Item)> = ix.files.iter().map(|(p, f)| (p.clone(), Stream::Data, f.clone())).collect();
            for s in [Stream::Thumbs, Stream::Converted] {
                for (owner, b) in ix.blobs(s) {
                    let name = names.get(owner).map(|n| n.as_str()).unwrap_or("?");
                    v.push((format!("{name}#{}", s.dir().trim_end_matches('s')), s, b.clone()));
                }
            }
            v
        };
        let total: u64 = items.iter().map(|(_, _, i)| i.size).sum();
        let mut done = 0u64;
        let mut report = VerifyReport::default();
        let mut buf = vec![0u8; COPY_PIECE];
        for (name, stream, item) in items {
            if cancel.load(Ordering::Relaxed) {
                return Err(ShardError::Cancelled);
            }
            report.items += 1;
            let ok = self.check_item(stream, item, &mut buf, &mut |n| {
                done += n;
                progress(done, total);
            });
            if !ok {
                report.damaged.push(name);
            }
        }
        Ok(report)
    }

    /// Reads one file back and checks it (used before deleting the original of a move).
    pub fn verify_file(&self, path: &str) -> Result<bool> {
        let path = norm(path)?;
        let item = self.index.read().unwrap().files.get(&path).cloned().ok_or(ShardError::NotFound(path))?;
        Ok(self.check_item(Stream::Data, item, &mut vec![0u8; COPY_PIECE], &mut |_| {}))
    }

    fn check_item(&self, stream: Stream, item: Item, buf: &mut [u8], on_read: &mut dyn FnMut(u64)) -> bool {
        let expected = item.sha256;
        let size = item.size;
        (|| -> Result<bool> {
            let r = self.reader(stream, item)?;
            let mut h = Sha256::new();
            let mut off = 0u64;
            while off < size {
                let n = r.read_full_at(off, buf)?;
                if n == 0 {
                    return Ok(false);
                }
                h.update(&buf[..n]);
                off += n as u64;
                on_read(n as u64);
            }
            Ok(expected.is_none_or(|e| e == Digest::from(h.finalize())))
        })()
        .unwrap_or(false)
    }

    /// Changes the password of an encrypted shard (the data is not rewritten).
    pub fn change_password(&self, password: &str, keyfile: Option<&[u8]>, kdf_memory_kib: u32, kdf_iterations: u32) -> Result<()> {
        self.check_writable()?;
        let key = self.key.as_ref().ok_or_else(|| ShardError::Invalid("the shard is not encrypted".into()))?;
        if password.is_empty() {
            return Err(ShardError::Invalid("password is empty".into()));
        }
        let mut info = self.info.write().unwrap();
        let enc = info.encryption.as_mut().expect("encrypted");
        let (kek, kdf) = vp_crypto::new_key(password, keyfile, kdf_memory_kib, kdf_iterations)?;
        enc.wrapped_key = vp_crypto::wrap_key(&kek, key)?;
        enc.kdf = kdf;
        write_atomic(&self.root.join(INFO_FILE), &serde_json::to_vec_pretty(&*info)?)?;
        Ok(())
    }

    /// The master key, for a locked bundle to remember.
    pub fn key(&self) -> Option<&BundleKey> {
        self.key.as_ref()
    }

    /// Copies everything into a new shard at `dst` created with `opts` (e.g. to add or
    /// remove encryption). `progress` gets bytes copied and the total.
    pub fn repack(
        &self,
        dst: &Path,
        opts: &CreateOptions,
        open: OpenOptions,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Shard> {
        let out = Shard::create_and_open(dst, opts, open)?;
        let (files, blobs) = {
            let ix = self.index.read().unwrap();
            let files: Vec<(String, Item)> = ix.files.iter().map(|(p, f)| (p.clone(), f.clone())).collect();
            let mut blobs: Vec<(Stream, String, Item)> = Vec::new();
            let names: HashMap<Id, &String> = ix.files.iter().map(|(p, f)| (f.id, p)).collect();
            for s in [Stream::Thumbs, Stream::Converted] {
                for (owner, b) in ix.blobs(s) {
                    if let Some(name) = names.get(owner) {
                        blobs.push((s, (*name).clone(), b.clone()));
                    }
                }
            }
            (files, blobs)
        };
        let total: u64 = files.iter().map(|(_, f)| f.size).sum::<u64>() + blobs.iter().map(|(_, _, b)| b.size).sum::<u64>();
        let mut done = 0u64;
        let mut buf = vec![0u8; COPY_PIECE];
        let mut copy = |r: ShardReader, mut w: ItemWriter<'_>, done: &mut u64| -> Result<()> {
            let mut off = 0u64;
            while off < r.len() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(ShardError::Cancelled);
                }
                let n = r.read_full_at(off, &mut buf)?;
                if n == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
                }
                w.write_all(&buf[..n])?;
                off += n as u64;
                *done += n as u64;
                progress(*done, total);
            }
            w.finish()
        };
        for (path, f) in files {
            let w = out.writer(&path, Some(f.size), f.created_ms, f.modified_ms)?;
            copy(self.reader(Stream::Data, f)?, w, &mut done)?;
        }
        for (s, owner, b) in blobs {
            let w = out.blob_writer(s, &owner, Some(b.size), b.attrs.clone())?;
            copy(self.reader(s, b)?, w, &mut done)?;
        }
        out.since_snapshot.fetch_add(1, Ordering::Relaxed);
        out.flush()?;
        Ok(out)
    }
}

impl Drop for Shard {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl index::Pos {
    fn max_with(self, chunk: u32, off: u64) -> Self {
        if (chunk, off) > (self.chunk, self.off) { index::Pos { chunk, off } } else { self }
    }
}
