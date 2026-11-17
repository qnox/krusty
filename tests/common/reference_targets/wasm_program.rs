//! The exact Kotlin/Wasm reference compiler's program, run under Node.js, for an owning test that
//! compares a product program with what the reference target observes.

use krusty::compilation_target::CompilationTarget;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::reference_invocation_support::run_reference;

/// Compile `paths` with the exact Kotlin/Wasm reference compiler for `target` into an executable
/// and run its `main` under Node.js, returning its standard output. The program is linked from the
/// KLIB the reference compiler first produces, as a Kotlin/Wasm build does. `None` when the
/// reference compiler is not provisioned; `Err` with the step's output when a step fails.
pub fn run_wasm_reference_program(
    target: CompilationTarget,
    work: &Path,
    paths: &[PathBuf],
) -> Option<Result<String, String>> {
    let flag = match target {
        CompilationTarget::WasmJs => "-Xwasm-target=wasm-js",
        CompilationTarget::WasmWasi => "-Xwasm-target=wasm-wasi",
        other => panic!("{other:?} is not a Kotlin/Wasm target"),
    };
    let (library, klib) = run_reference(target, work, paths, &[], false)?;
    if library.status != 0 {
        return Some(Err(format!("reference KLIB: {}", library.stderr)));
    }
    let lib = krusty::toolchain::kotlinc_lib_dir()
        .expect("the kotlinc distribution is provisioned for every test run");
    let stdlib = krusty::toolchain::kotlin_stdlib_klib(target)
        .expect("the exact target's Kotlin stdlib is provisioned");
    let program = work.join("reference-program");
    let linked = Command::new(lib.with_file_name("bin").join("kotlinc-wasm"))
        .arg(flag)
        .arg("-Xir-produce-js")
        .arg("-libraries")
        .arg(stdlib)
        .arg(format!("-Xinclude={}", klib.display()))
        .arg("-ir-output-dir")
        .arg(&program)
        .args(["-ir-output-name", "reference"])
        .output()
        .expect("run the reference compiler");
    if !linked.status.success() {
        return Some(Err(format!(
            "reference program: {}",
            String::from_utf8_lossy(&linked.stderr)
        )));
    }
    let run = Command::new("node")
        .arg("--no-warnings")
        .arg(program.join("reference.mjs"))
        .output()
        .expect("run Node.js");
    if !run.status.success() {
        return Some(Err(format!(
            "reference run: {}",
            String::from_utf8_lossy(&run.stderr)
        )));
    }
    Some(Ok(String::from_utf8_lossy(&run.stdout).into_owned()))
}
