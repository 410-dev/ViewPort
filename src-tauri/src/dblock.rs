//! The bundle database behind a lock that notes where each hold began, so holds long
//! enough to stall the viewer show up in the app log with their place in the code
//! (`log.slow_db_ms`). It derefs to the plain `Mutex<Db>` for crates that take one.

use std::ops::{Deref, DerefMut};
use std::panic::Location;
use std::sync::{LockResult, Mutex, MutexGuard, PoisonError, TryLockError, TryLockResult};
use std::time::Instant;

use vp_db::Db;

pub struct DbLock(Mutex<Db>);

pub struct DbGuard<'a> {
    guard: MutexGuard<'a, Db>,
    at: &'static Location<'static>,
    since: Instant,
}

impl DbLock {
    pub fn new(db: Db) -> Self {
        Self(Mutex::new(db))
    }

    #[track_caller]
    pub fn lock(&self) -> LockResult<DbGuard<'_>> {
        let at = Location::caller();
        let wrap = |guard| DbGuard { guard, at, since: Instant::now() };
        self.0.lock().map(wrap).map_err(|e| PoisonError::new(wrap(e.into_inner())))
    }

    #[track_caller]
    pub fn try_lock(&self) -> TryLockResult<DbGuard<'_>> {
        let at = Location::caller();
        let wrap = |guard| DbGuard { guard, at, since: Instant::now() };
        match self.0.try_lock() {
            Ok(g) => Ok(wrap(g)),
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
            Err(TryLockError::Poisoned(e)) => Err(TryLockError::Poisoned(PoisonError::new(wrap(e.into_inner())))),
        }
    }
}

impl Deref for DbLock {
    type Target = Mutex<Db>;
    fn deref(&self) -> &Mutex<Db> {
        &self.0
    }
}

impl Deref for DbGuard<'_> {
    type Target = Db;
    fn deref(&self) -> &Db {
        &self.guard
    }
}

impl DerefMut for DbGuard<'_> {
    fn deref_mut(&mut self) -> &mut Db {
        &mut self.guard
    }
}

impl Drop for DbGuard<'_> {
    fn drop(&mut self) {
        let held = self.since.elapsed();
        let limit = crate::recent::cached_app_settings().log.slow_db_ms;
        if limit > 0 && held.as_millis() >= limit as u128 {
            crate::applog::info(
                "db",
                format!("held the database {}ms at {}:{} (thread {})", held.as_millis(), self.at.file(), self.at.line(),
                    std::thread::current().name().unwrap_or("-")),
            );
        }
    }
}
