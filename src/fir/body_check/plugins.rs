//! Checked handoff for frontend-plugin expression plans.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn plugin_expression(
        &mut self,
        expression: ExprId,
        plan: crate::plugins::PluginExpressionPlan,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let cause = self.expression_origin(expression)?;
        let operands =
            plan.operands
                .into_iter()
                .map(|(source, expected)| {
                    let span = self.file.expr_span(source).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?;
                    let expected = self.resolved_type(span, expected)?;
                    Ok(FirPluginOperand {
                        value: self.expression(source)?,
                        conversion: self.selected_value_conversion(source, expected, cause)?,
                    })
                })
                .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        self.add_expression(
            expression,
            FirExprKind::PluginExpression {
                plugin: plan.plugin,
                operation: plan.operation,
                data: plan.data.into_boxed_slice(),
                operands: operands.into_boxed_slice(),
            },
        )
    }
}
