//! Shared storage and toolchain discovery for recorded reference-compiler observations.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

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
        // A live kotlinc-native reference is a whole JVM plus the native linker. Test threads
        // outnumber the cores they may use, so without a bound a cold cache starts one per thread
        // and the runner runs out of memory. Only Native is charged: the JVM, JS and Wasm
        // references are far lighter and must not queue behind native compiles.
        let _permit = (target == CompilationTarget::Native).then(|| {
            LIVE_REFERENCES.acquire(
                live_reference_cap(
                    super::common::server_pool::server_pool_cap(),
                    memory_total_kib(),
                ),
                &slot,
                permit_wait_deadline(),
            )
        });
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

/// Gate over the live Native reference compiles in flight. It remembers who holds a permit, so a
/// waiter that gives up can say which references it was queued behind.
struct LiveReferences {
    holders: Mutex<Vec<String>>,
    freed: Condvar,
}

struct Permit(&'static LiveReferences, String);

static LIVE_REFERENCES: LiveReferences = LiveReferences {
    holders: Mutex::new(Vec::new()),
    freed: Condvar::new(),
};

fn held(mutex: &Mutex<Vec<String>>) -> MutexGuard<'_, Vec<String>> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How long a test waits for a permit before it fails naming the holders: `KRUSTY_LIVE_REFERENCE_WAIT_MS`,
/// 20 minutes by default.
fn permit_wait_deadline() -> Duration {
    Duration::from_millis(super::common::timeout_ms_from(
        "KRUSTY_LIVE_REFERENCE_WAIT_MS",
        std::env::var("KRUSTY_LIVE_REFERENCE_WAIT_MS")
            .ok()
            .as_deref(),
        20 * 60 * 1000,
    ))
}

impl LiveReferences {
    fn acquire(&'static self, cap: usize, label: &str, wait: Duration) -> Permit {
        let deadline = Instant::now() + wait;
        let mut holders = held(&self.holders);
        while holders.len() >= cap {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "live reference {label} waited {wait:?} for a permit held by {holders:?}"
            );
            holders = self
                .freed
                .wait_timeout(holders, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        holders.push(label.to_owned());
        Permit(self, label.to_owned())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut holders = held(&self.0.holders);
        if let Some(at) = holders.iter().position(|holder| *holder == self.1) {
            holders.swap_remove(at);
        }
        drop(holders);
        self.0.freed.notify_one();
    }
}

/// How long a recorded program may run: `KRUSTY_REFERENCE_PROGRAM_TIMEOUT_MS`, five minutes by
/// default. A program that outlives it holds a live reference permit, so it is killed and fails.
fn program_deadline() -> Duration {
    Duration::from_millis(super::common::timeout_ms_from(
        "KRUSTY_REFERENCE_PROGRAM_TIMEOUT_MS",
        std::env::var("KRUSTY_REFERENCE_PROGRAM_TIMEOUT_MS")
            .ok()
            .as_deref(),
        300_000,
    ))
}

/// Run a kotlinc-native executable to completion within the program deadline.
pub(crate) fn run_program_bounded(executable: &Path) -> Output {
    run_bounded(Command::new(executable), program_deadline())
}

fn run_bounded(mut command: Command, deadline: Duration) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the reference program");
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for the reference program") {
            break status;
        }
        if started.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the reference program did not finish within {deadline:?} and was killed");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: stdout.join().expect("stdout reader"),
        stderr: stderr.join().expect("stderr reader"),
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
            holders: Mutex::new(Vec::new()),
            freed: Condvar::new(),
        };
        static PEAK: AtomicUsize = AtomicUsize::new(0);
        let workers: Vec<_> = (0..8)
            .map(|n| {
                std::thread::spawn(move || {
                    let _permit = GATE.acquire(2, &format!("w{n}"), Duration::from_secs(60));
                    PEAK.fetch_max(held(&GATE.holders).len(), Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("worker");
        }
        assert!(PEAK.load(Ordering::SeqCst) <= 2);
        assert!(held(&GATE.holders).is_empty());
    }

    #[test]
    fn a_waiter_that_times_out_names_the_holder() {
        static GATE: LiveReferences = LiveReferences {
            holders: Mutex::new(Vec::new()),
            freed: Condvar::new(),
        };
        let _holder = GATE.acquire(1, "slot-a", Duration::from_secs(1));
        let message = std::panic::catch_unwind(|| {
            GATE.acquire(1, "slot-b", Duration::from_millis(50));
        })
        .expect_err("the second reference cannot get a permit");
        let message = message.downcast_ref::<String>().expect("assert message");
        assert_eq!(
            message,
            "live reference slot-b waited 50ms for a permit held by [\"slot-a\"]"
        );
    }

    #[test]
    fn a_program_that_outlives_its_deadline_is_killed() {
        let mut command = Command::new("sleep");
        command.arg("30");
        let message = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_bounded(command, Duration::from_millis(100))
        }))
        .expect_err("the sleeping program is killed");
        assert_eq!(
            message.downcast_ref::<String>().expect("panic message"),
            "the reference program did not finish within 100ms and was killed"
        );
    }

    #[test]
    fn a_finished_program_returns_its_output() {
        let mut command = Command::new("echo");
        command.arg("done");
        let run = run_bounded(command, Duration::from_secs(30));
        assert!(run.status.success());
        assert_eq!(run.stdout, b"done\n");
    }

    #[test]
    fn the_wait_and_program_deadlines_default_to_twenty_and_five_minutes() {
        assert_eq!(permit_wait_deadline(), Duration::from_secs(20 * 60));
        assert_eq!(program_deadline(), Duration::from_secs(5 * 60));
    }
}
