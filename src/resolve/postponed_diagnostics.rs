use crate::ast::ExprId;
use crate::diag::{DiagSink, Diagnostic, Severity, Span};

/// Diagnostics from an open lambda-input probe that are not authoritative until the enclosing
/// statement either closes the input and rechecks the expression or finishes without doing so.
#[derive(Default)]
pub(super) struct PostponedDiagnostics {
    entries: Vec<(ExprId, Vec<Diagnostic>)>,
    captured: std::collections::HashSet<ExprId>,
}

impl PostponedDiagnostics {
    pub(super) fn was_captured(&self, key: ExprId) -> bool {
        self.captured.contains(&key)
    }

    pub(super) fn capture_since(&mut self, key: ExprId, sink: &mut DiagSink, mark: usize) {
        let diagnostics = sink.diags.split_off(mark);
        if diagnostics.is_empty() {
            return;
        }
        if diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
        {
            self.captured.insert(key);
        }
        if self.entries.iter().any(|(candidate, _)| *candidate == key) {
            return;
        }
        self.entries.push((key, diagnostics));
    }

    pub(super) fn discard(&mut self, key: ExprId) {
        if let Some(index) = self
            .entries
            .iter()
            .position(|(candidate, _)| *candidate == key)
        {
            self.entries.remove(index);
        }
    }

    pub(super) fn contains(&self, sink: &DiagSink, file: u32, span: Span, message: &str) -> bool {
        sink.diags
            .iter()
            .chain(self.entries.iter().flat_map(|(_, diagnostics)| diagnostics))
            .any(|diagnostic| {
                diagnostic.file == file && diagnostic.span == span && diagnostic.msg == message
            })
    }

    pub(super) fn contains_error_in_span(&self, file: u32, span: Span) -> bool {
        self.entries
            .iter()
            .flat_map(|(_, diagnostics)| diagnostics)
            .any(|diagnostic| {
                diagnostic.severity == Severity::Error
                    && diagnostic.file == file
                    && diagnostic.span.lo >= span.lo
                    && diagnostic.span.hi <= span.hi
            })
    }

    pub(super) fn commit(&mut self, sink: &mut DiagSink) {
        for (_, diagnostics) in self.entries.drain(..) {
            sink.diags.extend(diagnostics);
        }
    }
}
