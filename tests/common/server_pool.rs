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

            // A second or later server also needs the memory to hold it right now: a process can
            // keep several pools (one per classpath), and their idle servers stay resident.
            if len < cap && create.is_some() && (len == 0 || host_has_room_for_a_server()) {
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

/// How many persistent compiler-server JVMs to pool per classpath. Scales with the host — a single
/// server serializes every kotlinc-dependency compile AND every java-driver test behind one mutex,
/// which measured as the e2e suite's dominant wall-clock cost (hundreds of ~0.4s warm compiles all
/// queueing on one JVM while the other N-1 cores idle). One server per CPU, as many as the host's
/// memory holds at [`SERVER_FOOTPRINT_MIB`] each, clamped to [1, 6]. `KRUSTY_SERVER_POOL`
/// overrides in either direction (e.g. 1 on a swapping shared box).
pub(crate) fn server_pool_cap() -> usize {
    std::env::var("KRUSTY_SERVER_POOL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or_else(|| {
            let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
            pool_cap(cpus, host_memory_mib())
        })
}

/// The measured peak a pooled server adds to the host, its share of the test process that feeds
/// it included. A cold 2.4.21 JVM box corpus with four servers per pool peaked 5.1 GB above an
/// idle 4-CPU host (four kotlinc servers at 3.2 GB, the box JVM runners at 1.1 GB, the test process
/// at 0.8 GB), about 1.3 GB a server. With two servers on that host its kotlinc compiles waited
/// 978 s for a server and the corpus took 597 s; with four it took 398 s.
const SERVER_FOOTPRINT_MIB: u64 = 1536;

/// The pool size for a host with `cpus` CPUs and `memory_mib` of memory (`None` when the host
/// reports none, which leaves the CPU count alone to decide).
fn pool_cap(cpus: usize, memory_mib: Option<u64>) -> usize {
    let by_memory = memory_mib.map_or(usize::MAX, |mib| {
        usize::try_from(mib / SERVER_FOOTPRINT_MIB).unwrap_or(usize::MAX)
    });
    cpus.min(by_memory).clamp(1, 6)
}

/// `MemTotal` from `/proc/meminfo`, in MiB.
fn host_memory_mib() -> Option<u64> {
    meminfo_mib(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemTotal:")
}

/// Whether the host's available memory holds one more server at its measured peak. A host that
/// reports no figure (not Linux) is decided by the pool cap alone.
fn host_has_room_for_a_server() -> bool {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|meminfo| meminfo_mib(&meminfo, "MemAvailable:"))
        .is_none_or(|available| available >= SERVER_FOOTPRINT_MIB)
}

/// One `/proc/meminfo` field (`MemTotal:`, `MemAvailable:`), in MiB.
fn meminfo_mib(meminfo: &str, field: &str) -> Option<u64> {
    meminfo.lines().find_map(|line| {
        let kib = line.strip_prefix(field)?.trim().strip_suffix("kB")?;
        Some(kib.trim().parse::<u64>().ok()? / 1024)
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use super::{meminfo_mib, pool_cap};

    #[test]
    fn meminfo_fields_are_read_in_mib() {
        let meminfo = "MemTotal:        7969012 kB\nMemFree:          812340 kB\nMemAvailable:    1478656 kB\n";
        assert_eq!(meminfo_mib(meminfo, "MemTotal:"), Some(7782));
        assert_eq!(meminfo_mib(meminfo, "MemAvailable:"), Some(1444));
        assert_eq!(meminfo_mib(meminfo, "SwapTotal:"), None);
    }

    #[test]
    fn a_four_cpu_host_pools_one_server_per_cpu_when_its_memory_holds_them() {
        // The 4-vCPU, 16 GB GitHub-hosted runner and a 7.6 GiB self-hosted runner.
        assert_eq!(pool_cap(4, Some(15_990)), 4);
        assert_eq!(pool_cap(4, Some(7_782)), 4);
    }

    #[test]
    fn memory_caps_the_pool_below_the_cpu_count() {
        assert_eq!(pool_cap(8, Some(7_782)), 5);
        assert_eq!(pool_cap(4, Some(4_096)), 2);
        assert_eq!(pool_cap(4, Some(1_024)), 1);
    }

    #[test]
    fn the_pool_stays_between_one_and_six_servers() {
        assert_eq!(pool_cap(64, Some(262_144)), 6);
        assert_eq!(pool_cap(1, Some(262_144)), 1);
        assert_eq!(pool_cap(16, None), 6);
        assert_eq!(pool_cap(2, None), 2);
    }
}
