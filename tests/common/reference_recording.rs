//! Shared storage and toolchain discovery for recorded reference-compiler observations.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};

use krusty::compilation_target::CompilationTarget;

use super::common::byte_dump::{self, fingerprint_parts};

/// Read the recording for this source set, or produce and record it with `live`.
pub(crate) fn recorded(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
    live: impl FnOnce(&Path) -> Option<BTreeMap<String, Vec<u8>>>,
) -> Option<BTreeMap<String, Vec<u8>>> {
    let fingerprint = fingerprint(target, kind, sources, args);
    let slot = format!("reference-{kind}-{target:?}-{fingerprint:032x}");
    if let Some(files) = byte_dump::load_shared_files(&slot, fingerprint) {
        return Some(files);
    }
    let work = super::common::scratch_dir().expect("cannot allocate a reference-compiler fixture");
    let answer = {
        // A live reference compile is a whole JVM or native toolchain run. Test threads outnumber
        // the cores they may use, so without a bound a cold cache starts one per thread and the
        // runner runs out of memory.
        let _permit = LIVE_REFERENCES.acquire(live_reference_cap(
            super::common::server_pool::server_pool_cap(),
            memory_total_kib(),
        ));
        live(&work)
    };
    let _ = std::fs::remove_dir_all(&work);
    let answer = answer?;
    byte_dump::store_shared_files(&slot, fingerprint, &answer);
    Some(answer)
}

fn fingerprint(
    target: CompilationTarget,
    kind: &str,
    sources: &[(&str, &str)],
    args: &[String],
) -> u128 {
    let mut blob = Vec::new();
    let mut field = |bytes: &[u8]| {
        blob.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        blob.extend_from_slice(bytes);
    };
    field(format!("{target:?}").as_bytes());
    field(kind.as_bytes());
    for (name, text) in sources {
        field(name.as_bytes());
        field(text.as_bytes());
    }
    for argument in args {
        field(argument.as_bytes());
    }
    fingerprint_parts(&[&blob])
}

/// Counting gate over the live reference compiles in flight.
struct LiveReferences {
    running: Mutex<usize>,
    freed: Condvar,
}

struct Permit(&'static LiveReferences);

static LIVE_REFERENCES: LiveReferences = LiveReferences {
    running: Mutex::new(0),
    freed: Condvar::new(),
};

fn counted(mutex: &Mutex<usize>) -> MutexGuard<'_, usize> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LiveReferences {
    fn acquire(&'static self, cap: usize) -> Permit {
        let mut running = counted(&self.running);
        while *running >= cap {
            running = self
                .freed
                .wait(running)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *running += 1;
        Permit(self)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        *counted(&self.0.running) -= 1;
        self.0.freed.notify_one();
    }
}

/// Memory one live reference compile may take: kotlinc-native starts a JVM whose default heap
/// reaches 3 GiB and then runs the native linker.
const LIVE_REFERENCE_KIB: u64 = 4 * 1024 * 1024;

/// Total memory of this host in KiB, where the platform reports it.
fn memory_total_kib() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// How many live reference compiles may run at once: the compiler pool's width, or fewer where
/// the host's memory cannot hold that many. A 7.6 GiB hosted runner killed the lane with exit 137
/// when four of them started together.
fn live_reference_cap(pool_cap: usize, memory_kib: Option<u64>) -> usize {
    let by_memory = memory_kib.map_or(usize::MAX, |kib| {
        usize::try_from(kib / LIVE_REFERENCE_KIB).unwrap_or(usize::MAX)
    });
    pool_cap.min(by_memory).max(1)
}

/// The Kotlin/Native distribution root, when provisioned.
pub(crate) fn kotlin_native_root() -> Option<PathBuf> {
    let root = std::env::var_os("KRUSTY_KOTLIN_NATIVE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    if root.is_none() && std::env::var_os("KRUSTY_REQUIRE_KOTLIN_NATIVE").is_some() {
        panic!("KRUSTY_REQUIRE_KOTLIN_NATIVE is set but KRUSTY_KOTLIN_NATIVE is not");
    }
    root
}

/// How the kotlinc-native oracles build a program: a debug binary against the distribution's
/// prebuilt static caches, the configuration Kotlin's own Native box tests and Gradle's debug
/// binaries use. Without the caches every program recompiles the reachable stdlib and the runtime
/// bitcode through LLVM's optimization pipeline, about three quarters of each case's oracle time.
pub(crate) fn kotlin_native_program_arguments(root: &Path) -> Vec<OsString> {
    let caches = krusty::toolchain::kotlin_native_debug_static_caches(root)
        .expect("the kotlinc-native oracle runs on a Kotlin/Native host");
    assert!(
        caches.is_dir(),
        "the Kotlin/Native distribution has no debug static caches at {}",
        caches.display()
    );
    let mut directory = OsString::from("-Xcache-directory=");
    directory.push(caches);
    vec![OsString::from("-g"), directory]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_small_host_runs_one_live_reference_at_a_time() {
        const GIB: u64 = 1024 * 1024;
        assert_eq!(live_reference_cap(2, Some(7 * GIB + GIB * 6 / 10)), 1);
        assert_eq!(live_reference_cap(2, Some(15 * GIB + GIB * 6 / 10)), 2);
        assert_eq!(live_reference_cap(6, Some(15 * GIB + GIB * 6 / 10)), 3);
        assert_eq!(live_reference_cap(2, Some(1)), 1);
        assert_eq!(live_reference_cap(3, None), 3);
    }

    #[test]
    fn live_references_never_exceed_the_cap() {
        static GATE: LiveReferences = LiveReferences {
            running: Mutex::new(0),
            freed: Condvar::new(),
        };
        static PEAK: AtomicUsize = AtomicUsize::new(0);
        let workers: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    let _permit = GATE.acquire(2);
                    PEAK.fetch_max(*counted(&GATE.running), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(50));
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("worker");
        }
        assert_eq!(PEAK.load(Ordering::SeqCst), 2);
        assert_eq!(*counted(&GATE.running), 0);
    }
}
