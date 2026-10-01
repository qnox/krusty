//! `scripts/release-publisher.sh` and the ci workflow's use of it: each ref keeps only its newest
//! workflow run, and release publication never moves the release or badges back to an older commit.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const LOCK_REF: &str = "refs/krusty/release-lock";
const RELEASED_REF: &str = "refs/krusty/released";

/// Git exports repository-local variables to hooks; a test run under `pre-push` must not inherit
/// them, or its `git init` mutates the real repository. User and system configuration stay out
/// too, so a machine's push settings cannot change what the script prints.
fn isolated_command(program: &str) -> Command {
    static LOCAL_GIT_ENVIRONMENT: OnceLock<Vec<String>> = OnceLock::new();
    let local_environment = LOCAL_GIT_ENVIRONMENT.get_or_init(|| {
        let output = Command::new("git")
            .args(["rev-parse", "--local-env-vars"])
            .output()
            .expect("list repository-local Git environment variables");
        assert!(
            output.status.success(),
            "git rev-parse --local-env-vars failed: {output:?}"
        );
        String::from_utf8(output.stdout)
            .expect("Git environment variable names are UTF-8")
            .lines()
            .map(str::to_owned)
            .collect()
    });
    let mut command = Command::new(program);
    for variable in local_environment {
        command.env_remove(variable);
    }
    for variable in ["GITHUB_SHA", "GITHUB_RUN_ID"] {
        command.env_remove(variable);
    }
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");
    command
}

/// A bare remote standing in for GitHub and a clone of it standing in for the release job.
struct Remote {
    root: PathBuf,
}

impl Remote {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "krusty-release-publisher-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create release-publisher test directory");
        let remote = Self { root };
        remote.git_in(".", &["init", "-q", "--bare", "remote.git"]);
        remote.git_in(".", &["clone", "-q", "remote.git", "work"]);
        remote.git(&["checkout", "-q", "-b", "master"]);
        remote
    }

    fn git_in(&self, dir: &str, args: &[&str]) -> String {
        let output = isolated_command("git")
            .args(args)
            .current_dir(self.root.join(dir))
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?} failed: {output:?}");
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.git_in("work", args)
    }

    /// Commits on the clone's master and pushes it; returns the commit.
    fn push_commit(&self, message: &str) -> String {
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["push", "-q", "--force", "origin", "HEAD:refs/heads/master"]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn remote_ref(&self, name: &str) -> Option<String> {
        let output = isolated_command("git")
            .args(["rev-parse", "--verify", "--quiet", name])
            .current_dir(self.root.join("remote.git"))
            .output()
            .expect("run git rev-parse");
        output.status.success().then(|| {
            String::from_utf8(output.stdout)
                .expect("git output is UTF-8")
                .trim()
                .to_owned()
        })
    }

    fn publisher(&self, sha: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("release-publisher.sh");
        let mut command = isolated_command("bash");
        command
            .arg(script)
            .args(args)
            .current_dir(self.root.join("work"))
            .env("RELEASE_SHA", sha)
            .env("RELEASE_LOCK_POLL", "0");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("run release-publisher.sh")
    }

    fn acquire(&self, sha: &str) -> String {
        let output = self.publisher(sha, &["acquire"], &[]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stderr, b"");
        text(output.stdout).trim().to_owned()
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).expect("output is UTF-8")
}

#[test]
fn the_tip_publishes_records_itself_and_drops_the_lock() {
    let remote = Remote::new("tip");
    let first = remote.push_commit("first");

    let lock = remote.acquire(&first);
    assert_eq!(remote.remote_ref(LOCK_REF).as_deref(), Some(lock.as_str()));

    let check = remote.publisher(&first, &["check"], &[]);
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(text(check.stdout), "publish=true\n");
    assert_eq!(check.stderr, b"");

    let record = remote.publisher(&first, &["record"], &[]);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    assert_eq!(
        remote.remote_ref(RELEASED_REF).as_deref(),
        Some(first.as_str())
    );

    let release = remote.publisher(&first, &["release", &lock], &[]);
    assert_eq!(release.status.code(), Some(0), "{release:?}");
    assert_eq!(release.stderr, b"");
    assert_eq!(remote.remote_ref(LOCK_REF), None);

    let second = remote.push_commit("second");
    let check = remote.publisher(&second, &["check"], &[]);
    assert_eq!(text(check.stdout), "publish=true\n");
    let record = remote.publisher(&second, &["record"], &[]);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    assert_eq!(
        remote.remote_ref(RELEASED_REF).as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn a_commit_master_has_moved_past_does_not_publish() {
    let remote = Remote::new("superseded");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");

    let check = remote.publisher(&first, &["check"], &[]);

    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(text(check.stdout), "publish=false\n");
    assert_eq!(
        text(check.stderr),
        format!("release-publisher: master is at {second}, not {first}; a newer run publishes\n")
    );
}

#[test]
fn a_commit_older_than_the_last_release_neither_publishes_nor_records() {
    let remote = Remote::new("older");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let record = remote.publisher(&second, &["record"], &[]);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    remote.git(&["reset", "-q", "--hard", &first]);
    remote.git(&["push", "-q", "--force", "origin", "HEAD:refs/heads/master"]);

    let check = remote.publisher(&first, &["check"], &[]);
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(text(check.stdout), "publish=false\n");
    assert_eq!(
        text(check.stderr),
        format!("release-publisher: {second} was published and {first} does not descend from it\n")
    );

    let record = remote.publisher(&first, &["record"], &[]);
    assert_eq!(record.status.code(), Some(1), "{record:?}");
    assert_eq!(
        text(record.stderr),
        format!("release-publisher: refusing to record {first} over newer {second}\n")
    );
    assert_eq!(
        remote.remote_ref(RELEASED_REF).as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn a_held_lock_blocks_another_publisher_until_it_is_dropped() {
    let remote = Remote::new("held");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let lock = remote.acquire(&first);

    let blocked = remote.publisher(&second, &["acquire"], &[("RELEASE_LOCK_WAIT", "0")]);
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    assert_eq!(blocked.stdout, b"");
    assert_eq!(
        text(blocked.stderr),
        format!(
            "release-publisher: waiting for release lock for {first} (run local)\n\
             release-publisher: gave up waiting for {LOCK_REF} after 0s\n"
        )
    );
    assert_eq!(remote.remote_ref(LOCK_REF).as_deref(), Some(lock.as_str()));

    let release = remote.publisher(&first, &["release", &lock], &[]);
    assert_eq!(release.status.code(), Some(0), "{release:?}");
    let next = remote.acquire(&second);
    assert_eq!(remote.remote_ref(LOCK_REF).as_deref(), Some(next.as_str()));
}

#[test]
fn a_lock_older_than_its_ttl_is_taken_over_and_its_holder_leaves_it() {
    let remote = Remote::new("stale");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let stale = remote.acquire(&first);

    let takeover = remote.publisher(&second, &["acquire"], &[("RELEASE_LOCK_TTL", "-1")]);
    assert_eq!(takeover.status.code(), Some(0), "{takeover:?}");
    assert_eq!(
        text(takeover.stderr),
        format!("release-publisher: took over a stale release lock for {first} (run local)\n")
    );
    let lock = text(takeover.stdout).trim().to_owned();
    assert_eq!(remote.remote_ref(LOCK_REF).as_deref(), Some(lock.as_str()));

    let late = remote.publisher(&first, &["release", &stale], &[]);
    assert_eq!(late.status.code(), Some(0), "{late:?}");
    assert_eq!(
        text(late.stderr),
        format!("release-publisher: {LOCK_REF} is no longer {stale}; leaving it\n")
    );
    assert_eq!(remote.remote_ref(LOCK_REF).as_deref(), Some(lock.as_str()));
}

/// The ci workflow's `release` job, from its header to the end of the file.
fn release_job(workflow: &str) -> &str {
    let start = workflow
        .find("\n  release:\n")
        .expect("ci.yml has a release job");
    &workflow[start..]
}

fn steps(job: &str) -> Vec<&str> {
    job.split("\n      - ").skip(1).collect()
}

#[test]
fn non_master_refs_keep_only_their_latest_run_and_master_runs_finish_under_the_lock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");

    assert!(
        workflow.contains(
            "\nconcurrency:\n  group: ci-${{ github.ref }}\n  cancel-in-progress: ${{ github.ref != 'refs/heads/master' }}\n"
        ),
        "a newer run must cancel a superseded non-master ref, while every master run must finish"
    );

    let job = release_job(&workflow);
    assert!(
        !job.contains("concurrency:"),
        "a concurrency group on the release job cancels a queued release and its run"
    );

    let steps = steps(job);
    let position = |needle: &str| {
        steps
            .iter()
            .position(|step| step.contains(needle))
            .unwrap_or_else(|| panic!("release job has a step running `{needle}`"))
    };
    let acquire = position("release-publisher.sh acquire");
    let check = position("release-publisher.sh check");
    let publish = position("softprops/action-gh-release");
    let record = position("release-publisher.sh record");
    let drop = position("release-publisher.sh release");
    assert!(acquire < check && check < publish && publish < record && record < drop);
    assert_eq!(drop, steps.len() - 1, "dropping the lock is the last step");
    assert!(
        steps[drop].contains("if: always() && steps.lock.outputs.lock != ''"),
        "the lock is dropped even when publishing fails: {}",
        steps[drop]
    );
    for step in &steps[check + 1..drop] {
        assert!(
            step.contains("steps.tip.outputs.publish == 'true'"),
            "every step after the check writes only when the commit may publish: {step}"
        );
    }

    let timeout_minutes: u64 = job
        .lines()
        .find_map(|line| line.trim().strip_prefix("timeout-minutes: "))
        .expect("the release job has a timeout")
        .parse()
        .expect("the timeout is a number");
    let script = fs::read_to_string(root.join("scripts/release-publisher.sh"))
        .expect("read release-publisher.sh");
    let ttl_seconds: u64 = script
        .lines()
        .find_map(|line| line.strip_prefix("ttl=${RELEASE_LOCK_TTL:-"))
        .and_then(|rest| rest.strip_suffix('}'))
        .expect("release-publisher.sh has a default lock TTL")
        .parse()
        .expect("the TTL is a number");
    assert!(
        timeout_minutes * 60 < ttl_seconds,
        "a lock is taken over only after its job's timeout has killed it"
    );
}
