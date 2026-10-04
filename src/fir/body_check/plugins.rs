//! Checked handoff for frontend-plugin expression plans.

use super::*;

impl BodyFirChecker<'_> {
    pub(super) fn plugin_expression(
        &mut self,
        expression: ExprId,
        plan: crate::plugins::PluginExpressionPlan,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let cause = self.expression_origin(expression)?;
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let types = plan
            .types
            .into_iter()
            .map(|ty| self.resolved_type(span, ty))
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        let mut operands = Vec::new();
        if plan.implicit_receiver {
            let receiver = self.implicit_receiver(expression)?.ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
            })?;
            operands.push(FirPluginOperand {
                value: receiver.value,
                conversion: receiver.conversion,
            });
        }
        operands.extend(
            plan.operands
                .into_iter()
                .map(|(source, expected)| {
                    let span = self.file.expr_span(source).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?;
                    let expected = self.resolved_type(span, expected)?;
                    let value = self.expression(source)?;
                    Ok(FirPluginOperand {
                        value,
                        conversion: self
                            .selected_value_conversion(source, value, expected, cause)?,
                    })
                })
                .collect::<Result<Vec<_>, BodyCheckFailure>>()?,
        );
        for synthesized in plan.synthesized {
            let value = self.synthesized_plugin_operand(span, cause, synthesized)?;
            operands.push(FirPluginOperand {
                value,
                conversion: None,
            });
        }
        self.add_expression(
            expression,
            FirExprKind::PluginExpression {
                plugin: plan.plugin,
                operation: plan.operation,
                data: plan.data.into_boxed_slice(),
                types: types.into_boxed_slice(),
                operands: operands.into_boxed_slice(),
            },
        )
    }

    /// Materialize one operand a plugin plan composed from frontend-selected declarations. A
    /// selected call keeps its stable module or dependency identity, so lowering realizes exactly
    /// that declaration on its singleton receiver; nothing downstream selects a member again.
    fn synthesized_plugin_operand(
        &mut self,
        span: crate::diag::Span,
        cause: OriginId,
        operand: crate::plugins::PluginSynthesizedOperand,
    ) -> Result<FirExprId, BodyCheckFailure> {
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::PluginOperand);
        match operand {
            crate::plugins::PluginSynthesizedOperand::SingletonCall {
                receiver,
                target,
                params,
                ret,
                arguments,
            } => {
                if params.len() != arguments.len() {
                    return Err(
                        self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                    );
                }
                let receiver_ty = self.resolved_type(span, Ty::obj_name(receiver))?;
                let receiver_value = self.body.add_expr(FirExpr {
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::ImplicitReceiver),
                    ty: receiver_ty,
                    kind: FirExprKind::SingletonValue {
                        classifier: receiver,
                    },
                });
                let parameters = params
                    .iter()
                    .map(|&parameter| self.resolved_type(span, parameter))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let result = self.resolved_type(span, ret)?;
                let arguments = arguments
                    .into_iter()
                    .enumerate()
                    .map(|(parameter, argument)| {
                        Ok(FirCallArgument::Expression {
                            parameter: u32::try_from(parameter).map_err(|_| {
                                self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                            })?,
                            value: self.synthesized_plugin_operand(span, cause, argument)?,
                            conversion: None,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
                    .into_boxed_slice();
                let target = match target {
                    crate::plugins::FrontendCallableTarget::Module(callable) => {
                        FirCallTarget::Module(callable)
                    }
                    crate::plugins::FrontendCallableTarget::External(declaration) => {
                        FirCallTarget::External {
                            declaration,
                            default_provider: None,
                            receiver: Some(receiver_ty),
                            declared_receiver: None,
                            parameters: parameters.clone(),
                            result,
                            declared_result: None,
                            overridden_results: Box::new([]),
                            semantic_role: None,
                            suspend: false,
                            can_inline: false,
                            inline_plan: None,
                            extension_receiver_parameter: None,
                        }
                    }
                };
                Ok(self.body.add_expr(FirExpr {
                    origin,
                    ty: result,
                    kind: FirExprKind::Call(FirCall {
                        target,
                        dispatch_receiver: Some(FirReceiver {
                            value: receiver_value,
                            conversion: None,
                        }),
                        extension_receiver: None,
                        parameter_types: parameters,
                        arguments,
                        substitutions: Box::new([]),
                    }),
                }))
            }
            crate::plugins::PluginSynthesizedOperand::Operation {
                plugin,
                operation,
                data,
                types,
                result,
                operands,
            } => {
                let types = types
                    .into_iter()
                    .map(|ty| self.resolved_type(span, ty))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let result = self.resolved_type(span, result)?;
                let operands = operands
                    .into_iter()
                    .map(|operand| {
                        Ok(FirPluginOperand {
                            value: self.synthesized_plugin_operand(span, cause, operand)?,
                            conversion: None,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
                    .into_boxed_slice();
                Ok(self.body.add_expr(FirExpr {
                    origin,
                    ty: result,
                    kind: FirExprKind::PluginExpression {
                        plugin,
                        operation,
                        data: data.into_boxed_slice(),
                        types,
                        operands,
                    },
                }))
            }
        }
    }
}
