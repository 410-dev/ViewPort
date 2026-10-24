//! The in-memory index: where every item's parts are, plus chunk bookkeeping.
//!
//! The chunks are the source of truth. The index is saved as a snapshot from time to time;
//! records written after the snapshot are replayed on open, and a lost snapshot is rebuilt
//! by replaying every chunk. Replay orders changes by their sequence number, so the latest
//! write, delete or move of a path wins no matter where its record ended up after
//! compaction.
//!
//! Several files may share one stored body (deduplication, [`crate::format::LinkMeta`]):
//! each keeps its own id, and the body's records stay live while any file refers to it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Bound;

use serde::{Deserialize, Serialize};

use crate::format::{Digest, Id, LinkMeta, Meta, PartMeta, Stream};

/// Location of one stored part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub chunk: u32,
    /// Offset and total length of the record (header, body, meta) in the chunk file.
    pub rec: u64,
    pub rec_len: u64,
    /// Offset of the body in the chunk file and its stored length.
    pub at: u64,
    pub stored: u64,
    /// Plaintext range this part covers.
    pub off: u64,
    pub len: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: Id,
    pub seq: u64,
    #[serde(default)]
    pub nonce: Option<Id>,
    pub size: u64,
    #[serde(default)]
    pub created_ms: Option<i64>,
    #[serde(default)]
    pub modified_ms: Option<i64>,
    #[serde(default)]
    pub sha256: Option<Digest>,
    pub parts: Vec<Part>,
    #[serde(default)]
    pub attrs: serde_json::Map<String, serde_json::Value>,
    /// Id of the stored data this file shares with others, when it is not its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Id>,
}

impl Item {
    /// Id the item's data was written under: its own, or the body it links to.
    pub fn body_id(&self) -> Id {
        self.body.unwrap_or(self.id)
    }
}

/// A delete or move record, kept so compaction can carry it over: without it a rebuilt
/// index could bring back a file deleted long ago.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tomb {
    pub chunk: u32,
    pub rec: u64,
    pub rec_len: u64,
    pub meta: Meta,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ChunkStat {
    /// File length.
    pub len: u64,
    /// Bytes of records still in use; the rest is garbage that compaction reclaims.
    pub live: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Pos {
    pub chunk: u32,
    pub off: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Index {
    pub files: BTreeMap<String, Item>,
    /// Thumbnails and converted copies by owner file id.
    #[serde(default)]
    pub thumbs: HashMap<Id, Item>,
    #[serde(default)]
    pub converted: HashMap<Id, Item>,
    #[serde(default)]
    pub tombs: Vec<Tomb>,
    #[serde(default)]
    pub chunks: [BTreeMap<u32, ChunkStat>; 3],
    /// Everything up to here in each stream is reflected in this index.
    #[serde(default)]
    pub applied: [Pos; 3],
    /// Chunk numbers are never reused.
    #[serde(default)]
    pub next_chunk: [u32; 3],
    #[serde(default)]
    pub next_seq: u64,
    /// How many files refer to each stored body (rebuilt on load).
    #[serde(skip)]
    pub refs: HashMap<Id, u32>,
}

/// A record found while scanning a chunk.
#[derive(Debug, Clone)]
pub struct Found {
    pub stream: Stream,
    pub chunk: u32,
    pub rec: u64,
    pub rec_len: u64,
    pub at: u64,
    pub stored: u64,
    pub meta: Meta,
}

impl Found {
    pub fn part(&self, p: &PartMeta) -> Part {
        Part {
            chunk: self.chunk,
            rec: self.rec,
            rec_len: self.rec_len,
            at: self.at,
            stored: self.stored,
            off: p.off,
            len: p.len,
        }
    }
}

enum Event {
    File(String, Item),
    Blob(Stream, Id, Item),
    Tomb(Tomb),
}

/// What a link record turns into: the linked file with the body's data.
fn linked_item(l: &LinkMeta, body: &Item) -> Item {
    Item {
        id: l.id,
        seq: l.seq,
        nonce: body.nonce,
        size: body.size,
        created_ms: l.created_ms,
        modified_ms: l.modified_ms,
        sha256: body.sha256,
        parts: body.parts.clone(),
        attrs: Default::default(),
        body: (l.id != l.body).then_some(l.body),
    }
}

impl Index {
    pub fn blobs(&self, stream: Stream) -> &HashMap<Id, Item> {
        match stream {
            Stream::Thumbs => &self.thumbs,
            _ => &self.converted,
        }
    }

    fn blobs_mut(&mut self, stream: Stream) -> &mut HashMap<Id, Item> {
        match stream {
            Stream::Thumbs => &mut self.thumbs,
            _ => &mut self.converted,
        }
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    fn add_live(&mut self, stream: Stream, chunk: u32, n: u64) {
        let stat = self.chunks[stream.idx()].entry(chunk).or_default();
        stat.live += n;
    }

    fn sub_live(&mut self, stream: Stream, chunk: u32, n: u64) {
        if let Some(stat) = self.chunks[stream.idx()].get_mut(&chunk) {
            stat.live = stat.live.saturating_sub(n);
        }
    }

    /// Counts the files referring to each body; called after loading a snapshot.
    pub fn rebuild_refs(&mut self) {
        let mut refs = HashMap::new();
        for f in self.files.values() {
            *refs.entry(f.body_id()).or_insert(0) += 1;
        }
        self.refs = refs;
    }

    /// A file now refers to its body; the body's records become live with the first.
    fn acquire(&mut self, item: &Item) {
        let n = self.refs.entry(item.body_id()).or_insert(0);
        *n += 1;
        if *n == 1 {
            self.add_item_live(Stream::Data, item);
        }
    }

    /// A file stopped referring to its body; the body is garbage after the last one.
    fn release(&mut self, item: &Item, drop_blobs: bool) {
        let body = item.body_id();
        if let Some(n) = self.refs.get_mut(&body) {
            *n -= 1;
            if *n == 0 {
                self.refs.remove(&body);
                self.sub_item_live(Stream::Data, item);
            }
        }
        if drop_blobs {
            self.drop_blobs_of(item.id);
        }
    }

    /// Paths of the files whose data is `body`.
    pub fn holders(&self, body: Id, hint: &str) -> Vec<String> {
        if self.refs.get(&body).copied().unwrap_or(0) <= 1 && self.files.get(hint).is_some_and(|f| f.body_id() == body) {
            return vec![hint.to_string()];
        }
        self.files.iter().filter(|(_, f)| f.body_id() == body).map(|(p, _)| p.clone()).collect()
    }

    /// A file holding `body` (preferring `hint`), to copy its data locations from.
    pub fn find_body(&self, body: Id, hint: &str) -> Option<&Item> {
        if let Some(f) = self.files.get(hint).filter(|f| f.body_id() == body) {
            return Some(f);
        }
        if !self.refs.contains_key(&body) {
            return None;
        }
        self.files.values().find(|f| f.body_id() == body)
    }

    fn add_item_live(&mut self, stream: Stream, item: &Item) {
        for p in &item.parts {
            self.add_live(stream, p.chunk, p.rec_len);
        }
    }

    fn sub_item_live(&mut self, stream: Stream, item: &Item) {
        for p in &item.parts {
            self.sub_live(stream, p.chunk, p.rec_len);
        }
    }

    /// Records that the stream holds valid data up to `pos` (a record was completed).
    pub fn commit_pos(&mut self, stream: Stream, chunk: u32, end: u64) {
        let stat = self.chunks[stream.idx()].entry(chunk).or_default();
        stat.len = stat.len.max(end);
        self.applied[stream.idx()] = Pos { chunk, off: end };
    }

    /// Removes a file together with its thumbnail and converted copy.
    pub fn remove_file(&mut self, path: &str) -> Option<Item> {
        let item = self.files.remove(path)?;
        self.release(&item, true);
        Some(item)
    }

    fn drop_blobs_of(&mut self, owner: Id) {
        for s in [Stream::Thumbs, Stream::Converted] {
            if let Some(b) = self.blobs_mut(s).remove(&owner) {
                self.sub_item_live(s, &b);
            }
        }
    }

    /// Puts `item` at `path` unless a newer write is already there. Returns whether it
    /// was installed; a rejected item's bytes are garbage.
    pub fn install_file(&mut self, path: &str, item: Item) -> bool {
        if let Some(old) = self.files.get(path) {
            if old.seq > item.seq {
                return false;
            }
            // The same item seen again (a copy made by compaction, or now linked to shared
            // data) keeps its thumbnail and converted copy.
            let old = self.files.remove(path).unwrap();
            self.release(&old, old.id != item.id);
        }
        self.acquire(&item);
        self.files.insert(path.to_string(), item);
        true
    }

    pub fn install_blob(&mut self, stream: Stream, owner: Id, item: Item) -> bool {
        if let Some(old) = self.blobs(stream).get(&owner) {
            if old.seq > item.seq {
                return false;
            }
            let old = self.blobs_mut(stream).remove(&owner).unwrap();
            self.sub_item_live(stream, &old);
        }
        self.add_item_live(stream, &item);
        self.blobs_mut(stream).insert(owner, item);
        true
    }

    /// Applies a delete, move or link record and remembers it for compaction. `pool` has
    /// bodies found while replaying, for links whose data no file holds yet.
    pub fn apply_tomb(&mut self, tomb: Tomb, pool: Option<&HashMap<Id, Item>>) {
        match &tomb.meta {
            Meta::Link(l) => {
                let blocked = self.files.get(&l.path).is_some_and(|f| f.seq > l.seq);
                let body = self.find_body(l.body, &l.from).or_else(|| pool.and_then(|p| p.get(&l.body)));
                if let Some(item) = body.map(|b| linked_item(l, b)).filter(|_| !blocked) {
                    self.install_file(&l.path, item);
                }
            }
            Meta::Delete { seq, path } => {
                if self.files.get(path).is_some_and(|f| f.seq < *seq) {
                    self.remove_file(path);
                }
            }
            Meta::Move { seq, from, to } => {
                let movable = self.files.get(from).is_some_and(|f| f.seq < *seq);
                let blocked = self.files.get(to).is_some_and(|f| f.seq > *seq);
                if movable && !blocked {
                    let mut item = self.files.remove(from).unwrap();
                    item.seq = *seq;
                    if let Some(old) = self.files.remove(to) {
                        self.release(&old, old.id != item.id);
                    }
                    self.files.insert(to.clone(), item);
                }
            }
            Meta::Part(_) => return,
        }
        self.add_live(Stream::Data, tomb.chunk, tomb.rec_len);
        self.next_seq = self.next_seq.max(tomb.meta.seq() + 1);
        self.tombs.push(tomb);
    }

    /// Points the existing items a part record belongs to at that record (a copy made by
    /// compaction). Data parts are found by body: through the path the record names,
    /// else by a full search (the files may have moved or share the data). Returns the
    /// part's previous location, or `None` if no item has it.
    fn relocate_part(&mut self, stream: Stream, meta: &PartMeta, part: &Part) -> Option<Part> {
        let idx = meta.part as usize;
        match stream {
            Stream::Data => {
                let keys = if self.refs.contains_key(&meta.id) { self.holders(meta.id, &meta.path) } else { Vec::new() };
                let mut old = None;
                for k in keys {
                    if let Some(p) = self.files.get_mut(&k).and_then(|f| f.parts.get_mut(idx)) {
                        old = Some(std::mem::replace(p, part.clone()));
                    }
                }
                old
            }
            s => {
                let owner = meta.owner?;
                let item = self.blobs_mut(s).get_mut(&owner).filter(|b| b.id == meta.id)?;
                item.parts.get_mut(idx).map(|p| std::mem::replace(p, part.clone()))
            }
        }
    }

    /// Replays records found after the last snapshot (or every record, when rebuilding).
    pub fn replay(&mut self, found: Vec<Found>) {
        struct Group {
            stream: Stream,
            parts: BTreeMap<u32, (Part, PartMeta)>,
        }
        let mut groups: HashMap<Id, Group> = HashMap::new();
        let mut events: Vec<(u64, Event)> = Vec::new();
        let mut pool: HashMap<Id, Item> = HashMap::new();
        for f in found {
            match &f.meta {
                Meta::Part(p) => {
                    let part = f.part(p);
                    self.next_seq = self.next_seq.max(p.seq + 1);
                    // A copy of a part of a known item (compaction): point at the new place.
                    if let Some(old) = self.relocate_part(f.stream, p, &part) {
                        self.sub_live(f.stream, old.chunk, old.rec_len);
                        self.add_live(f.stream, part.chunk, part.rec_len);
                        continue;
                    }
                    groups
                        .entry(p.id)
                        .or_insert_with(|| Group { stream: f.stream, parts: BTreeMap::new() })
                        .parts
                        .insert(p.part, (part, p.clone()));
                }
                Meta::Delete { .. } | Meta::Move { .. } | Meta::Link(_) => events.push((
                    f.meta.seq(),
                    Event::Tomb(Tomb { chunk: f.chunk, rec: f.rec, rec_len: f.rec_len, meta: f.meta.clone() }),
                )),
            }
        }
        for (id, g) in groups {
            // Complete only with every part from 0 up to one marked last.
            let n = g.parts.len() as u32;
            let complete = g.parts.keys().copied().eq(0..n) && g.parts.values().last().is_some_and(|(_, m)| m.last);
            if !complete {
                continue; // an interrupted write: garbage
            }
            let first = g.parts.values().next().unwrap().1.clone();
            let last = g.parts.values().last().unwrap().1.clone();
            let item = Item {
                id,
                seq: first.seq,
                nonce: first.nonce,
                size: last.size.unwrap_or(last.off + last.len),
                created_ms: first.created_ms,
                modified_ms: first.modified_ms,
                sha256: last.sha256,
                parts: g.parts.into_values().map(|(p, _)| p).collect(),
                attrs: last.attrs.clone(),
                body: None,
            };
            let event = match g.stream {
                // Data no file holds as its own any more, kept only for the links to it.
                Stream::Data if first.path.is_empty() => {
                    pool.insert(id, item);
                    continue;
                }
                Stream::Data => {
                    pool.insert(id, item.clone());
                    Event::File(first.path.clone(), item)
                }
                s => match first.owner {
                    Some(owner) => Event::Blob(s, owner, item),
                    None => continue,
                },
            };
            events.push((first.seq, event));
        }
        events.sort_by_key(|(seq, _)| *seq);
        for (_, e) in events {
            match e {
                Event::File(path, item) => {
                    self.install_file(&path, item);
                }
                Event::Blob(s, owner, item) => {
                    self.install_blob(s, owner, item);
                }
                Event::Tomb(t) => self.apply_tomb(t, Some(&pool)),

            }
        }
        self.prune_orphans();
    }

    /// Drops thumbnails and converted copies whose file no longer exists.
    pub fn prune_orphans(&mut self) {
        let ids: HashSet<Id> = self.files.values().map(|f| f.id).collect();
        for s in [Stream::Thumbs, Stream::Converted] {
            let orphans: Vec<Id> = self.blobs(s).keys().filter(|o| !ids.contains(o)).copied().collect();
            for o in orphans {
                if let Some(b) = self.blobs_mut(s).remove(&o) {
                    self.sub_item_live(s, &b);
                }
            }
        }
    }

    /// Paths of the direct children of `dir`: files, then subfolders.
    pub fn children(&self, dir: &str) -> (Vec<(&String, &Item)>, Vec<String>) {
        let mut files = Vec::new();
        let mut dirs: Vec<String> = Vec::new();
        for (path, item) in self.files.range::<String, _>(prefix_range(dir)) {
            let rest = if dir.is_empty() { path.as_str() } else { &path[dir.len() + 1..] };
            match rest.split_once('/') {
                None => files.push((path, item)),
                Some((sub, _)) => {
                    if dirs.last().map(String::as_str) != Some(sub) {
                        dirs.push(sub.to_string());
                    }
                }
            }
        }
        dirs.dedup();
        (files, dirs)
    }

    /// Files at or under `path` (a file or a folder; `""` = everything).
    pub fn under<'a>(&'a self, path: &str) -> Vec<(&'a String, &'a Item)> {
        if let Some((k, v)) = self.files.get_key_value(path) {
            return vec![(k, v)];
        }
        self.files.range::<String, _>(prefix_range(path)).collect()
    }

    pub fn is_dir(&self, path: &str) -> bool {
        path.is_empty() || self.files.range::<String, _>(prefix_range(path)).next().is_some()
    }
}

/// Key range of everything below folder `dir`. `'0'` follows `'/'` in byte order.
fn prefix_range(dir: &str) -> (Bound<String>, Bound<String>) {
    if dir.is_empty() {
        (Bound::Unbounded, Bound::Unbounded)
    } else {
        (Bound::Included(format!("{dir}/")), Bound::Excluded(format!("{dir}0")))
    }
}
