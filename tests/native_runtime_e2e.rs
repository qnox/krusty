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
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

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
            // Runtime descriptors name the fields they define and intentionally leave the rest
            // zero-initialized. Keep every other warning an error.
            "-Wno-missing-field-initializers",
        ])
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
fn only_an_arrays_end_pointer_keeps_it_alive_and_no_neighbour_is_kept() {
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

fn compiled_build_script() -> PathBuf {
    static BUILD_SCRIPT: OnceLock<PathBuf> = OnceLock::new();
    BUILD_SCRIPT
        .get_or_init(|| {
            let scratch = common::scratch_dir().expect("scratch directory");
            let executable = scratch.join("native-runtime-build-script");
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = Command::new(rustc)
                .args(["--edition=2021", "build.rs", "-o"])
                .arg(&executable)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .expect("compile build.rs");
            assert_eq!(
                (output.status.success(), output.stdout, output.stderr),
                (true, Vec::new(), Vec::new()),
                "build.rs compiles as a standalone build-script executable"
            );
            executable
        })
        .clone()
}

fn run_build_script(compiler: &Path, out_dir: &Path) -> Output {
    Command::new(compiled_build_script())
        .env("OUT_DIR", out_dir)
        .env("KRUSTY_RUNTIME_CC", compiler)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run build.rs")
}

#[test]
fn a_missing_runtime_compiler_leaves_the_native_target_unavailable() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("missing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let missing = scratch.join("compiler-that-does-not-exist");

    let output = run_build_script(&missing, &out_dir);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stderr, b"");
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        format!(
            "cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
             cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
             cargo:rerun-if-changed=build.rs\n\
             cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n\
             cargo:warning=native runtime: compiler `{}` was not found; no native target will be available. Install clang, or set KRUSTY_RUNTIME_CC.\n\
             cargo:rerun-if-env-changed=PATH\n",
            missing.display()
        )
    );
    assert_eq!(
        fs::read_to_string(out_dir.join("prebuilt_runtime.rs"))
            .expect("read generated runtime table"),
        "// Generated by build.rs: the native runtime, prebuilt per target.\n\
         pub const PREBUILT: &[(crate::native::Arch, &[(&str, &[u8])])] = &[\n\
         ];\n\
         /// Whether any target's runtime was prebuilt (a C compiler was available when krusty was built).\n\
         pub const AVAILABLE: bool = false;\n"
    );
}

#[test]
fn a_failing_runtime_compiler_fails_the_build() {
    let scratch = common::scratch_dir().expect("scratch directory");
    let out_dir = scratch.join("failing-runtime-compiler-out");
    fs::create_dir(&out_dir).expect("create build-script output directory");
    let compiler = scratch.join("runtime-compiler-exits-one");
    fs::write(&compiler, "#!/bin/sh\nexit 1\n").expect("write failing compiler");
    let mut permissions = fs::metadata(&compiler)
        .expect("read failing compiler metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&compiler, permissions).expect("make failing compiler executable");

    let output = run_build_script(&compiler, &out_dir);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).expect("build-script stderr is UTF-8"),
        format!(
            "native runtime: `{}` failed compiling `krusty_fp.c` for \
             `x86_64-unknown-linux-gnu` (exit status: 1)\n",
            compiler.display()
        )
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("build-script stdout is UTF-8"),
        "cargo:rerun-if-changed=src/native/runtime/krusty_fp.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_gc.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_start.c\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_sys.h\n\
         cargo:rerun-if-changed=src/native/runtime/krusty_rt.h\n\
         cargo:rerun-if-changed=build.rs\n\
         cargo:rerun-if-env-changed=KRUSTY_RUNTIME_CC\n"
    );
    assert!(
        !out_dir.join("prebuilt_runtime.rs").exists(),
        "a failed compiler must not publish a partial target table"
    );
}
