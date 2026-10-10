//! `show settings` prints exactly what JetBrains' `kotlin show settings --all-modules` prints for
//! each case, byte for byte, and reports the same problems: every one with its file, line, column,
//! severity and message, in order. The cases are in `tests/cases/settings`; the toolchain's output
//! comes from the cached oracle (`tests/support/oracle.rs`).

#[path = "support/reported.rs"]
mod reported;
mod support;

use std::path::Path;
use std::process::{Command, Output};

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{configuration, model, show};
use reported::reported;
use support::kotlin::{self, Invocation};
use support::rendering::problems;
use support::ExpectedSeverity;

fn run_toolchain(root: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_krusty-toolchain"))
        .arg(format!("--project-dir={}", root.display()))
        .args(arguments)
        .output()
        .expect("run krusty-toolchain")
}

#[test]
fn every_settings_case_is_shown_as_the_toolchain_shows_it() {
    let cases = support::cases("settings");
    assert_eq!(cases.len(), 15, "the cases are missing");
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .map(|case| Invocation {
            case: &case.name,
            files: &case.files,
            repository: None,
            args: &["show", "settings", "--all-modules"],
        })
        .collect();
    let outputs = kotlin::kotlin_all(&invocations);
    let mut failures = Vec::new();
    for (case, toolchain) in cases.iter().zip(outputs) {
        let name = &case.name;
        // Errors go to stderr; warnings go to stdout, before the settings.
        let (errors, unread) = problems(&toolchain.root, &toolchain.stderr);
        assert!(
            unread.is_empty(),
            "{name}: stderr holds more than problems: {}",
            String::from_utf8_lossy(unread)
        );
        let (warnings, result) = problems(&toolchain.root, &toolchain.stdout);
        let temp = support::TempDir::new(&format!("settings-case-{name}"));
        let root = support::materialize(&temp, &case.files);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{name}: {error}"))
            .unwrap_or_else(|| panic!("{name}: the project was not read"));
        let configured = configuration::configure(&root, &model.modules, &mut diagnostics);
        let mut command_stdout = Vec::new();
        let mut command_stderr = Vec::new();
        for diagnostic in diagnostics.iter() {
            let rendered = format!("{diagnostic}\n").into_bytes();
            match diagnostic.severity {
                krusty_toolchain::diagnostic::Severity::Error => command_stderr.extend(rendered),
                krusty_toolchain::diagnostic::Severity::Warning
                | krusty_toolchain::diagnostic::Severity::WeakWarning => {
                    command_stdout.extend(rendered)
                }
            }
        }
        let mut actual_errors = Vec::new();
        let mut actual_warnings = Vec::new();
        for diagnostic in reported(&root, &diagnostics) {
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
        // The command's result, byte for byte: the settings, printed only when nothing was an
        // error.
        let printed = if diagnostics.has_errors() {
            String::new()
        } else {
            show::modules_settings(&model.modules, &configured, |_| true)
        };
        command_stdout.extend(printed.as_bytes());
        if printed.as_bytes() != result {
            failures.push(format!(
                "{name}: printed\n{printed}\nthe toolchain printed\n{}",
                String::from_utf8_lossy(result)
            ));
        }
        let command = run_toolchain(&root, &["show", "settings", "--all-modules"]);
        let expected_code = toolchain.code;
        if command.status.code() != Some(expected_code)
            || command.stdout != command_stdout
            || command.stderr != command_stderr
        {
            failures.push(format!(
                "{name}: command boundary\n  expected status {expected_code}, stdout {command_stdout:?}, stderr {command_stderr:?}\n  actual   status {:?}, stdout {:?}, stderr {:?}",
                command.status.code(), command.stdout, command.stderr
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_command_selects_named_modules_and_requires_a_selection_for_multiple_modules() {
    let case = support::cases("settings")
        .into_iter()
        .find(|case| case.name == "multi-module")
        .expect("the multi-module case exists");
    let temp = support::TempDir::new("settings-command-selection");
    let root = support::materialize(&temp, &case.files);
    let mut diagnostics = Diagnostics::default();
    let model = model::read(model::Start::Discover(&root), &mut diagnostics)
        .expect("read the project")
        .expect("the project exists");
    let configured = configuration::configure(&root, &model.modules, &mut diagnostics);
    assert!(diagnostics.is_empty());

    let selected = run_toolchain(&root, &["show", "settings", "-m", "b"]);
    assert_eq!(selected.status.code(), Some(0));
    assert_eq!(selected.stderr, b"");
    assert_eq!(
        selected.stdout,
        show::modules_settings(&model.modules, &configured, |module| module.name == "b")
            .into_bytes()
    );

    let missing = run_toolchain(&root, &["show", "settings"]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(missing.stdout, b"");
    assert_eq!(
        missing.stderr,
        b"ERROR: Please specify the module(s) to inspect with -m, or use --all-modules to inspect all modules\n"
    );
}
