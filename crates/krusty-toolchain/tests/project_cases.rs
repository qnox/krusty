//! Project files read by krusty-toolchain report exactly what JetBrains' `kotlin show modules`
//! reports for them: every problem with its file, line, column, severity and message, in order,
//! and, when the project is read without errors, the same module table, byte for byte. The cases
//! are in `tests/cases/projects`; the toolchain's output comes from the cached oracle
//! (`tests/support/oracle.rs`).

mod support;

use std::path::Path;
use std::process::{Command, Output};

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{model, show};
use support::kotlin::{self, Invocation};
use support::rendering::problems;
use support::{reported, ExpectedDiagnostic, ExpectedSeverity, KrustyExpected};

fn run_toolchain(root: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"))
        .arg(format!("--project-dir={}", root.display()))
        .args(["show", "modules"])
        .output()
        .expect("run krusty-toolchain")
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

#[test]
fn the_command_routes_warnings_before_its_result_on_stdout() {
    let temp = support::TempDir::new("command-warning-stream");
    let root = support::materialize(
        &temp,
        &[("project.yaml".to_string(), "modules: []\n".to_string())],
    );

    let output = run_toolchain(&root);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stderr, b"");
    assert_eq!(
        output.stdout,
        format!(
            "{}:1:1: WARNING: Project has no modules: no root module file and no modules listed in the project file\n{}",
            root.join("project.yaml").display(),
            show::modules_table(&[])
        )
        .into_bytes()
    );
}

#[test]
fn the_command_routes_errors_to_stderr_and_fails_without_a_result() {
    let temp = support::TempDir::new("command-error-stream");
    let root = support::materialize(
        &temp,
        &[
            (
                "project.yaml".to_string(),
                "modules: [a]\nunknown: value\n".to_string(),
            ),
            (
                "a/module.yaml".to_string(),
                "product: jvm/lib\n".to_string(),
            ),
        ],
    );

    let output = run_toolchain(&root);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(
        output.stderr,
        format!(
            "{}:2:1: ERROR: Unknown property `unknown`\n",
            root.join("project.yaml").display()
        )
        .into_bytes()
    );
}
