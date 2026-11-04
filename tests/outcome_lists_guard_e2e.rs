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
        let dir = std::env::temp_dir().join(format!(
            "krusty-outcome-lists-{name}-{}",
            std::process::id()
        ));
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

const FAILURES: &str = "tests/box_expected_failures/native-klib/2.4.20.txt";
const NOT_APPLICABLE: &str = "tests/box_expected_not_applicable/jvm/2.4.20.txt";
const CLI_FAILURES: &str = "tests/cli_expected_failures/jvm/2.4.20.txt";
const CLI_NOT_APPLICABLE: &str = "tests/cli_expected_not_applicable/jvm/2.4.20.txt";
const KLIB_UNCOVERED: &str = "tests/klib_uncovered_implementations/native/2.4.20.txt";
const MIGRATIONS: &str = "scripts/outcome-list-migrations.tsv";

fn migration_row(repo: &Repo, base: &str) -> String {
    let old = repo.git(&["rev-parse", &format!("{base}:{FAILURES}")]);
    let new = repo.git(&["hash-object", FAILURES]);
    format!("tests/box_expected_failures/native-klib\t2.4.20\t{old}\t{new}\t2459\n")
}

fn added_failure_diagnostic(paths: &[&str]) -> String {
    let mut message = format!("outcome-lists: {FAILURES} gains {} entries:\n", paths.len());
    if paths.len() == 1 {
        message = format!("outcome-lists: {FAILURES} gains 1 entry:\n");
    }
    for path in paths {
        message.push_str(&format!("    {path}\n"));
    }
    message.push_str("outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n");
    message
}

#[test]
fn the_approved_migration_accepts_only_its_exact_new_blob_transition() {
    let repo = Repo::new("approved-migration");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    repo.write(MIGRATIONS, &migration_row(&repo, &base));
    let head = repo.commit();
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        format!("outcome-lists: authorized #2459 migration for {FAILURES}: 1 added entry\n")
    );

    repo.write(FAILURES, "a/one.kt\nb/two.kt\nc/three.kt\n");
    let extra = repo.commit();
    let output = repo.check(&base, &extra);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        added_failure_diagnostic(&["b/two.kt", "c/three.kt"])
    );
}

#[test]
fn migration_scope_and_both_blob_identities_must_match() {
    for (name, column, replacement) in [
        ("lane", 0, "tests/box_expected_failures/wasm-js-klib"),
        ("release", 1, "2.4.21"),
        ("old-blob", 2, "0000000000000000000000000000000000000000"),
        ("new-blob", 3, "0000000000000000000000000000000000000000"),
        ("approval", 4, "9999"),
    ] {
        let repo = Repo::new(&format!("migration-{name}"));
        repo.write(FAILURES, "a/one.kt\n");
        let base = repo.commit();
        repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
        let row = migration_row(&repo, &base);
        let mut fields = row.trim_end().split('\t').collect::<Vec<_>>();
        fields[column] = replacement;
        repo.write(MIGRATIONS, &format!("{}\n", fields.join("\t")));
        let head = repo.commit();
        let output = repo.check(&base, &head);
        assert_eq!(output.status.code(), Some(1), "{name}: {output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            added_failure_diagnostic(&["b/two.kt"])
        );
    }
}

#[test]
fn a_historical_migration_cannot_authorize_replaying_a_regression() {
    let repo = Repo::new("migration-replay");
    repo.write(FAILURES, "a/one.kt\n");
    let original = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    repo.write(MIGRATIONS, &migration_row(&repo, &original));
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let head = repo.commit();
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        added_failure_diagnostic(&["b/two.kt"])
    );
}

#[test]
fn a_spent_approval_cannot_authorize_a_different_blob_transition() {
    let repo = Repo::new("migration-spent-approval");
    repo.write(FAILURES, "a/one.kt\n");
    let original = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let first_row = migration_row(&repo, &original);
    repo.write(MIGRATIONS, &first_row);
    let base = repo.commit();

    repo.write(FAILURES, "a/one.kt\nb/two.kt\nc/three.kt\n");
    let second_row = migration_row(&repo, &base);
    assert_ne!(first_row, second_row);
    repo.write(MIGRATIONS, &format!("{first_row}{second_row}"));
    let head = repo.commit();
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        added_failure_diagnostic(&["c/three.kt"])
    );
}

#[test]
fn a_target_approval_cannot_be_reused_by_a_branch_forked_before_it() {
    let repo = Repo::new("migration-stale-target-approval");
    repo.write(FAILURES, "a/one.kt\n");
    let original = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let first_row = migration_row(&repo, &original);
    repo.write(MIGRATIONS, &first_row);
    let target = repo.commit();

    repo.git(&["checkout", "-q", &original]);
    repo.write(FAILURES, "a/one.kt\nc/three.kt\n");
    let second_row = migration_row(&repo, &original);
    assert_ne!(first_row, second_row);
    repo.write(MIGRATIONS, &format!("{first_row}{second_row}"));
    let head = repo.commit();
    let output = repo.check(&target, &head);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        added_failure_diagnostic(&["c/three.kt"])
    );
}

#[test]
fn historical_migration_rows_cannot_be_removed_or_edited() {
    let repo = Repo::new("migration-history");
    repo.write(FAILURES, "a/one.kt\n");
    let original = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let row = migration_row(&repo, &original);
    repo.write(MIGRATIONS, &row);
    let base = repo.commit();
    for text in [String::new(), row.replace("\t2459\n", "\t9999\n")] {
        repo.write(MIGRATIONS, &text);
        let head = repo.commit();
        let output = repo.check(&base, &head);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(String::from_utf8(output.stderr).unwrap(),
            format!("outcome-lists: approved migration records are append-only; removed rows:\n    {row}"));
    }
}

#[test]
fn an_unreadable_historical_migration_cannot_renew_the_exception() {
    for (kind, path) in [("blob", MIGRATIONS), ("tree", "scripts")] {
        let repo = Repo::new(&format!("migration-unreadable-{kind}"));
        repo.write(FAILURES, "a/one.kt\n");
        let original = repo.commit();
        repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
        let row = migration_row(&repo, &original);
        repo.write(MIGRATIONS, &row);
        repo.write(FAILURES, "a/one.kt\n");
        let base = repo.commit();
        let blob = repo.git(&["rev-parse", &format!("{base}:{path}")]);
        repo.write(MIGRATIONS, &format!("# Preserved record\n{row}"));
        repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
        let head = repo.commit();
        fs::remove_file(
            repo.dir
                .join(".git/objects")
                .join(&blob[..2])
                .join(&blob[2..]),
        )
        .expect("remove the historical blob only in this isolated test repository");
        let output = repo.check(&base, &head);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            format!(
                "outcome-lists: cannot read committed migration records at {base}:{MIGRATIONS}\n"
            )
        );
    }
}

#[test]
fn a_migration_must_cover_each_effective_inherited_release() {
    let repo = Repo::new("migration-inheritance");
    repo.write("kotlin-versions", "2.4.20 a\n2.4.21 b\n");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let row = migration_row(&repo, &base);
    repo.write(MIGRATIONS, &row);
    let head = repo.commit();
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(),
        format!("outcome-lists: authorized #2459 migration for {FAILURES}: 1 added entry\n\
        outcome-lists: tests/box_expected_failures/native-klib/2.4.21.txt (reads {FAILURES}) gains 1 entry:\n    b/two.kt\n\
        outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"));

    repo.write(
        MIGRATIONS,
        &format!("{row}{}", row.replace("\t2.4.20\t", "\t2.4.21\t")),
    );
    let head = repo.commit();
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(String::from_utf8(output.stderr).unwrap(),
        format!("outcome-lists: authorized #2459 migration for {FAILURES}: 1 added entry\n\
        outcome-lists: authorized #2459 migration for tests/box_expected_failures/native-klib/2.4.21.txt (reads {FAILURES}): 1 added entry\n"));
}

#[test]
fn an_uncommitted_migration_does_not_authorize_growth() {
    let repo = Repo::new("migration-uncommitted");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let row = migration_row(&repo, &base);
    let head = repo.commit();
    repo.write(MIGRATIONS, &row);
    let output = repo.check(&base, &head);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        added_failure_diagnostic(&["b/two.kt"])
    );
}

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
        "outcome-lists: tests/box_expected_failures/native-klib/2.4.20.txt gains 1 entry:\n    c/three.kt\n\
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
        "tests/box_expected_failures/native-klib/2.5.0.txt",
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
    repo.write(
        CLI_FAILURES,
        "# header\nhelp/usage.args\nwarnings/newly_broken.args\n",
    );
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
fn a_growing_klib_implementation_backlog_fails() {
    let repo = Repo::new("klib-uncovered");
    repo.write(
        KLIB_UNCOVERED,
        "module=6:stdlib public package=1[6:kotlin] declaration=2[3:Int,4:plus] member=1 mask=0\n",
    );
    let base = repo.commit();
    repo.write(
        KLIB_UNCOVERED,
        "module=6:stdlib public package=1[6:kotlin] declaration=2[3:Int,4:plus] member=1 mask=0\n\
         module=6:stdlib public package=1[6:kotlin] declaration=1[14:newlyUncovered] member=2 mask=0\n",
    );
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/klib_uncovered_implementations/native/2.4.20.txt gains 1 entry:\n    module=6:stdlib public package=1[6:kotlin] declaration=1[14:newlyUncovered] member=2 mask=0\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn the_cli_not_applicable_inventory_is_proven_by_the_reference_run_instead() {
    let repo = Repo::new("cli-not-applicable");
    repo.write(
        CLI_NOT_APPLICABLE,
        "# runtime JDK 8\njdkHome/jdkHome.args\n",
    );
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

const LATER_FAILURES: &str = "tests/box_expected_failures/native-klib/2.4.21.txt";

#[test]
fn dropping_a_release_file_equal_to_the_one_it_inherits_passes() {
    let repo = Repo::new("drop-equal");
    repo.write("kotlin-versions", "2.4.20 a\n2.4.21 b\n");
    repo.write(FAILURES, "# 2.4.20\na/one.kt\n");
    repo.write(LATER_FAILURES, "# 2.4.21\na/one.kt\n");
    let base = repo.commit();
    repo.git(&["rm", "-q", LATER_FAILURES]);
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(output.stderr, b"");
}

#[test]
fn dropping_a_release_file_that_inherits_more_entries_fails() {
    let repo = Repo::new("drop-longer");
    repo.write("kotlin-versions", "2.4.20 a\n2.4.21 b\n");
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    repo.write(LATER_FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.git(&["rm", "-q", LATER_FAILURES]);
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/box_expected_failures/native-klib/2.4.21.txt (reads tests/box_expected_failures/native-klib/2.4.20.txt) gains 1 entry:\n    b/two.kt\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}

#[test]
fn a_release_that_inherits_a_growing_file_is_charged_too() {
    let repo = Repo::new("inherit-growth");
    repo.write("kotlin-versions", "2.4.20 a\n2.4.21 b\n");
    repo.write(FAILURES, "a/one.kt\n");
    let base = repo.commit();
    repo.write(FAILURES, "a/one.kt\nb/two.kt\n");
    let head = repo.commit();

    let output = repo.check(&base, &head);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr is UTF-8"),
        "outcome-lists: tests/box_expected_failures/native-klib/2.4.20.txt gains 1 entry:\n    b/two.kt\n\
         outcome-lists: tests/box_expected_failures/native-klib/2.4.21.txt (reads tests/box_expected_failures/native-klib/2.4.20.txt) gains 1 entry:\n    b/two.kt\n\
         outcome-lists: platform/version expectations only shrink; fix the files above instead of listing them\n"
    );
}
