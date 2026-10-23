//! The KSP host's own code, the Java sources in `ksp-adapter/`, compiled once per KSP version by the
//! host JDK's `javac` against that version's API and kept in the cache. Java needs no compiler but
//! the JDK the host runs on, so a KSP host never downloads kotlinc.

use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::host::HostError;
use crate::install;
use crate::provision::KspRuntime;
use crate::Environment;

macro_rules! source {
    ($name:literal) => {
        (
            concat!("krusty/ksphost/", $name),
            include_str!(concat!("../../ksp-adapter/krusty/ksphost/", $name)),
        )
    };
}

/// The adapter's sources, by path below `ksp-adapter/`.
const SOURCES: [(&str, &str); 28] = [
    source!("AnnotationTypeReference.java"),
    source!("ClassType.java"),
    source!("CodeGen.java"),
    source!("ConstantValue.java"),
    source!("Host.java"),
    source!("HostResolver.java"),
    source!("LibraryClass.java"),
    source!("Logger.java"),
    source!("Name.java"),
    source!("Nodes.java"),
    source!("NotImplemented.java"),
    source!("Parameter.java"),
    source!("Session.java"),
    source!("SetterParameter.java"),
    source!("SourceAccessor.java"),
    source!("SourceAnnotation.java"),
    source!("SourceClass.java"),
    source!("SourceFile.java"),
    source!("SourceFunction.java"),
    source!("SourceProperty.java"),
    source!("SourceType.java"),
    source!("SourceTypeAlias.java"),
    source!("SourceTypeArgument.java"),
    source!("SourceTypeParameter.java"),
    source!("SourceTypeReference.java"),
    source!("SourceValueArgument.java"),
    source!("Symbols.java"),
    source!("Wire.java"),
];

const JAR: &str = "krusty-ksp-host.jar";
/// The jar's SHA-256, recorded when it is built and checked before it is reused.
const DIGEST: &str = "krusty-ksp-host.jar.sha256";
pub(crate) const MAIN_CLASS: &str = "krusty.ksphost.Host";

/// The adapter jar for `runtime`, compiled first when the cache has none for these sources and the
/// runtime's exact API classpath. Compiling is announced on `report`; reusing it is silent.
pub(crate) fn jar(
    runtime: &KspRuntime,
    env: &Environment,
    report: &mut dyn Write,
) -> Result<PathBuf, HostError> {
    let dir = env.cache_dir.join("ksp-adapter").join(format!(
        "{}-{}",
        runtime.ksp_version,
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
        "info: compiling krusty's KSP host for KSP {}, once; later builds reuse it",
        runtime.ksp_version
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

/// Compiles the adapter with the runtime JDK's `javac` and packs it into `<dir>/krusty-ksp-host.jar`.
fn compile(runtime: &KspRuntime, dir: &Path) -> Result<(), HostError> {
    let sources = dir.join("src");
    let classes = dir.join("classes");
    let mut paths = Vec::new();
    for (name, text) in SOURCES {
        let source = sources.join(name);
        let parent = source.parent().expect("a source is in a package");
        std::fs::create_dir_all(parent).map_err(|error| HostError::io(parent, error))?;
        std::fs::write(&source, text).map_err(|error| HostError::io(&source, error))?;
        paths.push(source);
    }
    std::fs::create_dir_all(&classes).map_err(|error| HostError::io(&classes, error))?;
    let javac =
        runtime
            .java_home
            .join("bin")
            .join(if cfg!(windows) { "javac.exe" } else { "javac" });
    if !javac.is_file() {
        return Err(HostError::Build {
            detail: format!(
                "{} has no javac; the KSP host needs a JDK, not a JRE",
                runtime.java_home.display()
            ),
        });
    }
    let classpath =
        std::env::join_paths(&runtime.api_classpath).map_err(|error| HostError::Build {
            detail: format!("KSP's API classpath cannot be passed to javac: {error}"),
        })?;
    let mut command = std::process::Command::new(&javac);
    command.env("JAVA_HOME", &runtime.java_home);
    // The adapter runs on every JDK KSP's API does.
    if runtime.java_major >= 9 {
        command.args(["--release", "8"]);
    } else {
        command.args(["-source", "8", "-target", "8"]);
    }
    let output = command
        .args(["-encoding", "UTF-8", "-nowarn", "-cp"])
        .arg(&classpath)
        .arg("-d")
        .arg(&classes)
        .args(&paths)
        .output()
        .map_err(|error| HostError::Build {
            detail: format!("could not run {}: {error}", javac.display()),
        })?;
    if !output.status.success() {
        return Err(HostError::Build {
            detail: format!(
                "javac exited with {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    let jar = dir.join(JAR);
    pack(&classes, &jar).map_err(|error| HostError::io(&jar, error))?;
    let digest = file_digest(&jar).map_err(|error| HostError::io(&jar, error))?;
    let stamp = dir.join(DIGEST);
    std::fs::write(&stamp, digest).map_err(|error| HostError::io(&stamp, error))
}

/// Packs the class files below `classes` into the jar `jar`, in path order so the jar is the same for
/// the same classes.
fn pack(classes: &Path, jar: &Path) -> std::io::Result<()> {
    let mut files = Vec::new();
    collect(classes, classes, &mut files)?;
    files.sort();
    let mut writer = zip::ZipWriter::new(std::fs::File::create(jar)?);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    for name in files {
        writer.start_file(name.as_str(), options)?;
        writer.write_all(&std::fs::read(classes.join(&name))?)?;
    }
    writer.finish()?;
    Ok(())
}

fn collect(root: &Path, dir: &Path, into: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(root, &path, into)?;
        } else {
            let relative = path.strip_prefix(root).expect("below the root");
            let name: Vec<String> = relative
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            into.push(name.join("/"));
        }
    }
    Ok(())
}

/// Names the adapter build in the cache: the adapter sources, the API classpath identity they are
/// compiled against, and the JDK release that compiles them.
fn build_key(runtime: &KspRuntime) -> String {
    let mut digest = Sha256::new();
    for (name, text) in SOURCES {
        digest.update(name.as_bytes());
        digest.update([0]);
        digest.update(text.as_bytes());
        digest.update([0]);
    }
    digest.update(runtime.api_identity.as_bytes());
    digest.update(runtime.java_major.to_be_bytes());
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_adapter_source_is_compiled() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ksp-adapter/krusty/ksphost");
        let mut on_disk: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                format!(
                    "krusty/ksphost/{}",
                    entry.unwrap().file_name().to_string_lossy()
                )
            })
            .collect();
        on_disk.sort();
        let listed: Vec<String> = SOURCES.iter().map(|(name, _)| name.to_string()).collect();
        assert_eq!(listed, on_disk);
    }
}
