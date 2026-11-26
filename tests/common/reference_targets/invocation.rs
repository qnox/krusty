//! The existing non-JVM reference compiler invocation, shared by recorded outcomes and imports.

use krusty::compilation_target::CompilationTarget;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Reference {
    pub status: i32,
    pub stderr: String,
}

/// Run the reference compiler now, without consulting or writing a recording.
pub fn run_live(
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
            let root = crate::reference_recording_support::kotlin_native_root()?;
            let mut arguments: Vec<OsString> = Vec::new();
            if program {
                arguments.extend(
                    crate::reference_recording_support::kotlin_native_program_arguments(&root),
                );
                arguments.extend(["-p", "program", "-o"].map(OsString::from));
                arguments.push(work.join("program").into());
            } else {
                arguments.extend(["-p", "library", "-o"].map(OsString::from));
                arguments.push(out.clone().into());
            }
            arguments.extend(args.iter().map(OsString::from));
            arguments.extend(paths.iter().map(OsString::from));
            let compile = crate::common::kotlin_native_server::kotlinc_native(&root, &arguments);
            return Some((
                Reference {
                    status: compile.status,
                    stderr: compile.stderr,
                },
                out,
            ));
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
