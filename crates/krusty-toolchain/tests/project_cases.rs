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
        let expected_problems = support::rendering::problems(&toolchain.root, &toolchain.output);
        let temp = support::TempDir::new(&format!("project-case-{name}"));
        let root = support::materialize(&temp, &case.files);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let actual = reported(&root, &diagnostics);
        let rejects = |lines: &[String]| {
            lines
                .iter()
                .any(|line| line.contains(" ERROR: ") || line.starts_with("ERROR: "))
        };
        let expected = match &case.krusty {
            Some(krusty) => {
                // krusty-toolchain differs only by refusing, in its own name, or where the
                // toolchain rejects the project too.
                let refuses = krusty
                    .iter()
                    .any(|line| line.contains("ERROR: krusty-toolchain "));
                if !rejects(krusty) || !refuses && !rejects(&expected_problems) {
                    failures.push(format!("{name}: krusty-toolchain differs without refusing"));
                }
                if *krusty == expected_problems {
                    failures.push(format!(
                        "{name}: the `krusty` section is what the toolchain reports"
                    ));
                }
                krusty
            }
            None => &expected_problems,
        };
        if &actual != expected {
            failures.push(format!(
                "{name}:\n  expected {expected:#?}\n  actual   {actual:#?}"
            ));
        }
        if case.krusty.is_none() {
            let printed = model.map(|model| show::modules_table(&model.modules).into_bytes());
            let table = (toolchain.code == 0).then(|| {
                support::rendering::table(&toolchain.output)
                    .unwrap_or_default()
                    .to_vec()
            });
            if printed != table {
                failures.push(format!(
                    "{name}: printed\n{}\nthe toolchain printed\n{}",
                    String::from_utf8_lossy(&printed.unwrap_or_default()),
                    String::from_utf8_lossy(&table.unwrap_or_default())
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
