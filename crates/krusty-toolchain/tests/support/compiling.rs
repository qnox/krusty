//! How a `compileJvm` task runs, for JetBrains' `kotlin` and krusty-toolchain alike, and the
//! compiler arguments it gives krusty.
//!
//! A run is laid out as `kotlin::Resolving` lays it out (`<directory>` in compared output), with a
//! JDK beside the project (`jdk`: a `release` file naming `JAVA_VERSION="25.0.2"`, an empty
//! `bin/javac`, and a `bin/java` that records the KSP runs both make, which is all either reads of
//! it) and the file the recording krusty writes (`record`). `KRUSTY_BIN` names that recorder: it appends its arguments to the record, one
//! per line, then a `----` line, each invocation whole under a lock (a `record.lock` directory),
//! since the toolchain compiles independent modules at the same time; it exits 0, or with status 3
//! for the module `KRUSTY_RECORDER_FAIL` names. Nothing is compiled, so a `compilation-fingerprint`
//! request fails unrecorded and the Build Tools API implementation always compiles.
//!
//! krusty-toolchain runs a recorder of its own ([`toolchain_recorder`]) that records the same way.
//! When `KRUSTY_EVENTS` names a file, it appends `start <module>` and `end <module>` lines to it
//! around its work. When `KRUSTY_RECORDER_SLEEP` is set, a module it does not fail prints
//! `recorder: <module> begins` on stderr, sleeps that many seconds, prints
//! `recorder: <module> ends` on stderr and `recorder: compiled <module>` on stdout. The module
//! `KRUSTY_RECORDER_FAIL_LATE` names instead prints `recorder: failing <module> late` on stderr
//! after its sleep and exits with status 4. `KRUSTY_RECORDER_FAILURE_COMPANION` names a module
//! whose start the failing module waits for, and which waits for the failure's end before continuing.
//! Both waits are bounded; event markers are published after the corresponding event is recorded.
//! It records no `compilation-fingerprint` request: it
//! answers one with the contents of the file named for the module in the directory
//! `KRUSTY_RECORDER_FINGERPRINTS` names, and fails one when there is none.
//!
//! The toolchain compiles through the Build Tools API, whose implementation it loads from its
//! shared cache by Maven coordinates. krusty's implementation (`tools/krusty-build-tools`) is
//! installed there ([`RecordingKrusty`]), so the toolchain hands the recorder the arguments it would
//! hand krusty. The implementation's sources are part of the oracle entry's identity; a live run
//! builds it from them with its Gradle wrapper, in a scratch copy, run by the JDK
//! `KRUSTY_BUILD_TOOLS_JAVA_HOME` names (else `JAVA_HOME`) and the Gradle user home
//! `KRUSTY_BUILD_TOOLS_GRADLE_USER_HOME` names (else its workspace cache). The cached entry's stdout
//! is the record, followed by a `==== outputs` line and the files the task left in the modules'
//! artifact directories ([`artifact_outputs`]), its stderr what `kotlin` printed, its code
//! `kotlin`'s exit status.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::support::kotlin::{version, Compiler, Resolving};
use crate::support::oracle::{scratch, script, Output};

#[cfg(test)]
mod recorder_lock;

pub const JDK: &str = "jdk";
pub const RECORD: &str = "record";
/// The line the recorder writes after each invocation's arguments.
pub const END_OF_INVOCATION: &str = "----";
/// The line an oracle entry's stdout holds between the record and the run's artifact outputs.
pub const OUTPUTS: &str = "==== outputs";

const RECORDER: &str = "#!/bin/sh
# The recorder writes no classes, so it takes no compilation fingerprint: the Build Tools API
# implementation then compiles every time, and only compilations are recorded.
if [ \"${1:-}\" = compilation-fingerprint ]; then exit 1; fi
for argument in \"$@\"; do
  case $argument in -module-name=*) module=${argument#-module-name=} ;; esac
done
until (set -C; : > \"$KRUSTY_RECORD.lock\") 2>/dev/null; do sleep 0.01; done
{
  for argument in \"$@\"; do printf '%s\\n' \"$argument\"; done
  printf '%s\\n' '----'
} >> \"$KRUSTY_RECORD\"
rm \"$KRUSTY_RECORD.lock\"
if [ -n \"${KRUSTY_RECORDER_FAIL:-}\" ] && [ \"$module\" = \"$KRUSTY_RECORDER_FAIL\" ]; then
  echo \"recorder: failing $module\" >&2
  exit 3
fi
";

/// What the recorder that compiles a run's toolchain plugin module adds to [`RECORDER`]: the
/// invocation for the module `KRUSTY_RECORDER_PLUGIN` names, once recorded, is compiled by the
/// Kotlin compiler `KRUSTY_RECORDER_KOTLINC` against the JDK `KRUSTY_RECORDER_JDK` instead of the
/// run's stand-in JDK, so the toolchain finds the plugin's task actions and runs them.
const PLUGIN_COMPILATION: &str = "if [ \"$module\" = \"$KRUSTY_RECORDER_PLUGIN\" ]; then
  for argument do
    shift
    case $argument in
      -jdk-home=*|-Xjdk-release=*|-Xkotlin-reference-version=*) ;;
      *) set -- \"$@\" \"$argument\" ;;
    esac
  done
  JAVA_HOME=$KRUSTY_RECORDER_JDK exec \"$KRUSTY_RECORDER_KOTLINC\" -jdk-home \"$KRUSTY_RECORDER_JDK\" \"$@\"
fi
";

/// The run's stand-in `java`, which the toolchain and krusty-toolchain alike run KSP with: it
/// records its arguments as the recording krusty does, each `@file` argument replaced by the
/// arguments the file holds, one per line. It then acts as a processor that the module's files
/// drive (in `-project-base-dir`): it copies what the module's `ksp-generates` directory holds into
/// KSP's output directory (`-output-base-dir`), as if it generated those files, and when the module
/// holds a `ksp-fails` file, it prints that file on stderr and exits with status 5.
const JAVA_RECORDER: &str = "#!/bin/sh
arguments=$(
  for argument in \"$@\"; do
    case $argument in
      @*) sed -e '$a\\' \"${argument#@}\" ;;
      *) printf '%s\\n' \"$argument\" ;;
    esac
  done
)
until (set -C; : > \"$KRUSTY_RECORD.lock\") 2>/dev/null; do sleep 0.01; done
printf '%s\\n%s\\n' \"$arguments\" '----' >> \"$KRUSTY_RECORD\"
rm \"$KRUSTY_RECORD.lock\"
base=$(printf '%s\\n' \"$arguments\" | sed -n 's/^-project-base-dir=//p')
output=$(printf '%s\\n' \"$arguments\" | sed -n 's/^-output-base-dir=//p')
if [ -d \"$base/ksp-generates\" ]; then
  mkdir -p \"$output\"
  cp -R \"$base/ksp-generates/.\" \"$output\"
fi
if [ -f \"$base/ksp-fails\" ]; then
  cat \"$base/ksp-fails\" >&2
  exit 5
fi
";

const TOOLCHAIN_RECORDER: &str = "#!/bin/sh
for argument in \"$@\"; do
  case $argument in -module-name=*) module=${argument#-module-name=} ;; esac
done
if [ \"${1:-}\" = compilation-fingerprint ]; then
  fingerprint=\"${KRUSTY_RECORDER_FINGERPRINTS:-}/$module\"
  if [ -n \"${KRUSTY_RECORDER_FINGERPRINTS:-}\" ] && [ -f \"$fingerprint\" ]; then exec cat \"$fingerprint\"; fi
  exit 1
fi
event() {
  if [ -n \"${KRUSTY_EVENTS:-}\" ]; then printf '%s %s\\n' \"$1\" \"$module\" >> \"$KRUSTY_EVENTS\"; fi
  if [ -n \"${KRUSTY_RECORDER_FAILURE_COMPANION:-}\" ]; then
    touch \"$KRUSTY_EVENTS.$module.$1\"
  fi
}
wait_for_event() {
  attempts=0
  until [ -f \"$KRUSTY_EVENTS.$1.$2\" ]; do
    if [ \"$attempts\" -ge 3000 ]; then
      echo \"recorder: $module timed out waiting for $2 $1\" >&2
      exit 6
    fi
    attempts=$((attempts + 1))
    sleep 0.01
  done
}
event start
until (set -C; : > \"$KRUSTY_RECORD.lock\") 2>/dev/null; do sleep 0.01; done
{
  for argument in \"$@\"; do printf '%s\\n' \"$argument\"; done
  printf '%s\\n' '----'
} >> \"$KRUSTY_RECORD\"
rm \"$KRUSTY_RECORD.lock\"
if [ -n \"${KRUSTY_RECORDER_FAIL:-}\" ] && [ \"$module\" = \"$KRUSTY_RECORDER_FAIL\" ]; then
  if [ -n \"${KRUSTY_RECORDER_FAILURE_COMPANION:-}\" ]; then
    wait_for_event \"$KRUSTY_RECORDER_FAILURE_COMPANION\" start
  fi
  echo \"recorder: failing $module\" >&2
  event end
  exit 3
fi
if [ \"$module\" = \"${KRUSTY_RECORDER_FAILURE_COMPANION:-}\" ]; then
  wait_for_event \"$KRUSTY_RECORDER_FAIL\" end
fi
if [ -n \"${KRUSTY_RECORDER_SLEEP:-}\" ]; then
  echo \"recorder: $module begins\" >&2
  sleep \"$KRUSTY_RECORDER_SLEEP\"
  echo \"recorder: $module ends\" >&2
  if [ \"$module\" = \"${KRUSTY_RECORDER_FAIL_LATE:-}\" ]; then
    echo \"recorder: failing $module late\" >&2
    event end
    exit 4
  fi
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
            ("krusty", RECORDER.to_string()),
            ("krusty-compiling-plugin", plugin_compiling_recorder()),
            ("krusty-toolchain-krusty", TOOLCHAIN_RECORDER.to_string()),
            ("java", JAVA_RECORDER.to_string()),
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

fn plugin_compiling_recorder() -> String {
    format!("{RECORDER}{PLUGIN_COMPILATION}")
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
    std::fs::write(jdk.join("bin").join("javac"), "").unwrap();
    make_executable(&jdk.join("bin").join("javac"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(recorders().join("java"), jdk.join("bin").join("java")).unwrap();
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
        .env_remove("KRUSTY_RECORDER_FAIL_LATE")
        .env_remove("KRUSTY_RECORDER_FAILURE_COMPANION")
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
/// (`krusty-build-tools-gradle`), or `KRUSTY_BUILD_TOOLS_GRADLE_USER_HOME` when the caller must keep
/// mutable Gradle state outside that cache. It never uses `~/.gradle`: the distribution it unpacks
/// there ships kotlinx.serialization jars, which other tests of the workspace look for below it.
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
        let gradle_user_home = std::env::var_os("KRUSTY_BUILD_TOOLS_GRADLE_USER_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                implementation_directory().join("../../target/cache/krusty-build-tools-gradle")
            });
        command
            .args(["./gradlew", "-q", "jar"])
            .current_dir(&copy)
            .env("GRADLE_USER_HOME", gradle_user_home);
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
        let mut identity = vec![
            ("recorder".to_string(), RECORDER.as_bytes().to_vec()),
            ("outputs".to_string(), OUTPUTS.as_bytes().to_vec()),
            (
                "java recorder".to_string(),
                JAVA_RECORDER.as_bytes().to_vec(),
            ),
        ];
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
        collect(directory, ran, None)
    }
}

/// An oracle entry of a run in `directory`: the record and the artifact outputs, but the module
/// `skipped`'s, on stdout, and what `kotlin` printed on stderr.
fn collect(directory: &Path, ran: Output, skipped: Option<&str>) -> Output {
    let mut printed = ran.stdout;
    printed.extend_from_slice(&ran.stderr);
    let mut stdout = std::fs::read(directory.join(RECORD)).unwrap_or_default();
    stdout.extend_from_slice(format!("{OUTPUTS}\n").as_bytes());
    stdout.extend_from_slice(artifact_outputs(directory, skipped).as_bytes());
    Output {
        root: ran.root,
        code: ran.code,
        stdout,
        stderr: printed,
    }
}

/// The toolchain compiling as with [`RecordingKrusty`], except that the toolchain plugin module
/// `plugin` is really compiled: the toolchain compiles a plugin's module and runs its task actions
/// before it compiles a module that enables it. The plugin module's invocation is recorded with
/// the others, then compiled by the workspace's reference Kotlin compiler (`KRUSTY_KOTLINC`, else
/// the one provisioned in its target directory) against the JDK `JAVA_HOME` names.
///
/// The toolchain runs a task action in a class loader of its own over the plugin's runtime
/// classpath, which it resolves from the plugin module's repositories. A case lists `mavenLocal`
/// there and holds the plugin API's `pom` and a stand-in jar in its local repository; the run's
/// stand-in jars of the plugin API and of the standard library are replaced by real ones: the API
/// the provisioned distribution ships and the reference compiler's standard library.
pub struct RecordingKrustyCompilingPlugin<'a> {
    pub plugin: &'a str,
}

/// Where the plugin API and the standard library lie in a run's local Maven repository.
const PLUGIN_API_JAR: &str =
    "org/jetbrains/amper/amper-extensibility-api/0.13.0/amper-extensibility-api-0.13.0.jar";
const STDLIB_JAR: &str = "org/jetbrains/kotlin/kotlin-stdlib/2.4.20/kotlin-stdlib-2.4.20.jar";

impl Compiler for RecordingKrustyCompilingPlugin<'_> {
    fn identity(&self) -> Vec<(String, Vec<u8>)> {
        let mut identity = vec![
            (
                "recorder".to_string(),
                plugin_compiling_recorder().into_bytes(),
            ),
            ("plugin".to_string(), self.plugin.as_bytes().to_vec()),
            ("outputs".to_string(), OUTPUTS.as_bytes().to_vec()),
        ];
        identity.extend(implementation_sources());
        identity
    }

    fn prepare(&self, directory: &Path, command: &mut Command) {
        RecordingKrusty.prepare(directory, command);
        let workspace = implementation_directory().join("../..");
        let kotlinc = std::env::var_os("KRUSTY_KOTLINC")
            .map(PathBuf::from)
            .into_iter()
            .chain(
                std::fs::read_dir(workspace.join("target/cache/kotlinc"))
                    .into_iter()
                    .flatten()
                    .map(|entry| entry.unwrap().path().join("kotlinc/bin/kotlinc")),
            )
            .find(|kotlinc| kotlinc.is_file())
            .expect("a reference Kotlin compiler (`KRUSTY_KOTLINC`) compiles the plugin module");
        let jdk =
            std::env::var_os("JAVA_HOME").expect("a JDK (`JAVA_HOME`) compiles the plugin module");
        let wrapper = std::fs::read(script("kotlin")).expect("read the Kotlin Toolchain wrapper");
        // The distribution the wrapper provisioned, where it puts it.
        let distribution = std::env::var_os("KOTLIN_CLI_BOOTSTRAP_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/JetBrains/Kotlin/cli")
            })
            .join(format!("kotlin-cli-{}", version(&wrapper)));
        let local = directory.join(Resolving::HOME).join(".m2/repository");
        for (jar, real) in [
            (
                PLUGIN_API_JAR,
                distribution.join("extra/jars/amper-extensibility-api-jvm.jar"),
            ),
            (
                STDLIB_JAR,
                kotlinc.parent().unwrap().join("../lib/kotlin-stdlib.jar"),
            ),
        ] {
            assert!(
                local.join(jar).is_file(),
                "the case's local repository holds a stand-in for {jar}"
            );
            std::fs::copy(&real, local.join(jar))
                .unwrap_or_else(|error| panic!("copy {}: {error}", real.display()));
        }
        command
            .env("KRUSTY_BIN", recorders().join("krusty-compiling-plugin"))
            .env("KRUSTY_RECORDER_PLUGIN", self.plugin)
            .env("KRUSTY_RECORDER_KOTLINC", kotlinc)
            .env("KRUSTY_RECORDER_JDK", jdk);
    }

    fn collect(&self, directory: &Path, ran: Output) -> Output {
        // The toolchain really compiles the plugin module, and krusty-toolchain does not.
        collect(directory, ran, Some(self.plugin))
    }
}

/// The files the task left in the modules' artifact directories in a run's `directory`
/// (`build/artifacts/CompiledJvmArtifact`), in path order: for each, a `== <path>` line, then its
/// text. The artifact directory of the module `skipped` is left out.
pub fn artifact_outputs(directory: &Path, skipped: Option<&str>) -> String {
    fn walk(directory: &Path, prefix: &str, files: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().unwrap().is_dir() {
                walk(&entry.path(), &format!("{name}/"), files);
            } else {
                files.push((name, std::fs::read_to_string(entry.path()).unwrap()));
            }
        }
    }
    let artifacts = directory
        .join(Resolving::PROJECT)
        .join("build/artifacts/CompiledJvmArtifact");
    let skipped = skipped.map(|module| format!("{module}jvm"));
    let mut files = Vec::new();
    for artifact in std::fs::read_dir(&artifacts).into_iter().flatten() {
        let artifact = artifact.unwrap();
        let name = artifact.file_name().to_string_lossy().into_owned();
        if skipped.as_ref() != Some(&name) {
            walk(&artifact.path(), &format!("{name}/"), &mut files);
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|(name, text)| format!("== {name}\n{text}"))
        .collect()
}

/// An oracle entry's stdout read back: the record, then the artifact outputs.
pub fn record_and_outputs(stdout: &str) -> (&str, &str) {
    stdout
        .split_once(&format!("{OUTPUTS}\n"))
        .expect("the oracle entry lists the artifact outputs after the record")
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
