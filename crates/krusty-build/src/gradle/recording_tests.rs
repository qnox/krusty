//! Complete invocation records while Gradle launches independent compile or support-check tasks.

use super::tests::{clear_invocation_log, invocations, read_invocation_log, write_krusty_proxy};
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct RecordingDirectory(PathBuf);

impl RecordingDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "krusty-gradle-recording-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create recording directory: {error}"),
            }
        }
    }
}

impl Drop for RecordingDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove owned recording directory");
    }
}

#[test]
fn parallel_proxy_calls_keep_complete_compile_and_support_records() {
    let directory = RecordingDirectory::new();
    let proxy = directory.0.join("proxy");
    let actual = directory.0.join("compiler");
    let log = directory.0.join("compiles");
    let probes = directory.0.join("probes");
    let diagnostics = directory.0.join("diagnostics");
    std::fs::write(&actual, "#!/bin/sh\nexit 0\n").unwrap();
    write_krusty_proxy(&proxy, &log, &probes, &diagnostics, &actual);
    for path in [&proxy, &actual] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (support, record) in [(false, &log), (true, &probes)] {
        let payloads = (0..12)
            .map(|index| {
                let mut arguments = Vec::new();
                if support {
                    arguments.push("refusals".to_string());
                }
                arguments.extend([
                    format!("begin-{index}"),
                    format!("{index}:{}", "x".repeat(32768)),
                    format!("end-{index}"),
                ]);
                arguments
            })
            .collect::<Vec<_>>();
        let barrier = std::sync::Barrier::new(payloads.len());
        std::thread::scope(|scope| {
            for arguments in &payloads {
                let (proxy, barrier) = (&proxy, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let output = Command::new(proxy).args(arguments).output().unwrap();
                    assert!(output.status.success(), "{output:?}");
                    assert_eq!(output.stdout, b"");
                    assert_eq!(output.stderr, b"");
                });
            }
        });
        let text = read_invocation_log(record).unwrap();
        let runs = invocations(&text);
        assert_eq!(runs.len(), payloads.len(), "record count");
        // Independent tasks have no prescribed order; every entire argument vector must survive.
        let actual: HashSet<_> = runs.into_iter().collect();
        let expected: HashSet<_> = payloads.into_iter().collect();
        assert_eq!(actual, expected, "complete invocation payloads");
        if !support {
            assert!(!probes.exists(), "no support call was made");
        }
    }
    clear_invocation_log(&log);
    clear_invocation_log(&probes);
    assert!(!log.exists());
    assert!(!probes.exists());
}
