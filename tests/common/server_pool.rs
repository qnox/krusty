//! Claim a persistent JVM without herding every caller onto the first one.
//!
//! Admission and release share one mutex and condition variable. A caller either claims an idle
//! slot while holding that mutex, grows the pool, or waits until a claim is released. The claim
//! guard returns the slot even when the test body unwinds.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub(crate) struct Pool<S> {
    state: Mutex<PoolState<S>>,
    available: Condvar,
}

struct PoolState<S> {
    slots: Vec<SlotState<S>>,
    cursor: usize,
}

struct SlotState<S> {
    server: Arc<Mutex<S>>,
    claimed: bool,
}

struct Claim<'a, S> {
    pool: &'a Pool<S>,
    server: Arc<Mutex<S>>,
}

impl<S> Pool<S> {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PoolState {
                slots: Vec::new(),
                cursor: 0,
            }),
            available: Condvar::new(),
        }
    }

    pub(crate) fn with_server<R>(
        &self,
        cap: usize,
        create: impl FnOnce() -> Option<S>,
        body: impl FnOnce(&mut S) -> R,
    ) -> Option<R> {
        let claim = self.admit(cap, create)?;
        let mut server = lock(&claim.server);
        Some(body(&mut server))
    }

    fn admit(&self, cap: usize, create: impl FnOnce() -> Option<S>) -> Option<Claim<'_, S>> {
        let mut create = Some(create);
        let mut state = lock(&self.state);
        loop {
            let len = state.slots.len();
            let start = state.cursor;
            for offset in 0..len {
                let index = (start + offset) % len;
                if !state.slots[index].claimed {
                    state.slots[index].claimed = true;
                    state.cursor = index + 1;
                    return Some(Claim {
                        pool: self,
                        server: Arc::clone(&state.slots[index].server),
                    });
                }
            }

            if len < cap && create.is_some() {
                let make_server = create.take().expect("server creation attempted once");
                if let Some(server) = make_server() {
                    let server = Arc::new(Mutex::new(server));
                    state.slots.push(SlotState {
                        server: Arc::clone(&server),
                        claimed: true,
                    });
                    state.cursor = state.slots.len();
                    return Some(Claim { pool: self, server });
                }
                if len == 0 {
                    return None;
                }
            }

            if len == 0 {
                return None;
            }
            state = self
                .available
                .wait(state)
                .unwrap_or_else(|err| err.into_inner());
        }
    }
}

impl<S> Drop for Claim<'_, S> {
    fn drop(&mut self) {
        let mut state = lock(&self.pool.state);
        let slot = state
            .slots
            .iter_mut()
            .find(|slot| Arc::ptr_eq(&slot.server, &self.server))
            .expect("claimed server belongs to its pool");
        assert!(slot.claimed, "server claim released twice");
        slot.claimed = false;
        self.pool.available.notify_one();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
