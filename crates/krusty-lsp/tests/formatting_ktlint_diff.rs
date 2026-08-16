//! Byte-equality diff of the LSP formatting component against ktlint.
//!
//! Every case under `tests/fixtures/formatting/<name>/` holds an `input.kt` (the "before"
//! source), an optional `.editorconfig`, and an `expected.kt` (the "after" source produced
//! by the official ktlint CLI). Tests never invoke ktlint; fixtures are regenerated with
//! `crates/krusty-lsp/tools/bless-formatting.sh`, which is the only place the ktlint
//! binary runs.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use krusty_lsp::formatting::{format_document, ClientOptions};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/formatting")
}

fn case_dirs() -> Vec<PathBuf> {
    case_dirs_in(&fixtures_dir())
}

fn case_dirs_in(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(dir)
        .expect("formatting fixtures directory must exist")
        .map(|entry| entry.expect("readable fixture entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn run_cases(cases: Vec<PathBuf>) -> String {
    let mut failures = String::new();
    for case in cases {
        let name = case
            .file_name()
            .expect("fixture dir name")
            .to_string_lossy()
            .into_owned();
        let input = fs::read_to_string(case.join("input.kt"))
            .unwrap_or_else(|error| panic!("{name}: unreadable input.kt: {error}"));
        let expected = fs::read_to_string(case.join("expected.kt"))
            .unwrap_or_else(|error| panic!("{name}: unreadable expected.kt: {error}"));
        // Format a temp-dir copy of the case, exactly like bless-formatting.sh runs ktlint
        // in a temp dir: fixture output must depend only on the case's own `.editorconfig`,
        // never on a contributor's local `.idea` or `.editorconfig` above the repo.
        let work = std::env::temp_dir().join(format!("krusty-formatting-fixture-{name}"));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).expect("fixture work dir");
        let document_path = work.join("Input.kt");
        fs::write(&document_path, &input).expect("fixture input copy");
        let case_editorconfig = case.join(".editorconfig");
        if case_editorconfig.exists() {
            let editorconfig = fs::read(&case_editorconfig)
                .unwrap_or_else(|error| panic!("{name}: unreadable .editorconfig: {error}"));
            fs::write(work.join(".editorconfig"), editorconfig).expect("editorconfig copy");
        }
        let actual = format_document(Some(&document_path), &input, &ClientOptions::default());
        match actual {
            Some(actual) if actual == expected => {}
            Some(actual) => {
                failures.push_str(&diff_report(&name, &expected, &actual));
            }
            None => {
                let _ = writeln!(failures, "case {name}: engine declined the input");
            }
        }
    }
    failures
}

fn diff_report(case: &str, expected: &str, actual: &str) -> String {
    let mut report = format!("case {case}:\n");
    let expected_lines: Vec<&str> = expected.lines().collect();
    let actual_lines: Vec<&str> = actual.lines().collect();
    if expected_lines == actual_lines {
        let _ = writeln!(
            report,
            "  lines match; byte difference only (trailing newline or line endings)"
        );
        return report;
    }
    for index in 0..expected_lines.len().max(actual_lines.len()) {
        let before = expected_lines.get(index).copied();
        let after = actual_lines.get(index).copied();
        if before != after {
            let _ = writeln!(
                report,
                "  line {}: expected {:?} got {:?}",
                index + 1,
                before,
                after
            );
        }
    }
    report
}

#[test]
fn formatting_matches_ktlint_fixtures_byte_for_byte() {
    let cases = case_dirs();
    assert!(!cases.is_empty(), "no formatting fixtures found");
    let failures = run_cases(cases);
    assert!(failures.is_empty(), "ktlint byte diffs:\n{failures}");
}

#[test]
fn editorconfig_indent_size_drives_the_engine() {
    // Guard the option plumbing independent of any fixture: a 2-space indent from
    // `.editorconfig` must reach the engine even when the client asks for 4.
    let dir = std::env::temp_dir().join("krusty-formatting-editorconfig-plumbing");
    fs::create_dir_all(&dir).expect("temp dir");
    fs::write(
        dir.join(".editorconfig"),
        "root = true\n\n[*.kt]\nindent_size = 2\n",
    )
    .expect("editorconfig");
    let document = dir.join("src/main/kotlin/Sample.kt");
    fs::create_dir_all(document.parent().expect("document parent")).expect("source directory");
    let formatted = format_document(
        Some(&document),
        "fun f() {\n    call()\n}\n",
        &ClientOptions::default(),
    )
    .expect("engine accepts the input");
    assert_eq!(formatted, "fun f() {\n  call()\n}\n");
}
