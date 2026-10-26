//! Shares the disk (and decoding time) between the viewer, the thumbnailer and
//! background work (scans, hashing, copies, analysis), so a picture being looked at is
//! not stuck behind a scan.
//!
//! Work runs in a [`Class`]. Reads through any source driver (see [`Paced`]) and whole
//! operations wrapped in [`run`] are timed; while another class is busy, a class whose
//! share is below half waits after its work in proportion to the time it took
//! (duty cycling), so over time it holds the device for about its share. A class with no
//! competition never waits, so idle bandwidth is never left unused. Shares come from
//! `HardwareSettings` (`viewer_share_*` by scheduler mode, `thumb_share`).

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use vp_core::settings::HardwareSettings;
use vp_driver::{Capabilities, Entry, MediaDriver, MediaReader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// What the viewer shows and reads ahead.
    Viewer = 0,
    /// Thumbnails, on demand and in the background.
    Thumb = 1,
    /// Everything else.
    Background = 2,
}

#[derive(Clone, Copy)]
struct Config {
    viewer: f32,
    thumb: f32,
    idle_ms: u64,
}

static CONFIG: RwLock<Config> = RwLock::new(Config { viewer: 0.5, thumb: 0.7, idle_ms: 500 });
static INFLIGHT: [AtomicUsize; 3] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];
/// When each class last finished something, in ms since `epoch()` plus one (0 = never).
static LAST_END: [AtomicU64; 3] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
/// Until when each class holds off, after the time it used while others waited.
static NEXT_FREE: [Mutex<Option<Instant>>; 3] = [Mutex::new(None), Mutex::new(None), Mutex::new(None)];
/// Longest a class holds off for one piece of work.
const MAX_DEBT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);

thread_local! {
    /// The class of the work running on this thread, inside [`run`].
    static CURRENT: Cell<Option<Class>> = const { Cell::new(None) };
}

fn epoch() -> Instant {
    static E: OnceLock<Instant> = OnceLock::new();
    *E.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64
}

/// Takes the shares from the device settings (called whenever they are loaded or saved).
pub fn configure(h: &HardwareSettings) {
    *CONFIG.write().unwrap_or_else(|e| e.into_inner()) =
        Config { viewer: h.viewer_share(), thumb: h.thumb_share.clamp(0.05, 0.95), idle_ms: h.sched_idle_ms as u64 };
}

fn config() -> Config {
    *CONFIG.read().unwrap_or_else(|e| e.into_inner())
}

fn busy(c: Class, cfg: &Config) -> bool {
    let i = c as usize;
    if INFLIGHT[i].load(Ordering::Relaxed) > 0 {
        return true;
    }
    let last = LAST_END[i].load(Ordering::Relaxed);
    last > 0 && now_ms() < last - 1 + cfg.idle_ms
}

/// `c`'s share of the device against the classes busy now, when it is below half (it
/// must then make room); `None` when it may go at full speed.
fn share(c: Class) -> Option<f32> {
    let cfg = config();
    let (v, t, b) = (busy(Class::Viewer, &cfg), busy(Class::Thumb, &cfg), busy(Class::Background, &cfg));
    let s = match c {
        Class::Viewer if t || b => cfg.viewer,
        Class::Viewer => 1.0,
        Class::Thumb => (if v { 1.0 - cfg.viewer } else { 1.0 }) * (if b { cfg.thumb } else { 1.0 }),
        Class::Background => (if v { 1.0 - cfg.viewer } else { 1.0 }) * (if t { 1.0 - cfg.thumb } else { 1.0 }),
    };
    (s < 0.5).then_some(s)
}

/// Waits while `c` owes time to busier classes; returns early once they go idle.
fn wait(c: Class) {
    loop {
        let until = *NEXT_FREE[c as usize].lock().unwrap_or_else(|e| e.into_inner());
        let Some(until) = until else { return };
        let now = Instant::now();
        if now >= until {
            return;
        }
        if share(c).is_none() {
            *NEXT_FREE[c as usize].lock().unwrap_or_else(|e| e.into_inner()) = None;
            return;
        }
        std::thread::sleep((until - now).min(POLL));
    }
}

/// Records that `c` used the device for `used`: with competition, it holds off for
/// `used × (1 − share) / share` before its next piece of work.
fn owe(c: Class, used: Duration) {
    let Some(s) = share(c) else { return };
    let debt = used.mul_f32((1.0 - s) / s).min(MAX_DEBT);
    let mut next = NEXT_FREE[c as usize].lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let from = next.filter(|n| *n > now).unwrap_or(now);
    *next = Some(from + debt);
}

/// Restores the thread's class and settles the books even if the work panics.
struct Running {
    class: Class,
    start: Instant,
}

impl Drop for Running {
    fn drop(&mut self) {
        let i = self.class as usize;
        INFLIGHT[i].fetch_sub(1, Ordering::Relaxed);
        LAST_END[i].store(now_ms() + 1, Ordering::Relaxed);
        CURRENT.with(|c| c.set(None));
        owe(self.class, self.start.elapsed());
    }
}

/// Runs `f` as work of class `c`: waits for its turn, counts it as busy meanwhile, and
/// books the time it took. Nested calls (and reads inside) count as the outer work.
pub fn run<T>(c: Class, f: impl FnOnce() -> T) -> T {
    if CURRENT.with(|x| x.get()).is_some() {
        return f();
    }
    wait(c);
    CURRENT.with(|x| x.set(Some(c)));
    INFLIGHT[c as usize].fetch_add(1, Ordering::Relaxed);
    let _running = Running { class: c, start: Instant::now() };
    f()
}

/// A source driver whose reads are scheduled: reads outside [`run`] count as
/// background work.
pub struct Paced(pub Arc<dyn MediaDriver>);

struct PacedReader(Box<dyn MediaReader>);

impl MediaReader for PacedReader {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        run(Class::Background, || self.0.read_at(offset, buf))
    }

    fn read_full_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        run(Class::Background, || self.0.read_full_at(offset, buf))
    }
}

impl MediaDriver for Paced {
    fn kind(&self) -> &'static str {
        self.0.kind()
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn list(&self, dir: &str) -> vp_driver::Result<Vec<Entry>> {
        self.0.list(dir)
    }
    fn stat(&self, path: &str) -> vp_driver::Result<Entry> {
        self.0.stat(path)
    }
    fn open(&self, path: &str) -> vp_driver::Result<Box<dyn MediaReader>> {
        Ok(Box::new(PacedReader(self.0.open(path)?)))
    }
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.0.local_path(path)
    }
    fn write(&self, path: &str, data: &[u8]) -> vp_driver::Result<()> {
        self.0.write(path, data)
    }
    fn rename(&self, from: &str, to: &str) -> vp_driver::Result<()> {
        self.0.rename(from, to)
    }
    fn delete(&self, path: &str) -> vp_driver::Result<()> {
        self.0.delete(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: the counters are global.
    #[test]
    fn background_makes_room_for_the_viewer_only_while_it_is_busy() {
        configure(&HardwareSettings { viewer_share_predictive: 0.8, sched_idle_ms: 200, ..Default::default() });
        // Nobody else busy: no waiting.
        assert_eq!(share(Class::Background), None);
        run(Class::Background, || std::thread::sleep(Duration::from_millis(5)));
        assert!(NEXT_FREE[2].lock().unwrap().is_none());

        // The viewer reads: background gets a fifth and owes 4× its time.
        run(Class::Viewer, || {});
        let s = share(Class::Background).unwrap();
        assert!((s - 0.2).abs() < 1e-3);
        assert_eq!(share(Class::Viewer), None, "the larger share never waits");
        let t = Instant::now();
        run(Class::Background, || std::thread::sleep(Duration::from_millis(20)));
        run(Class::Background, || {});
        assert!(t.elapsed() >= Duration::from_millis(90), "{:?}", t.elapsed());

        // Nested work counts once, as the outer class.
        run(Class::Viewer, || run(Class::Background, || assert_eq!(CURRENT.with(|c| c.get()), Some(Class::Viewer))));

        // Once the viewer is idle, background runs freely again.
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(share(Class::Background), None);
        let t = Instant::now();
        run(Class::Background, || {});
        assert!(t.elapsed() < Duration::from_millis(50));
    }
}
