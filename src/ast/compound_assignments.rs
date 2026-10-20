//! Source provenance for the read synthesized by a compound assignment.

use super::{Expr, ExprId, File};
use crate::diag::Span;

impl File {
    /// Clone a compound-assignment target as its synthetic read and retain the exact source-name
    /// span selected by that read. Qualified targets use their terminal member segment; a missing
    /// member span is a parser invariant failure, never a reconstructed spelling fallback.
    pub(crate) fn add_compound_assignment_read(
        &mut self,
        original: ExprId,
        synthetic_span: Span,
    ) -> ExprId {
        let expression = self.expr(original).clone();
        let source_span = match &expression {
            Expr::Name(_) => self.expr_spans[original.0 as usize],
            Expr::Member { .. } => *self
                .exact_member_name_spans
                .get(&original.0)
                .expect("parsed member assignment target has a terminal segment span"),
            _ => unreachable!("compound assignment read must be a name or member"),
        };
        let read = self.add_expr(expression, synthetic_span);
        self.compound_assignment_read_spans
            .insert(read.0, source_span);
        read
    }
}
