//! In-memory media cache.
//!
//! Memory is managed as a pool of fixed-size chunks rather than one allocation per
//! file, so the whole budget can be allocated (and committed) up front, files of any
//! size fit without fragmentation, and evicting a file returns its chunks for reuse.
//! Files are written straight from the source into their chunks; readers copy out.
//!
//! An entry that is being read is never evicted: readers hold an `Arc` to it and
//! eviction skips entries with outstanding references.
//!
//! The cache can be resized while in use ([`MediaCache::resize`]): chunks past a smaller
//! size are released as soon as nothing holds them ("parked") and reused if it grows.
//!
//! Lock order: `meta`, then `slots`, then a slot. Nobody waits for `meta` while holding
//! `slots` or a slot.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::Serialize;

pub type Key = i64;

const PAGE: usize = 4096;

/// One chunk of cache memory; `None` until first allocated.
type Slot = RwLock<Option<Box<[u8]>>>;

struct Entry {
    len: u64,
    chunks: Vec<u32>,
    last_use: AtomicU64,
}

#[derive(Default)]
struct Meta {
    map: HashMap<Key, Arc<Entry>>,
    /// Unused chunks below `limit`.
    free: Vec<u32>,
    /// Unused chunks at or past `limit` (after shrinking), their memory released.
    parked: Vec<u32>,
    clock: u64,
}

pub struct MediaCache {
    chunk: usize,
    max_item: AtomicU64,
    slots: RwLock<Vec<Slot>>,
    /// Chunks in use may have indices up to here; changed only under `meta`.
    limit: AtomicU32,
    meta: Mutex<Meta>,
    allocated_chunks: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CacheStats {
    pub capacity_bytes: u64,
    /// Memory currently allocated for the cache (all of it after preallocation).
    pub allocated_bytes: u64,
    /// Bytes of cached file data.
    pub used_bytes: u64,
    /// Bytes held by chunks in use (≥ `used_bytes` because of partly filled chunks).
    pub reserved_bytes: u64,
    pub items: u64,
    pub hits: u64,
    pub misses: u64,
}

impl MediaCache {
    pub fn new(capacity_bytes: u64, chunk_bytes: u64, max_item_bytes: u64) -> Arc<Self> {
        let chunk = (chunk_bytes as usize).max(PAGE).next_multiple_of(PAGE);
        let count = (capacity_bytes / chunk as u64) as usize;
        let slots: Vec<Slot> = (0..count).map(|_| RwLock::new(None)).collect();
        // Pop from the end, so hand out low indices first.
        let free = (0..count as u32).rev().collect();
        Arc::new(Self {
            chunk,
            max_item: AtomicU64::new(max_item_bytes.min(count as u64 * chunk as u64)),
            slots: RwLock::new(slots),
            limit: AtomicU32::new(count as u32),
            meta: Mutex::new(Meta { free, ..Default::default() }),
            allocated_chunks: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.limit.load(Ordering::Relaxed) as u64 * self.chunk as u64
    }

    /// Returns chunks to the pool (`meta` held): below the limit for reuse, past it with
    /// their memory released.
    fn release(&self, meta: &mut Meta, chunks: Vec<u32>) {
        let limit = self.limit.load(Ordering::Relaxed);
        for c in chunks {
            if c < limit {
                meta.free.push(c);
            } else {
                self.deallocate(c);
                meta.parked.push(c);
            }
        }
    }

    /// Releases a chunk's memory (nobody may be using it).
    fn deallocate(&self, c: u32) {
        let slots = self.slots.read().unwrap();
        if slots[c as usize].write().unwrap().take().is_some() {
            self.allocated_chunks.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Changes the cache size (and the largest cacheable item) in place. Shrinking evicts
    /// what lies past the new size; items being read there go once their readers finish.
    pub fn resize(&self, capacity_bytes: u64, max_item_bytes: u64) {
        let count = (capacity_bytes / self.chunk as u64).min(u32::MAX as u64) as u32;
        let mut meta = self.meta.lock().unwrap();
        let old = self.limit.swap(count, Ordering::Relaxed);
        if count < old {
            let (keep, past): (Vec<u32>, Vec<u32>) = meta.free.iter().partition(|&&c| c < count);
            meta.free = keep;
            for c in past {
                self.deallocate(c);
                meta.parked.push(c);
            }
            let doomed: Vec<Key> = meta
                .map
                .iter()
                .filter(|(_, e)| Arc::strong_count(e) == 1 && e.chunks.iter().any(|&c| c >= count))
                .map(|(k, _)| *k)
                .collect();
            for k in doomed {
                let e = meta.map.remove(&k).unwrap();
                let chunks = Arc::try_unwrap(e).ok().expect("unreferenced").chunks;
                self.release(&mut meta, chunks);
            }
        } else {
            let (back, still): (Vec<u32>, Vec<u32>) = meta.parked.iter().partition(|&&c| c < count);
            meta.parked = still;
            meta.free.extend(back);
            let mut slots = self.slots.write().unwrap();
            let have = slots.len() as u32;
            if count > have {
                slots.extend((have..count).map(|_| RwLock::new(None)));
                meta.free.extend((have..count).rev());
            }
        }
        self.max_item.store(max_item_bytes.min(count as u64 * self.chunk as u64), Ordering::Relaxed);
    }

    /// Releases the memory of unused chunks (it is allocated again when needed).
    pub fn trim(&self) {
        let meta = self.meta.lock().unwrap();
        for &c in &meta.free {
            self.deallocate(c);
        }
    }

    fn allocate_chunk(&self) -> Box<[u8]> {
        let mut buf = vec![0u8; self.chunk].into_boxed_slice();
        // Zeroed allocations are usually backed by untouched pages that the OS only commits
        // on first write. Touch every page so the memory is really ours now.
        for i in (0..buf.len()).step_by(PAGE) {
            // SAFETY: `i` is in bounds; a volatile write keeps the store from being optimized out.
            unsafe { std::ptr::write_volatile(buf.as_mut_ptr().add(i), 0) };
        }
        self.allocated_chunks.fetch_add(1, Ordering::Relaxed);
        buf
    }

    /// Allocates and commits every chunk. Blocking and slow for large caches; run it on a
    /// background thread. Stops early when `stop` is set.
    pub fn preallocate(&self, stop: &AtomicBool) {
        self.preallocate_with(stop, &mut |_, _| {});
    }

    /// [`MediaCache::preallocate`] reporting `(chunks done, chunks total)`.
    pub fn preallocate_with(&self, stop: &AtomicBool, progress: &mut dyn FnMut(u64, u64)) {
        let mut i = 0u32;
        loop {
            let total = self.limit.load(Ordering::Relaxed);
            if i >= total || stop.load(Ordering::Relaxed) {
                return;
            }
            progress(i as u64, total as u64);
            let slots = self.slots.read().unwrap();
            if let Some(slot) = slots.get(i as usize) {
                let mut s = slot.write().unwrap();
                if s.is_none() {
                    *s = Some(self.allocate_chunk());
                }
            }
            i += 1;
        }
    }

    /// Returns the cached item and marks it recently used.
    pub fn get(self: &Arc<Self>, key: Key) -> Option<CachedMedia> {
        let mut meta = self.meta.lock().unwrap();
        meta.clock += 1;
        let clock = meta.clock;
        match meta.map.get(&key) {
            Some(e) => {
                e.last_use.store(clock, Ordering::Relaxed);
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(CachedMedia::new(self, e.clone()))
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    pub fn contains(&self, key: Key) -> bool {
        self.meta.lock().unwrap().map.contains_key(&key)
    }

    /// Whether an item of `len` bytes is cacheable at all.
    pub fn fits(&self, len: u64) -> bool {
        len > 0 && len <= self.max_item.load(Ordering::Relaxed)
    }

    /// Caches `len` bytes produced by `fill(offset, buf)`, which must fill `buf` completely.
    /// Least recently used items are evicted to make room. Returns `None` when the item
    /// does not fit (too large, or everything else is in use).
    pub fn insert_with(
        self: &Arc<Self>,
        key: Key,
        len: u64,
        mut fill: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
    ) -> io::Result<Option<CachedMedia>> {
        if !self.fits(len) {
            return Ok(None);
        }
        let needed = len.div_ceil(self.chunk as u64) as usize;
        let chunks = {
            let mut meta = self.meta.lock().unwrap();
            if meta.map.contains_key(&key) {
                drop(meta);
                return Ok(self.get(key));
            }
            while meta.free.len() < needed {
                if !self.evict_one(&mut meta) {
                    return Ok(None);
                }
            }
            let at = meta.free.len() - needed;
            meta.free.split_off(at)
        };

        for (i, &c) in chunks.iter().enumerate() {
            let offset = i as u64 * self.chunk as u64;
            let size = (len - offset).min(self.chunk as u64) as usize;
            let filled = {
                let slots = self.slots.read().unwrap();
                let mut slot = slots[c as usize].write().unwrap();
                let buf = slot.get_or_insert_with(|| self.allocate_chunk());
                fill(offset, &mut buf[..size])
            };
            if let Err(e) = filled {
                let mut meta = self.meta.lock().unwrap();
                self.release(&mut meta, chunks);
                return Err(e);
            }
        }

        let mut meta = self.meta.lock().unwrap();
        meta.clock += 1;
        if let Some(existing) = meta.map.get(&key).cloned() {
            // Someone else cached it meanwhile; keep theirs.
            self.release(&mut meta, chunks);
            return Ok(Some(CachedMedia::new(self, existing)));
        }
        let entry = Arc::new(Entry { len, chunks, last_use: AtomicU64::new(meta.clock) });
        meta.map.insert(key, entry.clone());
        Ok(Some(CachedMedia::new(self, entry)))
    }

    /// Drops an item. Chunks are reused once no reader holds it.
    pub fn remove(&self, key: Key) {
        let mut meta = self.meta.lock().unwrap();
        if let Some(e) = meta.map.remove(&key) {
            // If it is still being read, the last reader returns the chunks (see `Drop`).
            if let Ok(e) = Arc::try_unwrap(e) {
                self.release(&mut meta, e.chunks);
            }
        }
    }

    /// Drops every item whose key `pred` accepts.
    pub fn remove_where(&self, pred: impl Fn(Key) -> bool) {
        let keys: Vec<Key> = self.meta.lock().unwrap().map.keys().copied().filter(|&k| pred(k)).collect();
        for k in keys {
            self.remove(k);
        }
    }

    pub fn clear(&self) {
        let keys: Vec<Key> = self.meta.lock().unwrap().map.keys().copied().collect();
        for k in keys {
            self.remove(k);
        }
    }

    pub fn stats(&self) -> CacheStats {
        let meta = self.meta.lock().unwrap();
        let used: u64 = meta.map.values().map(|e| e.len).sum();
        let reserved_chunks = (self.limit.load(Ordering::Relaxed) as u64).saturating_sub(meta.free.len() as u64);
        CacheStats {
            capacity_bytes: self.capacity_bytes(),
            allocated_bytes: self.allocated_chunks.load(Ordering::Relaxed) * self.chunk as u64,
            used_bytes: used,
            reserved_bytes: reserved_chunks * self.chunk as u64,
            items: meta.map.len() as u64,
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
        }
    }
}

impl MediaCache {
    /// Evicts the least recently used item nobody is reading. Returns false if none could be.
    fn evict_one(&self, meta: &mut Meta) -> bool {
        let victim = meta
            .map
            .iter()
            .filter(|(_, e)| Arc::strong_count(e) == 1)
            .min_by_key(|(_, e)| e.last_use.load(Ordering::Relaxed))
            .map(|(k, _)| *k);
        let Some(k) = victim else { return false };
        let e = meta.map.remove(&k).unwrap();
        let chunks = Arc::try_unwrap(e).ok().expect("unreferenced").chunks;
        self.release(meta, chunks);
        true
    }
}

/// A pinned cached item. While it exists the item stays in memory.
pub struct CachedMedia {
    cache: Arc<MediaCache>,
    /// Always `Some` until dropped.
    entry: Option<Arc<Entry>>,
}

impl CachedMedia {
    fn new(cache: &Arc<MediaCache>, entry: Arc<Entry>) -> Self {
        Self { cache: cache.clone(), entry: Some(entry) }
    }

    fn entry(&self) -> &Entry {
        self.entry.as_ref().expect("live entry")
    }

    pub fn len(&self) -> u64 {
        self.entry().len
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Copies bytes starting at `offset` into `buf`; returns how many were copied.
    pub fn read_at(&self, mut offset: u64, buf: &mut [u8]) -> usize {
        let chunk = self.cache.chunk as u64;
        let entry = self.entry();
        let mut done = 0;
        while done < buf.len() && offset < entry.len {
            let idx = (offset / chunk) as usize;
            let within = (offset % chunk) as usize;
            let avail = ((entry.len - offset).min(chunk - within as u64)) as usize;
            let n = avail.min(buf.len() - done);
            let slots = self.cache.slots.read().unwrap();
            let slot = slots[entry.chunks[idx] as usize].read().unwrap();
            let data = slot.as_ref().expect("filled chunk");
            buf[done..done + n].copy_from_slice(&data[within..within + n]);
            done += n;
            offset += n as u64;
        }
        done
    }

    pub fn to_vec(&self, offset: u64, max_len: u64) -> Vec<u8> {
        let len = max_len.min(self.len().saturating_sub(offset)) as usize;
        let mut v = vec![0u8; len];
        let n = self.read_at(offset, &mut v);
        v.truncate(n);
        v
    }
}

impl Drop for CachedMedia {
    fn drop(&mut self) {
        // Released under the lock so exactly one holder sees itself as the last one. If the
        // map no longer references the item (removed while being read), that holder returns
        // its chunks.
        let mut meta = self.cache.meta.lock().unwrap();
        if let Some(Ok(e)) = self.entry.take().map(Arc::try_unwrap) {
            self.cache.release(&mut meta, e.chunks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: u64 = 4096;

    fn data(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
    }

    fn put(c: &Arc<MediaCache>, key: Key, bytes: &[u8]) -> Option<CachedMedia> {
        c.insert_with(key, bytes.len() as u64, |off, buf| {
            buf.copy_from_slice(&bytes[off as usize..off as usize + buf.len()]);
            Ok(())
        })
        .unwrap()
    }

    #[test]
    fn round_trip_across_chunks() {
        let c = MediaCache::new(16 * K, K, u64::MAX);
        let bytes = data(10_000, 7);
        drop(put(&c, 1, &bytes));
        let m = c.get(1).unwrap();
        assert_eq!(m.to_vec(0, u64::MAX), bytes);
        assert_eq!(m.to_vec(4000, 200), &bytes[4000..4200]);
        assert_eq!(m.to_vec(9990, 100), &bytes[9990..]);
        assert!(m.to_vec(20_000, 10).is_empty());
        let s = c.stats();
        assert_eq!((s.items, s.used_bytes, s.reserved_bytes), (1, 10_000, 3 * K));
    }

    #[test]
    fn evicts_least_recently_used_but_not_pinned() {
        let c = MediaCache::new(4 * K, K, u64::MAX);
        for k in 1..=4 {
            drop(put(&c, k, &data(K as usize, k as u8)));
        }
        drop(c.get(1)); // 1 is now most recent; 2 is the oldest
        let pinned = c.get(2).unwrap(); // ...but pinned, so 3 goes instead
        drop(put(&c, 5, &data(K as usize, 5)));
        assert!(c.contains(1) && c.contains(2) && !c.contains(3) && c.contains(4) && c.contains(5));
        assert_eq!(pinned.to_vec(0, 8), &data(K as usize, 2)[..8]);
    }

    #[test]
    fn rejects_what_cannot_fit() {
        let c = MediaCache::new(4 * K, K, 2 * K);
        assert!(put(&c, 1, &data(3 * K as usize, 1)).is_none(), "over max item size");
        let a = put(&c, 2, &data(2 * K as usize, 2)).unwrap();
        let b = put(&c, 3, &data(2 * K as usize, 3)).unwrap();
        assert!(put(&c, 4, &data(K as usize, 4)).is_none(), "everything is pinned");
        drop((a, b));
        assert!(put(&c, 4, &data(K as usize, 4)).is_some());
    }

    #[test]
    fn removed_while_reading_frees_on_drop() {
        let c = MediaCache::new(2 * K, K, u64::MAX);
        let m = put(&c, 1, &data(2 * K as usize, 1)).unwrap();
        c.remove(1);
        assert_eq!(m.to_vec(0, 4), &data(4, 1)[..], "reader still sees data");
        assert!(put(&c, 2, &data(K as usize, 2)).is_none());
        drop(m);
        assert!(put(&c, 2, &data(2 * K as usize, 2)).is_some());
    }

    #[test]
    fn failed_fill_returns_chunks() {
        let c = MediaCache::new(2 * K, K, u64::MAX);
        let r = c.insert_with(1, 2 * K, |_, _| Err(io::Error::other("boom")));
        assert!(r.is_err());
        assert!(put(&c, 2, &data(2 * K as usize, 2)).is_some());
    }

    #[test]
    fn preallocates() {
        let c = MediaCache::new(8 * K, K, u64::MAX);
        assert_eq!(c.stats().allocated_bytes, 0);
        c.preallocate(&AtomicBool::new(false));
        assert_eq!(c.stats().allocated_bytes, 8 * K);
    }

    #[test]
    fn resizes_in_place() {
        let c = MediaCache::new(4 * K, K, u64::MAX);
        for k in 1..=4 {
            drop(put(&c, k, &data(K as usize, k as u8)));
        }
        let pinned = c.get(4).unwrap();
        // Shrinking to two chunks evicts what lies past them, except what is being read.
        c.resize(2 * K, u64::MAX);
        assert_eq!(c.capacity_bytes(), 2 * K);
        assert!(c.contains(1) && c.contains(2) && !c.contains(3) && c.contains(4));
        assert!(put(&c, 5, &data(K as usize, 5)).is_some(), "evicts an old one below the limit");
        assert_eq!(pinned.to_vec(0, 8), &data(K as usize, 4)[..8]);
        c.remove(4);
        drop(pinned);
        assert!(c.stats().allocated_bytes <= 2 * K, "chunks past the limit are released");
        // Growing past the original size adds chunks.
        c.resize(6 * K, 3 * K);
        assert_eq!(c.capacity_bytes(), 6 * K);
        let big = data(3 * K as usize, 9);
        assert_eq!(put(&c, 9, &big).unwrap().to_vec(0, u64::MAX), big);
        for k in 10..14 {
            drop(put(&c, k, &data(K as usize, k as u8)));
        }
        assert!(c.stats().reserved_bytes <= 6 * K);
        c.clear();
        c.trim();
        assert_eq!(c.stats().allocated_bytes, 0);
    }

    #[test]
    fn concurrent_use() {
        let c = MediaCache::new(64 * K, K, u64::MAX);
        std::thread::scope(|s| {
            for t in 0..8u8 {
                let c = c.clone();
                s.spawn(move || {
                    for i in 0..200i64 {
                        let key = i % 40;
                        let bytes = data(3 * K as usize, key as u8);
                        let m = c.get(key).or_else(|| put(&c, key, &bytes));
                        if let Some(m) = m {
                            assert_eq!(m.to_vec(0, u64::MAX), bytes, "thread {t}");
                        }
                    }
                });
            }
        });
    }
}
