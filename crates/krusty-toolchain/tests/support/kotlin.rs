//! JetBrains' `kotlin`, run through the toolchain's own wrapper (`scripts/kotlin-toolchain/kotlin`),
//! which pins the exact version and checksum of the distribution it runs.

use std::process::Command;

use super::oracle::{run_merged, scratch, script, Fingerprint, Output, Reference};

/// One invocation of `kotlin` on a project.
pub struct Invocation<'a> {
    /// Named in a live run's report and a miss's failure.
    pub case: &'a str,
    /// The project's files, by `/`-separated path below its root.
    pub files: &'a [(String, String)],
    pub args: &'a [&'a str],
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
    let reference = Reference {
        slot: format!("kotlin-{}", version(&wrapper)),
        fingerprint: Fingerprint::of(&parts),
        case: invocation.case,
    };
    reference.output(|| {
        let directory = scratch();
        let root = directory.join("project");
        for (file, text) in invocation.files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        // The wrapper beside the project, as the toolchain expects it.
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(script("kotlin"), root.join("kotlin")).unwrap();
        let mut command = Command::new("./kotlin");
        command
            .args(invocation.args)
            .current_dir(&root)
            .env("LC_ALL", "C.UTF-8")
            .env("KOTLIN_CLI_NO_WELCOME_BANNER", "1");
        let output = run_merged(command, &root);
        let _ = std::fs::remove_dir_all(&directory);
        output
    })
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
