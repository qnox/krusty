use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn release_reaps_a_blocked_child_within_one_deadline() {
    let before = direct_children();
    for _ in 0..3 {
        let started = Instant::now();
        let grace = Duration::from_secs(1);
        let mut child = Command::new(env!("CARGO_BIN_EXE_reap-child"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn reap-child");
        let pid = child.id();
        let stdout = child.stdout.take().expect("stdout");
        krusty_lsp::release_worker_child(child, stdout, grace).expect("reap");
        let elapsed = started.elapsed();
        assert!(
            elapsed < grace + Duration::from_millis(500),
            "reap took {elapsed:?}, past one deadline"
        );
        assert_reaped(pid);
    }
    assert_eq!(
        direct_children(),
        before,
        "repeated reap left worker processes behind"
    );
}

fn assert_reaped(pid: u32) {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => {}
        Ok(stat) => panic!("worker {pid} still present after release: {stat}"),
    }
}

fn direct_children() -> Vec<u32> {
    let pid = std::process::id();
    let path = format!("/proc/{pid}/task/{pid}/children");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}
