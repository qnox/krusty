//! Running a module's KSP processors before it compiles, as the toolchain's `kspJvm` task does:
//! KSP's own command line (`com.google.devtools.ksp.cmdline.KSPJvmMain`) in a JVM on the module's
//! JDK, its arguments in an argument file, with KSP's artifacts (`symbol-processing-api`,
//! `symbol-processing-aa-embeddable`, `symbol-processing-common-deps` of `settings.kotlin.ksp.version`)
//! and their dependencies on the class path, and the processors' runtime classpath as KSP's last
//! argument.
//!
//! KSP writes classes, Kotlin and Java sources and resources below `build/generated/<module>/main`.
//! The module compiles against the generated classes (the last entry of its classpath), and its
//! resources output takes the generated resources after its other resource directories, so a module
//! running KSP always has a resources output, which the modules that compile against it put on
//! their classpath. krusty-toolchain does not yet compile the sources KSP generates: a run that
//! generates any fails the module.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::maven::Coordinates;

/// The group of KSP's artifacts.
const KSP_GROUP: &str = "com.google.devtools.ksp";
/// KSP's artifacts, in the order the toolchain resolves them for its class path.
const KSP_ARTIFACTS: [&str; 3] = [
    "symbol-processing-api",
    "symbol-processing-aa-embeddable",
    "symbol-processing-common-deps",
];
const MAIN_CLASS: &str = "com.google.devtools.ksp.cmdline.KSPJvmMain";
/// The JDK from which KSP's JVM is given the access the toolchain grants it.
const ACCESS_FLAGS_FROM: i64 = 25;
const ACCESS_FLAGS: [&str; 2] = [
    "--enable-native-access=ALL-UNNAMED",
    "--sun-misc-unsafe-memory-access=allow",
];

/// The artifacts that make up KSP `version`, to resolve with their dependencies.
pub(super) fn ksp_artifacts(version: &str) -> Vec<Coordinates> {
    KSP_ARTIFACTS
        .iter()
        .map(|artifact| Coordinates::new(KSP_GROUP, artifact, Some(version)))
        .collect()
}

/// Where KSP writes for a module's main fragment.
pub struct KspOutputs {
    pub base: PathBuf,
    pub classes: PathBuf,
    pub kotlin: PathBuf,
    pub java: PathBuf,
    pub resources: PathBuf,
}

impl KspOutputs {
    pub(super) fn of(root: &Path, module: &str) -> Self {
        let base = root
            .join("build")
            .join("generated")
            .join(module)
            .join("main");
        Self {
            classes: base.join("classes").join("ksp"),
            kotlin: base.join("src").join("ksp").join("kotlin"),
            java: base.join("src").join("ksp").join("java"),
            resources: base.join("resources").join("ksp"),
            base,
        }
    }
}

/// One module's KSP run.
pub struct Ksp {
    pub java: PathBuf,
    /// The JVM's arguments, KSP's included, written to `argument_file`.
    pub arguments: Vec<OsString>,
    pub argument_file: PathBuf,
    /// The JVM's temporary directory and KSP's caches, both kept between runs as the toolchain
    /// keeps them.
    pub temporary: PathBuf,
    pub caches: PathBuf,
    pub outputs: KspOutputs,
}

/// What one module's KSP run is made of.
pub(super) struct KspRun<'a> {
    pub root: &'a Path,
    pub module: &'a str,
    pub module_directory: &'a Path,
    pub jdk_home: &'a Path,
    /// `jvm.jdk.version`.
    pub jdk_version: i64,
    /// The `-jvm-target`: `jvm.release`.
    pub jvm_target: i64,
    /// `kotlin.version`, whose major and minor version are the language and API version.
    pub kotlin_version: &'a str,
    /// KSP's artifacts and their dependencies, in class path order.
    pub ksp_classpath: &'a [PathBuf],
    /// The processors and their runtime dependencies, in order.
    pub processors: &'a [PathBuf],
    /// `ksp.processorOptions`, in the order written.
    pub options: &'a [(String, String)],
    /// The fragment's source directories that exist, in order.
    pub sources: &'a [PathBuf],
    /// The module's compile classpath.
    pub libraries: &'a [PathBuf],
}

fn joined(prefix: &str, path: &Path) -> OsString {
    let mut argument = OsString::from(prefix);
    argument.push(path);
    argument
}

fn paths(prefix: &str, paths: &[PathBuf]) -> Result<OsString, String> {
    let joined = std::env::join_paths(paths)
        .map_err(|error| format!("cannot pass the paths of `{prefix}` to KSP: {error}"))?;
    let mut argument = OsString::from(prefix);
    argument.push(joined);
    Ok(argument)
}

/// The KSP run `run` describes.
pub(super) fn ksp(run: &KspRun<'_>) -> Result<Ksp, String> {
    let task = run
        .root
        .join("build")
        .join("tasks")
        .join(format!("_{}_kspJvm", run.module));
    let temporary = task.join("jvm-temp");
    let caches = task.join("ksp-cache");
    let outputs = KspOutputs::of(run.root, run.module);
    let version = language_version(run.kotlin_version)?;
    let mut arguments = vec![
        OsString::from("-cp"),
        std::env::join_paths(run.ksp_classpath)
            .map_err(|error| format!("cannot pass KSP's class path to java: {error}"))?,
        joined("-Djava.io.tmpdir=", &temporary),
    ];
    if run.jdk_version >= ACCESS_FLAGS_FROM {
        arguments.extend(ACCESS_FLAGS.map(OsString::from));
    }
    arguments.push(MAIN_CLASS.into());
    arguments.push(format!("-module-name={}", run.module).into());
    if !run.sources.is_empty() {
        arguments.push(paths("-source-roots=", run.sources)?);
    }
    if !run.libraries.is_empty() {
        arguments.push(paths("-libraries=", run.libraries)?);
    }
    if !run.options.is_empty() {
        let options: Vec<String> = run
            .options
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let separator = if cfg!(windows) { ";" } else { ":" };
        arguments.push(format!("-processor-options={}", options.join(separator)).into());
    }
    arguments.extend(
        [
            format!("-language-version={version}"),
            format!("-api-version={version}"),
            "-all-warnings-as-errors=false".to_string(),
            "-map-annotation-arguments-in-java=false".to_string(),
            "-incremental=false".to_string(),
            "-incremental-log=false".to_string(),
        ]
        .map(OsString::from),
    );
    arguments.push(joined("-project-base-dir=", run.module_directory));
    arguments.push(joined("-output-base-dir=", &outputs.base));
    arguments.push(joined("-caches-dir=", &caches));
    arguments.push(joined("-class-output-dir=", &outputs.classes));
    arguments.push(joined("-kotlin-output-dir=", &outputs.kotlin));
    arguments.push(joined("-resource-output-dir=", &outputs.resources));
    if !run.sources.is_empty() {
        arguments.push(paths("-java-source-roots=", run.sources)?);
    }
    arguments.push(joined("-java-output-dir=", &outputs.java));
    arguments.push(joined("-jdk-home=", run.jdk_home));
    arguments.push(format!("-jvm-target={}", run.jvm_target).into());
    arguments.push("-jvm-default-mode=disable".into());
    arguments.push(
        std::env::join_paths(run.processors)
            .map_err(|error| format!("cannot pass the processors' classpath to KSP: {error}"))?,
    );
    Ok(Ksp {
        java: run
            .jdk_home
            .join("bin")
            .join(if cfg!(windows) { "java.exe" } else { "java" }),
        arguments,
        argument_file: task.join("java-arguments.txt"),
        temporary,
        caches,
        outputs,
    })
}

/// The major and minor version of Kotlin release `version`.
fn language_version(version: &str) -> Result<String, String> {
    let mut parts = version.split('.');
    match (parts.next(), parts.next()) {
        (Some(major), Some(minor)) if !major.is_empty() && !minor.is_empty() => {
            Ok(format!("{major}.{minor}"))
        }
        _ => Err(format!(
            "Kotlin version `{version}` has no major and minor version"
        )),
    }
}

/// What a KSP run printed, and how it ended.
pub struct KspRan {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
}

/// Run `ksp` from empty output directories.
pub(super) fn run(ksp: &Ksp) -> io::Result<KspRan> {
    std::fs::create_dir_all(&ksp.temporary)?;
    std::fs::create_dir_all(&ksp.caches)?;
    match std::fs::remove_dir_all(&ksp.outputs.base) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::write(&ksp.argument_file, argument_file(&ksp.arguments))?;
    let mut file_argument = OsString::from("@");
    file_argument.push(&ksp.argument_file);
    let ran = Command::new(&ksp.java).arg(file_argument).output()?;
    Ok(KspRan {
        stdout: ran.stdout,
        stderr: ran.stderr,
        status: ran.status,
    })
}

/// The sources a KSP run generated, which krusty-toolchain does not compile yet.
pub(super) fn generated_sources(outputs: &KspOutputs) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for directory in [&outputs.kotlin, &outputs.java] {
        walk(directory, &mut found)?;
    }
    Ok(found)
}

fn walk(directory: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), found)?;
        } else {
            found.push(entry.path());
        }
    }
    Ok(())
}

/// The JDK's argument file format: one argument a line, quoted when it holds anything the JVM's
/// reader would split or unescape.
fn argument_file(arguments: &[OsString]) -> Vec<u8> {
    let mut file = Vec::new();
    for argument in arguments {
        let text = argument.to_string_lossy();
        if text.is_empty() || text.contains([' ', '\t', '\r', '\n', '"', '\'', '\\', '#']) {
            file.push(b'"');
            for character in text.chars() {
                match character {
                    '"' | '\\' => {
                        file.push(b'\\');
                        file.push(character as u8);
                    }
                    '\n' => file.extend_from_slice(b"\\n"),
                    '\r' => file.extend_from_slice(b"\\r"),
                    '\t' => file.extend_from_slice(b"\\t"),
                    _ => {
                        let mut buffer = [0; 4];
                        file.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                    }
                }
            }
            file.push(b'"');
        } else {
            file.extend_from_slice(text.as_bytes());
        }
        file.push(b'\n');
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_argument_the_jvm_would_split_or_unescape_is_quoted() {
        let file = argument_file(&[
            "-cp".into(),
            "/a/b.jar:/c.jar".into(),
            "/with space/x".into(),
            "C:\\jdk".into(),
            "say \"hi\"".into(),
            "".into(),
        ]);
        assert_eq!(
            String::from_utf8(file).unwrap(),
            "-cp\n/a/b.jar:/c.jar\n\"/with space/x\"\n\"C:\\\\jdk\"\n\"say \\\"hi\\\"\"\n\"\"\n"
        );
    }

    #[test]
    fn the_language_version_is_the_releases_major_and_minor_version() {
        assert_eq!(language_version("2.4.20").as_deref(), Ok("2.4"));
        assert_eq!(language_version("2.5.0-RC").as_deref(), Ok("2.5"));
        assert!(language_version("2").is_err());
    }
}
