//! Complete compiler rejection reports shared by corpus and in-process e2e compilation.

use krusty::diag::{DiagSink, DiagnosticNames};
use krusty::source::SourceKind;

pub(crate) fn report_rejection<'a>(
    module: &str,
    sources: impl IntoIterator<Item = (SourceKind, &'a str, &'a str)>,
    diagnostics: &DiagSink,
) {
    if let Some(report) = render_rejection(module, sources, diagnostics) {
        // One locked stderr write keeps different corpus workers' complete reports together.
        eprintln!("{report}");
    }
}

/// The complete report [`report_rejection`] prints, for a caller that prints it later.
pub(crate) fn render_rejection<'a>(
    module: &str,
    sources: impl IntoIterator<Item = (SourceKind, &'a str, &'a str)>,
    diagnostics: &DiagSink,
) -> Option<String> {
    if !diagnostics.has_errors() {
        return None;
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
    let thread = std::thread::current();
    let test = thread.name().unwrap_or("unnamed worker");
    Some(format!(
        "compiler diagnostics [{module}] test={test}:\n{}",
        diagnostics.render_all(&named, DiagnosticNames::Hidden)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::diag::{DiagSink, Span};
    use krusty::source::SourceKind;

    fn header(module: &str) -> String {
        format!(
            "compiler diagnostics [{module}] test={}:\n",
            std::thread::current().name().unwrap_or("unnamed worker")
        )
    }

    #[test]
    fn rejection_keeps_every_diagnostic_in_order_with_its_own_source_kind() {
        let mut diagnostics = DiagSink::new();
        diagnostics.set_file(2);
        diagnostics.error(Span::new(3, 4), "third-file error");
        diagnostics.set_file(0);
        diagnostics.warning(Span::new(1, 2), "first-file warning");
        diagnostics.set_file(1);
        diagnostics.error(Span::new(0, 1), "second-file error");
        let report = render_rejection(
            "sample",
            [
                (SourceKind::Java, "Helper", "class H {}"),
                (SourceKind::Kotlin, "Main.kt", "fun f() {}"),
                (SourceKind::KotlinScript, "script", "ab\ncd"),
            ],
            &diagnostics,
        );
        assert_eq!(
            report,
            Some(format!(
                "{}script.kts:2:1: error: third-file error\ncd\n^\nHelper.java:1:2: warning: first-file warning\nclass H {{}}\n ^\nMain.kt:1:1: error: second-file error\nfun f() {{}}\n^\n",
                header("sample")
            ))
        );
    }

    #[test]
    fn warnings_do_not_become_compiler_rejections() {
        let mut diagnostics = DiagSink::new();
        diagnostics.warning(Span::new(0, 1), "a warning");
        assert_eq!(
            render_rejection("main", [(SourceKind::Kotlin, "Main", "x")], &diagnostics),
            None
        );
    }

    #[test]
    fn an_unlocated_module_error_is_not_lost() {
        let mut diagnostics = DiagSink::new();
        diagnostics.module_error("the selected declaration has no annotation facts");
        assert_eq!(
            render_rejection("main", [], &diagnostics),
            Some(format!(
                "{}error: the selected declaration has no annotation facts\n",
                header("main")
            ))
        );
    }
}
