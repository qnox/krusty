//! The host's own code, `adapter/Host.kt`, compiled once per Kotlin version by that version's
//! `kotlin-compiler-embeddable` and kept in the cache. Compiling it against the exact compiler the
//! host loads means the adapter always links against that compiler, whatever changed between
//! releases.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::environment::Environment;
use crate::host::HostError;
use crate::install;
use crate::provision::{HostRuntime, COMPILER_MIN_JAVA};

const SOURCE: &str = include_str!("../adapter/Host.kt");
const SOURCE_NAME: &str = "Host.kt";
const JAR: &str = "krusty-plugin-host.jar";
/// The jar's SHA-256, recorded when it is built and checked before it is reused.
const DIGEST: &str = "krusty-plugin-host.jar.sha256";
pub(crate) const MAIN_CLASS: &str = "krusty.pluginhost.HostKt";

/// The adapter jar for `runtime`, compiled first when the cache has none for this source and the
/// runtime's exact compiler classpath. Compiling is announced on `report`; reusing the cached jar is
/// silent.
///
/// The cache key is the source and [`HostRuntime::compiler_identity`], the pins the classpath was
/// verified against, so a corrected pin for the same Kotlin version builds afresh. A cached jar is
/// reused only when it still has the digest recorded when it was built.
pub(crate) fn jar(
    runtime: &HostRuntime,
    env: &Environment,
    report: &mut dyn Write,
) -> Result<PathBuf, HostError> {
    let dir = env.cache_dir.join("adapter").join(format!(
        "{}-{}",
        runtime.kotlin_version,
        build_key(runtime)
    ));
    let jar = dir.join(JAR);
    if is_intact(&dir) {
        return Ok(jar);
    }
    // Anything else there is a broken or altered earlier build, which a rename would not replace.
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
    install::publish(&staged, &dir, is_intact).map_err(|error| HostError::io(&dir, error))?;
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
    let mut command = runtime.java_command();
    command
        .arg("-cp")
        .arg(&classpath)
        .arg("org.jetbrains.kotlin.cli.jvm.K2JVMCompiler")
        .args(["-no-stdlib", "-no-reflect", "-jvm-target"])
        // The adapter runs on every JDK the compiler does.
        .arg(jvm_target(COMPILER_MIN_JAVA));
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
    if output.status.success() {
        if let Ok(digest) = file_digest(&dir.join(JAR)) {
            let stamp = dir.join(DIGEST);
            return std::fs::write(&stamp, digest).map_err(|error| HostError::io(&stamp, error));
        }
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

/// Names the adapter build in the cache: the adapter source and the classpath identity it is
/// compiled against, so a change to either compiles afresh.
fn build_key(runtime: &HostRuntime) -> String {
    let mut digest = Sha256::new();
    digest.update(SOURCE.as_bytes());
    digest.update([0]);
    digest.update(runtime.compiler_identity.as_bytes());
    hex(&digest.finalize()[..8])
}

/// Whether `dir` holds a built adapter whose jar still has the digest recorded at build time.
fn is_intact(dir: &Path) -> bool {
    match (
        std::fs::read_to_string(dir.join(DIGEST)),
        file_digest(&dir.join(JAR)),
    ) {
        (Ok(recorded), Ok(actual)) => recorded == actual,
        _ => false,
    }
}

fn file_digest(path: &Path) -> std::io::Result<String> {
    Ok(hex(&Sha256::digest(std::fs::read(path)?)))
}

/// kotlinc's `-jvm-target` spelling of a Java feature release: `1.8` for Java 8, `11` from Java 9.
fn jvm_target(java: u32) -> String {
    if java <= 8 {
        format!("1.{java}")
    } else {
        java.to_string()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
