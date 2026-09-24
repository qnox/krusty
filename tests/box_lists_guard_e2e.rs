//! `scripts/check-box-lists.sh`: the box outcome manifests may only lose entries between a base and
//! a head revision.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("krusty-box-lists-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create box-lists repository");
        let repo = Self { dir };
        repo.git(&["init", "-q"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
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
            .join("check-box-lists.sh");
        Command::new("bash")
            .arg(script)
            .arg(base)
            .arg(head)
            .current_dir(&self.dir)
            .output()
            .expect("run check-box-lists.sh")
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

const FAILURES: &str = "tests/box_expected_failures/2.4.20.txt";
const NOT_APPLICABLE: &str = "tests/box_expected_not_applicable/2.4.20.txt";

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
        "box-lists: tests/box_expected_failures/2.4.20.txt gains 1 entry:\n    c/three.kt\n\
         box-lists: the box outcome manifests only shrink; fix the files above instead of listing them\n"
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
        "box-lists: tests/box_expected_not_applicable/2.4.20.txt gains 1 entry:\n    jvm/now_skipped.kt\n\
         box-lists: the box outcome manifests only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn a_manifest_for_a_newly_supported_version_is_exempt() {
    let repo = Repo::new("new-version");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(
        "tests/box_expected_failures/2.5.0.txt",
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
