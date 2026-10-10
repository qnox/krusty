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
            crate::plugins::PluginSynthesizedOperand::SingletonCall { call, arguments } => {
                if call.argument_parameters.len() != arguments.len()
                    || call.selected.member.params.len() != arguments.len()
                {
                    return Err(
                        self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                    );
                }
                let receiver_ty = self.resolved_type(span, call.selected.receiver)?;
                let receiver_value = self.body.add_expr(FirExpr {
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::ImplicitReceiver),
                    ty: receiver_ty,
                    kind: FirExprKind::SingletonValue {
                        classifier: call.receiver,
                    },
                });
                let parameters = call
                    .selected
                    .member
                    .params
                    .iter()
                    .map(|&parameter| self.resolved_type(span, parameter))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let result = self.resolved_type(span, call.selected.ret)?;
                let arguments = arguments
                    .into_iter()
                    .zip(call.argument_parameters.iter().copied())
                    .map(|(argument, parameter)| {
                        Ok(FirCallArgument::Expression {
                            parameter,
                            value: self.synthesized_plugin_operand(span, cause, argument)?,
                            conversion: None,
                        })
                    })
                    .collect::<Result<Vec<_>, BodyCheckFailure>>()?
                    .into_boxed_slice();
                let (target, substitutions) = self.synthesized_member_call_target(span, &call)?;
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
                        substitutions,
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

    fn synthesized_member_call_target(
        &self,
        span: crate::diag::Span,
        call: &crate::plugins::FrontendResolvedSingletonCall,
    ) -> Result<(FirCallTarget, Box<[FirTypeSubstitution]>), BodyCheckFailure> {
        let selected = &call.selected;
        let resolved = |ty| {
            ResolvedTy::new(ty).map_err(|error| {
                self.failure(Some(span), BodyCheckFailureKind::UnpublishableType(error))
            })
        };
        let declaration = selected.member.stable_declaration;
        let target = if let Some(declaration) = declaration {
            let callable = self
                .index
                .callable_for_declaration(declaration)
                .ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })?;
            FirCallTarget::Module(callable.id)
        } else {
            let external = selected.member.external_identity.ok_or_else(|| {
                self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
            FirCallTarget::External {
                declaration: external,
                default_provider: selected.member.external_default_provider,
                receiver: Some(resolved(selected.receiver)?),
                declared_receiver: None,
                parameters: selected
                    .member
                    .params
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                result: resolved(selected.ret)?,
                declared_result: selected.member.declared_ret.map(resolved).transpose()?,
                overridden_results: selected
                    .member
                    .overridden_results
                    .iter()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                semantic_role: selected.member.semantic_role,
                overridden_declarations: selected.member.overridden_declarations.clone(),
                suspend: selected.suspend,
                can_inline: selected.member.inline.can_inline(),
                inline_plan: super::inline_body_plan::publish(
                    selected.member.inline_body_plan.as_deref(),
                    None,
                    super::inline_body_plan::type_parameter_receiver(
                        selected
                            .member
                            .generic_sig
                            .as_ref()
                            .and_then(|signature| signature.receiver),
                    ),
                )
                .map_err(|_| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?,
                extension_receiver_parameter: None,
            }
        };
        let substitutions = call
            .type_arguments
            .iter()
            .enumerate()
            .map(|(ordinal, argument)| {
                let ordinal = u32::try_from(ordinal).map_err(|_| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let value = argument.ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let additional_bounds = call
                    .type_argument_bounds
                    .get(ordinal as usize)
                    .into_iter()
                    .flatten()
                    .copied()
                    .map(resolved)
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice();
                let (parameter, reified) = match declaration {
                    Some(declaration) => {
                        let parameter = self
                            .index
                            .type_parameter(declaration, ordinal)
                            .ok_or_else(|| {
                                self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                )
                            })?;
                        let header =
                            self.index.type_parameter_header(parameter).ok_or_else(|| {
                                self.failure(
                                    Some(span),
                                    BodyCheckFailureKind::MissingStableCallTarget,
                                )
                            })?;
                        (parameter.into(), header.flags.is_reified())
                    }
                    None => {
                        let external = selected.member.external_identity.ok_or_else(|| {
                            self.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                        })?;
                        (
                            FirTypeParameterRef::External {
                                callable: external,
                                ordinal,
                            },
                            selected
                                .member
                                .call_sig
                                .reified_type_parameter_ordinals
                                .contains(&ordinal),
                        )
                    }
                };
                let value = resolved(value)?;
                Ok(FirTypeSubstitution {
                    parameter,
                    reified,
                    value,
                    reified_runtime: self.reified_substitution_runtime(reified, value),
                    additional_bounds,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?
            .into_boxed_slice();
        Ok((target, substitutions))
    }
}
