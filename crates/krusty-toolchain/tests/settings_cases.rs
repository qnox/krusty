//! `show settings` prints exactly what JetBrains' `kotlin show settings --all-modules` prints for
//! each case, byte for byte, and reports the same problems: every one with its file, line, column,
//! severity and message, in order. The cases are in `tests/cases/settings`; the toolchain's output
//! comes from the cached oracle (`tests/support/oracle.rs`).

mod support;

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{configuration, model, show};
use support::kotlin::{self, Invocation};
use support::rendering::problems;
use support::{is_error, reported};

#[test]
fn every_settings_case_is_shown_as_the_toolchain_shows_it() {
    let cases = support::cases("settings");
    assert_eq!(cases.len(), 15, "the cases are missing");
    let invocations: Vec<Invocation<'_>> = cases
        .iter()
        .map(|case| Invocation {
            case: &case.name,
            files: &case.files,
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
        let (actual_errors, actual_warnings): (Vec<String>, Vec<String>) =
            reported(&root, &diagnostics)
                .into_iter()
                .partition(|line| is_error(line));
        if actual_errors != errors || actual_warnings != warnings {
            failures.push(format!(
                "{name}:\n  expected errors {errors:#?}\n  actual   errors {actual_errors:#?}\n  expected warnings {warnings:#?}\n  actual   warnings {actual_warnings:#?}"
            ));
        }
        // The command's result, byte for byte: the settings, printed only when nothing was an
        // error.
        let printed = if diagnostics.has_errors() {
            String::new()
        } else {
            show::modules_settings(&model.modules, &configured, |_| true)
        };
        if printed.as_bytes() != result {
            failures.push(format!(
                "{name}: printed\n{printed}\nthe toolchain printed\n{}",
                String::from_utf8_lossy(result)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
