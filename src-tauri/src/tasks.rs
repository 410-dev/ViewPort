//! Background task registry: everything that takes noticeable time (scans, thumbnail
//! generation, video probes, conversions, slow file reads, cache warm-up) registers a
//! [`Task`] here, and the UI shows them in the status bar.
//!
//! Progress is sent as `task` events, throttled per task by
//! `progress.emit_interval_ms`; state changes (start, finish, failure) are always sent.
//! New kinds of work only need to call [`begin`] and update the returned handle.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::workspace::now_ms;

pub const EVENT: &str = "task";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Items,
    Bytes,
    /// `done` / `total` are per-mille (0..=1000).
    Permille,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskInfo {
    pub id: u64,
    /// What kind of work this is (`scan`, `thumbs`, `read`, ...); the UI localizes it.
    pub kind: &'static str,
    /// Subject of the task: a source name, a file name.
    pub label: String,
    /// Current step, e.g. the folder being listed.
    pub detail: String,
    pub done: u64,
    pub total: Option<u64>,
    pub unit: Unit,
    pub state: TaskState,
    pub error: Option<String>,
    pub cancellable: bool,
    pub started_ms: i64,
    /// Units per second since the start.
    pub rate: f64,
}

struct Inner {
    info: Mutex<TaskInfo>,
    started: Instant,
    last_emit: Mutex<Option<Instant>>,
    cancel: Arc<AtomicBool>,
}

pub struct Hub {
    app: OnceLock<AppHandle>,
    next: AtomicU64,
    live: Mutex<BTreeMap<u64, Arc<Inner>>>,
}

static HUB: Hub = Hub { app: OnceLock::new(), next: AtomicU64::new(1), live: Mutex::new(BTreeMap::new()) };

/// Connects the registry to the app so it can emit events. Call once at startup.
pub fn init(app: &AppHandle) {
    let _ = HUB.app.set(app.clone());
}

fn emit_interval() -> Duration {
    Duration::from_millis(crate::recent::cached_app_settings().progress.emit_interval_ms.max(16))
}

/// Starts a task. Dropping the handle finishes it (as done, unless it was marked
/// otherwise), so early returns never leave a task running in the UI.
pub fn begin(kind: &'static str, label: impl Into<String>, unit: Unit, total: Option<u64>) -> Task {
    begin_with_cancel(kind, label, unit, total, None)
}

/// Like [`begin`], but cancellable from the UI through `cancel`.
pub fn begin_with_cancel(
    kind: &'static str,
    label: impl Into<String>,
    unit: Unit,
    total: Option<u64>,
    cancel: Option<Arc<AtomicBool>>,
) -> Task {
    let id = HUB.next.fetch_add(1, Ordering::Relaxed);
    let inner = Arc::new(Inner {
        info: Mutex::new(TaskInfo {
            id,
            kind,
            label: label.into(),
            detail: String::new(),
            done: 0,
            total,
            unit,
            state: TaskState::Running,
            error: None,
            cancellable: cancel.is_some(),
            started_ms: now_ms(),
            rate: 0.0,
        }),
        started: Instant::now(),
        last_emit: Mutex::new(None),
        cancel: cancel.unwrap_or_default(),
    });
    HUB.live.lock().unwrap().insert(id, inner.clone());
    let task = Task { inner };
    task.emit(true);
    task
}

/// Running tasks, for a UI that starts listening late.
pub fn list() -> Vec<TaskInfo> {
    let mut v: Vec<TaskInfo> = HUB.live.lock().unwrap().values().map(|t| t.info.lock().unwrap().clone()).collect();
    v.sort_by_key(|t| t.id);
    v
}

/// Requests cancellation. Returns false for unknown or non-cancellable tasks.
pub fn cancel(id: u64) -> bool {
    let Some(t) = HUB.live.lock().unwrap().get(&id).cloned() else { return false };
    if !t.info.lock().unwrap().cancellable {
        return false;
    }
    t.cancel.store(true, Ordering::Relaxed);
    true
}

pub struct Task {
    inner: Arc<Inner>,
}

impl Task {
    pub fn id(&self) -> u64 {
        self.inner.info.lock().unwrap().id
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancel.load(Ordering::Relaxed)
    }

    pub fn set(&self, done: u64, total: Option<u64>) {
        {
            let mut i = self.inner.info.lock().unwrap();
            i.done = done;
            if total.is_some() {
                i.total = total;
            }
        }
        self.emit(false);
    }

    pub fn add(&self, n: u64) {
        self.inner.info.lock().unwrap().done += n;
        self.emit(false);
    }

    pub fn add_total(&self, n: u64) {
        {
            let mut i = self.inner.info.lock().unwrap();
            i.total = Some(i.total.unwrap_or(0) + n);
        }
        self.emit(false);
    }

    pub fn detail(&self, detail: impl Into<String>) {
        self.inner.info.lock().unwrap().detail = detail.into();
        self.emit(false);
    }

    pub fn fail(&self, error: impl Into<String>) {
        self.end(TaskState::Failed, Some(error.into()));
    }

    pub fn cancelled(&self) {
        self.end(TaskState::Cancelled, None);
    }

    pub fn finish(&self) {
        self.end(TaskState::Done, None);
    }

    fn end(&self, state: TaskState, error: Option<String>) {
        {
            let mut i = self.inner.info.lock().unwrap();
            if i.state != TaskState::Running {
                return;
            }
            i.state = state;
            i.error = error;
            if state == TaskState::Done
                && let Some(t) = i.total
            {
                i.done = i.done.max(t);
            }
        }
        HUB.live.lock().unwrap().remove(&self.id());
        self.emit(true);
    }

    fn emit(&self, force: bool) {
        let now = Instant::now();
        {
            let mut last = self.inner.last_emit.lock().unwrap();
            if !force && last.is_some_and(|l| now.duration_since(l) < emit_interval()) {
                return;
            }
            *last = Some(now);
        }
        let info = {
            let mut i = self.inner.info.lock().unwrap();
            let secs = self.inner.started.elapsed().as_secs_f64();
            i.rate = if secs > 0.05 { i.done as f64 / secs } else { 0.0 };
            i.clone()
        };
        if let Some(app) = HUB.app.get() {
            let _ = app.emit(EVENT, info);
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        let cancelled = self.is_cancelled();
        if cancelled { self.cancelled() } else { self.finish() }
    }
}

/// Reports a read only if it turns out to be slow: nothing is shown for reads that
/// finish within `progress.slow_read_ms`, so fast local files never flash in the UI.
pub struct ReadTracker {
    label: String,
    total: u64,
    done: u64,
    started: Instant,
    threshold: Duration,
    task: Option<Task>,
}

impl ReadTracker {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        let threshold = Duration::from_millis(crate::recent::cached_app_settings().progress.slow_read_ms);
        Self { label: label.into(), total, done: 0, started: Instant::now(), threshold, task: None }
    }

    pub fn advance(&mut self, n: u64) {
        self.done += n;
        if self.task.is_none() && self.started.elapsed() >= self.threshold && self.done < self.total {
            self.task = Some(begin("read", std::mem::take(&mut self.label), Unit::Bytes, Some(self.total)));
        }
        if let Some(t) = &self.task {
            t.set(self.done, None);
        }
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        if let Some(t) = self.task.take() {
            t.fail(error);
        }
    }
}

/// Counts work that arrives in bursts (on-demand thumbnails for what is on screen) as
/// one task that lives while anything is pending.
pub struct Burst {
    kind: &'static str,
    state: Mutex<(Option<Task>, u64)>,
}

impl Burst {
    pub const fn new(kind: &'static str) -> Self {
        Self { kind, state: Mutex::new((None, 0)) }
    }

    pub fn enter(&self) {
        let mut s = self.state.lock().unwrap();
        s.1 += 1;
        match &s.0 {
            Some(t) => t.add_total(1),
            None => s.0 = Some(begin(self.kind, "", Unit::Items, Some(1))),
        }
    }

    pub fn leave(&self) {
        let mut s = self.state.lock().unwrap();
        s.1 = s.1.saturating_sub(1);
        if let Some(t) = &s.0 {
            t.add(1);
        }
        if s.1 == 0 {
            s.0 = None; // drop finishes it
        }
    }
}
