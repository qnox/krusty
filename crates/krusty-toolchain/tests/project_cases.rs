//! Project files read by krusty-toolchain report exactly what JetBrains' `kotlin show modules`
//! reports for them: every problem with its file, line, column, severity and message, in order,
//! and, when the project is read without errors, the same module table, byte for byte. The cases
//! are in `tests/cases/projects`; the toolchain's output comes from the cached oracle
//! (`tests/support/oracle.rs`).

#[path = "support/command.rs"]
mod command;
#[path = "support/reported.rs"]
mod reported;
mod support;

use krusty_toolchain::{model, show};
use reported::reported_reading;
use support::kotlin::{self, Invocation};
use support::rendering::problems;
use support::{ExpectedDiagnostic, ExpectedSeverity, KrustyExpected};

#[test]
fn every_project_case_is_read_as_the_toolchain_reads_it() {
    let cases = support::cases("projects");
    assert!(cases.len() >= 29, "the cases are missing");
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .map(|case| Invocation {
            case: &case.name,
            files: &case.files,
            repository: None,
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
        let mut problems = model::Problems::default();
        let read = model::read(model::Start::Discover(&root), &mut problems);
        let actual = reported_reading(&root, &problems, read.as_ref().err());
        match &case.krusty {
            Some(krusty) => {
                let expected = match krusty {
                    KrustyExpected::Difference(diagnostics)
                    | KrustyExpected::Refusal(diagnostics) => diagnostics,
                };
                let toolchain_diagnostics: Vec<ExpectedDiagnostic> =
                    errors.iter().chain(&warnings).cloned().collect();
                if &toolchain_diagnostics == expected {
                    failures.push(format!(
                        "{name}: the `krusty` section is what the toolchain reports"
                    ));
                }
                if &actual != expected {
                    failures.push(format!(
                        "{name}:\n  expected {:#?}\n  actual   {actual:#?}",
                        expected
                    ));
                }
            }
            None => {
                let mut actual_errors = Vec::new();
                let mut actual_warnings = Vec::new();
                for diagnostic in actual {
                    match diagnostic.severity {
                        ExpectedSeverity::Error => actual_errors.push(diagnostic.rendered),
                        ExpectedSeverity::Warning | ExpectedSeverity::WeakWarning => {
                            actual_warnings.push(diagnostic.rendered)
                        }
                    }
                }
                let expected_errors: Vec<String> = errors
                    .iter()
                    .map(|diagnostic| diagnostic.rendered.clone())
                    .collect();
                let expected_warnings: Vec<String> = warnings
                    .iter()
                    .map(|diagnostic| diagnostic.rendered.clone())
                    .collect();
                if actual_errors != expected_errors || actual_warnings != expected_warnings {
                    failures.push(format!(
                        "{name}:\n  expected errors {expected_errors:#?}\n  actual   errors {actual_errors:#?}\n  expected warnings {expected_warnings:#?}\n  actual   warnings {actual_warnings:#?}"
                    ));
                }
                // The command's result, byte for byte: the module table, or nothing.
                let printed = read
                    .ok()
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

/// `show modules` run whole does what `kotlin show modules` does: a clean success in each
/// format, a success with a warning before the table, a failure in the project file, a failure in
/// a module file and two modules with one name (`support/command.rs`).
#[test]
fn every_modules_command_does_what_the_toolchain_does() {
    let differences = command::differences(
        "projects",
        &[
            ("plain-paths", &["show", "modules"]),
            ("plain-paths", &["show", "modules", "--format=plain"]),
            ("empty-modules-list", &["show", "modules"]),
            ("module-and-project-errors", &["show", "modules"]),
            ("module-unknown-product", &["show", "modules"]),
            ("duplicate-names", &["show", "modules"]),
        ],
    );
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
