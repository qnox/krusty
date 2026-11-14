//! Every validated master commit publishes its immutable version; only latest and badges are
//! monotonic. Publication retries and latest promotion use separate remote compare-and-swap locks.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

const LATEST_LOCK_REF: &str = "refs/krusty/release-lock";

fn publication_lock(sha: &str) -> String {
    format!("refs/krusty/release-commits/{sha}")
}
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
            .env("RELEASE_LOCK_POLL", "0")
            .env("RELEASE_LOCK_WAIT", "0");
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
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(lock.as_str())
    );

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
    assert_eq!(remote.remote_ref(&publication_lock(&first)), None);

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
fn the_lock_uses_the_source_author_instead_of_the_checkout_or_caller_identity() {
    let remote = Remote::new("source-identity");
    let output = isolated_command("git")
        .args(["commit", "-q", "--allow-empty", "-m", "publication source"])
        .current_dir(remote.root.join("work"))
        .env("GIT_AUTHOR_NAME", "Source Author")
        .env("GIT_AUTHOR_EMAIL", "source@example.invalid")
        .env("GIT_COMMITTER_NAME", "Source Integrator")
        .env("GIT_COMMITTER_EMAIL", "integrator@example.invalid")
        .output()
        .expect("commit the publication source");
    assert!(output.status.success(), "{output:?}");
    let source = remote.git(&["rev-parse", "HEAD"]);
    remote.push_commit("unrelated checkout");

    let lock = remote.acquire(&source);
    assert_eq!(
        remote.git(&["show", "-s", "--format=%an <%ae> / %cn <%ce>", &lock]),
        "Source Author <source@example.invalid> / Source Author <source@example.invalid>"
    );
}

#[test]
fn every_validated_master_ancestor_publishes_even_after_a_newer_release() {
    let remote = Remote::new("superseded");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let record = remote.publisher(&second, &["record"], &[]);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    let check = remote.publisher(&first, &["check"], &[]);
    assert_eq!(check.status.code(), Some(0), "{check:?}");
    assert_eq!(text(check.stdout), "publish=true\n");
    assert_eq!(check.stderr, b"");
    let latest = remote.publisher(&first, &["check-promotion"], &[]);
    assert_eq!(latest.status.code(), Some(0), "{latest:?}");
    assert_eq!(text(latest.stdout), "promote=false\n");
    assert_eq!(latest.stderr, b"");
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
fn a_commit_removed_from_master_cannot_publish_or_promote() {
    let remote = Remote::new("offbranch");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    remote.git(&["reset", "-q", "--hard", &first]);
    remote.git(&["push", "-q", "--force", "origin", "HEAD:refs/heads/master"]);
    let message = format!("release-publisher: {second} is not on master at {first}\n");
    for (command, stdout) in [
        ("check", "publish=false\n"),
        ("check-promotion", "promote=false\n"),
    ] {
        let output = remote.publisher(&second, &[command], &[]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(text(output.stdout), stdout);
        assert_eq!(text(output.stderr), message);
    }
    let record = remote.publisher(&second, &["record"], &[]);
    assert_eq!(record.status.code(), Some(1), "{record:?}");
    assert_eq!(text(record.stderr), message);
    assert_eq!(remote.remote_ref(RELEASED_REF), None);
}

#[test]
fn inconsistent_released_lineage_fails_without_suppressing_the_release_job() {
    let remote = Remote::new("rewritten-lineage");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let record = remote.publisher(&second, &["record"], &[]);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    remote.git(&["reset", "-q", "--hard", &first]);
    remote.git(&["push", "-q", "--force", "origin", "HEAD:refs/heads/master"]);
    for command in ["check", "check-promotion", "record"] {
        let output = remote.publisher(&first, &[command], &[]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(
            text(output.stderr),
            format!(
                "release-publisher: master at {first} no longer descends from released {second}\n"
            )
        );
    }
    assert_eq!(
        remote.remote_ref(RELEASED_REF).as_deref(),
        Some(second.as_str())
    );
}

#[test]
fn unreadable_remote_is_an_error_instead_of_missing_release_evidence() {
    let remote = Remote::new("unreadable");
    let first = remote.push_commit("first");
    remote.git(&["remote", "set-url", "origin", "../missing.git"]);
    for command in ["check", "check-promotion", "record"] {
        let output = remote.publisher(&first, &[command], &[]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(
            text(output.stderr),
            "release-publisher: cannot read refs/heads/master from origin\n"
        );
    }
}

#[test]
fn immutable_tag_retries_accept_only_the_same_commit() {
    let remote = Remote::new("tag-retry");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    for _ in 0..2 {
        let output = remote.publisher(&first, &["bind-tag", "v1"], &[]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"");
    }
    let collision = remote.publisher(&second, &["bind-tag", "v1"], &[]);
    assert_eq!(collision.status.code(), Some(1), "{collision:?}");
    assert_eq!(
        text(collision.stderr),
        format!("release-publisher: refs/tags/v1 belongs to {first}, not {second}\n")
    );
    assert_eq!(
        remote.remote_ref("refs/tags/v1").as_deref(),
        Some(first.as_str())
    );
    for _ in 0..2 {
        let record = remote.publisher(&second, &["record"], &[]);
        assert_eq!(record.status.code(), Some(0), "{record:?}");
        let latest = remote.publisher(&second, &["check-promotion"], &[]);
        assert_eq!(text(latest.stdout), "promote=true\n");
    }
}

// PLATFORM-SPECIFIC: the real local Git receive hook rejects only the immutable tag write.
#[cfg(unix)]
#[test]
fn failed_tag_creation_retains_the_native_reason_and_distinguishes_an_absent_ref() {
    use std::os::unix::fs::PermissionsExt;
    for long_reason in [false, true] {
        let remote = Remote::new(if long_reason {
            "tag-create-bounded"
        } else {
            "tag-create-rejected"
        });
        let sha = remote.push_commit("source");
        let hook = remote.root.join("remote.git/hooks/update");
        let reason = if long_reason {
            "rejected ".repeat(1000)
        } else {
            "intentional server rejection".to_owned()
        };
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf '%s\\n' '{reason}' >&2\nexit 1\n"),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let native = isolated_command("git")
            .args([
                "push",
                "--quiet",
                "--force-with-lease=refs/tags/v1:",
                "origin",
                &format!("{sha}:refs/tags/v1"),
            ])
            .current_dir(remote.root.join("work"))
            .output()
            .unwrap();
        assert_eq!(native.status.code(), Some(1), "{native:?}");
        assert!(native.stdout.is_empty());
        let output = remote.publisher(&sha, &["bind-tag", "v1"], &[]);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output.stdout, b"");
        let mut expected =
            b"release-publisher: git push for refs/tags/v1 failed (exit 1):\n".to_vec();
        expected.extend_from_slice(&native.stderr[..native.stderr.len().min(4096)]);
        expected.extend_from_slice(
            b"\nrelease-publisher: cannot create refs/tags/v1; ref remains absent\n",
        );
        assert_eq!(output.stderr, expected);
        assert_eq!(remote.remote_ref("refs/tags/v1"), None);
    }
}

// PLATFORM-SPECIFIC: a rejected client hook lets a concurrent publisher win before revalidation.
#[cfg(unix)]
#[test]
fn a_failed_tag_push_accepts_only_a_racing_exact_binding() {
    use std::os::unix::fs::PermissionsExt;
    for same_source in [true, false] {
        let remote = Remote::new(if same_source {
            "tag-race-same"
        } else {
            "tag-race-other"
        });
        let first = remote.push_commit("first");
        let second = remote.push_commit("second");
        let raced = if same_source { &second } else { &first };
        let hook = remote.root.join("work/.git/hooks/pre-push");
        fs::write(&hook, format!("#!/bin/sh\ngit --git-dir=../remote.git update-ref refs/tags/v1 {raced}\nprintf 'racing tag creation\\n' >&2\nexit 1\n")).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let native = isolated_command("git")
            .args([
                "push",
                "--quiet",
                "--force-with-lease=refs/tags/v1:",
                "origin",
                &format!("{second}:refs/tags/v1"),
            ])
            .current_dir(remote.root.join("work"))
            .output()
            .unwrap();
        assert_eq!(native.status.code(), Some(1), "{native:?}");
        assert!(native.stdout.is_empty());
        remote.git_in("remote.git", &["update-ref", "-d", "refs/tags/v1"]);
        let output = remote.publisher(&second, &["bind-tag", "v1"], &[]);
        assert_eq!(
            output.status.code(),
            Some(i32::from(!same_source)),
            "{output:?}"
        );
        assert_eq!(output.stdout, b"");
        let mut expected = Vec::new();
        if !same_source {
            expected.extend_from_slice(
                b"release-publisher: git push for refs/tags/v1 failed (exit 1):\n",
            );
            expected.extend_from_slice(&native.stderr);
            expected.extend_from_slice(
                format!("\nrelease-publisher: refs/tags/v1 belongs to {first}, not {second}\n")
                    .as_bytes(),
            );
        }
        assert_eq!(output.stderr, expected);
        assert_eq!(
            remote.remote_ref("refs/tags/v1").as_deref(),
            Some(raced.as_str())
        );
    }
}

#[test]
fn a_held_lock_blocks_another_publisher_until_it_is_dropped() {
    let remote = Remote::new("held");
    let first = remote.push_commit("first");
    let lock_ref = publication_lock(&first);
    let lock = remote.acquire(&first);

    let blocked = remote.publisher(&first, &["acquire"], &[("RELEASE_LOCK_WAIT", "0")]);
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    assert_eq!(blocked.stdout, b"");
    assert_eq!(
        text(blocked.stderr),
        format!(
            "release-publisher: waiting for release lock for {first} (run local)\n\
             release-publisher: gave up waiting for {lock_ref} after 0s\n"
        )
    );
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(lock.as_str())
    );

    let release = remote.publisher(&first, &["release", &lock], &[]);
    assert_eq!(release.status.code(), Some(0), "{release:?}");
    let next = remote.acquire(&first);
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(next.as_str())
    );
}

#[test]
fn a_lock_older_than_its_ttl_is_taken_over_and_its_holder_leaves_it() {
    let remote = Remote::new("stale");
    let first = remote.push_commit("first");
    let lock_ref = publication_lock(&first);
    let stale = remote.acquire(&first);

    let takeover = remote.publisher(&first, &["acquire"], &[("RELEASE_LOCK_TTL", "-1")]);
    assert_eq!(takeover.status.code(), Some(0), "{takeover:?}");
    assert_eq!(
        text(takeover.stderr),
        format!("release-publisher: took over a stale release lock for {first} (run local)\n")
    );
    let lock = text(takeover.stdout).trim().to_owned();
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(lock.as_str())
    );

    let late = remote.publisher(&first, &["release", &stale], &[]);
    assert_eq!(late.status.code(), Some(0), "{late:?}");
    assert_eq!(
        text(late.stderr),
        format!("release-publisher: {lock_ref} is no longer {stale}; leaving it\n")
    );
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(lock.as_str())
    );
}

#[test]
fn different_commits_publish_in_parallel_but_latest_promotion_is_serialized() {
    let remote = Remote::new("lock-scopes");
    let first = remote.push_commit("first");
    let second = remote.push_commit("second");
    let first_lock = remote.acquire(&first);
    let second_lock = remote.acquire(&second);
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(first_lock.as_str())
    );
    assert_eq!(
        remote.remote_ref(&publication_lock(&second)).as_deref(),
        Some(second_lock.as_str())
    );
    let latest = remote.publisher(&second, &["acquire-latest"], &[]);
    assert_eq!(latest.status.code(), Some(0), "{latest:?}");
    let lock = text(latest.stdout).trim().to_owned();
    assert_eq!(
        remote.remote_ref(LATEST_LOCK_REF).as_deref(),
        Some(lock.as_str())
    );
    let blocked = remote.publisher(&first, &["acquire-latest"], &[("RELEASE_LOCK_WAIT", "0")]);
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    let release = remote.publisher(&second, &["release-latest", &lock], &[]);
    assert_eq!(release.status.code(), Some(0), "{release:?}");
    assert_eq!(remote.remote_ref(LATEST_LOCK_REF), None);
    assert_eq!(
        remote.remote_ref(&publication_lock(&first)).as_deref(),
        Some(first_lock.as_str())
    );
    assert_eq!(
        remote.remote_ref(&publication_lock(&second)).as_deref(),
        Some(second_lock.as_str())
    );
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

// PLATFORM-SPECIFIC: execute the Linux release job's checked shell lock acquisitions.
#[cfg(unix)]
#[test]
fn failed_lock_acquisition_stops_before_any_publication_or_promotion() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let remote = Remote::new("failed-lock");
    let work = remote.root.join("work");
    fs::create_dir(work.join("scripts")).unwrap();
    let publisher = work.join("scripts/release-publisher.sh");
    fs::write(&publisher, "#!/usr/bin/env bash\nexit 9\n").unwrap();
    fs::set_permissions(publisher, fs::Permissions::from_mode(0o755)).unwrap();
    let workflow =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"))
            .unwrap();
    let steps = steps(release_job(&workflow));
    for command in ["acquire)", "acquire-latest)"] {
        let step = steps
            .iter()
            .find(|step| step.contains(&format!("release-publisher.sh {command}")))
            .unwrap();
        let source = step
            .split_once("run: |\n")
            .unwrap()
            .1
            .lines()
            .map_while(|line| line.strip_prefix("          "))
            .collect::<Vec<_>>()
            .join("\n");
        let output_file = work.join("outputs");
        let continued = work.join("continued");
        let _ = fs::remove_file(&output_file);
        let _ = fs::remove_file(&continued);
        let mut child = isolated_command("bash")
            .arg("-s")
            .current_dir(&work)
            .env("GITHUB_OUTPUT", &output_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = format!("{source}\nprintf continued > continued\n");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(9), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"");
        assert!(!output_file.exists());
        assert!(!continued.exists());
    }
}

// PLATFORM-SPECIFIC: executable shell mocks exercise the Linux release job's Portal step.
#[cfg(unix)]
#[test]
fn portal_retry_requires_portal_owned_artifacts_not_a_central_proxy_hit() {
    use std::os::unix::fs::PermissionsExt;
    let remote = Remote::new("portal-retry");
    let root = remote.root.join("portal");
    for name in ["scripts", "tools/krusty-gradle", "bin"] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/publish-gradle-plugin-portal.sh"),
        root.join("scripts/publish-gradle-plugin-portal.sh"),
    )
    .unwrap();
    fs::write(root.join("bin/curl"), r#"#!/usr/bin/env bash
set -euo pipefail
url=${!#}
printf '%s\n' "$url" >> "$PORTAL_REQUEST"
for arg in "$@"; do test "$arg" != --location || exit 99; done
if [[ $url == https://plugins-artifacts.gradle.org/* ]]; then
  printf '%s' "$PORTAL_ARTIFACT_STATUS"
else
  while [[ $# -gt 0 ]]; do
    if [[ $1 == --dump-header ]]; then printf 'HTTP/1.1 %s\r\nlOcAtIoN: %s\r\n\r\n' "$PORTAL_STATUS" "$PORTAL_LOCATION" > "$2"; fi
    shift
  done
  printf '%s' "$PORTAL_STATUS"
fi
exit "$PORTAL_CURL_EXIT"
"#).unwrap();
    fs::write(
        root.join("tools/krusty-gradle/gradlew"),
        "#!/usr/bin/env bash\nprintf '%s\n' \"$*\" > \"$PORTAL_UPLOAD\"\n",
    )
    .unwrap();
    for path in [
        root.join("bin/curl"),
        root.join("tools/krusty-gradle/gradlew"),
    ] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let marker = "https://plugins.gradle.org/m2/dev/krustythecompiler/dev.krustythecompiler.gradle.plugin/2.4.21-build.123/dev.krustythecompiler.gradle.plugin-2.4.21-build.123.pom";
    let owned = "https://plugins-artifacts.gradle.org/marker.pom";
    for (status, location, artifact, curl_exit, code, uploads, visits_owned) in [
        ("303", owned, "200", "0", 0, false, true),
        // Central already has this marker; a 200 there must never suppress Portal publication.
        (
            "303",
            "https://repo.maven.apache.org/maven2/marker.pom",
            "200",
            "0",
            0,
            true,
            false,
        ),
        ("404", "", "200", "0", 0, true, false),
        ("303", owned, "404", "0", 0, true, true),
        ("303", owned, "500", "0", 1, false, true),
        (
            "303",
            "https://plugins-artifacts.gradle.org.evil/marker.pom",
            "200",
            "0",
            1,
            false,
            false,
        ),
        (
            "303",
            "https://plugins-artifacts.gradle.org@evil/marker.pom",
            "200",
            "0",
            1,
            false,
            false,
        ),
        ("303", "https://plugins-artifacts.gradle.org:443/marker.pom", "200", "0", 1, false, false),
        ("303", "https://plugins-artifacts.gradle.org/a\r\nLocation: https://repo.maven.apache.org/maven2/b", "200", "0", 1, false, false),
        ("200", "", "200", "0", 1, false, false),
        ("401", "", "200", "0", 1, false, false),
        ("000", "", "200", "7", 7, false, false),
    ] {
        let upload = root.join("uploaded");
        let request = root.join("request");
        let _ = fs::remove_file(&upload);
        let _ = fs::remove_file(&request);
        let output = isolated_command("bash")
            .arg(root.join("scripts/publish-gradle-plugin-portal.sh"))
            .arg("2.4.21-build.123")
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    root.join("bin").display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .env("PORTAL_STATUS", status)
            .env("PORTAL_LOCATION", location)
            .env("PORTAL_ARTIFACT_STATUS", artifact)
            .env("PORTAL_CURL_EXIT", curl_exit)
            .env("PORTAL_REQUEST", &request)
            .env("PORTAL_UPLOAD", &upload)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(code),
            "{status} {location}: {output:?}"
        );
        assert_eq!(upload.exists(), uploads);
        let expected = if visits_owned {
            format!("{marker}\n{owned}\n")
        } else {
            format!("{marker}\n")
        };
        assert_eq!(fs::read_to_string(&request).unwrap(), expected);
        if uploads {
            assert_eq!(fs::read_to_string(upload).unwrap(), format!("--no-daemon -p {}/tools/krusty-gradle -PkrustyPluginVersion=2.4.21-build.123 publishPlugins\n", root.display()));
        }
    }
}

// PLATFORM-SPECIFIC: execute the Linux release step with a local Git remote and shell CLI mock.
#[cfg(unix)]
#[test]
fn public_assets_are_immutable_and_failed_draft_queries_stop_the_retry() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let remote = Remote::new("github-retry");
    let sha = remote.push_commit("first");
    let work = remote.root.join("work");
    for name in ["scripts", "dist", "bin"] {
        fs::create_dir(work.join(name)).unwrap();
    }
    let script = work.join("scripts/release-publisher.sh");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release-publisher.sh"),
        &script,
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/format-release-notes.py"),
        work.join("scripts/format-release-notes.py"),
    )
    .unwrap();
    fs::write(work.join("dist/asset"), "validated artifact").unwrap();
    let gh = work.join("bin/gh");
    fs::write(
        &gh,
        r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_CALLS"
case "$*" in
  'release view v1') test -f "$GH_STATE" ;;
  'release view v1 --json isDraft --jq .isDraft')
    test "$GH_QUERY_FAIL" = 0 || exit 7
    cat "$GH_STATE" ;;
  'api --method POST repos/owner/repo/releases/generate-notes '*)
    printf '%s\n' '{"body":"* Keep @Target by @qnox in https://github.com/owner/repo/pull/1\n"}' ;;
  'release create '*)
    grep -Fqx '* ` Keep @Target ` by @qnox in https://github.com/owner/repo/pull/1' "$RUNNER_TEMP/krusty-release-notes.md"
    printf true > "$GH_STATE" ;;
  'release upload '*) ;;
  'release edit v1 --draft=false --latest=false') printf false > "$GH_STATE" ;;
  *) exit 9 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let workflow =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"))
            .unwrap();
    let publish = steps(release_job(&workflow))
        .into_iter()
        .find(|step| step.contains("gh release create"))
        .unwrap();
    let source = publish
        .split_once("run: |\n")
        .unwrap()
        .1
        .lines()
        .map_while(|line| line.strip_prefix("          "))
        .collect::<Vec<_>>()
        .join("\n");
    let calls = work.join("calls");
    let state = work.join("state");
    for (initial, query_fail, code, expected) in [
        (None, "0", 0, format!("release view v1\napi --method POST repos/owner/repo/releases/generate-notes -f tag_name=v1 -f target_commitish={sha}\nrelease create v1 --target {sha} --draft --latest=false --title krusty 1: Kotlin compiler written in Rust --notes-file {}/krusty-release-notes.md dist/asset\nrelease edit v1 --draft=false --latest=false\n", work.display())),
        (Some("false"), "0", 0, "release view v1\nrelease view v1 --json isDraft --jq .isDraft\n".to_string()),
        (Some("true"), "0", 0, "release view v1\nrelease view v1 --json isDraft --jq .isDraft\nrelease upload v1 --clobber dist/asset\nrelease edit v1 --draft=false --latest=false\n".to_string()),
        (Some("true"), "1", 7, "release view v1\nrelease view v1 --json isDraft --jq .isDraft\n".to_string()),
    ] {
        let _ = fs::remove_file(&calls);
        let _ = fs::remove_file(&state);
        if let Some(initial) = initial { fs::write(&state, initial).unwrap(); }
        let mut child = isolated_command("bash").arg("-s").current_dir(&work)
            .env("PATH", format!("{}:{}", work.join("bin").display(), std::env::var("PATH").unwrap()))
            .env("RELEASE_SHA", &sha).env("GITHUB_SHA", &sha).env("TAG", "v1").env("VER", "1")
            .env("GH_REPO", "owner/repo").env("RUNNER_TEMP", &work)
            .env("GH_CALLS", &calls).env("GH_STATE", &state).env("GH_QUERY_FAIL", query_fail)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(source.as_bytes()).unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert_eq!(output.stdout, b"");
        assert_eq!(output.stderr, b"");
        assert_eq!(fs::read_to_string(&calls).unwrap(), expected);
    }
}

#[test]
fn the_push_token_reaches_git_without_being_persisted() {
    let remote = Remote::new("token");
    let first = remote.push_commit("first");
    let token = [("RELEASE_TOKEN", "release-token-for-tests")];

    let acquire = remote.publisher(&first, &["acquire"], &token);
    assert_eq!(acquire.status.code(), Some(0), "{acquire:?}");
    assert_eq!(acquire.stderr, b"");
    let lock = text(acquire.stdout).trim().to_owned();
    let bind = remote.publisher(&first, &["bind-tag", "v1"], &token);
    assert_eq!(bind.status.code(), Some(0), "{bind:?}");
    assert_eq!(bind.stderr, b"");
    let latest = remote.publisher(&first, &["acquire-latest"], &token);
    assert_eq!(latest.status.code(), Some(0), "{latest:?}");
    assert_eq!(latest.stderr, b"");
    let latest_lock = text(latest.stdout).trim().to_owned();
    let record = remote.publisher(&first, &["record"], &token);
    assert_eq!(record.status.code(), Some(0), "{record:?}");
    assert_eq!(record.stderr, b"");
    let release_latest = remote.publisher(&first, &["release-latest", &latest_lock], &token);
    assert_eq!(release_latest.status.code(), Some(0), "{release_latest:?}");
    assert_eq!(release_latest.stderr, b"");
    let release = remote.publisher(&first, &["release", &lock], &token);
    assert_eq!(release.status.code(), Some(0), "{release:?}");
    assert_eq!(release.stderr, b"");
    assert_eq!(
        remote.remote_ref(RELEASED_REF).as_deref(),
        Some(first.as_str())
    );
    assert_eq!(
        remote.remote_ref("refs/tags/v1").as_deref(),
        Some(first.as_str())
    );
    assert_eq!(remote.remote_ref(&publication_lock(&first)), None);
    assert_eq!(remote.remote_ref(LATEST_LOCK_REF), None);

    let config = fs::read_to_string(remote.root.join("work/.git/config")).expect("read config");
    assert!(
        !config.contains("extraheader") && !config.contains("release-token-for-tests"),
        "the token lives only in the publisher's environment: {config}"
    );
}

#[test]
fn release_job_holds_its_credentials_in_the_release_environment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    let job = release_job(&workflow);
    assert!(
        !workflow[..workflow.len() - job.len()].contains("secrets."),
        "only the release job, which runs in the release environment, reads secrets"
    );
    assert!(
        job.lines().any(|line| line == "    environment: release"),
        "the publishing secrets are deployable to master only"
    );

    let steps = steps(job);
    assert!(
        steps[0].starts_with("uses: actions/checkout@")
            && steps[0].contains("\n          persist-credentials: false\n"),
        "the checkout leaves no token in .git/config for later steps: {}",
        steps[0]
    );
    fn heading(step: &str) -> &str {
        step.lines().next().unwrap().trim()
    }
    let with_token: Vec<&str> = steps
        .iter()
        .filter(|step| step.contains("github.token"))
        .map(|step| heading(step))
        .collect();
    assert_eq!(
        with_token,
        [
            "name: take the commit publication lock",
            "name: publish",
            "name: refresh the Bazel registry",
            "name: take the latest promotion lock",
            "name: record the promoted commit",
            "name: promote the GitHub release",
            "name: drop the latest promotion lock",
            "name: drop the release lock",
        ],
        "only the steps that push or dispatch get the token"
    );
    for step in &steps {
        let pushes = step.contains("release-publisher.sh acquire")
            || step.contains("release-publisher.sh bind-tag")
            || step.contains("release-publisher.sh record")
            || step.contains("release-publisher.sh release");
        let fetches = step.contains("release-publisher.sh check");
        if pushes {
            assert!(
                step.contains("RELEASE_TOKEN: ${{ github.token }}"),
                "a publisher push authenticates only through RELEASE_TOKEN: {step}"
            );
        }
        if fetches && !pushes {
            assert!(
                !step.contains("github.token"),
                "a publisher read of the public repository gets no token: {step}"
            );
        }
    }
}
