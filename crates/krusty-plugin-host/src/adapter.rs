//! The host's own code, the Kotlin sources in `adapter/`, compiled once per Kotlin version by that version's
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

/// The adapter's sources every compiler builds, by file name.
const SOURCES: [(&str, &str); 4] = [
    ("Host.kt", include_str!("../adapter/Host.kt")),
    ("ModuleIr.kt", include_str!("../adapter/ModuleIr.kt")),
    ("Session.kt", include_str!("../adapter/Session.kt")),
    ("Wire.kt", include_str!("../adapter/Wire.kt")),
];

/// The adapter's sources for compiler API that changed between Kotlin releases, under
/// `adapter/compat/<first release>/`.
struct Compat {
    /// The releases these sources build for, each exactly.
    versions: &'static [&'static str],
    sources: &'static [(&'static str, &'static str)],
}

/// Every supported Kotlin version is listed in exactly one.
const COMPAT: [Compat; 2] = [
    Compat {
        versions: &["2.4.0", "2.4.10"],
        sources: &[(
            "IrFiles.kt",
            include_str!("../adapter/compat/2.4.0/IrFiles.kt"),
        )],
    },
    Compat {
        versions: &["2.4.20", "2.4.21"],
        sources: &[(
            "IrFiles.kt",
            include_str!("../adapter/compat/2.4.20/IrFiles.kt"),
        )],
    },
];

/// Every source of the adapter for `kotlin_version`, or `None` when no compat set lists it.
fn sources(kotlin_version: &str) -> Option<Vec<(&'static str, &'static str)>> {
    let compat = COMPAT
        .iter()
        .find(|compat| compat.versions.contains(&kotlin_version))?;
    Some(SOURCES.iter().chain(compat.sources).copied().collect())
}

const JAR: &str = "krusty-plugin-host.jar";
/// The jar's SHA-256, recorded when it is built and checked before it is reused.
const DIGEST: &str = "krusty-plugin-host.jar.sha256";
pub(crate) const MAIN_CLASS: &str = "krusty.pluginhost.HostKt";

/// The adapter jar for `runtime`, compiled first when the cache has none for this source and the
/// runtime's exact compiler classpath. Compiling is announced on `report`; reusing the cached jar is
/// silent.
///
/// The cache key is the sources and [`HostRuntime::compiler_identity`], the pins the classpath was
/// verified against, so a corrected pin for the same Kotlin version builds afresh. A cached jar is
/// reused only when it still has the digest recorded when it was built.
pub(crate) fn jar(
    runtime: &HostRuntime,
    env: &Environment,
    report: &mut dyn Write,
) -> Result<PathBuf, HostError> {
    let Some(sources) = sources(&runtime.kotlin_version) else {
        return Err(HostError::Build {
            detail: format!(
                "the adapter has no sources for kotlinc {}",
                runtime.kotlin_version
            ),
        });
    };
    let dir = env.cache_dir.join("adapter").join(format!(
        "{}-{}",
        runtime.kotlin_version,
        build_key(runtime, &sources)
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
    let built = compile(runtime, &sources, &staged);
    if built.is_err() {
        install::remove(&staged);
    }
    built?;
    install::publish(&staged, &dir, is_intact).map_err(|error| HostError::io(&dir, error))?;
    Ok(jar)
}

/// Compiles the adapter into `<dir>/krusty-plugin-host.jar` with the runtime's own compiler.
fn compile(runtime: &HostRuntime, texts: &[(&str, &str)], dir: &Path) -> Result<(), HostError> {
    let sources = dir.join("src");
    std::fs::create_dir_all(&sources).map_err(|error| HostError::io(&sources, error))?;
    for (name, text) in texts {
        let source = sources.join(name);
        std::fs::write(&source, text).map_err(|error| HostError::io(&source, error))?;
    }
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
        .arg(&sources)
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

/// Names the adapter build in the cache: the adapter sources and the classpath identity they are
/// compiled against, so a change to either compiles afresh.
fn build_key(runtime: &HostRuntime, sources: &[(&str, &str)]) -> String {
    let mut digest = Sha256::new();
    for (name, text) in sources {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(text.as_bytes());
        digest.update([0]);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_supported_kotlin_version_has_the_adapter_sources_for_its_compiler() {
        let versions = include_str!("../../../kotlin-versions");
        let supported: Vec<&str> = versions
            .lines()
            .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
            .map(|line| line.split_whitespace().next().unwrap())
            .collect();
        let listed: Vec<&str> = COMPAT
            .iter()
            .flat_map(|compat| compat.versions.iter().copied())
            .collect();
        assert_eq!(listed, supported);
        for version in supported {
            let names: Vec<&str> = sources(version)
                .unwrap()
                .iter()
                .map(|(name, _)| *name)
                .collect();
            assert_eq!(
                names,
                [
                    "Host.kt",
                    "ModuleIr.kt",
                    "Session.kt",
                    "Wire.kt",
                    "IrFiles.kt"
                ],
                "{version}"
            );
        }
    }

    #[test]
    fn a_kotlin_version_no_compat_set_lists_has_no_adapter() {
        assert_eq!(sources("2.4.20-RC"), None);
    }
}
