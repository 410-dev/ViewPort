//! Shares the disk (and decoding time) between the viewer, the thumbnailer and
//! background work (scans, hashing, copies, analysis), so a picture being looked at is
//! not stuck behind a scan.
//!
//! Work runs in a [`Class`]. Reads through any source driver (see [`Paced`]) and whole
//! operations wrapped in [`run`] are timed; while another class is busy, a class whose
//! share is below half (or exactly half, against a class that comes first) waits after
//! its work in proportion to the time it took (duty cycling), so over time it holds the
//! device for about its share. A class with no competition never waits, so idle
//! bandwidth is never left unused. Shares come from `HardwareSettings` (`viewer_share_*`
//! by scheduler mode, `thumb_share`), so every rule here follows the mode.
//!
//! CPU: thumbnails are timed whole (decoding included). Background work is timed by its
//! reads, and the time a background thread spends between two reads (parsing, hashing)
//! is booked as its use too. While background work has to make room, its threads also
//! run at a lower OS priority ([`lower_priority`]).
//!
//! A viewer request being answered ([`urgent`]) also holds back the viewer's own
//! look-ahead ([`wait_urgent`]). Meanwhile nobody is held back at all: the thread made
//! to wait may hold the database (or another lock) the request needs.
//!
//! Performance mode can pause thumbnails and background work outright while the viewer
//! is busy (`pause_background`).

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
    /// Others stop while the viewer is busy.
    pause: bool,
}

/// The books of one scheduler; the app has one ([`GLOBAL`]).
struct Sched {
    config: RwLock<Config>,
    inflight: [AtomicUsize; 3],
    /// When each class last finished something, in ms since `epoch()` plus one (0 = never).
    last_end: [AtomicU64; 3],
    /// Until when each class holds off, after the time it used while others waited.
    next_free: [Mutex<Option<Instant>>; 3],
}

static GLOBAL: Sched = Sched::new();

/// Longest a class holds off for one piece of work.
const MAX_DEBT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
/// A gap between two background reads on one thread longer than this was not work
/// (the thread was idle or picked up something else).
const MAX_GAP: Duration = Duration::from_millis(250);

thread_local! {
    /// The class of the work running on this thread, inside [`run`].
    static CURRENT: Cell<Option<Class>> = const { Cell::new(None) };
    /// When this thread last finished a background read.
    static BG_LAST: Cell<Option<Instant>> = const { Cell::new(None) };
    /// This thread runs at a lower OS priority.
    static LOWERED: Cell<bool> = const { Cell::new(false) };
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
    GLOBAL.configure(h);
}

/// Runs `f` as work of class `c`: waits for its turn, counts it as busy meanwhile, and
/// books the time it took. Nested calls (and reads inside) count as the outer work.
pub fn run<T>(c: Class, f: impl FnOnce() -> T) -> T {
    GLOBAL.run(c, f)
}

impl Sched {
    const fn new() -> Self {
        Self {
            config: RwLock::new(Config { viewer: 0.5, thumb: 0.7, idle_ms: 500, pause: false }),
            inflight: [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)],
            last_end: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
            next_free: [Mutex::new(None), Mutex::new(None), Mutex::new(None)],
        }
    }

    fn configure(&self, h: &HardwareSettings) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = Config {
            viewer: h.viewer_share(),
            thumb: h.thumb_share.clamp(0.05, 0.95),
            idle_ms: h.sched_idle_ms as u64,
            pause: h.pause_background && h.scheduler == vp_core::settings::SchedulerMode::Performance,
        };
    }

    fn config(&self) -> Config {
        *self.config.read().unwrap_or_else(|e| e.into_inner())
    }

    fn busy(&self, c: Class, cfg: &Config) -> bool {
        let i = c as usize;
        if self.inflight[i].load(Ordering::Relaxed) > 0 {
            return true;
        }
        let last = self.last_end[i].load(Ordering::Relaxed);
        last > 0 && now_ms() < last - 1 + cfg.idle_ms
    }

    /// `c`'s share of the device against the classes busy now, when it must make room:
    /// below half, or exactly half against a class that comes first (viewer > thumbnails
    /// > background). `None` when it may go at full speed.
    fn share(&self, c: Class) -> Option<f32> {
        let cfg = self.config();
        let (v, t, b) = (self.busy(Class::Viewer, &cfg), self.busy(Class::Thumb, &cfg), self.busy(Class::Background, &cfg));
        let (s, before) = match c {
            Class::Viewer if t || b => (cfg.viewer, false),
            Class::Viewer => (1.0, false),
            Class::Thumb => ((if v { 1.0 - cfg.viewer } else { 1.0 }) * (if b { cfg.thumb } else { 1.0 }), v),
            Class::Background => ((if v { 1.0 - cfg.viewer } else { 1.0 }) * (if t { 1.0 - cfg.thumb } else { 1.0 }), v || t),
        };
        (s < 0.5 || (s <= 0.5 && before)).then_some(s.max(0.01))
    }

    /// Waits while `c` owes time to busier classes (or, paused, while the viewer is busy);
    /// returns early once they go idle, and at once while a viewer request is waiting.
    fn wait(&self, c: Class) {
        loop {
            if urgent_waiting() {
                return;
            }
            let cfg = self.config();
            if c != Class::Viewer && cfg.pause && self.busy(Class::Viewer, &cfg) {
                std::thread::sleep(POLL);
                continue;
            }
            let until = *self.next_free[c as usize].lock().unwrap_or_else(|e| e.into_inner());
            let Some(until) = until else { return };
            let now = Instant::now();
            if now >= until {
                return;
            }
            if self.share(c).is_none() {
                *self.next_free[c as usize].lock().unwrap_or_else(|e| e.into_inner()) = None;
                return;
            }
            std::thread::sleep((until - now).min(POLL));
        }
    }

    /// Records that `c` used the device for `used`: with competition, it holds off for
    /// `used × (1 − share) / share` before its next piece of work.
    fn owe(&self, c: Class, used: Duration) {
        let Some(s) = self.share(c) else { return };
        let debt = used.mul_f32((1.0 - s) / s).min(MAX_DEBT);
        let mut next = self.next_free[c as usize].lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let from = next.filter(|n| *n > now).unwrap_or(now);
        *next = Some(from + debt);
    }

    fn run<T>(&self, c: Class, f: impl FnOnce() -> T) -> T {
        if CURRENT.with(|x| x.get()).is_some() {
            return f();
        }
        if c == Class::Background {
            // What the thread did since its last read (parsing, hashing) used the CPU.
            if let Some(gap) = BG_LAST.with(|l| l.take()).map(|t| t.elapsed()).filter(|g| *g < MAX_GAP) {
                self.owe(c, gap);
            }
            set_lowered(self.share(c).is_some());
        } else {
            set_lowered(false);
        }
        self.wait(c);
        CURRENT.with(|x| x.set(Some(c)));
        self.inflight[c as usize].fetch_add(1, Ordering::Relaxed);
        let _running = Running { sched: self, class: c, start: Instant::now() };
        f()
    }
}

/// Restores the thread's class and settles the books even if the work panics.
struct Running<'a> {
    sched: &'a Sched,
    class: Class,
    start: Instant,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let i = self.class as usize;
        self.sched.inflight[i].fetch_sub(1, Ordering::Relaxed);
        self.sched.last_end[i].store(now_ms() + 1, Ordering::Relaxed);
        CURRENT.with(|c| c.set(None));
        self.sched.owe(self.class, self.start.elapsed());
        if self.class == Class::Background {
            BG_LAST.with(|l| l.set(Some(Instant::now())));
        }
    }
}

/// Viewer requests being answered now.
static URGENT: AtomicUsize = AtomicUsize::new(0);

/// Runs `f`, a request the viewer is waiting for: its look-ahead holds back meanwhile.
pub fn urgent<T>(f: impl FnOnce() -> T) -> T {
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            URGENT.fetch_sub(1, Ordering::Relaxed);
        }
    }
    URGENT.fetch_add(1, Ordering::Relaxed);
    let _done = Done;
    f()
}

/// Whether a viewer request is being answered.
pub fn urgent_waiting() -> bool {
    URGENT.load(Ordering::Relaxed) > 0
}

/// Waits (up to `max`) while a viewer request is being answered.
pub fn wait_urgent(max: Duration) {
    let start = Instant::now();
    while urgent_waiting() && start.elapsed() < max {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The item the viewer shows now (the first of its look-ahead).
static SHOWN: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
/// Requests for the shown item being answered.
static SHOWN_WAITING: AtomicUsize = AtomicUsize::new(0);

/// Notes which item the viewer shows now.
pub fn set_shown(id: i64) {
    SHOWN.store(id, Ordering::Relaxed);
}

pub fn is_shown(id: i64) -> bool {
    SHOWN.load(Ordering::Relaxed) == id
}

/// Runs `f`, a request for item `id`; if the viewer shows it, everything else that makes
/// pictures (neighbour preloads, the look-ahead) lets it go first meanwhile.
pub fn request<T>(id: i64, f: impl FnOnce() -> T) -> T {
    if !is_shown(id) {
        return f();
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            SHOWN_WAITING.fetch_sub(1, Ordering::Relaxed);
        }
    }
    SHOWN_WAITING.fetch_add(1, Ordering::Relaxed);
    let _done = Done;
    f()
}

/// Waits (up to `max`) while a request for the shown item is being answered.
pub fn wait_shown(max: Duration) {
    let start = Instant::now();
    while SHOWN_WAITING.load(Ordering::Relaxed) > 0 && start.elapsed() < max {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the viewer is busy and the mode puts it first (its share is at least half).
pub fn viewer_first_busy() -> bool {
    let cfg = GLOBAL.config();
    cfg.viewer >= 0.5 && GLOBAL.busy(Class::Viewer, &cfg)
}

/// Longest a write waits for the viewer in [`write_gate`] while paused.
const MAX_PAUSE: Duration = Duration::from_secs(60);

/// Called by background work before it takes the database to write (scans): lets a
/// viewer request that is waiting go first, and while paused (`pause_background`) waits
/// for the viewer to go idle. Holds no lock itself.
pub fn write_gate() {
    wait_urgent(Duration::from_secs(2));
    let start = Instant::now();
    loop {
        let cfg = GLOBAL.config();
        if !(cfg.pause && GLOBAL.busy(Class::Viewer, &cfg)) || start.elapsed() > MAX_PAUSE {
            return;
        }
        std::thread::sleep(POLL);
    }
}

/// Which classes are busy now, for diagnostics (`v`, `t`, `b`).
pub fn busy_now() -> String {
    let cfg = GLOBAL.config();
    [(Class::Viewer, 'v'), (Class::Thumb, 't'), (Class::Background, 'b')]
        .iter()
        .filter(|(c, _)| GLOBAL.busy(*c, &cfg))
        .map(|(_, n)| *n)
        .collect()
}

/// Lowering failed to be undone once (Linux without the right to raise priority):
/// threads are left alone from then on.
static PRIORITY_STUCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Lowers (or restores) this thread's OS priority, only when it changes.
fn set_lowered(low: bool) {
    if LOWERED.with(|l| l.get()) == low || (low && PRIORITY_STUCK.load(Ordering::Relaxed)) {
        return;
    }
    if lower_priority(low) {
        LOWERED.with(|l| l.set(low));
    } else if !low {
        PRIORITY_STUCK.store(true, Ordering::Relaxed);
        crate::applog::warn("sched", "could not restore a thread's priority; background threads keep theirs from now on");
    }
}

/// Sets this thread to background (or normal) OS priority; false if that failed.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn lower_priority(low: bool) -> bool {
    // SAFETY: plain syscalls on the calling thread.
    unsafe { libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, if low { 10 } else { 0 }) == 0 }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn lower_priority(low: bool) -> bool {
    let class = if low { libc::qos_class_t::QOS_CLASS_UTILITY } else { libc::qos_class_t::QOS_CLASS_USER_INITIATED };
    // SAFETY: changes only the calling thread's QoS class.
    unsafe { libc::pthread_set_qos_class_self_np(class, 0) == 0 }
}

#[cfg(windows)]
fn lower_priority(low: bool) -> bool {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL};
    // SAFETY: the pseudo handle of the calling thread.
    unsafe { SetThreadPriority(GetCurrentThread(), if low { THREAD_PRIORITY_BELOW_NORMAL } else { THREAD_PRIORITY_NORMAL }) != 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios", windows)))]
fn lower_priority(_low: bool) -> bool {
    true
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

    fn settings(mode: vp_core::settings::SchedulerMode, viewer: f32) -> HardwareSettings {
        HardwareSettings {
            scheduler: mode,
            viewer_share_performance: viewer,
            viewer_share_predictive: viewer,
            viewer_share_preprocess: viewer,
            sched_idle_ms: 200,
            ..Default::default()
        }
    }

    #[test]
    fn background_makes_room_for_the_viewer_only_while_it_is_busy() {
        use vp_core::settings::SchedulerMode::*;
        let s = Sched::new();
        s.configure(&settings(Predictive, 0.8));
        // Nobody else busy: no waiting.
        assert_eq!(s.share(Class::Background), None);
        s.run(Class::Background, || std::thread::sleep(Duration::from_millis(5)));
        assert!(s.next_free[2].lock().unwrap().is_none());

        // The viewer reads: background gets a fifth and owes 4× its time.
        s.run(Class::Viewer, || {});
        let share = s.share(Class::Background).unwrap();
        assert!((share - 0.2).abs() < 1e-3);
        assert_eq!(s.share(Class::Viewer), None, "the larger share never waits");
        let t = Instant::now();
        s.run(Class::Background, || std::thread::sleep(Duration::from_millis(20)));
        s.run(Class::Background, || {});
        assert!(t.elapsed() >= Duration::from_millis(90), "{:?}", t.elapsed());

        // Nested work counts once, as the outer class.
        s.run(Class::Viewer, || s.run(Class::Background, || assert_eq!(CURRENT.with(|c| c.get()), Some(Class::Viewer))));

        // Once the viewer is idle, background runs freely again.
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(s.share(Class::Background), None);
        let t = Instant::now();
        s.run(Class::Background, || {});
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn pausing_holds_others_until_the_viewer_is_idle() {
        use vp_core::settings::SchedulerMode::*;
        let s = Sched::new();
        s.configure(&HardwareSettings { pause_background: true, ..settings(Performance, 0.7) });
        s.run(Class::Viewer, || {});
        let t = Instant::now();
        s.run(Class::Thumb, || {});
        assert!(t.elapsed() >= Duration::from_millis(150), "{:?}", t.elapsed());
        // Only in performance mode.
        s.configure(&HardwareSettings { pause_background: true, ..settings(Predictive, 0.5) });
        s.run(Class::Viewer, || {});
        let t = Instant::now();
        s.run(Class::Background, || {});
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn ties_go_to_the_class_that_comes_first_and_modes_set_the_shares() {
        use vp_core::settings::SchedulerMode::*;
        let s = Sched::new();
        // An even split: the thumbnailer makes room for the viewer, not the other way.
        s.configure(&settings(Predictive, 0.5));
        s.run(Class::Viewer, || {});
        s.run(Class::Thumb, || {});
        assert_eq!(s.share(Class::Thumb), Some(0.5));
        assert_eq!(s.share(Class::Viewer), None);

        // Preprocessing first: the viewer is the one that makes room.
        let s = Sched::new();
        s.configure(&settings(Preprocess, 0.3));
        s.run(Class::Viewer, || {});
        s.run(Class::Background, || {});
        assert!((s.share(Class::Viewer).unwrap() - 0.3).abs() < 1e-3);
        assert_eq!(s.share(Class::Background), None);

        // Performance: background gets 30% against the viewer.
        let s = Sched::new();
        s.configure(&settings(Performance, 0.7));
        s.run(Class::Viewer, || {});
        assert!((s.share(Class::Background).unwrap() - 0.3).abs() < 1e-3);
    }
}
