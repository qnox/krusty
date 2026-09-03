//! Function-value and checker-selected `invoke` convention FIR.

use super::*;
use crate::resolve::{ReceiverFnValueOrigin, ResolvedCall, ResolvedContextArgument};

impl BodyFirChecker<'_> {
    pub(super) fn extension_function_binding(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        callable: ExprId,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Some(ExprLowering::ExtensionFunctionBinding { target, result }) =
            self.info.expr_lowers.get(&expression).cloned()
        else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let (Ty::Fun(target), Ty::Fun(result)) = (target.non_null(), result.non_null()) else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        let receiver_parameter = target.context_count;
        if !target.has_receiver
            || receiver_parameter >= target.params.len()
            || result.has_receiver
            || result.context_count != target.context_count
            || result.params.len() + 1 != target.params.len()
            || result.ret != target.ret
            || result.suspend != target.suspend
        {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let origin = self.expression_origin(expression)?;
        let receiver_value = self.expression(receiver)?;
        let receiver_ty = self
            .body
            .expr(receiver_value)
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(receiver),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?
            .ty;
        let expected_receiver = self.resolved_type(
            self.file
                .expr_span(receiver)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            target.params[receiver_parameter],
        )?;
        let callable = self.expression(callable)?;
        let target_parameters = target
            .params
            .iter()
            .copied()
            .map(|parameter| {
                self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    parameter,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FirExprKind::ExtensionFunctionBinding {
            receiver: FirReceiver {
                value: receiver_value,
                conversion: self.selected_type_conversion(receiver_ty, expected_receiver, origin),
            },
            callable,
            target_parameters: target_parameters.into_boxed_slice(),
            receiver_parameter: u32::try_from(receiver_parameter).map_err(|_| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?,
            target_result: self.resolved_type(
                self.file
                    .expr_span(expression)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                target.ret,
            )?,
            suspend: target.suspend,
        })
    }

    pub(super) fn receiver_function_invoke(
        &mut self,
        expression: ExprId,
        explicit_receiver: Option<ExprId>,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let Some(ExprLowering::ReceiverFnInvoke {
            name,
            params,
            ret,
            origin,
            property_callee,
            implicit_receiver,
            suspend,
        }) = self.info.expr_lowers.get(&expression).cloned()
        else {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        };
        if params.len() != arguments.len() + 1
            || explicit_receiver.is_some() == implicit_receiver.is_some()
        {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let origin_id = self.expression_origin(expression)?;
        let function_ty = self.resolved_type(
            self.file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            Ty::fun_with_shape(params.clone(), ret, 0, true, suspend),
        )?;
        let callee = match origin {
            ReceiverFnValueOrigin::Local => {
                if let Some((depth, binding)) = self.delegated_binding(&name) {
                    self.delegated_read(expression, depth, binding)?
                } else if let Some(binding) = self.local_binding(&name) {
                    self.body.add_expr(FirExpr {
                        origin: origin_id,
                        ty: binding.ty,
                        kind: FirExprKind::ValueRead(binding.value),
                    })
                } else if let Some((enclosing_depth, binding)) =
                    self.outer_values.get(&name).copied()
                {
                    self.body.add_capture(FirCapture {
                        origin: origin_id,
                        enclosing_depth,
                        source: binding.value,
                        ty: binding.ty,
                        shared_cell: false,
                    });
                    self.body.add_expr(FirExpr {
                        origin: origin_id,
                        ty: binding.ty,
                        kind: FirExprKind::CapturedValueRead {
                            enclosing_depth,
                            source: binding.value,
                        },
                    })
                } else {
                    return Err(self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnknownLocal,
                    ));
                }
            }
            ReceiverFnValueOrigin::ClassStorage(field)
            | ReceiverFnValueOrigin::EnumEntryPropertyStorage { field, .. } => {
                let kind = self.direct_class_storage_read_kind(field, function_ty, origin_id)?;
                self.body.add_expr(FirExpr {
                    origin: origin_id,
                    ty: function_ty,
                    kind,
                })
            }
            ReceiverFnValueOrigin::DispatchProperty { .. }
            | ReceiverFnValueOrigin::TopLevelProperty => {
                self.expression(property_callee.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStablePropertyTarget,
                    )
                })?)?
            }
        };
        let receiver = match explicit_receiver {
            Some(receiver) => FirReceiver {
                value: self.expression(receiver)?,
                conversion: None,
            },
            None => self.implicit_receiver(expression)?.ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?,
        };
        let receiver_conversion =
            self.receiver_conversion(expression, origin_id, receiver, params.first().copied())?;
        let mut checked_arguments = vec![FirCallArgument::Expression {
            parameter: 0,
            value: receiver.value,
            conversion: receiver_conversion,
        }];
        checked_arguments
            .extend(self.call_arguments_from(expression, arguments, &params, 1, None)?);
        let parameter_types = params
            .iter()
            .copied()
            .map(|parameter| {
                self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    parameter,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(FirExprKind::FunctionInvoke {
            callee,
            context_arguments: Box::new([]),
            arguments: checked_arguments.into_boxed_slice(),
            parameter_types: parameter_types.into_boxed_slice(),
            result: self.resolved_type(
                self.file
                    .expr_span(expression)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                ret,
            )?,
            suspend,
        })
    }

    pub(super) fn invoke_result_type(
        &self,
        expression: ExprId,
        kind: &FirExprKind,
    ) -> Result<ResolvedTy, BodyCheckFailure> {
        let result = match kind {
            FirExprKind::FunctionInvoke { result, .. } => result.get(),
            FirExprKind::Call(call) => match &call.target {
                FirCallTarget::Module(target) => {
                    let callable = self.index.callable(*target).ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::MissingStableCallTarget,
                        )
                    })?;
                    let signature =
                        self.index.signature(callable.declaration).ok_or_else(|| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::MissingStableCallTarget,
                            )
                        })?;
                    let bindings = call
                        .substitutions
                        .iter()
                        .filter_map(|substitution| {
                            let FirTypeParameterRef::Module(parameter) = substitution.parameter
                            else {
                                return None;
                            };
                            self.index
                                .type_parameter_semantic_name(parameter)
                                .map(|name| (name.to_owned(), substitution.value.get()))
                        })
                        .collect::<std::collections::HashMap<_, _>>();
                    crate::types::ty_subst_keep_unbound(signature.result.get(), &bindings)
                }
                FirCallTarget::External { result, .. }
                | FirCallTarget::Intrinsic { result, .. }
                | FirCallTarget::Classifier { result, .. }
                | FirCallTarget::Super { result, .. } => result.get(),
            },
            _ => {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
        };
        self.resolved_type(
            self.file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            result,
        )
    }

    pub(super) fn invoke(
        &mut self,
        expression: ExprId,
        receiver: ExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
        kind: InvokeKind,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let callee = self.expression(receiver)?;
        self.invoke_on_value(expression, callee, arguments, parameters, kind)
    }

    pub(super) fn invoke_on_value(
        &mut self,
        expression: ExprId,
        callee: FirExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
        kind: InvokeKind,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        match kind {
            InvokeKind::Function {
                context_params,
                ret,
                suspend,
            } => {
                let callee_ty = self
                    .body
                    .expr(callee)
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?
                    .ty
                    .get();
                crate::trace_compiler!(
                    "fir",
                    "publish function invoke expression={expression:?} callee={callee:?} callee_ty={callee_ty:?} recorded_context={context_params:?} recorded_parameters={parameters:?} arguments={arguments:?}",
                );
                // A plain function value publishes its complete stable `Ty::Fun`. Callable
                // objects such as `KProperty0<V>` are nominal object types, so their resolver-
                // selected `InvokeKind::Function` signature is the authoritative checked shape.
                // Both paths cross `ResolvedTy` below; neither can publish pending/error types.
                let (context_params, parameters, ret, suspend) =
                    if let Ty::Fun(signature) = callee_ty.non_null() {
                        let implicit_context_count = context_params.len();
                        if signature.params.len()
                            != implicit_context_count.saturating_add(parameters.len())
                        {
                            return Err(self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedCallShape,
                            ));
                        }
                        let (context_params, parameters) =
                            signature.params.split_at(implicit_context_count);
                        (context_params, parameters, signature.ret, signature.suspend)
                    } else {
                        (context_params.as_slice(), parameters, ret, suspend)
                    };
                if parameters.len() != arguments.len() {
                    return Err(self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    ));
                };
                let context_arguments =
                    self.function_context_arguments(expression, context_params)?;
                let parameter_types = context_params
                    .iter()
                    .chain(parameters)
                    .map(|parameter| {
                        self.resolved_type(
                            self.file.expr_span(expression).ok_or_else(|| {
                                self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                            })?,
                            *parameter,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(FirExprKind::FunctionInvoke {
                    callee,
                    context_arguments,
                    arguments: self.call_arguments(expression, arguments, parameters)?,
                    parameter_types: parameter_types.into_boxed_slice(),
                    result: self.resolved_type(
                        self.file.expr_span(expression).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        ret,
                    )?,
                    suspend,
                })
            }
            InvokeKind::Operator { target, .. } => {
                self.operator_invoke_on_value(expression, callee, arguments, *target)
            }
        }
    }

    fn function_context_arguments(
        &mut self,
        expression: ExprId,
        expected: &[Ty],
    ) -> Result<Box<[FirReceiver]>, BodyCheckFailure> {
        let selected = self
            .info
            .context_args
            .get(&expression)
            .map(Vec::as_slice)
            .unwrap_or_default();
        if selected.len() != expected.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let cause = self.expression_origin(expression)?;
        selected
            .iter()
            .zip(expected)
            .map(|(argument, expected)| {
                let receiver = self.materialize_context_argument(expression, cause, argument)?;
                Ok(FirReceiver {
                    value: receiver.value,
                    conversion: self.receiver_conversion(
                        expression,
                        cause,
                        receiver,
                        Some(*expected),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()
            .map(Vec::into_boxed_slice)
    }

    pub(super) fn materialize_context_argument(
        &mut self,
        expression: ExprId,
        cause: OriginId,
        argument: &ResolvedContextArgument,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        self.materialize_context_argument_at(self.file.expr_span(expression), cause, argument)
    }

    /// Materialize a selected context argument for a synthetic call site. Operator conventions
    /// attached to statements have no call-expression arena id, so their checked FIR must use the
    /// statement span and origin directly instead of inventing a transient AST identity.
    pub(super) fn materialize_context_argument_at(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        argument: &ResolvedContextArgument,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        let value = match argument {
            ResolvedContextArgument::Binding { name, shadow_depth } => {
                let (enclosing_depth, binding) = self
                    .binding_source_at_shadow_depth(name, *shadow_depth)
                    .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnknownLocal))?;
                let kind = if let Some(enclosing_depth) = enclosing_depth {
                    self.body.add_capture(FirCapture {
                        origin: cause,
                        enclosing_depth,
                        source: binding.value,
                        ty: binding.ty,
                        shared_cell: false,
                    });
                    FirExprKind::CapturedValueRead {
                        enclosing_depth,
                        source: binding.value,
                    }
                } else {
                    FirExprKind::ValueRead(binding.value)
                };
                self.body.add_expr(FirExpr {
                    origin: cause,
                    ty: binding.ty,
                    kind,
                })
            }
            ResolvedContextArgument::ImplicitReceiver(selection) => {
                self.materialize_implicit_receiver(cause, span, selection)?
                    .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?
                    .value
            }
        };
        Ok(FirReceiver {
            value,
            conversion: None,
        })
    }

    fn operator_invoke_on_value(
        &mut self,
        expression: ExprId,
        receiver: FirExprId,
        arguments: &[ExprId],
        selected: ResolvedCall,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let (
            target,
            substitutions,
            dispatch_receiver,
            extension_receiver,
            context_arguments,
            vararg_index,
            parameters,
            extension_parameter,
        ) = match selected {
            ResolvedCall::Member(member) => {
                let target = self.member_call_target(expression, &member)?;
                let parameters = self.selected_call_parameters(
                    expression,
                    member.member.stable_declaration,
                    &member.member.params,
                )?;
                (
                    target.0,
                    target.1,
                    Some(FirReceiver {
                        value: receiver,
                        conversion: None,
                    }),
                    None,
                    member.context_args,
                    member.member.call_sig.vararg_index,
                    parameters,
                    None,
                )
            }
            ResolvedCall::Extension(extension) => {
                let target = self.extension_call_target(expression, &extension)?;
                let context_count = extension
                    .callable
                    .context_count
                    .min(extension.callable.params.len());
                let parameters = extension.callable.params[..context_count]
                    .iter()
                    .copied()
                    .chain(extension.params.iter().copied())
                    .collect::<Vec<_>>();
                let parameters = self.selected_call_parameters(
                    expression,
                    extension.stable_declaration,
                    &parameters,
                )?;
                (
                    target.0,
                    target.1,
                    None,
                    Some(FirReceiver {
                        value: receiver,
                        conversion: None,
                    }),
                    extension.context_args.into_iter().map(Some).collect(),
                    extension.vararg_index,
                    parameters,
                    None,
                )
            }
            ref selected @ ResolvedCall::MemberExtension {
                ref dispatch_receiver,
                vararg_index,
                ref context_args,
                ..
            } => {
                let target = self.member_extension_call_target(expression, selected)?;
                let cause = self.expression_origin(expression)?;
                let dispatch_receiver = self
                    .materialize_implicit_receiver(cause, span, &dispatch_receiver)?
                    .ok_or_else(|| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?;
                (
                    target.target,
                    target.substitutions,
                    Some(dispatch_receiver),
                    Some(FirReceiver {
                        value: receiver,
                        conversion: None,
                    }),
                    context_args.clone(),
                    vararg_index,
                    target.parameters,
                    target.extension_parameter,
                )
            }
            ResolvedCall::TopLevel(_)
            | ResolvedCall::Companion(_)
            | ResolvedCall::LocalFunction(_) => {
                return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
            }
        };
        let arguments = self.call_arguments_with_context(
            expression,
            arguments,
            &parameters,
            context_arguments.iter().map(Option::as_ref),
            vararg_index,
        )?;
        Ok(FirExprKind::Call(FirCall {
            target,
            dispatch_receiver,
            extension_receiver,
            parameter_types: self.published_parameter_types(span, &parameters)?,
            arguments: self.member_extension_arguments(
                expression,
                arguments,
                extension_parameter,
            )?,
            substitutions,
        }))
    }
}
