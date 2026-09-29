//! Claim a persistent JVM without herding every caller onto the first one.
//!
//! An idle check that locks and immediately drops the guard reports every server as free. The
//! callers then all block on the first mutex, so a pool of size N still runs one JVM. The claim
//! flag is flipped while the pool lock is held; the server mutex is taken only after that lock is
//! released.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

pub(crate) struct Pool<S> {
    slots: Mutex<Vec<Arc<Slot<S>>>>,
    cursor: AtomicUsize,
}

struct Slot<S> {
    server: Mutex<S>,
    claimed: AtomicBool,
}

enum Admit {
    /// This caller set `claimed` and clears it after releasing the server mutex.
    Claimed,
    /// Every server was already claimed. This caller waits on one mutex and must not clear the flag.
    Queued,
}

impl<S> Pool<S> {
    pub(crate) fn new() -> Self {
        Self {
            slots: Mutex::new(Vec::new()),
            cursor: AtomicUsize::new(0),
        }
    }

    pub(crate) fn with_server<R>(
        &self,
        cap: usize,
        create: impl FnOnce() -> Option<S>,
        body: impl FnOnce(&mut S) -> R,
    ) -> Option<R> {
        let (slot, admit) = self.admit(cap, create)?;
        // Clear a claim if the caller panics. A stuck claim would shrink the pool for every later
        // test in the process.
        let _release = ReleaseClaim {
            claimed: matches!(admit, Admit::Claimed).then_some(&slot.claimed),
        };
        let mut guard = lock_server(&slot.server);
        let result = body(&mut guard);
        drop(guard);
        Some(result)
    }

    fn admit(
        &self,
        cap: usize,
        create: impl FnOnce() -> Option<S>,
    ) -> Option<(Arc<Slot<S>>, Admit)> {
        let mut slots = self.slots.lock().unwrap_or_else(|err| err.into_inner());
        let len = slots.len();
        let start = self.cursor.fetch_add(1, Ordering::Relaxed);
        for offset in 0..len {
            let idx = (start + offset) % len;
            if slots[idx]
                .claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.cursor.store(idx + 1, Ordering::Relaxed);
                return Some((Arc::clone(&slots[idx]), Admit::Claimed));
            }
        }
        if len < cap {
            let server = create()?;
            let slot = Arc::new(Slot {
                server: Mutex::new(server),
                claimed: AtomicBool::new(true),
            });
            slots.push(Arc::clone(&slot));
            self.cursor.store(slots.len(), Ordering::Relaxed);
            return Some((slot, Admit::Claimed));
        }
        if len == 0 {
            return None;
        }
        let idx = start % len;
        Some((Arc::clone(&slots[idx]), Admit::Queued))
    }
}

fn lock_server<S>(server: &Mutex<S>) -> MutexGuard<'_, S> {
    server.lock().unwrap_or_else(|err| err.into_inner())
}

struct ReleaseClaim<'a> {
    claimed: Option<&'a AtomicBool>,
}

impl Drop for ReleaseClaim<'_> {
    fn drop(&mut self) {
        if let Some(claimed) = self.claimed {
            claimed.store(false, Ordering::Release);
        }
    }
}
