//! JetBrains' `kotlin`, run through the toolchain's own wrapper (`scripts/kotlin-toolchain/kotlin`),
//! which pins the exact version and checksum of the distribution it runs.

use std::process::Command;
use std::sync::Once;

use super::oracle::{run_captured, scratch, script, Fingerprint, Output, Reference};

/// One invocation of `kotlin` on a project.
pub struct Invocation<'a> {
    /// Named in a live run's report and a miss's failure.
    pub case: &'a str,
    /// The project's files, by `/`-separated path below its root.
    pub files: &'a [(String, String)],
    pub args: &'a [&'a str],
    /// The files of the user's local Maven repository, by `/`-separated path below it, for a
    /// command that resolves dependencies; see [`Resolving`].
    pub repository: Option<&'a [(String, String)]>,
}

/// How a command that resolves dependencies runs, for `kotlin` and krusty-toolchain alike: in a
/// directory holding the project (`project`), a home directory whose `.m2/repository` holds the
/// invocation's repository (`home`), and an empty shared cache (`cache`), with every download
/// sent to an unreachable proxy. The output names this directory, not the project.
pub struct Resolving;

impl Resolving {
    pub const PROJECT: &'static str = "project";
    pub const HOME: &'static str = "home";
    pub const CACHE: &'static str = "cache";

    /// Write `repository` as the local Maven repository below `directory`'s home.
    pub fn write_repository(directory: &std::path::Path, repository: &[(String, String)]) {
        let local = directory.join(Self::HOME).join(".m2/repository");
        std::fs::create_dir_all(&local).unwrap();
        for (file, text) in repository {
            let path = local.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }
}

/// The toolchain version the wrapper pins: the cache's slot.
fn version(wrapper: &[u8]) -> String {
    let text = String::from_utf8_lossy(wrapper);
    text.lines()
        .find_map(|line| line.strip_prefix("kotlin_cli_version="))
        .expect("the wrapper pins `kotlin_cli_version`")
        .to_string()
}

/// What `kotlin` does for `invocation`: replayed from the cache, or run when allowed.
pub fn kotlin(invocation: &Invocation<'_>) -> Output {
    let wrapper = std::fs::read(script("kotlin")).expect("read the Kotlin Toolchain wrapper");
    let mut parts: Vec<&[u8]> = vec![b"kotlin", &wrapper];
    for argument in invocation.args {
        parts.push(argument.as_bytes());
    }
    let mut files: Vec<&(String, String)> = invocation.files.iter().collect();
    files.sort();
    for (path, text) in &files {
        parts.push(path.as_bytes());
        parts.push(text.as_bytes());
    }
    let mut repository: Vec<&(String, String)> =
        invocation.repository.into_iter().flatten().collect();
    repository.sort();
    if invocation.repository.is_some() {
        parts.push(b"repository");
    }
    for (path, text) in &repository {
        parts.push(path.as_bytes());
        parts.push(text.as_bytes());
    }
    let reference = Reference {
        slot: format!("kotlin-{}", version(&wrapper)),
        fingerprint: Fingerprint::of(&parts),
        case: invocation.case,
    };
    reference.output(|| {
        provision();
        let directory = scratch();
        let root = directory.join(Resolving::PROJECT);
        for (file, text) in invocation.files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        // The wrapper beside the project, as the toolchain expects it.
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(script("kotlin"), root.join("kotlin")).unwrap();
        // Run by `sh` rather than executed: a copy just written may still be open in a process
        // another thread is starting, and executing it would fail with ETXTBSY.
        let mut command = Command::new("sh");
        command
            .arg("./kotlin")
            .args(invocation.args)
            .current_dir(&root)
            .env("LC_ALL", "C.UTF-8")
            .env("KOTLIN_CLI_NO_WELCOME_BANNER", "1");
        let output = match invocation.repository {
            Some(repository) => {
                Resolving::write_repository(&directory, repository);
                let home = directory.join(Resolving::HOME);
                let options = format!(
                    "{} -Duser.home={} -Dhttps.proxyHost=127.0.0.1 -Dhttps.proxyPort=9 \
                     -Dhttp.proxyHost=127.0.0.1 -Dhttp.proxyPort=9",
                    std::env::var("JAVA_TOOL_OPTIONS").unwrap_or_default(),
                    home.display()
                );
                command
                    .env("JAVA_TOOL_OPTIONS", options)
                    .env("KOTLIN_SHARED_CACHE_DIR", directory.join(Resolving::CACHE))
                    .env_remove("AMPER_SHARED_CACHE_DIR")
                    .env_remove("M2_HOME");
                run_captured(command, &directory)
            }
            None => run_captured(command, &root),
        };
        let _ = std::fs::remove_dir_all(&directory);
        output
    })
}

/// Downloads the distribution the wrapper pins, once per process, before any live run. A run that
/// downloads it prints its progress to stdout, and runs that start while another downloads print
/// that they wait for it; neither belongs in a recorded output.
fn provision() {
    static PROVISIONED: Once = Once::new();
    PROVISIONED.call_once(|| {
        let directory = scratch();
        std::fs::copy(script("kotlin"), directory.join("kotlin")).unwrap();
        let output = Command::new("sh")
            .args(["./kotlin", "--version"])
            .current_dir(&directory)
            .env("KOTLIN_CLI_NO_WELCOME_BANNER", "1")
            .output()
            .expect("run the Kotlin Toolchain wrapper");
        let _ = std::fs::remove_dir_all(&directory);
        assert!(
            output.status.success(),
            "the Kotlin Toolchain wrapper could not provision its distribution:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    });
}

/// [`kotlin`] for every invocation, a few at a time: a live run spends most of its time starting a
/// JVM, so a cold cache is filled in parallel. The outputs are in `invocations`' order.
pub fn kotlin_all(invocations: &[Invocation<'_>]) -> Vec<Output> {
    let workers = std::thread::available_parallelism().map_or(1, |count| count.get().min(8));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut outputs: Vec<(usize, Output)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(invocation) = invocations.get(index) else {
                            return done;
                        };
                        done.push((index, kotlin(invocation)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("an oracle worker panicked"))
            .collect()
    });
    outputs.sort_by_key(|(index, _)| *index);
    outputs.into_iter().map(|(_, output)| output).collect()
}
