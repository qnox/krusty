//! How a `compileJvm` task runs, for JetBrains' `kotlin` and krusty-toolchain alike, and the
//! compiler arguments it gives krusty.
//!
//! A run is laid out as `kotlin::Resolving` lays it out (`<directory>` in compared output), with a
//! JDK beside the project (`jdk`: a `release` file naming `JAVA_VERSION="25.0.2"` and empty
//! `bin/javac` and `bin/java`, which is all either reads of it) and the file the recording krusty
//! writes (`record`). `KRUSTY_BIN` names that recorder: it appends its arguments to the record, one
//! per line, then a `----` line, and exits 0, or with status 3 for the module
//! `KRUSTY_RECORDER_FAIL` names. Nothing is compiled.
//!
//! The toolchain compiles through the Build Tools API, whose implementation it loads from its
//! shared cache by Maven coordinates. krusty's implementation (`tools/krusty-build-tools`) is
//! installed there ([`RecordingKrusty`]), so the toolchain hands the recorder the arguments it would
//! hand krusty. The implementation's sources are part of the oracle entry's identity; a live run
//! builds it from them with its Gradle wrapper, in a scratch copy, run by the JDK
//! `KRUSTY_BUILD_TOOLS_JAVA_HOME` names (else `JAVA_HOME`). The cached entry's stdout is the record,
//! its stderr what `kotlin` printed, its code `kotlin`'s exit status.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::support::kotlin::{Compiler, Resolving};
use crate::support::oracle::{scratch, script, Output};

pub const JDK: &str = "jdk";
pub const RECORD: &str = "record";
/// The line the recorder writes after each invocation's arguments.
pub const END_OF_INVOCATION: &str = "----";

const RECORDER: &str = "#!/bin/sh
for argument in \"$@\"; do
  printf '%s\\n' \"$argument\"
  case $argument in -module-name=*) module=${argument#-module-name=} ;; esac
done >> \"$KRUSTY_RECORD\"
printf '%s\\n' '----' >> \"$KRUSTY_RECORD\"
if [ -n \"${KRUSTY_RECORDER_FAIL:-}\" ] && [ \"$module\" = \"$KRUSTY_RECORDER_FAIL\" ]; then
  echo \"recorder: failing $module\" >&2
  exit 3
fi
";

/// The recording krusty, written once per process before any test of the binary starts a process:
/// a script written while another thread starts a process could not be executed (`ETXTBSY`).
pub fn recorder() -> &'static Path {
    static RECORDER_PATH: OnceLock<PathBuf> = OnceLock::new();
    RECORDER_PATH.get_or_init(|| {
        let directory =
            std::env::temp_dir().join(format!("krusty-toolchain-recorder-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("krusty");
        std::fs::write(&path, RECORDER).unwrap();
        make_executable(&path);
        path
    })
}

fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Lay out what a task needs in a run's `directory` beside the project and the local Maven
/// repository: the JDK and an empty shared cache.
pub fn lay_out(directory: &Path) {
    let jdk = directory.join(JDK);
    std::fs::create_dir_all(jdk.join("bin")).unwrap();
    std::fs::write(jdk.join("release"), "JAVA_VERSION=\"25.0.2\"\n").unwrap();
    for tool in ["javac", "java"] {
        std::fs::write(jdk.join("bin").join(tool), "").unwrap();
        make_executable(&jdk.join("bin").join(tool));
    }
    std::fs::create_dir_all(directory.join(Resolving::CACHE)).unwrap();
}

/// The environment a task runs in, in `directory`.
pub fn environment(command: &mut Command, directory: &Path) {
    command
        .env("JAVA_HOME", directory.join(JDK))
        .env("KOTLIN_SHARED_CACHE_DIR", directory.join(Resolving::CACHE))
        .env("KRUSTY_BIN", recorder())
        .env("KRUSTY_RECORD", directory.join(RECORD))
        .env_remove("KRUSTY_RECORDER_FAIL")
        .env_remove("AMPER_SHARED_CACHE_DIR")
        .env_remove("M2_HOME");
}

fn implementation_directory() -> PathBuf {
    script("kotlin")
        .parent()
        .unwrap()
        .join("../../tools/krusty-build-tools")
}

/// The files the Build Tools API implementation is built and installed from, by path below its
/// directory.
fn implementation_sources() -> Vec<(String, Vec<u8>)> {
    fn walk(directory: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), files);
            } else {
                files.push((name, std::fs::read(entry.path()).unwrap()));
            }
        }
    }
    let root = implementation_directory();
    let mut files = Vec::new();
    walk(&root.join("src/main"), "src/main/", &mut files);
    walk(&root.join("gradle"), "gradle/", &mut files);
    for file in [
        "build.gradle.kts",
        "settings.gradle.kts",
        "gradle.properties",
        "gradlew",
        "install-into-kotlin-cache.sh",
    ] {
        files.push((file.to_string(), std::fs::read(root.join(file)).unwrap()));
    }
    files.sort();
    files
}

/// The Build Tools API implementation jar, built once per process from its sources in a scratch
/// copy.
fn implementation_jar() -> &'static Path {
    static JAR: OnceLock<PathBuf> = OnceLock::new();
    JAR.get_or_init(|| {
        let copy = scratch().join("krusty-build-tools");
        for (file, bytes) in implementation_sources() {
            let path = copy.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let mut command = Command::new("sh");
        command.args(["./gradlew", "-q", "jar"]).current_dir(&copy);
        if let Some(java_home) = std::env::var_os("KRUSTY_BUILD_TOOLS_JAVA_HOME") {
            command.env("JAVA_HOME", java_home);
        }
        let built = command
            .output()
            .expect("run the implementation's Gradle wrapper");
        assert!(
            built.status.success(),
            "the Build Tools API implementation did not build:\n{}{}",
            String::from_utf8_lossy(&built.stdout),
            String::from_utf8_lossy(&built.stderr)
        );
        std::fs::read_dir(copy.join("build/libs"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|extension| extension == "jar"))
            .expect("the build made a jar")
    })
}

/// The toolchain compiling with krusty's Build Tools API implementation and the recorder.
pub struct RecordingKrusty;

impl Compiler for RecordingKrusty {
    fn identity(&self) -> Vec<(String, Vec<u8>)> {
        let mut identity = vec![("recorder".to_string(), RECORDER.as_bytes().to_vec())];
        identity.extend(implementation_sources());
        identity
    }

    fn prepare(&self, directory: &Path, command: &mut Command) {
        lay_out(directory);
        let installed = Command::new("sh")
            .arg(implementation_directory().join("install-into-kotlin-cache.sh"))
            .arg(implementation_jar())
            .arg(directory.join(Resolving::CACHE))
            .output()
            .expect("install the Build Tools API implementation");
        assert!(installed.status.success(), "{installed:?}");
        environment(command, directory);
    }

    fn collect(&self, directory: &Path, ran: Output) -> Output {
        let mut printed = ran.stdout;
        printed.extend_from_slice(&ran.stderr);
        Output {
            root: ran.root,
            code: ran.code,
            stdout: std::fs::read(directory.join(RECORD)).unwrap_or_default(),
            stderr: printed,
        }
    }
}

/// The invocations in a record, each its arguments with the sources (those after `-d <dir>`)
/// sorted: the toolchain passes them in the order it walks their directories.
pub fn invocations(record: &str) -> Vec<Vec<String>> {
    let mut invocations = Vec::new();
    let mut current = Vec::new();
    for line in record.lines() {
        if line == END_OF_INVOCATION {
            if let Some(output) = current.iter().position(|argument| argument == "-d") {
                current[output + 2..].sort();
            }
            invocations.push(std::mem::take(&mut current));
        } else {
            current.push(line.to_string());
        }
    }
    assert!(
        current.is_empty(),
        "a record ends inside an invocation: {record}"
    );
    invocations
}
