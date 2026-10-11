//! Final source-argument mapping for already selected calls.

use super::*;

#[cfg(test)]
#[path = "vararg_default_tests.rs"]
mod vararg_default_tests;
use crate::resolve::ResolvedContextArgument;

/// Name kotlinc puts in `checkNotNullExpressionValue` for one property read.
fn property_platform_check_name(
    property: &str,
    getter_name: Option<&str>,
    producer: crate::libraries::PropertyProducer,
) -> String {
    producer.platform_check_name(property, getter_name)
}

#[cfg(test)]
thread_local! {
    static OMIT_RECORDED_SAM_PUBLICATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While this guard is alive, a recorded fun-interface conversion does not materialize.
///
/// Production cannot build that mismatch today — the same map was just checked — so the
/// regression arms it explicitly.
#[cfg(test)]
pub(super) struct OmitRecordedSamPublication;

#[cfg(test)]
impl OmitRecordedSamPublication {
    pub(super) fn arm() -> Self {
        OMIT_RECORDED_SAM_PUBLICATION.with(|flag| flag.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for OmitRecordedSamPublication {
    fn drop(&mut self) {
        OMIT_RECORDED_SAM_PUBLICATION.with(|flag| flag.set(false));
    }
}

/// The expression a block chain actually yields. A recorded SAM conversion names that lambda, not
/// the block written around it.
fn sam_conversion_producer(file: &crate::ast::File, mut expression: ExprId) -> ExprId {
    while let Expr::Block {
        trailing: Some(trailing),
        ..
    } = file.expr(expression)
    {
        expression = *trailing;
    }
    expression
}

/// The selected parameters one source call's arguments are checked against: their types, the
/// leading parameters no source argument names, and the vararg parameter, if any.
#[derive(Clone, Copy)]
struct SourceCallParameters<'a> {
    types: &'a [Ty],
    offset: usize,
    vararg_index: Option<usize>,
}

impl BodyFirChecker<'_> {
    /// Whether an exact-Unit expression is an effect that still needs the language-level singleton
    /// at a value boundary. Stored reads and `Unit` itself already produce that value; calls,
    /// function-value invocations, and Unit `when`/`try` use their statement-valued Unit
    /// convention. (A Unit `if` converts each branch instead.) This decision is published as a
    /// FIR conversion so common lowering never reconstructs it from call or storage identities.
    pub(super) fn unit_effect_requires_value(&self, value: FirExprId) -> bool {
        let Some(expression) = self.body.expr(value) else {
            return false;
        };
        if expression.ty.get().canonical_semantic() != Ty::Unit {
            return false;
        }
        match &expression.kind {
            FirExprKind::Call(_)
            | FirExprKind::LocalCall { .. }
            | FirExprKind::FunctionInvoke { .. }
            | FirExprKind::When { .. }
            | FirExprKind::Try { .. } => true,
            FirExprKind::ClassStorageSharedWrite { .. }
            | FirExprKind::ConstructorCaptureSharedWrite { .. }
            | FirExprKind::CapturedClassStorageSharedWrite { .. }
            | FirExprKind::CapturedValueWrite { .. }
            | FirExprKind::ValueWrite { .. }
            | FirExprKind::PropertyWrite { .. }
            | FirExprKind::BackingFieldWrite { .. }
            | FirExprKind::IndexedWrite { .. } => true,
            FirExprKind::Block {
                result: Some(result),
                ..
            } => self.unit_effect_requires_value(*result),
            FirExprKind::Block { result: None, .. } => true,
            FirExprKind::ImplicitConversion { conversion, .. }
                if matches!(conversion.kind, FirConversionKind::CoerceToUnit) =>
            {
                false
            }
            _ => false,
        }
    }

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
                        SourceCallParameters {
                            types: parameters,
                            offset: parameter_offset,
                            vararg_index,
                        },
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
                SourceCallParameters {
                    types: parameters,
                    offset: parameter_offset,
                    vararg_index,
                },
            )?);
        }
        if let Some(parameter) = vararg_index.filter(|_| !saw_vararg) {
            checked.push(self.omitted_vararg_argument(
                expression,
                cause,
                parameter,
                parameter_offset,
            )?);
        }
        for (parameter, slot) in slots.iter().enumerate() {
            if slot.is_none() && vararg_index != Some(parameter) {
                let ordinal =
                    self.call_parameter_ordinal(expression, parameter, parameter_offset)?;
                // A dependency default the provider states as a CONSTANT is passed as an ordinary
                // argument rather than left for a default-call ABI to fill. It is the same value
                // either way — a constant has no side effects and depends on no other argument —
                // and this form needs no `$default` symbol, which is what a klib cannot name.
                if let Some(value) = self.library_default_literal(
                    expression,
                    parameter,
                    parameters,
                    parameter_offset,
                )? {
                    checked.push(FirCallArgument::Expression {
                        parameter: ordinal,
                        value,
                        conversion: None,
                    });
                    continue;
                }
                checked.push(FirCallArgument::Default {
                    parameter: ordinal,
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                });
            }
        }
        Ok(checked.into_boxed_slice())
    }

    /// The checked constant a dependency default resolves to at this call site, when the provider
    /// stated one.
    ///
    /// Its type is the PARAMETER's, not the literal's own: `message: String? = null` passes a
    /// `null` typed `String?`, and an `Int` default filling a `Long` parameter is already ruled out
    /// by the fit check resolution made before recording it.
    fn library_default_literal(
        &mut self,
        expression: ExprId,
        parameter: usize,
        parameters: &[Ty],
        parameter_offset: usize,
    ) -> Result<Option<FirExprId>, BodyCheckFailure> {
        let Some(value) = self
            .info
            .resolved_library_default_literals
            .get(&expression)
            .and_then(|literals| {
                literals
                    .iter()
                    .find(|(ordinal, _)| *ordinal == parameter)
                    .map(|(_, value)| value.clone())
            })
        else {
            return Ok(None);
        };
        // Provider defaults are parallel to VALUE parameters. `parameters` is the physical FIR
        // signature and may begin with context/receiver parameters, so translate the value ordinal
        // at this one boundary. The call argument ordinal uses the same offset independently.
        let physical = parameter.checked_add(parameter_offset).ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        let ty = parameters.get(physical).copied().ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            )
        })?;
        let constant = match value {
            crate::libraries::DefaultValue::Null => FirConstant::Null,
            crate::libraries::DefaultValue::Bool(value) => FirConstant::Boolean(value),
            crate::libraries::DefaultValue::Char(value) => FirConstant::Char(value),
            crate::libraries::DefaultValue::Int(value) => FirConstant::Int(value),
            crate::libraries::DefaultValue::Long(value) => FirConstant::Long(value),
            crate::libraries::DefaultValue::Float(value) => FirConstant::Float(value),
            crate::libraries::DefaultValue::Double(value) => FirConstant::Double(value),
            crate::libraries::DefaultValue::Str(value) => FirConstant::String(value),
            // Resolution never records a declaration, class literal, array, or nested annotation as
            // a call literal. Seeing one here means the checked call facts disagree.
            crate::libraries::DefaultValue::Object(_)
            | crate::libraries::DefaultValue::EnumEntry { .. }
            | crate::libraries::DefaultValue::KClass(_)
            | crate::libraries::DefaultValue::Array { .. }
            | crate::libraries::DefaultValue::Annotation { .. } => {
                return Err(self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                ));
            }
        };
        let cause = self.expression_origin(expression)?;
        let origin = self
            .origins
            .synthetic(cause, SyntheticOriginKind::DefaultArgument);
        let ty = ResolvedTy::new(ty).map_err(|error| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnpublishableType(error),
            )
        })?;
        Ok(Some(self.body.add_expr(FirExpr {
            origin,
            ty,
            kind: FirExprKind::Constant(constant),
        })))
    }

    fn checked_source_call_argument(
        &mut self,
        expression: ExprId,
        cause: OriginId,
        argument: ExprId,
        parameter: usize,
        shape: SourceCallParameters<'_>,
    ) -> Result<FirCallArgument, BodyCheckFailure> {
        let SourceCallParameters {
            types: parameters,
            offset: parameter_offset,
            vararg_index,
        } = shape;
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
                conversion: self.selected_value_conversion(argument, value, target, cause)?,
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
            return Ok(FirCallArgument::Vararg {
                parameter: parameter_id,
                origin: cause,
                elements: vec![FirVarargElement {
                    value,
                    spread: true,
                    conversion: self.selected_value_conversion(argument, value, target, cause)?,
                }]
                .into_boxed_slice(),
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
                conversion: self.selected_value_conversion(argument, value, target, cause)?,
            }]
            .into_boxed_slice(),
        })
    }

    fn selected_argument_conversion(
        &mut self,
        argument: ExprId,
        cause: OriginId,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        let Some(selected) = self.info.resolved_sam_conversions.get(&argument).cloned() else {
            return Ok(None);
        };
        let span = self.file.expr_span(argument);
        let nullable = self.info.semantic_ty(argument).is_nullable();
        let conversion = self.published_sam_conversion(
            span,
            &selected.signature,
            nullable,
            selected.source_suspend,
        )?;
        let conversion = self.body.add_sam_conversion(conversion);
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
        value: crate::fir::FirExprId,
        target: ResolvedTy,
        cause: OriginId,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        let actual = self
            .body
            .expr(value)
            .map(|expression| expression.ty)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        self.selected_value_conversion_from(expression, value, actual, target, cause)
    }

    pub(super) fn selected_value_conversion_from(
        &mut self,
        expression: ExprId,
        value: crate::fir::FirExprId,
        actual: ResolvedTy,
        target: ResolvedTy,
        cause: OriginId,
    ) -> Result<Option<FirConversion>, BodyCheckFailure> {
        if let Some(narrowing) = self.info.platform_narrowings.get(&expression).copied() {
            if narrowing == PlatformNarrowing::Declaration
                && is_value_boundary(self.file.expr(expression))
            {
                self.guard_platform_conditional(expression, value, target)?;
            } else if let Some(conversion) =
                self.platform_producer_conversion(expression, cause, target)
            {
                return Ok(Some(conversion));
            }
        }
        if self
            .info
            .selected_numeric_conversions
            .get(&expression)
            .copied()
            == Some(target.get())
        {
            let conversion = FirConversion {
                origin: cause,
                kind: FirConversionKind::NumericConversion { to: target },
            };
            // An expected numeric type reaches every result edge of a conditional. Publish that
            // topology in checked FIR so constant evaluation can fold each converted constant and
            // common lowering never has to rediscover an `if`/`when`/block shape.
            if self.distribute_numeric_conditional(expression, value, target, conversion)? {
                return Ok(None);
            }
            return Ok(Some(conversion));
        }
        if let Some(conversion) = self.selected_argument_conversion(expression, cause)? {
            return Ok(Some(conversion));
        }
        if let Some((from, to)) = self
            .info
            .selected_function_value_conversions
            .get(&expression)
            .copied()
            .filter(|(_, selected_target)| *selected_target == target.get())
        {
            let span = self.file.expr_span(expression);
            let info = self.info;
            let name = info
                .generated_class_names
                .conversions
                .get(&expression)
                .ok_or_else(|| {
                    self.failure(span, BodyCheckFailureKind::UnnamedFunctionValueConversion)
                })?;
            // The converted value is never itself a lambda or reference with a class of its own,
            // so its node carries the name of the class its conversion compiles to.
            self.record_class_name_provenance(&name.provenance, value);
            return Ok(Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::FunctionValue {
                    from: ResolvedTy::new(from).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?,
                    to: ResolvedTy::new(to).map_err(|error| {
                        self.failure(span, BodyCheckFailureKind::UnpublishableType(error))
                    })?,
                    ordinal: name.ordinal,
                },
            }));
        }
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
        // An exact Unit-to-Unit boundary can still change representation: a Unit-returning
        // function is effect-only, while an argument, local, field, or other stored value requires
        // the language-level singleton. Record that semantic boundary in FIR instead of asking
        // lowering to recognize a call shape and manufacture the value.
        if actual.get().canonical_semantic() == Ty::Unit
            && target.get().canonical_semantic() == Ty::Unit
            && self.unit_effect_requires_value(value)
        {
            return Ok(Some(FirConversion {
                origin: cause,
                kind: FirConversionKind::CoerceToUnit,
            }));
        }
        Ok(self.selected_type_conversion(actual, target, cause))
    }

    /// Put one already-selected numeric conversion on every value edge of a conditional. Returns
    /// `false` without mutation when `source` is not a supported conditional boundary, so the
    /// caller can retain the ordinary conversion around the whole value.
    fn distribute_numeric_conditional(
        &mut self,
        source: ExprId,
        value: crate::fir::FirExprId,
        target: ResolvedTy,
        conversion: FirConversion,
    ) -> Result<bool, BodyCheckFailure> {
        match self.file.expr(source).clone() {
            Expr::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                let (then_value, then_conversion, else_value, else_conversion) = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::Conditional {
                        then_branch,
                        then_conversion,
                        else_branch,
                        else_conversion,
                        ..
                    } => (
                        *then_branch,
                        *then_conversion,
                        *else_branch,
                        *else_conversion,
                    ),
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                // A branch conversion to the conditional's original result must happen before a
                // later conversion of the conditional as a whole. Keep that sequence intact.
                if then_conversion.is_some() || else_conversion.is_some() {
                    return Ok(false);
                }
                let then_value =
                    self.numeric_conditional_branch(then_branch, then_value, target, conversion)?;
                let else_value =
                    self.numeric_conditional_branch(else_branch, else_value, target, conversion)?;
                let checked = self
                    .body
                    .expr_mut(value)
                    .expect("the checked conditional expression still exists");
                let FirExprKind::Conditional {
                    then_branch,
                    else_branch,
                    ..
                } = &mut checked.kind
                else {
                    unreachable!("the checked conditional shape was validated above")
                };
                *then_branch = then_value;
                *else_branch = else_value;
                checked.ty = target;
                Ok(true)
            }
            Expr::When { arms, .. } => {
                let values = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::When { branches, .. } if branches.len() == arms.len() => branches
                        .iter()
                        .map(|branch| branch.result)
                        .collect::<Vec<_>>(),
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let converted = arms
                    .iter()
                    .zip(values)
                    .map(|(arm, value)| {
                        self.numeric_conditional_branch(arm.body, value, target, conversion)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let checked = self
                    .body
                    .expr_mut(value)
                    .expect("the checked when expression still exists");
                let FirExprKind::When { branches, .. } = &mut checked.kind else {
                    unreachable!("the checked when shape was validated above")
                };
                for (branch, converted) in branches.iter_mut().zip(converted) {
                    branch.result = converted;
                }
                checked.ty = target;
                Ok(true)
            }
            Expr::Block {
                trailing: Some(trailing),
                ..
            } => {
                let result = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::Block {
                        result: Some(result),
                        ..
                    } => *result,
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let converted =
                    self.numeric_conditional_branch(trailing, result, target, conversion)?;
                let checked = self
                    .body
                    .expr_mut(value)
                    .expect("the checked block expression still exists");
                let FirExprKind::Block { result, .. } = &mut checked.kind else {
                    unreachable!("the checked block shape was validated above")
                };
                *result = Some(converted);
                checked.ty = target;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn numeric_conditional_branch(
        &mut self,
        source: ExprId,
        value: crate::fir::FirExprId,
        target: ResolvedTy,
        conversion: FirConversion,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        if self.distribute_numeric_conditional(source, value, target, conversion)? {
            return Ok(value);
        }
        let origin = self.expression_origin(source)?;
        Ok(self.convert_fir_value(
            value,
            target,
            origin,
            Some(FirConversion {
                origin,
                kind: conversion.kind,
            }),
        ))
    }

    /// Put a checked platform-type boundary on the producer selected by Kotlin's conditional
    /// rules. A declared expected type reaches `if`/`when` branches and an Elvis fallback, while an
    /// Elvis left operand remains the nullable probe. A block hands the type on to its trailing
    /// value (`when (val x = ...)` is a block declaring the subject before the `when`).
    fn guard_platform_conditional(
        &mut self,
        source: ExprId,
        value: crate::fir::FirExprId,
        target: ResolvedTy,
    ) -> Result<(), BodyCheckFailure> {
        match self.file.expr(source).clone() {
            Expr::If {
                then_branch,
                else_branch: Some(else_branch),
                ..
            } => {
                let (then_value, else_value) = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::Conditional {
                        then_branch,
                        else_branch,
                        ..
                    } => (*then_branch, *else_branch),
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let then_value = self.guard_platform_branch(then_branch, then_value, target)?;
                let else_value = self.guard_platform_branch(else_branch, else_value, target)?;
                let FirExprKind::Conditional {
                    then_branch,
                    else_branch,
                    ..
                } = &mut self
                    .body
                    .expr_mut(value)
                    .expect("the checked conditional expression still exists")
                    .kind
                else {
                    unreachable!("the checked conditional shape was validated above")
                };
                *then_branch = then_value;
                *else_branch = else_value;
            }
            Expr::When { arms, .. } => {
                let values = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::When { branches, .. } if branches.len() == arms.len() => branches
                        .iter()
                        .map(|branch| branch.result)
                        .collect::<Vec<_>>(),
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let guarded = arms
                    .iter()
                    .zip(values)
                    .map(|(arm, value)| self.guard_platform_branch(arm.body, value, target))
                    .collect::<Result<Vec<_>, _>>()?;
                let FirExprKind::When { branches, .. } = &mut self
                    .body
                    .expr_mut(value)
                    .expect("the checked when expression still exists")
                    .kind
                else {
                    unreachable!("the checked when shape was validated above")
                };
                for (branch, guarded) in branches.iter_mut().zip(guarded) {
                    branch.result = guarded;
                }
            }
            Expr::Elvis { rhs, .. } => {
                let rhs_value = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::Elvis { rhs, .. } => *rhs,
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let guarded = self.guard_platform_branch(rhs, rhs_value, target)?;
                let FirExprKind::Elvis { rhs, .. } = &mut self
                    .body
                    .expr_mut(value)
                    .expect("the checked Elvis expression still exists")
                    .kind
                else {
                    unreachable!("the checked Elvis shape was validated above")
                };
                *rhs = guarded;
            }
            Expr::Block {
                trailing: Some(trailing),
                ..
            } => {
                let result = match &self
                    .body
                    .expr(value)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?
                    .kind
                {
                    FirExprKind::Block {
                        result: Some(result),
                        ..
                    } => *result,
                    _ => {
                        return Err(self.failure(None, BodyCheckFailureKind::UnsupportedCallShape));
                    }
                };
                let guarded = self.guard_platform_branch(trailing, result, target)?;
                let FirExprKind::Block { result, .. } = &mut self
                    .body
                    .expr_mut(value)
                    .expect("the checked block expression still exists")
                    .kind
                else {
                    unreachable!("the checked block shape was validated above")
                };
                *result = Some(guarded);
            }
            _ => {}
        }
        Ok(())
    }

    fn guard_platform_branch(
        &mut self,
        source: ExprId,
        value: crate::fir::FirExprId,
        target: ResolvedTy,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        match self.file.expr(source).clone() {
            Expr::If { .. } | Expr::When { .. } | Expr::Elvis { .. } => {
                self.guard_platform_conditional(source, value, target)?;
                Ok(value)
            }
            // A block produces its value through its trailing expression, past its statements and
            // any nested block, as `conditional_branch::branch_value_expression` reads it. A
            // conditional or block there is guarded in place; any other unchecked Java value is
            // guarded around the innermost block, which kotlinc's check cannot name.
            Expr::Block {
                trailing: Some(trailing),
                ..
            } => {
                if is_value_boundary(self.file.expr(trailing)) {
                    self.guard_platform_conditional(source, value, target)?;
                    return Ok(value);
                }
                if !self.info.produces_unchecked_java_value(trailing) {
                    return Ok(value);
                }
                let cause = self.expression_origin(source)?;
                let conversion = self.platform_narrowing_conversion(None, cause, target);
                Ok(self.body.add_expr(crate::fir::FirExpr {
                    origin: cause,
                    ty: target,
                    kind: FirExprKind::ImplicitConversion { value, conversion },
                }))
            }
            Expr::Block { .. } => Ok(value),
            _ if !self.info.produces_unchecked_java_value(source) => Ok(value),
            _ => {
                let cause = self.expression_origin(source)?;
                let conversion = self
                    .platform_producer_conversion(source, cause, target)
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(source),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?;
                Ok(self.body.add_expr(crate::fir::FirExpr {
                    origin: cause,
                    ty: target,
                    kind: FirExprKind::ImplicitConversion { value, conversion },
                }))
            }
        }
    }

    /// An extension call's explicit `receiver`, converted as the argument of the receiver
    /// parameter it is. Besides a smart cast of its own value, a Java value the checker committed
    /// to a receiver type that rejects `null` is guarded (`System.getProperty(key).ext()` checks
    /// `getProperty(...)`).
    pub(super) fn explicit_extension_receiver(
        &mut self,
        call: ExprId,
        receiver: ExprId,
        parameter: Ty,
    ) -> Result<FirReceiver, BodyCheckFailure> {
        let mut checked = self.explicit_receiver(receiver)?;
        if checked.conversion.is_none() && self.info.platform_narrowings.contains_key(&receiver) {
            let span = self
                .file
                .expr_span(receiver)
                .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
            let target = self.resolved_type(span, parameter)?;
            let cause = self.expression_origin(call)?;
            checked.conversion = self.platform_producer_conversion(receiver, cause, target);
        }
        Ok(checked)
    }

    fn platform_producer_conversion(
        &mut self,
        source: ExprId,
        cause: OriginId,
        target: ResolvedTy,
    ) -> Option<FirConversion> {
        // Reordered named arguments make the call a composite expression: their values are
        // evaluated in source order into temporaries before the invocation. The check still
        // exists, but cannot name that composite producer, so kotlinc uses `checkNotNull(Object)`.
        let message = if self.call_reorders_arguments(source) {
            None
        } else {
            Some(self.platform_narrowing_message(source)?)
        };
        Some(self.platform_narrowing_conversion(message, cause, target))
    }

    fn call_reorders_arguments(&self, source: ExprId) -> bool {
        matches!(self.file.expr(source), Expr::Call { .. })
            && self
                .info
                .resolved_call_arg_slots
                .get(&source)
                .is_some_and(|commitment| {
                    commitment
                        .argument_bindings
                        .windows(2)
                        .any(|pair| pair[0].parameter > pair[1].parameter)
                })
    }

    fn platform_narrowing_conversion(
        &mut self,
        message: Option<Box<str>>,
        cause: OriginId,
        target: ResolvedTy,
    ) -> FirConversion {
        let narrowing = self
            .body
            .add_platform_narrowing(FirPlatformNarrowing { message });
        FirConversion {
            origin: cause,
            kind: FirConversionKind::PlatformNarrowing {
                narrowing,
                to: target,
            },
        }
    }

    /// kotlinc names the platform value by the callable that produced it. A call, including an
    /// index `get`, is `name(...)`. A Java bean property names its accessor method the same way.
    /// A declared Kotlin property is `<get-name>(...)`. A physical field is the bare field name.
    fn platform_narrowing_message(&self, source: ExprId) -> Option<Box<str>> {
        match self.file.expr(source) {
            Expr::Call { callee, .. } => match self.file.expr(*callee) {
                Expr::Name(name) | Expr::Member { name, .. } => Some(format!("{name}(...)").into()),
                _ => None,
            },
            Expr::Index { .. } => self.callable_assertion_name(source),
            Expr::Member { name, .. } | Expr::Name(name) => self
                .property_assertion_name(source)
                .or(Some(name.clone().into_boxed_str())),
            _ => None,
        }
    }

    fn property_assertion_name(&self, source: ExprId) -> Option<Box<str>> {
        let spelled = match self.file.expr(source) {
            Expr::Member { name, .. } | Expr::Name(name) => Some(name.clone()),
            _ => None,
        };
        let message = match self.info.expr_lowers.get(&source)? {
            ExprLowering::MemberPropertyRead {
                name,
                accessor,
                producer,
                ..
            } => property_platform_check_name(
                name,
                accessor.as_ref().map(|getter| getter.name.as_str()),
                *producer,
            ),
            ExprLowering::TopLevelPropertyGet(access)
            | ExprLowering::ExtensionPropertyGet { access } => property_platform_check_name(
                &access.property.name,
                Some(access.property.getter.name.as_str()),
                access.property.producer,
            ),
            ExprLowering::MemberExtensionPropertyRead { .. } => {
                format!("<get-{}>(...)", spelled?)
            }
            _ => return None,
        };
        Some(message.into())
    }

    fn callable_assertion_name(&self, source: ExprId) -> Option<Box<str>> {
        let name = match self.info.resolved_calls.get(&source)? {
            ResolvedCall::Member(member) => member.member.name.clone(),
            ResolvedCall::Companion(member) => member.name.clone(),
            ResolvedCall::Extension(call) => call.callable.name.clone(),
            ResolvedCall::TopLevel(call) => call.callable.name.clone(),
            ResolvedCall::MemberExtension { name, .. } => name.clone(),
            ResolvedCall::LocalFunction(_) => return None,
        };
        Some(format!("{name}(...)").into())
    }

    /// Materialize one frontend-committed value boundary. The target and any representation-changing
    /// conversion were already selected by resolution; this only embeds that checked decision in FIR.
    pub(super) fn value_at_selected_boundary(
        &mut self,
        expression: ExprId,
        target: ResolvedTy,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        let origin = self.expression_origin(expression)?;
        let value = self.expression(expression)?;
        let actual = self
            .body
            .expr(value)
            .map(|expression| expression.ty)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        let conversion =
            self.selected_value_conversion_from(expression, value, actual, target, origin)?;
        let Some(conversion) = conversion else {
            return Ok(value);
        };
        Ok(self.body.add_expr(crate::fir::FirExpr {
            origin,
            ty: target,
            kind: crate::fir::FirExprKind::ImplicitConversion { value, conversion },
        }))
    }

    /// The value a block yields, including a fun-interface conversion recorded on a trailing lambda.
    ///
    /// The block's checked type may already be the interface while that lambda is still the
    /// function value. Publishing the recorded conversion makes the block yield the interface
    /// instance.
    pub(super) fn trailing_value_with_recorded_sam(
        &mut self,
        block: ExprId,
        trailing: ExprId,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        let value = self.expression(trailing)?;
        if !self.branch_has_recorded_sam(trailing) {
            return Ok(value);
        }
        let target = self.expression_type(block)?;
        self.with_recorded_sam_conversion(trailing, value, target)
    }

    /// Whether `expression`, or the value a block chain yields, has a recorded fun-interface
    /// conversion.
    pub(super) fn branch_has_recorded_sam(&self, expression: ExprId) -> bool {
        let producer = sam_conversion_producer(self.file, expression);
        self.info.resolved_sam_conversions.contains_key(&producer)
    }

    /// Embed a checker-recorded fun-interface conversion on the value that produces it.
    ///
    /// `if` and `when` arms, and the block that is such an arm, type the lambda as the interface
    /// while the published child is still the function. The conversion stays on the lambda, so the
    /// arm has to carry it or the interface-typed join check-casts the function object. A recorded
    /// conversion that cannot be published is a frontend error: a function-typed target, or a
    /// record that does not materialize a conversion, must not degrade to the original value.
    pub(super) fn with_recorded_sam_conversion(
        &mut self,
        producer: ExprId,
        value: crate::fir::FirExprId,
        target: ResolvedTy,
    ) -> Result<crate::fir::FirExprId, BodyCheckFailure> {
        let producer = sam_conversion_producer(self.file, producer);
        if !self.info.resolved_sam_conversions.contains_key(&producer) {
            return Ok(value);
        }
        if self.value_has_sam_conversion(value) {
            return Ok(value);
        }
        let span = self.file.expr_span(producer);
        if matches!(target.get().non_null(), Ty::Fun(_)) {
            return Err(self.failure(span, BodyCheckFailureKind::UnpublishedRecordedSamConversion));
        }
        let origin = self.expression_origin(producer)?;
        #[cfg(test)]
        let conversion = if OMIT_RECORDED_SAM_PUBLICATION.with(|flag| flag.get()) {
            None
        } else {
            self.selected_argument_conversion(producer, origin)?
        };
        #[cfg(not(test))]
        let conversion = self.selected_argument_conversion(producer, origin)?;
        let Some(conversion) = conversion else {
            return Err(self.failure(span, BodyCheckFailureKind::UnpublishedRecordedSamConversion));
        };
        Ok(self.body.add_expr(crate::fir::FirExpr {
            origin,
            ty: target,
            kind: crate::fir::FirExprKind::ImplicitConversion { value, conversion },
        }))
    }

    fn value_has_sam_conversion(&self, value: crate::fir::FirExprId) -> bool {
        let Some(expression) = self.body.expr(value) else {
            return false;
        };
        match &expression.kind {
            crate::fir::FirExprKind::ImplicitConversion { value, conversion } => {
                matches!(conversion.kind, FirConversionKind::Sam(_))
                    || self.value_has_sam_conversion(*value)
            }
            crate::fir::FirExprKind::Block {
                result: Some(result),
                ..
            } => self.value_has_sam_conversion(*result),
            _ => false,
        }
    }

    pub(super) fn selected_type_conversion(
        &self,
        actual: ResolvedTy,
        target: ResolvedTy,
        cause: OriginId,
    ) -> Option<FirConversion> {
        let actual_ty = actual.get().canonical_semantic();
        let target_ty = target.get().canonical_semantic();
        if actual_ty == target_ty || matches!(actual_ty, Ty::Nothing | Ty::Null) {
            return None;
        }
        let kind = if target_ty == Ty::Unit {
            FirConversionKind::CoerceToUnit
        } else if target_ty.accepts_numeric(actual_ty) {
            FirConversionKind::NumericWidening { to: target }
        } else if matches!(
            actual_ty.non_null(),
            Ty::Intersection(parts) if parts
                .iter()
                .any(|part| part.non_null() == target_ty.non_null())
        ) {
            // Resolution has selected the declaration on one exact constituent of a semantic
            // intersection. Preserve the source value and publish the selected dispatch view as
            // an explicit checked conversion; lowering must not rediscover the member owner.
            FirConversionKind::SmartCast { to: target }
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

    /// An omitted `vararg` with no declared default is an empty array. A declared default is a
    /// real omission: the `$default` stub evaluates it, and the mask bit for that parameter is set.
    fn omitted_vararg_argument(
        &mut self,
        expression: ExprId,
        cause: OriginId,
        parameter: usize,
        parameter_offset: usize,
    ) -> Result<FirCallArgument, BodyCheckFailure> {
        let ordinal = self.call_parameter_ordinal(expression, parameter, parameter_offset)?;
        if self.omitted_vararg_declares_default(expression, parameter)? {
            return Ok(FirCallArgument::Default {
                parameter: ordinal,
                origin: self
                    .origins
                    .synthetic(cause, SyntheticOriginKind::DefaultArgument),
            });
        }
        Ok(FirCallArgument::Vararg {
            parameter: ordinal,
            origin: self
                .origins
                .synthetic(cause, SyntheticOriginKind::VarargArray),
            elements: Box::new([]),
        })
    }

    /// The declaration-owned default flag recorded for this selected call's parameter.
    /// A missing call record or slot is a frontend failure: guessing `false` would pack an empty
    /// array instead of evaluating a declared default.
    fn omitted_vararg_declares_default(
        &self,
        expression: ExprId,
        parameter: usize,
    ) -> Result<bool, BodyCheckFailure> {
        let commitment = self
            .info
            .resolved_call_arg_slots
            .get(&expression)
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        commitment
            .declares_default
            .get(parameter)
            .copied()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
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
                    SourceCallParameters {
                        types: parameters,
                        offset: context.len(),
                        vararg_index,
                    },
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
            let value = self.expression(*argument)?;
            let target = self.resolved_type(
                self.file
                    .expr_span(expression)
                    .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?,
                *parameters.get(parameter).ok_or_else(|| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?,
            )?;
            checked.push(FirCallArgument::Expression {
                parameter: u32::try_from(parameter).map_err(|_| {
                    self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::UnsupportedCallShape,
                    )
                })?,
                value,
                conversion: self.selected_value_conversion(*argument, value, target, cause)?,
            });
        }
        if let Some(parameter) = vararg_index.filter(|_| !saw_vararg) {
            checked.push(self.omitted_vararg_argument(
                expression,
                cause,
                parameter,
                context.len(),
            )?);
        }
        for (parameter, slot) in slots.iter().take(ordinary_count).enumerate() {
            if slot.is_none() && vararg_index != Some(parameter) {
                let ordinal = self.call_parameter_ordinal(expression, parameter, context.len())?;
                if let Some(value) =
                    self.library_default_literal(expression, parameter, parameters, context.len())?
                {
                    checked.push(FirCallArgument::Expression {
                        parameter: ordinal,
                        value,
                        conversion: None,
                    });
                    continue;
                }
                checked.push(FirCallArgument::Default {
                    parameter: ordinal,
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
        let actual_is_unit = actual.ty.get().canonical_semantic() == Ty::Unit
            || actual.ty.get().non_null().kotlin_class_internal()
                == Some(crate::types::type_name("kotlin/Unit"));
        if actual_is_unit && target.get().is_reference() {
            return Ok(self
                .unit_effect_requires_value(receiver.value)
                .then_some(FirConversion {
                    origin: cause,
                    kind: FirConversionKind::CoerceToUnit,
                }));
        }
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
            sam,
            source_suspend,
            ..
        }) = self.info.expr_lowers.get(&expression).cloned()
        else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let [operand] = args else {
            return Err(self.failure(span, BodyCheckFailureKind::UnsupportedCallShape));
        };
        let cause = self.expression_origin(expression)?;
        let conversion = self.published_sam_conversion(span, &sam, false, source_suspend)?;
        let conversion = self.body.add_sam_conversion(conversion);
        let value = self.expression(*operand)?;
        Ok(FirExprKind::ImplicitConversion {
            value,
            conversion: FirConversion {
                origin: cause,
                kind: FirConversionKind::Sam(conversion),
            },
        })
    }

    /// The checked FIR for a conversion to the functional interface `sam` selected, naming the
    /// abstract method it implements by that method's declaration identity. A provider member
    /// published without an identity cannot be named, so such a conversion is rejected.
    pub(super) fn published_sam_conversion(
        &self,
        span: Option<Span>,
        sam: &crate::symbol_resolver::SamSignature,
        nullable: bool,
        source_suspend: bool,
    ) -> Result<FirSamConversion, BodyCheckFailure> {
        let resolved = |ty| {
            ResolvedTy::new(ty)
                .map_err(|error| self.failure(span, BodyCheckFailureKind::UnpublishableType(error)))
        };
        let resolved_all = |types: &[crate::types::Ty]| {
            types
                .iter()
                .copied()
                .map(resolved)
                .collect::<Result<Box<[_]>, _>>()
        };
        let method_target = match sam.declaration {
            Some(crate::symbol_resolver::SamMethodDeclaration::Module(declaration)) => {
                crate::fir::FirSamMethod::Declared(
                    crate::fir::ResolvedFunctionOverrideTarget::Module(
                        self.index
                            .callable_for_declaration(declaration)
                            .ok_or_else(|| {
                                self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)
                            })?
                            .id,
                    ),
                )
            }
            Some(crate::symbol_resolver::SamMethodDeclaration::External(callable)) => {
                crate::fir::FirSamMethod::Declared(
                    crate::fir::ResolvedFunctionOverrideTarget::External(callable),
                )
            }
            Some(crate::symbol_resolver::SamMethodDeclaration::FunctionTypeInvoke) => {
                crate::fir::FirSamMethod::FunctionTypeInvoke
            }
            None => return Err(self.failure(span, BodyCheckFailureKind::MissingStableCallTarget)),
        };
        Ok(FirSamConversion {
            classifier: sam.internal,
            method: sam.method.as_str().into(),
            method_target,
            parameters: resolved_all(&sam.params)?,
            contravariant_parameters: sam.contravariant_params.clone().into_boxed_slice(),
            result: resolved(sam.ret)?,
            declared_parameters: resolved_all(&sam.declared_params)?,
            declared_result: resolved(sam.declared_ret)?,
            context_count: u32::try_from(sam.context_count)
                .map_err(|_| self.failure(span, BodyCheckFailureKind::UnsupportedCallShape))?,
            has_receiver: sam.has_receiver,
            suspend: sam.suspend,
            source_suspend,
            overridden_results: resolved_all(&sam.overridden_results)?,
            nullable,
            kotlin_interface: sam.kotlin_interface,
            parameter_identities: sam.parameter_identities.clone(),
        })
    }
}

/// A conditional, or a block producing its trailing value: the shapes whose value a declared
/// platform narrowing guards where it is produced rather than as a whole.
fn is_value_boundary(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::If { .. } | Expr::When { .. } | Expr::Elvis { .. } | Expr::Block { .. }
    )
}
