//! The native runtime, run on the host.
//!
//! The runtime under `src/native/runtime/` is freestanding C that `build.rs` compiles for every
//! target and krusty links into a user's program. Compiling it proves only that it compiles; this
//! harness RUNS it. Each driver under `tests/native_runtime/` is a small freestanding program that
//! supplies `kt_program_entry`, calls into the runtime, and prints `OK` when what it checked held.
//! The harness links the driver with every runtime source on the branch — with the host's clang,
//! `-nostdlib -static`, so nothing but the runtime itself answers its symbols — and runs it.
//!
//! A driver reports failure by exiting non-zero with a message on stderr (`KT_SYS_FAIL`), or by
//! crashing; either fails the test with what it printed. A driver whose subject is the runtime's
//! own abort (exhausted memory, say) is run by `run_aborting_driver` instead, which requires that
//! abort and its message.
//!
//! The drivers need a C compiler for the host. CI has one and must run them; a local build without
//! clang is told why they did not run rather than failing on a missing tool.

use super::common;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Whether every function the runtime sources on this branch call is defined by them. The runtime
/// lands in tiers, and a tier below the last one calls functions a later tier defines; those links
/// leave the missing symbols unresolved (a driver that reaches one crashes, it does not pass). The
/// tier that completes the runtime turns this on, and from then on a missing definition fails the
/// link.
const RUNTIME_COMPLETE: bool = false;

/// Warnings a tier below the last one cannot help giving. Such a tier DECLARES the internal
/// functions a later tier defines, and defines helpers only a later tier's code calls; the tier that
/// completes the runtime turns `RUNTIME_COMPLETE` on and with it every one of these back into an
/// error.
const INCOMPLETE_RUNTIME_WARNINGS: &[&str] = &[
    "-Wno-undefined-internal",
    "-Wno-unused-function",
    "-Wno-unused-const-variable",
];

fn runtime_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native/runtime")
}

fn driver_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/native_runtime")
}

/// Whether the host is a target the runtime supports and a clang is there to build for it.
fn host_can_run() -> bool {
    let supported = cfg!(target_os = "linux")
        && cfg!(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ));
    let clang = Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if supported && clang {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI must run the native runtime drivers, but this host cannot (supported target: \
         {supported}, clang: {clang})"
    );
    eprintln!("native runtime drivers skipped: this host has no clang or is not a runtime target");
    false
}

/// Link `driver` with every runtime source; the path of the executable. `None` when the host cannot
/// run drivers at all.
fn build_driver(driver: &str) -> Option<PathBuf> {
    if !host_can_run() {
        return None;
    }
    let mut sources: Vec<PathBuf> = std::fs::read_dir(runtime_dir())
        .expect("read the runtime directory")
        .map(|entry| entry.expect("runtime directory entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    sources.sort();
    let scratch = common::scratch_dir().expect("scratch directory");
    let executable = scratch.join(driver);
    let build = Command::new("clang")
        .args([
            "-std=c11",
            "-ffreestanding",
            "-nostdlib",
            "-static",
            "-fno-pic",
            "-fno-stack-protector",
            "-fno-asynchronous-unwind-tables",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
        ])
        .args((!RUNTIME_COMPLETE).then_some("-Wl,--unresolved-symbols=ignore-all"))
        .args(if RUNTIME_COMPLETE {
            &[][..]
        } else {
            INCOMPLETE_RUNTIME_WARNINGS
        })
        .arg("-I")
        .arg(runtime_dir())
        .args(&sources)
        .arg(driver_dir().join(format!("{driver}.c")))
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run clang");
    assert!(
        build.status.success(),
        "{driver}: the driver and runtime did not build:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    Some(executable)
}

/// Link `driver` with every runtime source and run it; the process must exit 0 and its stdout must
/// end in `OK`. Returns the output for a driver whose test checks more than that. `None` when the
/// host cannot run drivers at all.
fn run_driver(driver: &str) -> Option<Output> {
    let executable = build_driver(driver)?;
    let output = Command::new(&executable).output().expect("run the driver");
    let stdout = &output.stdout;
    assert!(
        output.status.success() && stdout.ends_with(b"OK\n"),
        "{driver}: {}\nstdout (last 200 bytes): {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&stdout[stdout.len().saturating_sub(200)..]),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(output)
}

/// Link `driver` with every runtime source and run it; the RUNTIME must end the process the way
/// `KT_SYS_FAIL` does — status 134 — with `message` on stderr. A driver that comes back from the
/// call it makes reports that on stdout and exits 0, which fails here.
fn run_aborting_driver(driver: &str, message: &str) {
    let Some(executable) = build_driver(driver) else {
        return;
    };
    let output = Command::new(&executable).output().expect("run the driver");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() == Some(134) && stderr.contains(message),
        "{driver}: expected the runtime to abort with {message:?}, got {}\nstdout: {}\nstderr: {stderr}",
        output.status,
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_program_that_returns_ends_with_status_zero() {
    run_driver("program_returns");
}

#[test]
fn a_write_to_a_full_non_blocking_pipe_keeps_writing() {
    let Some(output) = run_driver("write_nonblocking_stdout") else {
        return;
    };
    let payload = &output.stdout[..output.stdout.len() - "OK\n".len()];
    assert_eq!(payload.len(), 1 << 20, "every byte of the payload arrives");
    assert!(
        payload
            .iter()
            .enumerate()
            .all(|(index, &byte)| byte == b'a' + (index % 26) as u8),
        "the payload arrives in order"
    );
}

#[test]
fn an_array_too_large_for_the_allocator_is_out_of_memory() {
    run_aborting_driver("array_new_overflow", "krusty: out of memory");
}

#[test]
fn a_negative_string_index_is_out_of_bounds() {
    run_driver("string_get_negative_index");
}

#[test]
fn a_substring_outside_the_text_is_out_of_bounds() {
    run_driver("string_substring_bounds");
}

#[test]
fn whitespace_is_the_jvm_set() {
    run_driver("string_whitespace");
}

#[test]
fn a_repeat_too_long_for_memory_is_out_of_memory() {
    run_aborting_driver("string_repeat_overflow", "krusty: out of memory");
}

#[test]
fn surrogate_halves_concatenate_into_their_character() {
    run_driver("string_plus_surrogates");
}
