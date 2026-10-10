//! The references krusty-toolchain is compared with, their output cached the way krusty caches
//! kotlinc's: JetBrains' `kotlin` ([`kotlin`]) and the JDK's `glob:` path matcher ([`jdk_globs`]).
//!
//! `kotlin` runs through the toolchain's own wrapper, `scripts/kotlin-toolchain/kotlin`, which pins
//! the exact version and checksum of the distribution it runs. The JDK matcher runs through
//! `scripts/kotlin-toolchain/GlobOracle.java` on the JDK `JAVA_HOME` names. Each invocation's exit
//! code and raw output bytes are cached outside the repository (`KRUSTY_TOOLCHAIN_ORACLE_DIR`, or
//! `target/cache/kotlin-toolchain-oracle`), under the reference's identity (the wrapper's pinned
//! version, or the JDK's) and a fingerprint of the reference's own bytes (the wrapper, or
//! `GlobOracle.java` and the JDK's `release` record) and every input.
//!
//! stdout and stderr are kept as written. Nothing is removed from them but the line a JVM prints on
//! stderr for `JAVA_TOOL_OPTIONS`, which is the environment's, not the reference's.
//!
//! A cached entry is replayed. A miss fails, naming the case, unless the reference may run:
//! `KRUSTY_TOOLCHAIN_RUN_MISSING=1` (CI) runs it for a missing entry, and `KRUSTY_RECORD=1` runs it
//! for every entry; either stores what it ran. CI restores the cache master saved, runs only what
//! is missing, and master saves the result. Each live run prints one line to stderr.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

/// What a reference did.
pub struct Output {
    /// The directory it ran in, as it appears in the output.
    pub root: String,
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// The entry format, part of every entry's directory, so an entry of another format is a miss.
const FORMAT: &str = "3";
const MAGIC: &[u8] = b"krusty-toolchain oracle 3\n";
/// What a JVM prints on its own when `JAVA_TOOL_OPTIONS` is set.
const JAVA_TOOL_OPTIONS_LINE: &[u8] = b"Picked up JAVA_TOOL_OPTIONS: ";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn script(name: &str) -> PathBuf {
    workspace().join("scripts/kotlin-toolchain").join(name)
}

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| value == "1")
}

/// FNV-1a over length-prefixed parts, 128 bits.
pub struct Fingerprint(u128);

impl Fingerprint {
    fn part(&mut self, bytes: &[u8]) {
        for byte in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            self.0 ^= u128::from(*byte);
            self.0 = self
                .0
                .wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013B);
        }
    }
}

impl Fingerprint {
    pub fn of(parts: &[&[u8]]) -> u128 {
        let mut fingerprint = Fingerprint(0x6c62_272e_07bb_0142_62b8_2175_6295_c58d);
        for part in parts {
            fingerprint.part(part);
        }
        fingerprint.0
    }
}

/// One reference: its slot in the cache, a fingerprint of its identity and inputs, and how to run
/// it.
pub struct Reference<'a> {
    /// `<reference>-<version>`.
    pub slot: String,
    pub fingerprint: u128,
    /// Named in a live run's report and a miss's failure.
    pub case: &'a str,
}

impl Reference<'_> {
    /// The cached output, or `live`'s when the reference may run.
    pub fn output(&self, live: impl FnOnce() -> Output) -> Output {
        let root = std::env::var_os("KRUSTY_TOOLCHAIN_ORACLE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| workspace().join("target/cache/kotlin-toolchain-oracle"));
        let path = root
            .join(format!("{}.v{FORMAT}", self.slot))
            .join(format!("{:032x}", self.fingerprint));
        let record = flag("KRUSTY_RECORD");
        if !record {
            if let Ok(bytes) = std::fs::read(&path) {
                return decode(&bytes)
                    .unwrap_or_else(|| panic!("{}: not a cached oracle entry", path.display()));
            }
            assert!(
                flag("KRUSTY_TOOLCHAIN_RUN_MISSING"),
                "{}: no cached {} output (fingerprint {:032x}); run with KRUSTY_RECORD=1 to \
                 record it from the reference",
                self.case,
                self.slot,
                self.fingerprint
            );
        }
        let reason = if record { "record" } else { "cache-miss" };
        eprintln!(
            "kotlin-toolchain oracle: live {} {reason} case={} fingerprint={:032x}",
            self.slot, self.case, self.fingerprint
        );
        let output = live();
        store(&path, &output);
        output
    }
}

/// A fresh directory for one live run.
pub fn scratch() -> PathBuf {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "krusty-toolchain-oracle-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    directory.canonicalize().unwrap()
}

/// `command`'s exit code, stdout and stderr.
pub fn run_captured(mut command: Command, directory: &Path) -> Output {
    let ran = command
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("run {command:?}: {error}"));
    Output {
        root: directory.display().to_string(),
        code: ran.status.code().expect("the reference exits with a code"),
        stdout: ran.stdout,
        stderr: without_java_tool_options(&ran.stderr),
    }
}

fn without_java_tool_options(output: &[u8]) -> Vec<u8> {
    output
        .split_inclusive(|&byte| byte == b'\n')
        .filter(|line| !line.starts_with(JAVA_TOOL_OPTIONS_LINE))
        .flatten()
        .copied()
        .collect()
}

fn store(path: &Path, output: &Output) {
    let mut bytes = MAGIC.to_vec();
    for part in [output.root.as_bytes(), &output.stdout, &output.stderr] {
        bytes.extend_from_slice(format!("{}\n", part.len()).as_bytes());
        bytes.extend_from_slice(part);
    }
    bytes.extend_from_slice(format!("{}\n", output.code).as_bytes());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes).unwrap();
    std::fs::rename(&temporary, path).unwrap();
}

fn decode(bytes: &[u8]) -> Option<Output> {
    let mut rest = bytes.strip_prefix(MAGIC)?;
    let number = |rest: &mut &[u8]| -> Option<i64> {
        let end = rest.iter().position(|&byte| byte == b'\n')?;
        let value = std::str::from_utf8(&rest[..end]).ok()?.parse().ok()?;
        *rest = &rest[end + 1..];
        Some(value)
    };
    let mut parts = Vec::new();
    for _ in 0..3 {
        let length = usize::try_from(number(&mut rest)?).ok()?;
        parts.push(rest.get(..length)?.to_vec());
        rest = &rest[length..];
    }
    let code = i32::try_from(number(&mut rest)?).ok()?;
    if !rest.is_empty() {
        return None;
    }
    let stderr = parts.pop()?;
    let stdout = parts.pop()?;
    let root = String::from_utf8(parts.pop()?).ok()?;
    Some(Output {
        root,
        code,
        stdout,
        stderr,
    })
}
