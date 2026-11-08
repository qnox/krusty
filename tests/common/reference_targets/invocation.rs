//! The existing non-JVM reference compiler invocation, shared by recorded outcomes and imports.

use krusty::compilation_target::CompilationTarget;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Reference {
    pub status: i32,
    pub stderr: String,
}

pub fn run_reference(
    target: CompilationTarget,
    work: &Path,
    paths: &[PathBuf],
    args: &[String],
    program: bool,
) -> Option<(Reference, PathBuf)> {
    let out = work.join("reference-out");
    let mut command = match target {
        CompilationTarget::Jvm => {
            panic!("the JVM reference compiler runs through `reference_compiler_run`")
        }
        CompilationTarget::Native => {
            let mut command = Command::new(
                crate::reference_recording_support::kotlin_native_root()?
                    .join("bin/kotlinc-native"),
            );
            if program {
                command
                    .args(["-p", "program", "-o"])
                    .arg(work.join("program"));
            } else {
                command.args(["-p", "library", "-o"]).arg(&out);
            }
            command
        }
        CompilationTarget::Js | CompilationTarget::WasmJs | CompilationTarget::WasmWasi => {
            assert!(
                !program,
                "only Kotlin/Native programs are run by this oracle"
            );
            let lib = krusty::toolchain::kotlinc_lib_dir()
                .expect("the kotlinc distribution is provisioned for every test run");
            let bin = lib.with_file_name("bin");
            let (compiler, flags): (_, &[&str]) = match target {
                CompilationTarget::Js => ("kotlinc-js", &[]),
                CompilationTarget::WasmJs => ("kotlinc-wasm", &["-Xwasm-target=wasm-js"]),
                _ => ("kotlinc-wasm", &["-Xwasm-target=wasm-wasi"]),
            };
            let stdlib = krusty::toolchain::kotlin_stdlib_klib(target)
                .expect("the exact target's Kotlin stdlib is provisioned");
            let mut command = Command::new(bin.join(compiler));
            command
                .args(flags)
                .args(["-Xir-produce-klib-dir", "-libraries"])
                .arg(stdlib)
                .arg("-ir-output-dir")
                .arg(&out)
                .args(["-ir-output-name", "reference"]);
            command
        }
    };
    let output = command
        .args(args)
        .args(paths)
        .output()
        .expect("run the reference compiler");
    Some((
        Reference {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        out,
    ))
}
