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
//! crashing; either fails the test with what it printed. A driver that checks the runtime ENDS the
//! program, as it does on exhausted memory, is instead expected to exit with the runtime's failure
//! status and exactly the runtime's message; anything the driver prints itself fails the test.
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

/// Link `driver` with every runtime source and run it. `None` when the host cannot run drivers at
/// all.
fn build_and_run(driver: &str) -> Option<Output> {
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
            // The runtime's descriptor tables name their leading fields and leave the rest zero, as
            // C initializers are meant to; every other extra warning stays an error.
            "-Wno-missing-field-initializers",
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
    Some(Command::new(&executable).output().expect("run the driver"))
}

/// Run `driver`; the process must exit 0 and its stdout must end in `OK`. Returns the output for a
/// driver whose test checks more than that. `None` when the host cannot run drivers at all.
fn run_driver(driver: &str) -> Option<Output> {
    let output = build_and_run(driver)?;
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

/// Run `driver`, which must end the way the runtime ends a program it cannot continue
/// (`kt_sys_fail`: status 134) with exactly `message` on stderr and nothing on stdout.
fn run_driver_expecting_failure(driver: &str, message: &str) {
    let Some(output) = build_and_run(driver) else {
        return;
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() == Some(134) && stderr == message && output.stdout.is_empty(),
        "{driver}: expected status 134 and stderr {message:?}, got {}\nstdout: {:?}\n\
         stderr: {stderr}",
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
fn the_collector_frees_what_is_unreachable_and_reuses_it() {
    run_driver("gc_collects_unreachable");
}

#[test]
fn an_allocation_whose_size_would_wrap_runs_out_of_memory() {
    run_driver_expecting_failure("gc_rejects_wrapping_size", "krusty: out of memory\n");
}

#[test]
fn every_registered_global_root_is_kept_past_four_thousand() {
    run_driver("gc_many_global_roots");
}

#[test]
fn a_pointer_one_past_an_object_keeps_it_alive() {
    run_driver("gc_end_pointer_keeps_object");
}

#[test]
fn a_collection_started_during_a_collection_fails() {
    run_driver_expecting_failure(
        "gc_reentrant_collection_fails",
        "krusty: a collection started during a collection\n",
    );
}

#[test]
fn a_double_or_float_renders_as_the_jvm_renders_it() {
    run_driver("fp_render_known_answers");
}

#[test]
fn a_floating_remainder_is_exact_and_a_nan_comes_back_quiet() {
    run_driver("fp_remainder_known_answers");
}

#[test]
fn an_array_too_large_for_the_allocator_is_out_of_memory() {
    run_driver_expecting_failure("array_new_overflow", "krusty: out of memory\n");
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
    run_driver_expecting_failure("string_repeat_overflow", "krusty: out of memory\n");
}

#[test]
fn surrogate_halves_concatenate_into_their_character() {
    run_driver("string_plus_surrogates");
}

#[test]
fn unboxing_null_raises_and_comes_back() {
    run_driver("unbox_null_raises");
}

#[test]
fn a_number_conversion_of_null_raises_and_comes_back() {
    run_driver("number_conversion_null_raises");
}

#[test]
fn a_builder_joins_a_surrogate_pair_appended_unit_by_unit() {
    run_driver("builder_joins_surrogate_pair");
}

#[test]
fn a_lazy_whose_initializer_throws_stays_uninitialized() {
    run_driver("lazy_initializer_throws");
}

#[test]
fn an_append_whose_to_string_throws_leaves_the_builder_alone() {
    run_driver("builder_append_throwing_to_string");
}

#[test]
fn a_builder_made_from_a_program_char_sequence_holds_its_text() {
    run_driver("builder_from_program_char_sequence");
}

#[test]
fn a_builder_grown_past_the_largest_length_is_out_of_memory() {
    run_driver_expecting_failure("builder_length_overflow", "krusty: out of memory\n");
}
