//! Recorder exclusivity when a directory utility reports success after a racing EEXIST.

use crate::support::TempDir;
use std::path::Path;
use std::process::Command;

#[test]
fn recorder_lock_excludes_a_second_writer_when_mkdir_reports_a_racing_success() {
    for recorder in [
        super::recorder(),
        super::toolchain_recorder(),
        super::recorders().join("java"),
    ] {
        assert_exclusive(&recorder);
    }
}

fn assert_exclusive(recorder: &Path) {
    let temp = TempDir::new("recorder-claim-race");
    let tools = temp.0.join("tools");
    let barrier = temp.0.join("barrier");
    std::fs::create_dir(&tools).unwrap();
    std::fs::create_dir(&barrier).unwrap();
    let records = super::InvocationRecords::new(&temp.0);
    records.create().unwrap();
    let record = records.path();
    let resolved = Command::new("sh")
        .args(["-c", "command -v mkdir"])
        .output()
        .unwrap();
    assert!(resolved.status.success(), "{resolved:?}");
    let real_mkdir = String::from_utf8(resolved.stdout).unwrap();
    let mkdir = tools.join("mkdir");
    // Both callers observe absence before creation, then both finish the create attempt before
    // either returns. This models uutils/coreutils#13970's is_dir success guard after EEXIST.
    std::fs::write(
        &mkdir,
        r#"#!/bin/sh
wait_for_pair() {
  attempts=0
  until [ -f "$KRUSTY_CLAIM_RACE/$1.left" ] && [ -f "$KRUSTY_CLAIM_RACE/$1.right" ]; do
    if [ "$attempts" -ge 1000 ]; then exit 9; fi
    attempts=$((attempts + 1))
    sleep 0.01
  done
}
if [ -e "$1" ]; then exit 1; fi
: > "$KRUSTY_CLAIM_RACE/before.$KRUSTY_CLAIM_ACTOR"
wait_for_pair before
"$KRUSTY_REAL_MKDIR" "$1" 2>/dev/null
if [ ! -d "$1" ]; then exit 9; fi
: > "$KRUSTY_CLAIM_RACE/after.$KRUSTY_CLAIM_ACTOR"
wait_for_pair after
exit 0
"#,
    )
    .unwrap();
    super::make_executable(&mkdir);
    let path = std::env::join_paths(
        std::iter::once(tools).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let payload = "x".repeat(32768);
    let project = format!("-project-base-dir={}", temp.0.display());
    let run = |actor: &str| {
        Command::new(recorder)
            .args([
                format!("-module-name={actor}"),
                project.clone(),
                payload.clone(),
            ])
            .env("PATH", &path)
            .env("KRUSTY_RECORD", &record)
            .env("KRUSTY_CLAIM_RACE", &barrier)
            .env("KRUSTY_CLAIM_ACTOR", actor)
            .env("KRUSTY_REAL_MKDIR", real_mkdir.trim_end())
            .env_remove("KRUSTY_RECORDER_FAIL")
            .env_remove("KRUSTY_RECORDER_SLEEP")
            .env_remove("KRUSTY_RECORDER_FAIL_LATE")
            .env_remove("KRUSTY_RECORDER_FAILURE_COMPANION")
            .env_remove("KRUSTY_EVENTS")
            .output()
            .unwrap()
    };
    let outputs = std::thread::scope(|scope| {
        let left = scope.spawn(|| run("left"));
        let right = scope.spawn(|| run("right"));
        [left.join().unwrap(), right.join().unwrap()]
    });
    for output in outputs {
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"");
    }
    let record = String::from_utf8(records.read_snapshot().unwrap()).unwrap();
    let mut actual = record
        .split_inclusive("----\n")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut expected = ["left", "right"]
        .map(|actor| format!("-module-name={actor}\n{project}\n{payload}\n----\n"));
    // Independent writers may acquire the lock in either order; compare complete payloads.
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}
