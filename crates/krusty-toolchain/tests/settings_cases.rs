//! `show settings` prints exactly what JetBrains' `kotlin show settings --all-modules` printed for
//! each recorded project, and reports the same problems: every one with its file, line, column,
//! severity and message, in order. The cases and their recorded output are in
//! `tests/recorded/settings`; re-record them with
//! `scripts/kotlin-toolchain/record_projects.py --settings`.

mod support;

use krusty_toolchain::diagnostic::Diagnostics;
use krusty_toolchain::{configuration, model, show};

use support::{materialize, reported};

#[test]
fn every_recorded_settings_case_is_shown_as_the_toolchain_shows_it() {
    let cases = support::cases("settings", &["expected", "settings"]);
    assert_eq!(cases.len(), 13, "the recorded cases are missing");
    let mut failures = Vec::new();
    for case in &cases {
        let (_temp, root) = materialize("settings", case);
        let mut diagnostics = Diagnostics::default();
        let model = model::read(model::Start::Discover(&root), &mut diagnostics)
            .unwrap_or_else(|error| panic!("{}: {error}", case.name))
            .unwrap_or_else(|| panic!("{}: the project was not read", case.name));
        let configured = configuration::configure(&root, &model.modules, &mut diagnostics);
        let actual = reported(&root, &diagnostics);
        let expected = case.section("expected").cloned().unwrap_or_default();
        if actual != expected {
            failures.push(format!(
                "{}:\n  expected {expected:#?}\n  actual   {actual:#?}",
                case.name
            ));
        }
        // The settings are printed only when nothing was an error.
        let printed = (!diagnostics.has_errors()).then(|| {
            show::modules_settings(&model.modules, &configured, |_| true)
                .lines()
                .map(|line| line.trim_end().to_string())
                .collect::<Vec<_>>()
        });
        if printed.as_ref() != case.section("settings") {
            failures.push(format!(
                "{}: printed\n{}\nrecorded\n{}",
                case.name,
                printed.unwrap_or_default().join("\n"),
                case.section("settings")
                    .cloned()
                    .unwrap_or_default()
                    .join("\n")
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
