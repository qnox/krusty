//! Context and publication of values checked against an expected expression type.

use super::{Checker, CheckerScope};
use crate::ast::{Expr, ExprId};
use crate::fir::{DeclarationId, ResolvedModuleIndex};
use crate::types::Ty;

/// Inference evidence may be more precise than a declaration's denotable public result. Rechecking
/// its body consumes the former; call sites and declaration ABI continue to consume the latter.
pub(super) fn declaration_body_type(
    index: Option<&ResolvedModuleIndex>,
    declaration: Option<DeclarationId>,
    published: Ty,
) -> Ty {
    declaration
        .and_then(|declaration| index?.signature(declaration))
        .and_then(|signature| signature.inferred_body_result())
        .map_or(published, |result| result.get())
}

impl Checker<'_> {
    pub(super) fn recorded_expression_type_for_expected(
        &mut self,
        scope: &CheckerScope<'_>,
        expression: ExprId,
        nominal: Ty,
        expected: Ty,
    ) -> Ty {
        let contextual = self.expression_type_for_expected(scope, expression, nominal, expected);
        let selected_intersection_projection = contextual != nominal
            && (self
                .expr_access_path(expression)
                .map(|path| self.lookup_intersection_narrowing(scope, &path))
                .is_some_and(|constituents| constituents.contains(&contextual))
                || self
                    .intersection_constituent_for_expected(nominal, contextual)
                    .is_some());
        if selected_intersection_projection {
            // This is a committed value boundary, not candidate probing. Publish the exact
            // constituent selected from the proven non-denotable intersection so checked FIR can
            // carry the cast and every later phase remains purely structural.
            self.selected_value_smartcasts
                .insert(expression, contextual);
        }
        if contextual != nominal
            && !matches!(
                self.file.expr(expression),
                Expr::IntLit(_) | Expr::UIntLit(_)
            )
            && self.integer_constant_provenance(expression).is_some()
        {
            self.selected_numeric_conversions
                .insert(expression, expected);
        }
        contextual
    }
}
