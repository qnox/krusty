//! Project files read by krusty-toolchain report exactly what JetBrains' `kotlin show modules`
//! reported for them: every problem with its file, line, column, severity and message, in order.
//! The cases and their recorded output are in `tests/recorded/projects`; re-record them with
//! `scripts/kotlin-toolchain/record_projects.py`.

use std::path::{Path, PathBuf};

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{model, show};

struct Case {
    files: Vec<(String, String)>,
    expected: Vec<String>,
    /// What krusty-toolchain reports instead, where it deliberately refuses what the toolchain
    /// accepts or rejects for another reason.
    krusty: Option<Vec<String>>,
    /// The module table `kotlin` printed, when the project was read without errors.
    table: Option<Vec<String>>,
}

fn read_case(path: &Path) -> Case {
    let text = std::fs::read_to_string(path).unwrap();
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    for line in text.lines() {
        match line.strip_prefix("--- ") {
            Some(name) => sections.push((name.to_string(), Vec::new())),
            None => {
                if let Some((_, lines)) = sections.last_mut() {
                    lines.push(line);
                }
            }
        }
    }
    let mut case = Case {
        files: Vec::new(),
        expected: Vec::new(),
        krusty: None,
        table: None,
    };
    for (name, lines) in sections {
        let owned = lines.iter().map(|line| line.to_string()).collect();
        match name.as_str() {
            "expected" => case.expected = owned,
            "stdout" => case.table = Some(owned),
            "krusty" => case.krusty = Some(owned),
            _ => {
                let mut text = lines.join("\n");
                if !lines.is_empty() {
                    text.push('\n');
                }
                case.files.push((name, text));
            }
        }
    }
    case
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn materialize(name: &str, case: &Case) -> (TempDir, PathBuf) {
    let temp = std::env::temp_dir().join(format!(
        "krusty-toolchain-project-case-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&temp);
    // The same directory name the recorder used, since the root module is named after it.
    let root = temp.join("project");
    for (file, text) in &case.files {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let root = root.canonicalize().unwrap();
    (TempDir(temp), root)
}

fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<String> {
    let prefix = format!("{}/", root.display());
    diagnostics
        .iter()
        .map(|diagnostic| {
            diagnostic
                .to_string()
                .replace(&prefix, "")
                .replace('\n', "\\n")
        })
        .collect()
}

#[test]
fn every_recorded_project_case_is_read_as_the_toolchain_reads_it() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/recorded/projects");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "case")
        })
        .collect();
    cases.sort();
    assert!(cases.len() >= 29, "the recorded cases are missing");
    let mut failures = Vec::new();
    for path in &cases {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let case = read_case(path);
        let (_temp, root) = materialize(&name, &case);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let actual = reported(&root, &diagnostics);
        let expected = match &case.krusty {
            Some(krusty) => {
                // krusty-toolchain differs only by refusing, in its own name, or where the
                // toolchain rejects the project too.
                let rejects = |lines: &[String]| {
                    lines
                        .iter()
                        .any(|line| line.contains(" ERROR: ") || line.starts_with("ERROR: "))
                };
                let refuses = krusty
                    .iter()
                    .any(|line| line.contains("ERROR: krusty-toolchain "));
                if !rejects(krusty) || !refuses && !rejects(&case.expected) {
                    failures.push(format!("{name}: krusty-toolchain differs without refusing"));
                }
                krusty
            }
            None => &case.expected,
        };
        if &actual != expected {
            failures.push(format!(
                "{name}:\n  expected {expected:#?}\n  actual   {actual:#?}"
            ));
        }
        let printed = model.map(|model| show::modules_table(&model.modules));
        let recorded = case
            .table
            .as_ref()
            .filter(|_| case.krusty.is_none())
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect::<String>()
            });
        if case.krusty.is_none() && printed != recorded {
            failures.push(format!(
                "{name}: printed\n{}\nrecorded\n{}",
                printed.unwrap_or_default(),
                recorded.unwrap_or_default()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
