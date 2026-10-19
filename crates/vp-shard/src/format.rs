//! On-disk building blocks: ids, chunk and record headers, record metadata.
//!
//! All integers are little-endian and fixed width so a shard reads the same on every
//! platform.

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Fixed-size bytes written as lowercase hex in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hex<const N: usize>(pub [u8; N]);

/// Random identifier of a shard, a stored item, or a nonce prefix.
pub type Id = Hex<16>;
/// SHA-256 of an item's plaintext.
pub type Digest = Hex<32>;

impl<const N: usize> Hex<N> {
    pub fn random() -> Result<Self, vp_crypto::CryptoError> {
        let mut b = [0u8; N];
        vp_crypto::fill_random(&mut b)?;
        Ok(Self(b))
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.as_bytes();
        if s.len() != N * 2 {
            return None;
        }
        let mut out = [0u8; N];
        for (i, pair) in s.chunks(2).enumerate() {
            out[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl<const N: usize> fmt::Display for Hex<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl<const N: usize> fmt::Debug for Hex<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<const N: usize> Serialize for Hex<N> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de, const N: usize> Deserialize<'de> for Hex<N> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| D::Error::custom("invalid hex value"))
    }
}

/// The separate series of chunk files in a shard.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    /// The stored files themselves.
    Data,
    /// Thumbnails, keyed by the file they belong to.
    Thumbs,
    /// Converted (high-compatibility) video copies, keyed by the file they belong to.
    Converted,
}

impl Stream {
    pub const ALL: [Stream; 3] = [Stream::Data, Stream::Thumbs, Stream::Converted];

    pub fn dir(self) -> &'static str {
        match self {
            Stream::Data => "data",
            Stream::Thumbs => "thumbs",
            Stream::Converted => "converted",
        }
    }

    pub fn idx(self) -> usize {
        self as usize
    }
}

pub const CHUNK_EXTENSION: &str = "vpchnk";
pub const CHUNK_MAGIC: &[u8; 8] = b"VPCHNK01";
pub const CHUNK_HEADER_LEN: u64 = 64;
pub const RECORD_MAGIC: &[u8; 4] = b"VPRC";
pub const RECORD_HEADER_LEN: u64 = 32;
/// Space kept free at the end of a chunk for the metadata that closes a record.
pub const META_RESERVE: u64 = 8 * 1024;
/// Longest stored path, in bytes; keeps record metadata inside [`META_RESERVE`].
pub const MAX_PATH_BYTES: usize = 4096;

pub fn chunk_name(chunk: u32) -> String {
    format!("{chunk:06}.{CHUNK_EXTENSION}")
}

pub fn parse_chunk_name(name: &str) -> Option<u32> {
    name.strip_suffix(&format!(".{CHUNK_EXTENSION}"))?.parse().ok()
}

/// CRC-32 (IEEE) for header integrity; headers are tiny, so a bitwise loop is enough.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Chunk header: magic, shard id, stream, flags (bit 0 = encrypted), chunk number, CRC.
pub fn chunk_header(shard: Id, stream: Stream, chunk: u32, encrypted: bool) -> [u8; CHUNK_HEADER_LEN as usize] {
    let mut h = [0u8; CHUNK_HEADER_LEN as usize];
    h[..8].copy_from_slice(CHUNK_MAGIC);
    h[8..24].copy_from_slice(&shard.0);
    h[24] = stream as u8;
    h[25] = encrypted as u8;
    h[28..32].copy_from_slice(&chunk.to_le_bytes());
    let crc = crc32(&h[..60]);
    h[60..64].copy_from_slice(&crc.to_le_bytes());
    h
}

pub fn check_chunk_header(h: &[u8], shard: Id, stream: Stream, chunk: u32) -> bool {
    h.len() >= CHUNK_HEADER_LEN as usize
        && &h[..8] == CHUNK_MAGIC
        && h[8..24] == shard.0
        && h[24] == stream as u8
        && h[28..32] == chunk.to_le_bytes()
        && h[60..64] == crc32(&h[..60]).to_le_bytes()
}

/// A record is `header | body | meta`: the body (file data) comes first so data of
/// unknown length can be streamed in, and the header is filled in last. A record whose
/// header was never completed marks the end of the valid data in a chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordHeader {
    pub body_len: u64,
    pub meta_len: u32,
}

impl RecordHeader {
    pub fn encode(&self) -> [u8; RECORD_HEADER_LEN as usize] {
        let mut h = [0u8; RECORD_HEADER_LEN as usize];
        h[..4].copy_from_slice(RECORD_MAGIC);
        h[4..6].copy_from_slice(&1u16.to_le_bytes());
        h[8..16].copy_from_slice(&self.body_len.to_le_bytes());
        h[16..20].copy_from_slice(&self.meta_len.to_le_bytes());
        let crc = crc32(&h[..28]);
        h[28..32].copy_from_slice(&crc.to_le_bytes());
        h
    }

    pub fn decode(h: &[u8]) -> Option<Self> {
        if h.len() < RECORD_HEADER_LEN as usize || &h[..4] != RECORD_MAGIC || h[28..32] != crc32(&h[..28]).to_le_bytes() {
            return None;
        }
        Some(Self {
            body_len: u64::from_le_bytes(h[8..16].try_into().unwrap()),
            meta_len: u32::from_le_bytes(h[16..20].try_into().unwrap()),
        })
    }
}

/// What a record holds. Stored as JSON, sealed in encrypted shards so names, sizes and
/// times are never readable without the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Meta {
    /// One part of a stored item. Items larger than the room left in a chunk are split
    /// into several parts at segment boundaries.
    Part(PartMeta),
    Delete { seq: u64, path: String },
    Move { seq: u64, from: String, to: String },
}

impl Meta {
    pub fn seq(&self) -> u64 {
        match self {
            Meta::Part(p) => p.seq,
            Meta::Delete { seq, .. } | Meta::Move { seq, .. } => *seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartMeta {
    /// Order of changes: the latest write of a path wins when the index is rebuilt.
    pub seq: u64,
    pub id: Id,
    /// Path of a file (data stream).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// The file a thumbnail or converted copy belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<Id>,
    /// Nonce prefix of encrypted segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<Id>,
    pub part: u32,
    /// Plaintext offset and length of this part.
    pub off: u64,
    pub len: u64,
    pub last: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Digest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attrs: serde_json::Map<String, serde_json::Value>,
}

/// Associated data of an encrypted segment: binds it to its shard, stream, item and
/// position, and marks the final segment so truncation is detected.
pub fn segment_aad(shard: Id, stream: Stream, id: Id, seg: u64, last: bool) -> [u8; 47] {
    let mut a = [0u8; 47];
    a[..5].copy_from_slice(b"VPSEG");
    a[5..21].copy_from_slice(&shard.0);
    a[21] = stream as u8;
    a[22..38].copy_from_slice(&id.0);
    a[38..46].copy_from_slice(&seg.to_le_bytes());
    a[46] = last as u8;
    a
}

/// Per-item random prefix followed by the segment number.
pub fn segment_nonce(prefix: Id, seg: u64) -> [u8; 24] {
    let mut n = [0u8; 24];
    n[..16].copy_from_slice(&prefix.0);
    n[16..].copy_from_slice(&seg.to_le_bytes());
    n
}

pub fn meta_label(shard: Id, stream: Stream) -> Vec<u8> {
    let mut l = b"VPMETA".to_vec();
    l.extend_from_slice(&shard.0);
    l.push(stream as u8);
    l
}

pub fn index_label(shard: Id) -> Vec<u8> {
    let mut l = b"VPIDX".to_vec();
    l.extend_from_slice(&shard.0);
    l
}

/// Normalizes a stored path: `/`-separated, no empty, `.` or `..` parts.
pub fn normalize_path(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for p in path.split(['/', '\\']) {
        match p {
            "" | "." => {}
            ".." => return None,
            p if p.contains('\0') => return None,
            p => parts.push(p),
        }
    }
    let out = parts.join("/");
    (out.len() <= MAX_PATH_BYTES).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_round_trip() {
        let id = Id::random().unwrap();
        let h = chunk_header(id, Stream::Thumbs, 7, true);
        assert!(check_chunk_header(&h, id, Stream::Thumbs, 7));
        assert!(!check_chunk_header(&h, id, Stream::Data, 7));
        let r = RecordHeader { body_len: 1 << 33, meta_len: 99 };
        assert_eq!(RecordHeader::decode(&r.encode()), Some(r));
        let mut bad = r.encode();
        bad[9] ^= 1;
        assert_eq!(RecordHeader::decode(&bad), None);
        assert_eq!(RecordHeader::decode(&[0u8; 32]), None);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn ids_and_paths() {
        let id = Id::random().unwrap();
        assert_eq!(Id::parse(&id.to_string()), Some(id));
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<Id>(&json).unwrap(), id);
        assert_eq!(normalize_path("/a\\b/./c/").as_deref(), Some("a/b/c"));
        assert_eq!(normalize_path("a/../b"), None);
        assert_eq!(chunk_name(3), "000003.vpchnk");
        assert_eq!(parse_chunk_name("000003.vpchnk"), Some(3));
    }
}
