//! krusty-toolchain's diagnostics in the form the toolchain's problems are read back in
//! (`rendering.rs`).

use std::path::Path;

use krusty_toolchain::diagnostic::Diagnostics;

use crate::rendering::Reported;

/// krusty-toolchain's problems as the toolchain's are read back: each one's severity, and its
/// rendering with paths relative to `root` and line breaks as `\n`.
pub fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<Reported> {
    let prefix = format!("{}/", root.display());
    diagnostics
        .iter()
        .map(|diagnostic| Reported {
            severity: diagnostic.severity,
            text: diagnostic
                .to_string()
                .replace(&prefix, "")
                .replace('\n', "\\n"),
        })
        .collect()
}
