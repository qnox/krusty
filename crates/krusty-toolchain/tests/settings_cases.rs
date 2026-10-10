//! `show settings` prints exactly what JetBrains' `kotlin show settings --all-modules` prints for
//! each case, byte for byte, and reports the same problems: every one with its file, line, column,
//! severity and message, in order. The cases are in `tests/cases/settings`; the toolchain's output
//! comes from the cached oracle (`tests/support/oracle.rs`).

#[path = "support/command.rs"]
mod command;
#[path = "support/reported.rs"]
mod reported;
mod support;

use krusty_toolchain::{model, show};
use reported::reported_reading;
use support::kotlin::{self, Invocation};
use support::rendering::problems;
use support::ExpectedSeverity;

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
        let mut problems = model::Problems::default();
        let read = model::read(model::Start::Discover(&root), &mut problems);
        let mut actual_errors = Vec::new();
        let mut actual_warnings = Vec::new();
        for diagnostic in reported_reading(&root, &problems, read.as_ref().err()) {
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
        // The command's result, byte for byte: the settings, printed only when the model was read.
        let printed = read
            .map(|model| show::modules_settings(&model.modules, &model.configured, |_| true))
            .unwrap_or_default();
        if printed.as_bytes() != result {
            failures.push(format!(
                "{name}: printed\n{printed}\nthe toolchain printed\n{}",
                String::from_utf8_lossy(result)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `show settings` run whole does what `kotlin show settings` does: a clean success with and
/// without naming the only module, a success with warnings before the settings, a failure with
/// errors only, and the selection among several modules (`support/command.rs`).
#[test]
fn every_settings_command_does_what_the_toolchain_does() {
    let differences = command::differences(
        "settings",
        &[
            ("library", &["show", "settings"]),
            ("library", &["show", "settings", "-m", "project"]),
            ("misplaced-settings", &["show", "settings", "--all-modules"]),
            ("wrong-values", &["show", "settings", "--all-modules"]),
            ("multi-module", &["show", "settings", "--all-modules"]),
            ("multi-module", &["show", "settings", "-m", "b"]),
            ("multi-module", &["show", "settings"]),
            ("multi-module", &["show", "settings", "-m", "c"]),
        ],
    );
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
