//! Preserve the compiler's actual diagnostics before a JVM corpus rejection becomes `None`.

use krusty::diag::{DiagSink, DiagnosticNames};
use krusty::source::SourceKind;

/// The source order and stems are the same ones passed into this compilation's frontend.
/// Render one complete report while stderr is locked by the single print operation, so
/// concurrent corpus workers do not interleave individual diagnostic lines.
pub(super) fn report_rejection<'a>(
    module: &str,
    sources: impl IntoIterator<Item = (SourceKind, &'a str, &'a str)>,
    diagnostics: &DiagSink,
) {
    if !diagnostics.has_errors() {
        return;
    }
    let sources = sources.into_iter().collect::<Vec<_>>();
    let names = sources
        .iter()
        .map(|(kind, stem, _)| {
            let extension = match kind {
                SourceKind::Kotlin => ".kt",
                SourceKind::KotlinScript => ".kts",
                SourceKind::Java => ".java",
            };
            if stem.ends_with(extension) {
                (*stem).to_string()
            } else {
                format!("{stem}{extension}")
            }
        })
        .collect::<Vec<_>>();
    let named = names
        .iter()
        .zip(&sources)
        .map(|(name, (_, _, source))| (name.as_str(), *source))
        .collect::<Vec<_>>();
    eprintln!(
        "box compiler diagnostics [{module}]:\n{}",
        diagnostics.render_all(&named, DiagnosticNames::Hidden)
    );
}
