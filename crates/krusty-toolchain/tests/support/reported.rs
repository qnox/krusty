//! krusty-toolchain's diagnostics in the form the toolchain's problems are read back in
//! (`rendering.rs`).

use std::path::Path;

use krusty_toolchain::diagnostic::Diagnostics;

/// krusty-toolchain's problems as the toolchain's are read back: paths relative to `root`, line
/// breaks as `\n`.
pub fn reported(root: &Path, diagnostics: &Diagnostics) -> Vec<String> {
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

/// Whether a problem, as [`reported`] writes it, is an error.
pub fn is_error(line: &str) -> bool {
    line.contains(": ERROR: ") || line.starts_with("ERROR: ")
}
