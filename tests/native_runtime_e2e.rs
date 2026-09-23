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
//! crashing; either fails the test with what it printed.
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

/// Link `driver` with every runtime source, returning the executable. `None` when the host cannot
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

/// Link and run a driver whose runtime call must END the program: it must exit non-zero, never
/// reach its `OK`, and say `message` on stderr. A crash says nothing, so it fails this too.
fn run_driver_expecting_failure(driver: &str, message: &str) {
    let Some(executable) = build_driver(driver) else {
        return;
    };
    let output = Command::new(&executable).output().expect("run the driver");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && !output.stdout.ends_with(b"OK\n") && stderr.contains(message),
        "{driver}: expected a failure saying {message:?}, got {}\nstderr: {stderr}",
        output.status
    );
}

#[test]
fn mapping_the_full_long_range_is_too_long_to_collect() {
    run_driver_expecting_failure("range_map_full_span", "krusty: a range too long to collect");
}

#[test]
fn a_list_write_out_of_bounds_raises_and_leaves_the_list_alone() {
    run_driver("mutable_list_bounds");
}

#[test]
fn a_list_read_out_of_bounds_raises_and_answers_nothing() {
    run_driver("list_read_bounds");
}

#[test]
fn a_list_added_to_itself_doubles() {
    run_driver("list_add_all_self");
}

#[test]
fn a_list_modified_during_for_each_ends_the_walk() {
    run_driver("walk_modified_during_for_each");
}

#[test]
fn a_throwing_lambda_ends_the_walk_that_called_it() {
    run_driver("walk_stops_on_throw");
}

#[test]
fn an_exhausted_array_or_string_iterator_raises_no_such_element() {
    run_driver("iterator_exhausted");
}

#[test]
fn an_indexed_value_hash_code_wraps() {
    run_driver("indexed_value_hash_overflow");
}

#[test]
fn an_array_list_of_negative_capacity_raises() {
    run_driver("array_list_negative_capacity");
}

#[test]
fn walking_a_string_is_linear_and_yields_its_utf16_units() {
    run_driver("string_iterator_linear");
}
