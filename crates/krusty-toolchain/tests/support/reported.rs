//! krusty-toolchain's diagnostics in the form the toolchain's problems are read back in
//! (`rendering.rs`), for the cases that read a project in process.

use std::path::Path;

use krusty_toolchain::diagnostic::{Diagnostics, Severity};
use krusty_toolchain::model::{Problems, Stopped};

use crate::support::{ExpectedDiagnostic, ExpectedSeverity};

/// The problems reading a model reported, the project file's first, and the error it stopped on
/// when that is not a problem in a file: what the toolchain prints for the same project, read back.
pub fn reported_reading(
    root: &Path,
    problems: &Problems,
    stopped: Option<&Stopped>,
) -> Vec<ExpectedDiagnostic> {
    let mut reported = diagnostics(root, &problems.project_file);
    reported.extend(diagnostics(root, &problems.modules));
    match stopped {
        Some(Stopped::NotUnique(message)) => reported.push(ExpectedDiagnostic {
            severity: ExpectedSeverity::Error,
            rendered: format!("ERROR: {}", relative(root, message)),
        }),
        Some(Stopped::Failed(message)) => panic!("reading the project failed: {message}"),
        _ => {}
    }
    reported
}

/// `text` with the files under `root` named relative to it and line breaks written as `\n`.
fn relative(root: &Path, text: &str) -> String {
    text.replace(&format!("{}/", root.display()), "")
        .replace('\n', "\\n")
}

/// krusty-toolchain's problems as the toolchain's are read back: paths relative to `root`, line
/// breaks as `\n`.
fn diagnostics(root: &Path, diagnostics: &Diagnostics) -> Vec<ExpectedDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| ExpectedDiagnostic {
            severity: match diagnostic.severity {
                Severity::Error => ExpectedSeverity::Error,
                Severity::Warning => ExpectedSeverity::Warning,
                Severity::WeakWarning => ExpectedSeverity::WeakWarning,
            },
            rendered: relative(root, &diagnostic.to_string()),
        })
        .collect()
}
