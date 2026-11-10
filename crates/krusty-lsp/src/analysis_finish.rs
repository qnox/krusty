//! Turn one worker response into per-document analyses.
//!
//! A crash is a finished, failed pass. A timeout or an interrupt is not: the editor keeps the
//! previous diagnostics and the index does not treat the gap as a deletion.

use std::io;

use krusty_lsp::DocumentAnalysis;

pub(crate) fn remains_pending(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}

pub(super) fn finish_analysis(
    analysis_pending: &mut bool,
    result: io::Result<Vec<DocumentAnalysis>>,
    document_count: usize,
) -> Vec<DocumentAnalysis> {
    match result {
        Ok(analysis) => {
            *analysis_pending = false;
            analysis
        }
        Err(error) if remains_pending(error.kind()) => {
            *analysis_pending = true;
            eprintln!("krusty-lsp: {error}; source analysis remains pending");
            Vec::new()
        }
        Err(error) => {
            *analysis_pending = false;
            (0..document_count)
                .map(|_| {
                    DocumentAnalysis::with_diagnostics(vec![krusty::diag::Diagnostic {
                        span: krusty::diag::Span::new(0, 0),
                        editor_span: None,
                        identity: None,
                        name: None,
                        severity: krusty::diag::Severity::Error,
                        kind: krusty::diag::DiagnosticKind::Compiler,
                        msg: format!("analysis worker failed: {error}"),
                        file: 0,
                    }])
                })
                .collect()
        }
    }
}
