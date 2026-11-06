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

#[test]
fn every_master_run_releases_without_holding_latest_during_gradle() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    assert!(workflow.contains("\nconcurrency:\n  group: ci-${{ github.ref == 'refs/heads/master' && github.run_id || github.ref }}\n  cancel-in-progress: ${{ github.ref != 'refs/heads/master' }}\n"));
    let job = release_job(&workflow);
    assert!(
        !job.contains("concurrency:"),
        "no queued master release is cancelled"
    );
    let steps = steps(job);
    let position = |needle: &str| {
        steps
            .iter()
            .position(|step| step.contains(needle))
            .unwrap_or_else(|| panic!("release job has {needle}"))
    };
    let acquire = position("release-publisher.sh acquire)");
    let check = position("release-publisher.sh check >>");
    let publish = position("gh release create \"$TAG\"");
    let central = position("publish-maven-central.sh");
    let portal = position("publish-gradle-plugin-portal.sh");
    let latest_lock = position("release-publisher.sh acquire-latest");
    let latest = position("release-publisher.sh check-promotion");
    let record = position("release-publisher.sh record");
    let promote = position("gh release edit \"$TAG\" --latest");
    let badges = position("publish badges");
    let latest_drop = position("release-publisher.sh release-latest");
    let drop = position("release-publisher.sh release ");
    assert!(
        acquire < check
            && check < publish
            && publish < central
            && central < portal
            && portal < latest_lock
            && latest_lock < latest
            && latest < record
            && record < promote
            && promote < badges
            && badges < latest_drop
            && latest_drop < drop
    );
    assert_eq!(drop, steps.len() - 1);
    assert!(steps[drop].contains("if: always() && steps.lock.outputs.lock != ''"));
    assert!(steps[latest_drop].contains("if: always() && steps.latest_lock.outputs.lock != ''"));
    for step in &steps[check + 1..latest_lock] {
        assert!(
            step.contains("steps.tip.outputs.publish == 'true'"),
            "{step}"
        );
    }
    for index in [record, promote, badges] {
        assert!(
            steps[index].contains("steps.latest.outputs.promote == 'true'"),
            "{}",
            steps[index]
        );
    }
    assert!(steps[publish].contains("--draft --latest=false"));
    assert!(steps[publish].contains("bind-tag \"$TAG\""));
    assert!(
        steps[publish].contains("draft=$(gh release view \"$TAG\" --json isDraft --jq .isDraft)")
    );
    let timeout_minutes: u64 = job
        .lines()
        .find_map(|line| line.trim().strip_prefix("timeout-minutes: "))
        .expect("release deadline")
        .parse()
        .expect("number");
    let script = fs::read_to_string(root.join("scripts/release-publisher.sh")).expect("publisher");
    let ttl_seconds: u64 = script
        .lines()
        .find_map(|line| line.strip_prefix("ttl=${RELEASE_LOCK_TTL:-"))
        .and_then(|rest| rest.strip_suffix('}'))
        .expect("lock TTL")
        .parse()
        .expect("number");
    assert!(timeout_minutes * 60 < ttl_seconds);
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
  'release create '*) printf true > "$GH_STATE" ;;
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
        (None, "0", 0, format!("release view v1\nrelease create v1 --target {sha} --draft --latest=false --title krusty 1: Kotlin compiler written in Rust --generate-notes dist/asset\nrelease edit v1 --draft=false --latest=false\n")),
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

// This guards only the injection paths the release job itself adds. Gradle, Kotlin and the
// publishing plugin remain trusted third-party build dependencies that see the credentials.
#[test]
fn release_job_adds_no_injection_path_to_secret_bearing_steps() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow = fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("read ci.yml");
    let job = release_job(&workflow);
    assert!(
        !job.contains("gradle/actions/setup-gradle"),
        "setup-gradle injects an init script into the Gradle builds that hold the signing key"
    );
    for step in steps(job)
        .into_iter()
        .filter(|step| step.contains("secrets."))
    {
        assert!(
            !step.contains("<<<"),
            "a here-string can put a secret in a temporary file: {step}"
        );
    }
    let wrapper = fs::read_to_string(
        root.join("tools/krusty-gradle/gradle/wrapper/gradle-wrapper.properties"),
    )
    .expect("read gradle-wrapper.properties");
    let checksum = wrapper
        .lines()
        .find_map(|line| line.strip_prefix("distributionSha256Sum="))
        .expect("the plugin's wrapper pins its Gradle distribution");
    assert!(
        checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "a SHA-256 checksum: {checksum}"
    );
}
