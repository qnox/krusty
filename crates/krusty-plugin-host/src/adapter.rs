//! The host's own code, `adapter/Host.kt`, compiled once per Kotlin version by that version's
//! `kotlin-compiler-embeddable` and kept in the cache. Compiling it against the exact compiler the
//! host loads means the adapter always links against that compiler, whatever changed between
//! releases.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::environment::Environment;
use crate::host::HostError;
use crate::install;
use crate::provision::HostRuntime;

const SOURCE: &str = include_str!("../adapter/Host.kt");
const SOURCE_NAME: &str = "Host.kt";
const JAR: &str = "krusty-plugin-host.jar";
pub(crate) const MAIN_CLASS: &str = "krusty.pluginhost.HostKt";

/// Compiler APIs the adapter opts in to. A marker a release does not have only draws a warning.
const OPT_INS: [&str; 2] = [
    "org.jetbrains.kotlin.compiler.plugin.ExperimentalCompilerApi",
    "org.jetbrains.kotlin.config.CompilerConfiguration.Internals",
];

/// The adapter jar for `runtime`, compiled first when the cache has none for this source and Kotlin
/// version. Compiling is announced on `report`; reusing the cached jar is silent.
pub(crate) fn jar(
    runtime: &HostRuntime,
    env: &Environment,
    report: &mut dyn Write,
) -> Result<PathBuf, HostError> {
    let dir = env.cache_dir.join("adapter").join(format!(
        "{}-{}",
        runtime.kotlin_version,
        source_digest()
    ));
    let jar = dir.join(JAR);
    if jar.is_file() {
        return Ok(jar);
    }
    // A directory without the jar is a broken earlier build, which a rename would not replace.
    install::remove_entry(&dir).map_err(|error| HostError::io(&dir, error))?;
    let parent = dir
        .parent()
        .expect("the adapter directory is inside the cache");
    std::fs::create_dir_all(parent).map_err(|error| HostError::io(parent, error))?;
    let _ = writeln!(
        report,
        "info: compiling krusty's plugin host for kotlinc {}, once; later builds reuse it",
        runtime.kotlin_version
    );
    let staged = install::staging_path(&dir);
    let built = compile(runtime, &staged);
    if built.is_err() {
        install::remove(&staged);
    }
    built?;
    install::publish(&staged, &dir, |dir| dir.join(JAR).is_file())
        .map_err(|error| HostError::io(&dir, error))?;
    Ok(jar)
}

/// Compiles the adapter into `<dir>/krusty-plugin-host.jar` with the runtime's own compiler.
fn compile(runtime: &HostRuntime, dir: &Path) -> Result<(), HostError> {
    std::fs::create_dir_all(dir).map_err(|error| HostError::io(dir, error))?;
    let source = dir.join(SOURCE_NAME);
    std::fs::write(&source, SOURCE).map_err(|error| HostError::io(&source, error))?;
    let classpath =
        std::env::join_paths(&runtime.compiler_classpath).map_err(|error| HostError::Build {
            detail: format!("the compiler classpath cannot be passed to java: {error}"),
        })?;
    let mut command = Command::new(&runtime.java);
    command
        .arg("-cp")
        .arg(&classpath)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args(["-no-stdlib", "-no-reflect", "-jvm-target", "1.8"]);
    for marker in OPT_INS {
        command.arg(format!("-opt-in={marker}"));
    }
    let output = command
        .arg("-classpath")
        .arg(&classpath)
        .arg("-d")
        .arg(dir.join(JAR))
        .arg(&source)
        .output()
        .map_err(|error| HostError::Build {
            detail: format!("could not run {}: {error}", runtime.java.display()),
        })?;
    if output.status.success() && dir.join(JAR).is_file() {
        return Ok(());
    }
    let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        detail = format!("{}\n{detail}", stdout.trim());
    }
    Err(HostError::Build {
        detail: format!(
            "kotlinc {} exited with {}: {detail}",
            runtime.kotlin_version, output.status
        ),
    })
}

/// Names the adapter build in the cache, so an edited adapter is compiled afresh.
fn source_digest() -> String {
    Sha256::digest(SOURCE.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
