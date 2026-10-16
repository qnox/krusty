//! Compact flow effects of simple-name assignments and augmented assignments.

use crate::ast::{Expr, ExprId, File};

use super::{ExpressionForm, OriginId, SigExprId, SignatureConstraintExtractor, SignatureScopeId};

impl SignatureConstraintExtractor {
    pub(super) fn simple_name_assignment(
        &mut self,
        file: &File,
        name: &str,
        value: ExprId,
        scope: SignatureScopeId,
        origin: &mut impl FnMut(crate::diag::Span) -> OriginId,
    ) -> Result<SigExprId, ExpressionForm> {
        let effect = self.expression(file, value, scope, origin)?;
        let is_compound = !file.value_operator_spans.contains_key(&value.0)
            && matches!(
                file.expr(value),
                Expr::Binary { lhs, .. }
                    if matches!(file.expr(*lhs), Expr::Name(lhs) if lhs == name)
            );
        if !is_compound {
            self.forget_boolean_vals_depending_on(name);
        }
        Ok(effect)
    }
}
