//! Claim a persistent JVM without herding every caller onto the first one.
//!
//! Admission and release share one mutex and condition variable. A caller either claims an idle
//! slot while holding that mutex, grows the pool, or waits until a claim is released. The claim
//! guard returns the slot even when the test body unwinds.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

pub(crate) struct Pool<S> {
    shared: Arc<Shared<S>>,
}

struct Shared<S> {
    state: Mutex<PoolState<S>>,
    available: Condvar,
    /// Resident memory one server of this pool holds, and the budget that counts it. A pool with
    /// no budget is unaccounted.
    resident: Option<(Arc<Budget>, u64)>,
}

struct PoolState<S> {
    slots: Vec<SlotState<S>>,
    cursor: usize,
}

struct SlotState<S> {
    server: Arc<Mutex<S>>,
    claimed: bool,
    /// When the server was last released; an idle server is evictable once it has sat this long.
    idle_since: Instant,
}

struct Claim<'a, S> {
    pool: &'a Shared<S>,
    server: Arc<Mutex<S>>,
}

impl<S> Pool<S> {
    pub(crate) fn new() -> Self {
        Self::with_resident(None)
    }

    /// A pool whose servers each hold `mib` of resident memory against `budget`. A pool never
    /// creates a server that would exceed the budget while another pool's idle server could be
    /// stopped instead; see [`Budget`].
    pub(crate) fn resident(budget: &Arc<Budget>, mib: u64) -> Self
    where
        S: Send + 'static,
    {
        let pool = Self::with_resident(Some((Arc::clone(budget), mib)));
        budget.register(Arc::downgrade(&pool.shared) as Weak<dyn Evict>);
        pool
    }

    fn with_resident(resident: Option<(Arc<Budget>, u64)>) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(PoolState {
                    slots: Vec::new(),
                    cursor: 0,
                }),
                available: Condvar::new(),
                resident,
            }),
        }
    }

    pub(crate) fn with_server<R>(
        &self,
        cap: usize,
        create: impl FnOnce() -> Option<S>,
        body: impl FnOnce(&mut S) -> R,
    ) -> Option<R> {
        let claim = self.shared.admit(cap, create)?;
        let mut server = lock(&claim.server);
        Some(body(&mut server))
    }
}

impl<S> Shared<S> {
    fn weight(&self) -> u64 {
        self.resident.as_ref().map_or(0, |(_, mib)| *mib)
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
                // Making room stops other pools' servers, which takes their locks: not while
                // holding this one's.
                let reserved = match &self.resident {
                    Some((budget, mib)) => {
                        drop(state);
                        let reserved = budget.reserve(*mib, len == 0);
                        state = lock(&self.state);
                        reserved
                    }
                    None => true,
                };
                if reserved {
                    if state.slots.len() != len {
                        // Another caller grew the pool or released a server while this one waited.
                        self.give_back_reservation();
                        continue;
                    }
                    let make_server = create.take().expect("server creation attempted once");
                    if let Some(server) = make_server() {
                        let server = Arc::new(Mutex::new(server));
                        state.slots.push(SlotState {
                            server: Arc::clone(&server),
                            claimed: true,
                            idle_since: Instant::now(),
                        });
                        state.cursor = state.slots.len();
                        return Some(Claim { pool: self, server });
                    }
                    self.give_back_reservation();
                    if len == 0 {
                        return None;
                    }
                } else if state.slots.len() != len || state.slots.iter().any(|slot| !slot.claimed) {
                    // A caller grew the pool or released its server while this one waited on the
                    // budget with the lock dropped, and its notification is already gone: take
                    // it, do not wait (or give up on an empty pool that is no longer empty).
                    continue;
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

    fn give_back_reservation(&self) {
        if let Some((budget, mib)) = &self.resident {
            budget.release(*mib);
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
        slot.idle_since = Instant::now();
        self.pool.available.notify_one();
        if let Some((budget, _)) = &self.pool.resident {
            budget.idle_server_appeared();
        }
    }
}

/// A pool whose idle servers another pool may stop to make room.
pub(crate) trait Evict: Send + Sync {
    /// Stops one server that has been idle for at least `idle_for`, returning the resident memory
    /// it held, or `None` when every server is busy or was used more recently.
    fn evict_idle(&self, idle_for: Duration) -> Option<u64>;
}

impl<S: Send + 'static> Evict for Shared<S> {
    fn evict_idle(&self, idle_for: Duration) -> Option<u64> {
        let evicted = {
            let mut state = lock(&self.state);
            let index = state
                .slots
                .iter()
                .position(|slot| !slot.claimed && slot.idle_since.elapsed() >= idle_for)?;
            let slot = state.slots.remove(index);
            state.cursor = 0;
            slot
        };
        // Dropping the slot stops its JVM, which waits for the process: not under the lock.
        drop(evicted);
        Some(self.weight())
    }
}

/// How much resident memory the persistent servers of every pool may hold together.
///
/// Each pool is sized per host on its own, so a cold run that reaches the JVM kotlinc, the Native
/// library oracle and the kotlinc-native oracle at once held six server JVMs beside the test
/// process: about 5 GiB measured, more on a slower host, on a runner with 7.6 GiB. A server counts
/// its pool's weight from creation until it is stopped. When creating one would pass the limit, the
/// pool first stops another pool's server that has been idle for a while, then waits for one to
/// become evictable. The first server is always admitted, and a creation that has waited past
/// [`Budget::patience`] proceeds over the limit rather than deadlock a test.
pub(crate) struct Budget {
    limit_mib: Option<u64>,
    idle_before_eviction: Duration,
    patience: Duration,
    inner: Mutex<BudgetState>,
    changed: Condvar,
}

struct BudgetState {
    used_mib: u64,
    pools: Vec<Weak<dyn Evict>>,
}

impl Budget {
    pub(crate) fn new(
        limit_mib: Option<u64>,
        idle_before_eviction: Duration,
        patience: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            limit_mib,
            idle_before_eviction,
            patience,
            inner: Mutex::new(BudgetState {
                used_mib: 0,
                pools: Vec::new(),
            }),
            changed: Condvar::new(),
        })
    }

    /// The process-wide budget: `KRUSTY_SERVER_MEMORY_MIB` when set (`0` for none), otherwise what
    /// is left of the host's memory after the test process and the machine's own needs.
    pub(crate) fn process() -> &'static Arc<Budget> {
        static PROCESS: OnceLock<Arc<Budget>> = OnceLock::new();
        PROCESS.get_or_init(|| {
            let limit = match std::env::var("KRUSTY_SERVER_MEMORY_MIB")
                .ok()
                .and_then(|value| value.trim().parse::<u64>().ok())
            {
                Some(0) => None,
                Some(mib) => Some(mib),
                None => host_memory_mib().map(default_limit_mib),
            };
            Budget::new(limit, Duration::from_secs(5), Duration::from_secs(60))
        })
    }

    fn register(&self, pool: Weak<dyn Evict>) {
        lock(&self.inner).pools.push(pool);
    }

    fn release(&self, mib: u64) {
        let mut inner = lock(&self.inner);
        inner.used_mib = inner.used_mib.saturating_sub(mib);
        self.changed.notify_all();
    }

    fn idle_server_appeared(&self) {
        self.changed.notify_all();
    }

    /// Counts `mib` for a server about to be created, stopping other pools' idle servers to make
    /// room. `false` when `may_wait` is unset and there is no room; a pool with no server of its
    /// own waits, because it cannot make progress without one.
    fn reserve(&self, mib: u64, may_wait: bool) -> bool {
        let Some(limit) = self.limit_mib else {
            return true;
        };
        let deadline = Instant::now() + self.patience;
        loop {
            let pools = {
                let mut inner = lock(&self.inner);
                if inner.used_mib == 0 || inner.used_mib + mib <= limit {
                    inner.used_mib += mib;
                    return true;
                }
                inner.pools.retain(|pool| pool.strong_count() > 0);
                inner.pools.clone()
            };
            let freed = pools
                .iter()
                .filter_map(Weak::upgrade)
                .find_map(|pool| pool.evict_idle(self.idle_before_eviction));
            if let Some(freed) = freed {
                self.release(freed);
                continue;
            }
            if !may_wait {
                return false;
            }
            let mut inner = lock(&self.inner);
            if Instant::now() >= deadline {
                inner.used_mib += mib;
                return true;
            }
            let (guard, _) = self
                .changed
                .wait_timeout(inner, Duration::from_millis(500))
                .unwrap_or_else(|err| err.into_inner());
            drop(guard);
        }
    }
}

/// What the servers may hold on a host with `total_mib`: all but the test process and the
/// machine's own needs, which are the larger of 3.5 GiB and two fifths of the host. A 7.6 GiB CI
/// runner leaves about 4 GiB, a 16 GiB workstation about 9.5 GiB.
pub(crate) fn default_limit_mib(total_mib: u64) -> u64 {
    total_mib.saturating_sub((total_mib * 2 / 5).max(3584))
}

/// The memory the test process may use: the host's `MemTotal`, or the container's limit when that
/// is lower (cgroup v2 `memory.max`, cgroup v1 `memory.limit_in_bytes`), so a 4 GiB container on a
/// larger host is budgeted as a 4 GiB machine.
fn host_memory_mib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let limit = [
        "/sys/fs/cgroup/memory.max",
        "/sys/fs/cgroup/memory/memory.limit_in_bytes",
    ]
    .iter()
    .find_map(|path| cgroup_limit_bytes(&std::fs::read_to_string(path).ok()?));
    Some(effective_memory_mib(kib / 1024, limit))
}

/// A cgroup limit file's value in bytes; `None` for `max` (no limit) or text that is not a number.
pub(crate) fn cgroup_limit_bytes(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// The lower of the host's memory and the container's limit. A cgroup v1 file that reports no limit
/// holds a value near `u64::MAX`, which is never lower than the host's memory.
pub(crate) fn effective_memory_mib(host_mib: u64, cgroup_limit_bytes: Option<u64>) -> u64 {
    cgroup_limit_bytes.map_or(host_mib, |bytes| host_mib.min(bytes / (1024 * 1024)))
}

/// How many persistent JVMs a pool that is not counted against the resident-server budget may hold
/// (the JavaRunner and ASM text pools, each at `-Xmx512m`): half the CPUs, clamped to [1, 6].
/// `KRUSTY_SERVER_POOL` overrides in either direction (e.g. 1 on a swapping shared box).
pub(crate) fn server_pool_cap() -> usize {
    pool_cap_override().unwrap_or_else(|| (cpus() / 2).clamp(1, 6))
}

/// How many persistent kotlinc-server JVMs one pool may hold: one per CPU, clamped to [1, 6]. A
/// single server serializes every kotlinc-dependency compile behind one mutex, and a cold corpus
/// that waits for two servers on a 4-CPU host leaves half its CPUs idle. Memory is not decided
/// here: the resident-server budget shared by the kotlinc and kotlinc-native pools stops a server
/// when another of them needs the room, so a pool that cannot grow waits for its own server.
pub(crate) fn kotlinc_pool_cap() -> usize {
    pool_cap_override().unwrap_or_else(|| pool_cap(cpus()))
}

pub(crate) fn pool_cap(cpus: usize) -> usize {
    cpus.clamp(1, 6)
}

fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn pool_cap_override() -> Option<usize> {
    std::env::var("KRUSTY_SERVER_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}
