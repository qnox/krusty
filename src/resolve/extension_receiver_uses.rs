//! Which extension receivers a body uses: the expressions and statements that read each extension
//! declaration's receiver, implicitly or through its label. Capture discovery and unused-receiver
//! diagnostics count these uses by the receiver's declaration span.

use super::*;

impl Checker<'_> {
    pub(super) fn mark_extension_receiver_used(
        &mut self,
        expression: ExprId,
        receiver: ImplicitReceiver,
    ) {
        if !self.suppress_receiver_capture_accounting {
            self.implicit_receiver_identity_uses
                .record(receiver.identity);
        }
        if let Some(span) = receiver.extension_receiver {
            self.mark_extension_receiver_span_used(expression, span);
        }
    }

    pub(super) fn mark_extension_receiver_stmt_used(
        &mut self,
        statement: StmtId,
        receiver: ImplicitReceiver,
    ) {
        self.implicit_receiver_identity_uses
            .record(receiver.identity);
        if let Some(span) = receiver.extension_receiver {
            self.mark_extension_receiver_stmt_span_used(statement, span);
        }
    }

    pub(super) fn mark_extension_receiver_span_used(&mut self, expression: ExprId, span: Span) {
        let uses = &mut self.extension_receiver_expr_uses[expression.0 as usize];
        if !uses.contains(&span) {
            uses.push(span);
        }
        self.implicit_receiver_identity_uses.record_extension(span);
    }

    pub(super) fn mark_extension_receiver_stmt_span_used(&mut self, statement: StmtId, span: Span) {
        let uses = &mut self.extension_receiver_stmt_uses[statement.0 as usize];
        if !uses.contains(&span) {
            uses.push(span);
        }
        self.implicit_receiver_identity_uses.record_extension(span);
    }

    pub(super) fn extension_receiver_use_count(&self, declaration: Span) -> usize {
        self.extension_receiver_expr_uses
            .iter()
            .filter(|uses| uses.contains(&declaration))
            .count()
            + self
                .extension_receiver_stmt_uses
                .iter()
                .filter(|uses| uses.contains(&declaration))
                .count()
    }

    pub(super) fn mark_extension_receiver_label_used(
        &mut self,
        expression: ExprId,
        label_index: usize,
    ) {
        if let Some(span) = self
            .extension_receiver_labels
            .iter()
            .rev()
            .find_map(|(index, span)| (*index == label_index).then_some(*span))
        {
            self.mark_extension_receiver_span_used(expression, span);
        }
    }

    pub(super) fn mark_current_extension_receiver_used(&mut self, expression: ExprId) {
        if let Some(span) = self.this_extension_receiver {
            self.mark_extension_receiver_span_used(expression, span);
        }
    }
}
