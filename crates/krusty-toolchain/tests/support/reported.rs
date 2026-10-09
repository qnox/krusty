//! krusty-toolchain's diagnostics in the form the toolchain's problems are read back in
//! (`rendering.rs`), for the cases that read a project in process.

use std::path::Path;

use krusty_toolchain::diagnostic::{Diagnostics, Severity};

use crate::support::{ExpectedDiagnostic, ExpectedSeverity};

/// krusty-toolchain's problems as the toolchain's are read back: paths relative to `root`, line
/// breaks as `\n`.
pub fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<ExpectedDiagnostic> {
    let prefix = format!("{}/", root.display());
    diagnostics
        .iter()
        .map(|diagnostic| ExpectedDiagnostic {
            severity: match diagnostic.severity {
                Severity::Error => ExpectedSeverity::Error,
                Severity::Warning => ExpectedSeverity::Warning,
                Severity::WeakWarning => ExpectedSeverity::WeakWarning,
            },
            rendered: diagnostic
                .to_string()
                .replace(&prefix, "")
                .replace('\n', "\\n"),
        })
        .collect()
}
