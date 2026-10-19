//! How a `compileJvm` task runs, for JetBrains' `kotlin` and krusty-toolchain alike, and the
//! compiler arguments it gives krusty.
//!
//! A run is laid out as `kotlin::Resolving` lays it out (`<directory>` in compared output), with a
//! JDK beside the project (`jdk`: a `release` file naming `JAVA_VERSION="25.0.2"` and empty
//! `bin/javac` and `bin/java`, which is all either reads of it) and the file the recording krusty
//! writes (`record`). `KRUSTY_BIN` names that recorder: it appends its arguments to the record, one
//! per line, then a `----` line, and exits 0, or with status 3 for the module
//! `KRUSTY_RECORDER_FAIL` names. Nothing is compiled, so a `compilation-fingerprint` request fails
//! unrecorded and the Build Tools API implementation always compiles.
//!
//! krusty-toolchain runs a recorder of its own ([`toolchain_recorder`]), since it runs modules
//! together: it appends each invocation whole, under a lock, and when `KRUSTY_EVENTS` names a file
//! it appends `start <module>` and `end <module>` lines to it around its work. When
//! `KRUSTY_RECORDER_SLEEP` is set, a module it does not fail prints `recorder: <module> begins` on
//! stderr, sleeps that many seconds, prints `recorder: <module> ends` on stderr and
//! `recorder: compiled <module>` on stdout.
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
# The recorder writes no classes, so it takes no compilation fingerprint: the Build Tools API
# implementation then compiles every time, and only compilations are recorded.
if [ \"${1:-}\" = compilation-fingerprint ]; then exit 1; fi
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

const TOOLCHAIN_RECORDER: &str = "#!/bin/sh
for argument in \"$@\"; do
  case $argument in -module-name=*) module=${argument#-module-name=} ;; esac
done
event() {
  if [ -n \"${KRUSTY_EVENTS:-}\" ]; then printf '%s %s\\n' \"$1\" \"$module\" >> \"$KRUSTY_EVENTS\"; fi
}
event start
until mkdir \"$KRUSTY_RECORD.lock\" 2>/dev/null; do sleep 0.01; done
{
  for argument in \"$@\"; do printf '%s\\n' \"$argument\"; done
  printf '%s\\n' '----'
} >> \"$KRUSTY_RECORD\"
rmdir \"$KRUSTY_RECORD.lock\"
if [ -n \"${KRUSTY_RECORDER_FAIL:-}\" ] && [ \"$module\" = \"$KRUSTY_RECORDER_FAIL\" ]; then
  echo \"recorder: failing $module\" >&2
  event end
  exit 3
fi
if [ -n \"${KRUSTY_RECORDER_SLEEP:-}\" ]; then
  echo \"recorder: $module begins\" >&2
  sleep \"$KRUSTY_RECORDER_SLEEP\"
  echo \"recorder: $module ends\" >&2
  echo \"recorder: compiled $module\"
fi
event end
";

/// The directory of the recording krustys, both written once per process before any test of the
/// binary starts a process: a script written while another thread starts a process could not be
/// executed (`ETXTBSY`).
fn recorders() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let directory =
            std::env::temp_dir().join(format!("krusty-toolchain-recorder-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for (name, script) in [
            ("krusty", RECORDER),
            ("krusty-toolchain-krusty", TOOLCHAIN_RECORDER),
        ] {
            let path = directory.join(name);
            std::fs::write(&path, script).unwrap();
            make_executable(&path);
        }
        directory
    })
}

/// The recording krusty the toolchain runs.
pub fn recorder() -> PathBuf {
    recorders().join("krusty")
}

/// The recording krusty krusty-toolchain runs.
pub fn toolchain_recorder() -> PathBuf {
    recorders().join("krusty-toolchain-krusty")
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

/// The environment a task runs in, in `directory`, with the recording krusty `krusty`.
pub fn environment(command: &mut Command, directory: &Path, krusty: &Path) {
    command
        .env("JAVA_HOME", directory.join(JDK))
        .env("KOTLIN_SHARED_CACHE_DIR", directory.join(Resolving::CACHE))
        .env("KRUSTY_BIN", krusty)
        .env("KRUSTY_RECORD", directory.join(RECORD))
        .env_remove("KRUSTY_RECORDER_FAIL")
        .env_remove("KRUSTY_RECORDER_SLEEP")
        .env_remove("KRUSTY_EVENTS")
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
/// copy. The Gradle wrapper runs with a user home of its own in the workspace's target cache
/// (`krusty-build-tools-gradle`), never `~/.gradle`: the distribution it unpacks there ships
/// kotlinx.serialization jars, which other tests of the workspace look for below `~/.gradle`.
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
        command
            .args(["./gradlew", "-q", "jar"])
            .current_dir(&copy)
            .env(
                "GRADLE_USER_HOME",
                implementation_directory().join("../../target/cache/krusty-build-tools-gradle"),
            );
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
        environment(command, directory, &recorder());
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
