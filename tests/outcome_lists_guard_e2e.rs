//! `scripts/check-outcome-lists.sh`: each platform/version expectation may only lose entries between
//! a base and a head revision.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

/// Git exports repository-local variables to hooks. A test repository created by a test that runs
/// under `pre-push` must not inherit those variables, or `git init` mutates the real repository
/// instead of the temporary one. Ask Git for the complete local-variable set rather than keeping a
/// second hand-written list here.
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
    command
}

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("krusty-outcome-lists-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create outcome-lists repository");
        let repo = Self { dir };
        repo.git(&["init", "-q"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let output = isolated_command("git")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?} failed: {output:?}");
        String::from_utf8(output.stdout)
            .expect("git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn write(&self, manifest: &str, text: &str) {
        let path = self.dir.join(manifest);
        fs::create_dir_all(path.parent().expect("manifest has a directory"))
            .expect("create manifest directory");
        fs::write(path, text).expect("write manifest");
    }

    fn commit(&self) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "manifests"]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn check(&self, base: &str, head: &str) -> Output {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("scripts")
            .join("check-outcome-lists.sh");
        isolated_command("bash")
            .arg(script)
            .arg(base)
            .arg(head)
            .current_dir(&self.dir)
            .output()
            .expect("run check-outcome-lists.sh")
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const FAILURES: &str = "tests/box_expected_failures/native/2.4.20.txt";
const NOT_APPLICABLE: &str = "tests/box_expected_not_applicable/jvm/2.4.20.txt";
const CLI_FAILURES: &str = "tests/cli_expected_failures/jvm/2.4.20.txt";
const CLI_NOT_APPLICABLE: &str = "tests/cli_expected_not_applicable/jvm/2.4.20.txt";

#[test]
fn removing_entries_passes() {
    let repo = Repo::new("remove");
    repo.write(FAILURES, "# header\na/one.kt\nb/two.kt\n");
    repo.write(NOT_APPLICABLE, "js/only.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "# header, reworded\nb/two.kt\n");
    repo.write(NOT_APPLICABLE, "");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stderr, b"");
}

#[test]
fn replacing_an_entry_fails_and_names_the_added_one() {
    let repo = Repo::new("replace");
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nc/three.kt\n");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/box_expected_failures/native/2.4.20.txt gains 1 entry:\n    c/three.kt\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn a_growing_not_applicable_manifest_fails() {
    let repo = Repo::new("not-applicable");
    repo.write(NOT_APPLICABLE, "js/only.kt\n");
    let base = repo.commit();
    repo.write(NOT_APPLICABLE, "js/only.kt\njvm/now_skipped.kt\n");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/box_expected_not_applicable/jvm/2.4.20.txt gains 1 entry:\n    jvm/now_skipped.kt\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn a_manifest_for_a_newly_supported_version_is_exempt() {
    let repo = Repo::new("new-version");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(
        "tests/box_expected_failures/native/2.5.0.txt",
        "a/one.kt\nnew/file.kt\n",
    );
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn a_branch_behind_a_base_that_since_dropped_entries_passes() {
    let repo = Repo::new("stale-branch");
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let fork = repo.commit();
    repo.write(FAILURES, "b/two.kt\n");
    let base = repo.commit();
    repo.git(&["checkout", "-q", &fork]);
    repo.write("unrelated.txt", "change\n");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn a_growing_cli_expected_failure_manifest_fails() {
    let repo = Repo::new("cli-failures");
    repo.write(CLI_FAILURES, "# header\nhelp/usage.args\n");
    let base = repo.commit();
    repo.write(CLI_FAILURES, "# header\nhelp/usage.args\nwarnings/newly_broken.args\n");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/cli_expected_failures/jvm/2.4.20.txt gains 1 entry:\n    warnings/newly_broken.args\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn the_cli_not_applicable_inventory_is_proven_by_the_reference_run_instead() {
    let repo = Repo::new("cli-not-applicable");
    repo.write(CLI_NOT_APPLICABLE, "# runtime JDK 8\njdkHome/jdkHome.args\n");
    let base = repo.commit();
    repo.write(
        CLI_NOT_APPLICABLE,
        "# runtime JDK 8\njdkHome/jdkHome.args\n# .env\nreadingConfigFromEnvironment/simple.args\n",
    );
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stderr, b"");
}
