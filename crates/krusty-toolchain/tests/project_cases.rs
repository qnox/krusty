//! Project files read by krusty-toolchain report exactly what JetBrains' `kotlin show modules`
//! reported for them: every problem with its file, line, column, severity and message, in order.
//! The cases and their recorded output are in `tests/recorded/projects`; re-record them with
//! `scripts/kotlin-toolchain/record_projects.py`.

mod support;

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{model, show};

use support::{materialize, reported};

#[test]
fn every_recorded_project_case_is_read_as_the_toolchain_reads_it() {
    let cases = support::cases("projects", &["expected", "stdout", "krusty"]);
    assert!(cases.len() >= 29, "the recorded cases are missing");
    let mut failures = Vec::new();
    for case in &cases {
        let name = &case.name;
        let (_temp, root) = materialize("project", case);
        let expected_lines = case.section("expected").cloned().unwrap_or_default();
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let actual = reported(&root, &diagnostics);
        let expected = match case.section("krusty") {
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
                if !rejects(krusty) || !refuses && !rejects(&expected_lines) {
                    failures.push(format!("{name}: krusty-toolchain differs without refusing"));
                }
                krusty
            }
            None => &expected_lines,
        };
        if &actual != expected {
            failures.push(format!(
                "{name}:\n  expected {expected:#?}\n  actual   {actual:#?}"
            ));
        }
        let printed = model.map(|model| show::modules_table(&model.modules));
        let recorded = case
            .section("stdout")
            .filter(|_| case.section("krusty").is_none())
            .map(|lines| {
                lines
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect::<String>()
            });
        if case.section("krusty").is_none() && printed != recorded {
            failures.push(format!(
                "{name}: printed\n{}\nrecorded\n{}",
                printed.unwrap_or_default(),
                recorded.unwrap_or_default()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
