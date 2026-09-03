//! Final source-argument mapping for already selected calls.

use super::*;
use crate::resolve::ResolvedContextArgument;

impl BodyFirChecker<'_> {
    pub(super) fn published_parameter_types(
        &self,
        span: Option<crate::diag::Span>,
        parameters: &[Ty],
    ) -> Result<Box<[ResolvedTy]>, BodyCheckFailure> {
        parameters
            .iter()
            .copied()
            .map(|parameter| {
                ResolvedTy::new(parameter).map_err(|error| {
                    self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }

    /// Return the call-site-specialized semantic parameter list for one selected declaration.
    ///
    /// Resolver selection records inferred/explicit type arguments separately from the declaration
    /// signature. Checked FIR must join those facts before publishing argument conversions: using a
    /// probe-time parameter here can, for example, leave `identity<Long>(value = 1)` with an `Int`
    /// context slot even though the selected declaration slot is `Long`.
    pub(super) fn selected_call_parameters(
        &self,
        expression: ExprId,
        declaration: Option<DeclarationId>,
        fallback: &[Ty],
    ) -> Result<Vec<Ty>, BodyCheckFailure> {
        let Some(declaration) = declaration else {
            return Ok(fallback.to_vec());
        };
        let signature = self.index.signature(declaration).ok_or_else(|| {
            crate::trace_compiler!(
                "fir",
                "missing call signature declaration={declaration:?} name={:?}",
                self.index.declaration_name(declaration),
            );
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let mut substitutions = HashMap::new();
        for (ordinal, value) in self
            .info
            .resolved_call_type_args
            .get(&expression)
            .into_iter()
            .flatten()
            .enumerate()
        {
            let Some(value) = value else { continue };
            let ordinal = u32::try_from(ordinal).map_err(|_| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            let parameter = self
                .index
                .type_parameter(declaration, ordinal)
                .ok_or_else(|| {
                    crate::trace_compiler!(
                        "fir",
                        "missing parameter specialization declaration={declaration:?} ordinal={ordinal}",
                    );
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            let name = self
                .index
                .type_parameter_semantic_name(parameter)
                .ok_or_else(|| {
                    crate::trace_compiler!(
                        "fir",
                        "missing parameter semantic name declaration={declaration:?} ordinal={ordinal}",
                    );
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    )
                })?;
            substitutions.insert(name.to_owned(), *value);
        }
        if signature.parameters.len() != fallback.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        // `fallback` is the resolver's selected member shape and therefore already contains
        // substitutions contributed by the dispatch receiver (`Box<Int>.set(..., T)` → `Int`).
        // Callable type arguments are recorded separately, so apply those on top. Starting again
        // from the declaration signature here discarded the receiver substitution and forced
        // lowering to reconstruct it from syntax/receiver types.
        Ok(fallback
            .iter()
            .copied()
            .map(|parameter| {
                crate::symbol_resolver::ty_subst_keep_unbound(parameter, &substitutions)
            })
            .collect())
    }

    pub(super) fn call_arguments(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        self.call_arguments_from(expression, arguments, parameters, 0, None)
    }

    pub(super) fn call_arguments_from(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
        parameter_offset: usize,
        vararg_index: Option<usize>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let cause = self.expression_origin(expression)?;
        let Some(slots) = self.info.resolved_call_arg_slots.get(&expression).cloned() else {
            return arguments
                .iter()
                .enumerate()
                .map(|(source, argument)| {
                    let parameter = vararg_index
                        .filter(|vararg| source >= *vararg)
                        .unwrap_or(source);
                    self.checked_source_call_argument(
                        expression,
                        cause,
                        *argument,
                        parameter,
                        parameters,
                        parameter_offset,
                        vararg_index,
                    )
                })
                .collect::<Result<Vec<_>, BodyCheckFailure>>()
                .map(Vec::into_boxed_slice);
        };
        let mut checked = Vec::with_capacity(arguments.len().max(slots.len()));
        let mut saw_vararg = false;
        for argument in arguments {
            let parameter = match slots.iter().position(|slot| *slot == Some(*argument)) {
                Some(parameter) => parameter,
                None => vararg_index.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?,
            };
            saw_vararg |= vararg_index == Some(parameter);
            checked.push(self.checked_source_call_argument(
                expression,
                cause,
                *argument,
                parameter,
                parameters,
                parameter_offset,
                vararg_index,
            )?);
        }
        if let Some(parameter) = vararg_index.filter(|_| !saw_vararg) {
            checked.push(FirCallArgument::Vararg {
                parameter: self.call_parameter_ordinal(expression, parameter, parameter_offset)?,
                origin: self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::VarargArray),
                elements: Box::new([]),
            });
        }
        for (parameter, slot) in slots.iter().enumerate() {
            if slot.is_none() && vararg_index != Some(parameter) {
                checked.push(FirCallArgument::Default {
                    parameter: self.call_parameter_ordinal(
                        expression,
                        parameter,
                        parameter_offset,
                    )?,
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                });
            }
        }
        Ok(checked.into_boxed_slice())
    }

    fn checked_source_call_argument(
        &mut self,
        expression: ExprId,
        cause: OriginId,
        argument: ExprId,
        parameter: usize,
        parameters: &[Ty],
        parameter_offset: usize,
        vararg_index: Option<usize>,
    ) -> Result<FirCallArgument, BodyCheckFailure> {
        let parameter_id = self.call_parameter_ordinal(expression, parameter, parameter_offset)?;
        let physical_parameter = parameter
            .checked_add(parameter_offset)
            .and_then(|parameter| parameters.get(parameter))
            .copied()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        let value = self.expression(argument)?;
        if vararg_index != Some(parameter) {
            let target = self.resolved_type(
                self.file
                    .expr_span(expression)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                physical_parameter,
            )?;
            return Ok(FirCallArgument::Expression {
                parameter: parameter_id,
                value,
                conversion: self.selected_value_conversion(argument, target, cause)?,
            });
        }
        if self
            .info
            .resolved_whole_array_vararg_args
            .contains(&argument)
        {
            let target = self.resolved_type(
                self.file
                    .expr_span(expression)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                physical_parameter,
            )?;
            return Ok(FirCallArgument::Expression {
                parameter: parameter_id,
                value,
                conversion: self.selected_value_conversion(argument, target, cause)?,
            });
        }
        let expected = if self.file.is_spread_arg(argument) {
            physical_parameter
        } else {
            physical_parameter.array_elem().ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?
        };
        let target = self.resolved_type(
            self.file
                .expr_span(expression)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
            expected,
        )?;
        Ok(FirCallArgument::Vararg {
            parameter: parameter_id,
            origin: cause,
            elements: vec![FirVarargElement {
                value,
                spread: self.file.is_spread_arg(argument),
                conversion: self.selected_value_conversion(argument, target, cause)?,
            }]
            .into_boxed_slice(),
        })
    }

    fn selected_argument_conversion(
        &mut self,
        argument: ExprId,
        cause: OriginId,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        let Some(sam) = self.info.resolved_sam_conversions.get(&argument).cloned() else {
            return Ok(None);
        };
        let span = self.file.expr_span(argument);
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let parameters = sam
            .params
            .into_iter()
            .map(resolved)
            .collect::<Result<Vec<_>, _>>()?;
        let declared_parameters = sam
            .declared_params
            .into_iter()
            .map(resolved)
            .collect::<Result<Vec<_>, _>>()?;
        let context_count = u32::try_from(sam.context_count)
            .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let conversion = self.body.add_sam_conversion(FirSamConversion {
            classifier: sam.internal,
            method: sam.method.into_boxed_str(),
            parameters: parameters.into_boxed_slice(),
            result: resolved(sam.ret)?,
            declared_parameters: declared_parameters.into_boxed_slice(),
            declared_result: resolved(sam.declared_ret)?,
            context_count,
            has_receiver: sam.has_receiver,
            suspend: sam.suspend,
            nullable: self.info.semantic_ty(argument).is_nullable(),
        });
        Ok(Some(FirConversion {
            origin: cause,
            kind: FirConversionKind::Sam(conversion),
        }))
    }

    /// Publish the representation-changing part of an assignment the ordinary checker already
    /// accepted. This does not decide assignability: `TypeInfo` owns that decision, while the
    /// checked target type supplied by the enclosing declaration determines the exact FIR boundary.
    pub(super) fn selected_value_conversion(
        &mut self,
        expression: ExprId,
        target: ResolvedTy,
        cause: OriginId,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        if self
            .info
            .selected_numeric_conversions
            .get(&expression)
            .copied()
            == Some(target.get())
        {
            return Ok(Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::NumericConversion { to: target },
            }));
        }
        if let Some(conversion) = self.selected_argument_conversion(expression, cause)? {
            return Ok(Some(conversion));
        }
        if let Some((from, to)) = self
            .info
            .selected_suspend_function_conversions
            .get(&expression)
            .copied()
            .filter(|(_, selected_target)| *selected_target == target.get())
        {
            let span = self.file.expr_span(expression);
            return Ok(Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::SuspendFunction {
                    from: ResolvedTy::new(from).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?,
                    to: ResolvedTy::new(to).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?,
                },
            }));
        }
        let actual = self.expression_type(expression)?;
        if self
            .info
            .selected_value_smartcasts
            .get(&expression)
            .copied()
            == Some(target.get())
        {
            return Ok(Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::SmartCast { to: target },
            }));
        }
        Ok(self.selected_type_conversion(actual, target, cause))
    }

    /// Materialize one frontend-committed value boundary. The target and any representation-changing
    /// conversion were already selected by resolution; this only embeds that checked decision in FIR.
    pub(super) fn value_at_selected_boundary(
        &mut self,
        expression: ExprId,
        target: ResolvedTy,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(expression)?;
        let conversion = self.selected_value_conversion(expression, target, origin)?;
        let value = self.expression(expression)?;
        let Some(conversion) = conversion else {
            return Ok(value);
        };
        Ok(self.body.add_expr(crate::fir::FirExpr {
            origin,
            ty: target,
            kind: crate::fir::FirExprKind::ImplicitConversion { value, conversion },
        }))
    }

    pub(super) fn selected_type_conversion(
        &self,
        actual: ResolvedTy,
        target: ResolvedTy,
        cause: OriginId,
    ) -> Option<FirConversion> {
        if actual == target || matches!(actual.get(), Ty::Nothing | Ty::Null) {
            return None;
        }
        let actual_ty = actual.get();
        let target_ty = target.get();
        let kind = if target_ty == Ty::Unit {
            FirConversionKind::CoerceToUnit
        } else if target_ty.accepts_numeric(actual_ty) {
            FirConversionKind::NumericWidening { to: target }
        } else if target_ty.is_nullable() || (!actual_ty.is_reference() && target_ty.is_reference())
        {
            FirConversionKind::NullabilityWidening { to: target }
        } else if actual_ty.is_reference() && !target_ty.is_reference() {
            FirConversionKind::SmartCast { to: target }
        } else {
            return None;
        };
        Some(FirConversion {
            origin: cause,
            kind,
        })
    }

    fn call_parameter_ordinal(
        &self,
        expression: ExprId,
        parameter: usize,
        parameter_offset: usize,
    ) -> Result<u32, BodyCheckFailure> {
        u32::try_from(parameter.checked_add(parameter_offset).ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?)
        .map_err(|_| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })
    }

    pub(super) fn call_arguments_with_context<'a>(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
        context: impl Iterator<Item = Option<&'a ResolvedContextArgument>>,
        vararg_index: Option<usize>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let context = context.collect::<Vec<_>>();
        let cause = self.expression_origin(expression)?;
        let mut checked = context
            .iter()
            .enumerate()
            .filter_map(|(parameter, argument)| {
                let argument = (*argument)?;
                Some((parameter, argument))
            })
            .map(|(parameter, argument)| {
                let receiver = self.materialize_context_argument(expression, cause, argument)?;
                Ok(FirCallArgument::Expression {
                    parameter: parameter as u32,
                    value: receiver.value,
                    conversion: self.receiver_conversion(
                        expression,
                        cause,
                        receiver,
                        parameters.get(parameter).copied(),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        let explicit_context = context
            .iter()
            .enumerate()
            .filter_map(|(parameter, argument)| argument.is_none().then_some(parameter))
            .collect::<Vec<_>>();
        if explicit_context.is_empty() {
            checked.extend(self.call_arguments_from(
                expression,
                arguments,
                parameters,
                context.len(),
                vararg_index,
            )?);
            return Ok(checked.into_boxed_slice());
        }
        let slots = self
            .info
            .resolved_call_arg_slots
            .get(&expression)
            .cloned()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        let ordinary_count = slots
            .len()
            .checked_sub(explicit_context.len())
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        let mut saw_vararg = false;
        for argument in arguments {
            let visible_parameter = match slots.iter().position(|slot| *slot == Some(*argument)) {
                Some(parameter) => parameter,
                None => vararg_index.ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?,
            };
            if visible_parameter < ordinary_count {
                saw_vararg |= vararg_index == Some(visible_parameter);
                checked.push(self.checked_source_call_argument(
                    expression,
                    cause,
                    *argument,
                    visible_parameter,
                    parameters,
                    context.len(),
                    vararg_index,
                )?);
                continue;
            }
            let explicit = visible_parameter - ordinary_count;
            let parameter = *explicit_context.get(explicit).ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
            checked.push(FirCallArgument::Expression {
                parameter: u32::try_from(parameter).map_err(|_| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?,
                value: self.expression(*argument)?,
                conversion: self.selected_value_conversion(
                    *argument,
                    self.resolved_type(
                        self.file.expr_span(expression).ok_or_else(|| {
                            self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                        })?,
                        *parameters.get(parameter).ok_or_else(|| {
                            self.failure(
                                self.file.expr_span(expression),
                                BodyCheckFailureKind::UnsupportedCallShape,
                            )
                        })?,
                    )?,
                    cause,
                )?,
            });
        }
        if let Some(parameter) = vararg_index.filter(|_| !saw_vararg) {
            checked.push(FirCallArgument::Vararg {
                parameter: self.call_parameter_ordinal(expression, parameter, context.len())?,
                origin: self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::VarargArray),
                elements: Box::new([]),
            });
        }
        for (parameter, slot) in slots.iter().take(ordinary_count).enumerate() {
            if slot.is_none() && vararg_index != Some(parameter) {
                checked.push(FirCallArgument::Default {
                    parameter: self.call_parameter_ordinal(expression, parameter, context.len())?,
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                });
            }
        }
        Ok(checked.into_boxed_slice())
    }

    pub(super) fn receiver_conversion(
        &self,
        expression: ExprId,
        cause: OriginId,
        receiver: FirReceiver,
        target: Option<Ty>,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        self.receiver_conversion_at(self.file.expr_span(expression), cause, receiver, target)
    }

    pub(super) fn receiver_conversion_at(
        &self,
        span: Option<Span>,
        cause: OriginId,
        receiver: FirReceiver,
        target: Option<Ty>,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        if receiver.conversion.is_some() {
            return Ok(receiver.conversion);
        }
        let target =
            target.ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let actual = self
            .body
            .expr(receiver.value)
            .ok_or_else(|| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let target = ResolvedTy::new(target)
            .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))?;
        Ok(self.selected_type_conversion(actual.ty, target, cause))
    }
}

impl BodyFirChecker<'_> {
    /// `I { … }` — a fun-interface SAM constructor. The single operand is converted to the interface
    /// by exactly the conversion an argument in SAM position would get.
    pub(super) fn sam_constructor_call(
        &mut self,
        expression: ExprId,
        args: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let span = self.file.expr_span(expression);
        let Some(ExprLowering::SamConstructor {
            internal,
            result: _,
            method,
            params,
            ret,
            declared_params,
            declared_ret,
            context_count,
            has_receiver,
            suspend,
            ..
        }) = self.info.expr_lowers.get(&expression).cloned()
        else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let [operand] = args else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let cause = self.expression_origin(expression)?;
        let mut resolved = |ty: crate::types::Ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let parameters = params
            .iter()
            .copied()
            .map(&mut resolved)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let result = resolved(ret)?;
        let declared_parameters = declared_params
            .iter()
            .copied()
            .map(&mut resolved)
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let declared_result = resolved(declared_ret)?;
        let context_count = u32::try_from(context_count)
            .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?;
        let conversion = self.body.add_sam_conversion(FirSamConversion {
            classifier: internal,
            method: method.into_boxed_str(),
            parameters: parameters.clone(),
            result,
            declared_parameters,
            declared_result,
            context_count,
            has_receiver,
            suspend,
            nullable: false,
        });
        let value = self.expression(*operand)?;
        Ok(FirExprKind::ImplicitConversion {
            value,
            conversion: FirConversion {
                origin: cause,
                kind: FirConversionKind::Sam(conversion),
            },
        })
    }
}
