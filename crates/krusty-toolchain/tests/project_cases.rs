//! Project files read by krusty-toolchain report exactly what JetBrains' `kotlin show modules`
//! reports for them: every problem with its file, line, column, severity and message, in order,
//! and, when the project is read without errors, the same module table, byte for byte. The cases
//! are in `tests/cases/projects`; the toolchain's output comes from the cached oracle
//! (`tests/support/oracle.rs`).

mod support;

use std::path::Path;

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{model, show};
use support::kotlin::{self, Invocation};
use support::rendering::problems;

fn is_error(line: &str) -> bool {
    line.contains(": ERROR: ") || line.starts_with("ERROR: ")
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
fn every_project_case_is_read_as_the_toolchain_reads_it() {
    let cases = support::cases("projects");
    assert!(cases.len() >= 29, "the cases are missing");
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .map(|case| Invocation {
            case: &case.name,
            files: &case.files,
            args: &["show", "modules"],
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let mut failures = Vec::new();
    for (case, toolchain) in cases.iter().zip(outputs) {
        let name = &case.name;
        // Errors go to stderr; warnings go to stdout, before the module table.
        let (errors, unread) = problems(&toolchain.root, &toolchain.stderr);
        assert!(
            unread.is_empty(),
            "{name}: stderr holds more than problems: {}",
            String::from_utf8_lossy(unread)
        );
        let (warnings, result) = problems(&toolchain.root, &toolchain.stdout);
        let temp = support::TempDir::new(&format!("project-case-{name}"));
        let root = support::materialize(&temp, &case.files);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let actual = reported(&root, &diagnostics);
        match &case.krusty {
            Some(krusty) => {
                // krusty-toolchain differs only by refusing, in its own name, or where the
                // toolchain rejects the project too.
                let refuses = krusty
                    .iter()
                    .any(|line| line.contains("ERROR: krusty-toolchain "));
                if !krusty.iter().any(|line| is_error(line)) || !refuses && errors.is_empty() {
                    failures.push(format!("{name}: krusty-toolchain differs without refusing"));
                }
                if errors.iter().chain(&warnings).eq(krusty.iter()) {
                    failures.push(format!(
                        "{name}: the `krusty` section is what the toolchain reports"
                    ));
                }
                if &actual != krusty {
                    failures.push(format!(
                        "{name}:\n  expected {krusty:#?}\n  actual   {actual:#?}"
                    ));
                }
            }
            None => {
                let (actual_errors, actual_warnings): (Vec<String>, Vec<String>) =
                    actual.into_iter().partition(|line| is_error(line));
                if actual_errors != errors || actual_warnings != warnings {
                    failures.push(format!(
                        "{name}:\n  expected errors {errors:#?}\n  actual   errors {actual_errors:#?}\n  expected warnings {warnings:#?}\n  actual   warnings {actual_warnings:#?}"
                    ));
                }
                // The command's result, byte for byte: the module table, or nothing.
                let printed = model
                    .map(|model| show::modules_table(&model.modules).into_bytes())
                    .unwrap_or_default();
                if printed != result {
                    failures.push(format!(
                        "{name}: printed\n{}\nthe toolchain printed\n{}",
                        String::from_utf8_lossy(&printed),
                        String::from_utf8_lossy(result)
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
