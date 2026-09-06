//! Checked constructor applications and constructor body units with final semantic targets.

use super::*;
use crate::ast::{ClassDecl, CtorDelegation, Decl, DeclId, SecondaryCtor};
use crate::libraries::LibraryMember;
use crate::resolve::{
    ResolvedAnnotationConstruction, ResolvedConstructor, ResolvedContextArgument,
    ResolvedCtorDelegation, ResolvedCtorDelegationTarget,
};

impl BodyFirChecker<'_> {
    pub(super) fn constructor_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let selected = self
            .info
            .resolved_constructor(expression)
            .cloned()
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::UnsupportedCallShape,
                )
            })?;
        crate::trace_compiler!(
            "fir",
            "constructor expression={expression:?} type={:?} arguments={arguments:?} selected={selected:?}",
            self.info.expr_types.get(expression.0 as usize),
        );
        match selected {
            ResolvedConstructor::Source {
                owner,
                stable_declaration,
                outer,
                primary,
                context_args,
                params,
                argument_slots,
                omitted,
                vararg,
                ..
            } => self.source_constructor_call(
                expression,
                arguments,
                owner,
                primary,
                context_args,
                params,
                stable_declaration,
                outer,
                argument_slots,
                omitted,
                vararg,
            ),
            ResolvedConstructor::Plain {
                owner,
                outer,
                member,
                annotation,
                context_args,
                args,
            } => {
                let argument_slots = (0..args.len())
                    .map(|parameter| {
                        member
                            .call_sig
                            .vararg_index
                            .filter(|vararg| parameter >= *vararg)
                            .unwrap_or(parameter)
                    })
                    .collect::<Vec<_>>();
                crate::trace_compiler!(
                    "fir",
                    "plain constructor owner={owner} stable={:?} module_classifier={:?} external={:?}",
                    member.stable_declaration,
                    self.index.classifier_declaration(owner),
                    member.external_identity,
                );
                if member.stable_declaration.is_some()
                    || self.index.classifier_declaration(owner).is_some()
                {
                    let primary = member
                        .stable_declaration
                        .and_then(|declaration| self.index.declaration_anchor(declaration))
                        .is_none_or(|anchor| anchor.sibling == 0);
                    return self.source_constructor_call(
                        expression,
                        &args,
                        owner,
                        primary,
                        context_args,
                        member.params.clone(),
                        member.stable_declaration,
                        outer,
                        argument_slots,
                        Vec::new(),
                        member.call_sig.vararg_index,
                    );
                }
                self.external_constructor_call(
                    expression,
                    &args,
                    owner,
                    outer,
                    member,
                    context_args,
                    argument_slots,
                    Vec::new(),
                    annotation,
                )
            }
            ResolvedConstructor::PlainSlots {
                owner,
                outer,
                member,
                annotation,
                context_args,
                slots,
            } => {
                let vararg = member.call_sig.vararg_index;
                let argument_slots = arguments
                    .iter()
                    .map(|argument| {
                        slots
                            .iter()
                            .position(|slot| *slot == Some(*argument))
                            .or(vararg)
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?;
                let omitted = slots
                    .iter()
                    .enumerate()
                    .filter_map(|(parameter, slot)| {
                        (parameter >= context_args.len()
                            && slot.is_none()
                            && vararg != Some(parameter))
                        .then_some(parameter)
                    })
                    .collect();
                crate::trace_compiler!(
                    "fir",
                    "slotted constructor owner={owner} stable={:?} module_classifier={:?} external={:?}",
                    member.stable_declaration,
                    self.index.classifier_declaration(owner),
                    member.external_identity,
                );
                if member.stable_declaration.is_some()
                    || self.index.classifier_declaration(owner).is_some()
                {
                    let primary = member
                        .stable_declaration
                        .and_then(|declaration| self.index.declaration_anchor(declaration))
                        .is_none_or(|anchor| anchor.sibling == 0);
                    return self.source_constructor_call(
                        expression,
                        arguments,
                        owner,
                        primary,
                        context_args,
                        member.params.clone(),
                        member.stable_declaration,
                        outer,
                        argument_slots,
                        omitted,
                        vararg,
                    );
                }
                self.external_constructor_call(
                    expression,
                    arguments,
                    owner,
                    outer,
                    member,
                    context_args,
                    argument_slots,
                    omitted,
                    annotation,
                )
            }
            ResolvedConstructor::Synthetic {
                owner,
                outer,
                ctor,
                annotation,
                context_args,
                args,
            } => {
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
                let argument_slots = args
                    .iter()
                    .map(|argument| slots.iter().position(|slot| *slot == Some(*argument)))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnsupportedCallShape,
                        )
                    })?;
                let vararg = ctor.declaration.call_sig.vararg_index;
                let omitted = slots
                    .iter()
                    .enumerate()
                    .filter_map(|(parameter, slot)| {
                        (parameter >= context_args.len()
                            && slot.is_none()
                            && vararg != Some(parameter))
                        .then_some(parameter)
                    })
                    .collect();
                self.external_constructor_call(
                    expression,
                    &args,
                    owner,
                    outer,
                    ctor.declaration,
                    context_args,
                    argument_slots,
                    omitted,
                    annotation,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn source_constructor_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        owner: crate::types::TypeName,
        primary: bool,
        context_args: Vec<ResolvedContextArgument>,
        parameters: Vec<Ty>,
        stable_declaration: Option<DeclarationId>,
        outer: Option<ExprId>,
        argument_slots: Vec<usize>,
        omitted: Vec<usize>,
        vararg: Option<usize>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let context_parameter_count = u32::try_from(context_args.len())
            .map_err(|_| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        if argument_slots.len() != arguments.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let declaration = stable_declaration
            .filter(|declaration| self.index.callable_for_declaration(*declaration).is_some())
            .or_else(|| {
                self.index
                    .constructor_declaration(owner, primary, &parameters)
            })
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?;
        let target = self
            .index
            .callable_for_declaration(declaration)
            .ok_or_else(|| {
                self.failure(
                    self.file.expr_span(expression),
                    BodyCheckFailureKind::MissingStableCallTarget,
                )
            })?
            .id;
        let checked_arguments = self.checked_constructor_arguments(
            expression,
            arguments,
            &parameters,
            argument_slots,
            omitted,
            vararg,
        )?;
        let checked_arguments = self.constructor_arguments_with_context(
            expression,
            &parameters,
            &context_args,
            checked_arguments,
        )?;
        let outer_receiver = self.constructor_outer_receiver(expression, outer)?;
        let origin = self.expression_origin(expression)?;
        let external_capture_arguments =
            self.external_constructor_capture_arguments(declaration, origin)?;
        let outer_parameter =
            if outer_receiver.is_some() {
                let outer = self
                    .index
                    .classifier_declaration(owner)
                    .and_then(|declaration| self.index.enclosing_owner_classifier(declaration))
                    .ok_or_else(|| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::MissingStableCallTarget,
                        )
                    })?;
                Some(self.resolved_type(
                    self.file.expr_span(expression).ok_or_else(|| {
                        self.failure(None, BodyCheckFailureKind::MissingSourceSpan)
                    })?,
                    Ty::obj_name(outer.classifier),
                )?)
            } else {
                None
            };
        Ok(FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::Module(target),
            context_parameter_count,
            outer_parameter,
            outer_receiver,
            external_capture_arguments,
            parameter_types: self
                .published_parameter_types(self.file.expr_span(expression), &parameters)?,
            arguments: checked_arguments,
            substitutions: self.constructor_substitutions(expression, declaration)?,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn external_constructor_call(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        owner: crate::types::TypeName,
        outer: Option<ExprId>,
        member: LibraryMember,
        context_args: Vec<ResolvedContextArgument>,
        argument_slots: Vec<usize>,
        omitted: Vec<usize>,
        annotation: Option<ResolvedAnnotationConstruction>,
    ) -> Result<FirExprKind, BodyCheckFailure> {
        let context_parameter_count = u32::try_from(context_args.len())
            .map_err(|_| self.failure(None, BodyCheckFailureKind::UnsupportedCallShape))?;
        if argument_slots.len() != arguments.len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        let vararg = member.call_sig.vararg_index;
        let checked_arguments = self.checked_constructor_arguments(
            expression,
            arguments,
            &member.params,
            argument_slots,
            omitted,
            vararg,
        )?;
        let checked_arguments = self.constructor_arguments_with_context(
            expression,
            &member.params,
            &context_args,
            checked_arguments,
        )?;
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        let parameters = member
            .params
            .iter()
            .copied()
            .map(|parameter| self.resolved_type(span, parameter))
            .collect::<Result<Vec<_>, _>>()?;
        crate::trace_compiler!(
            "fir",
            "external constructor owner={owner} parameters={:?} identity={:?}",
            member.params,
            member.external_identity,
        );
        let declaration = member.external_identity.ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        let outer_receiver = self.constructor_outer_receiver(expression, outer)?;
        let outer_parameter = outer_receiver.and_then(|receiver| {
            self.body
                .expr(receiver.value)
                .map(|expression| expression.ty)
        });
        Ok(FirExprKind::ConstructorCall(FirConstructorCall {
            target: FirConstructorTarget::External {
                declaration,
                classifier: owner,
                parameters: parameters.clone().into_boxed_slice(),
                annotation: annotation
                    .map(|annotation| self.checked_annotation_construction(span, annotation))
                    .transpose()?
                    .map(Box::new),
            },
            context_parameter_count,
            outer_parameter,
            outer_receiver,
            external_capture_arguments: None,
            parameter_types: parameters.into_boxed_slice(),
            arguments: checked_arguments,
            substitutions: Box::new([]),
        }))
    }

    fn checked_annotation_construction(
        &self,
        span: Span,
        annotation: ResolvedAnnotationConstruction,
    ) -> Result<FirAnnotationConstruction, BodyCheckFailure> {
        use crate::libraries::DefaultValue;

        if annotation.members.len() != annotation.defaults.len() {
            return Err(self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape));
        }
        let members = annotation
            .members
            .into_iter()
            .map(|(name, ty)| {
                self.resolved_type(span, crate::types::stored_value_ty(ty))
                    .map(|ty| (name.into_boxed_str(), ty))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let defaults = annotation
            .defaults
            .into_iter()
            .map(|default| {
                default.map(|default| match default {
                    DefaultValue::Int(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Int(value))
                    }
                    DefaultValue::Long(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Long(value))
                    }
                    DefaultValue::Double(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Double(value))
                    }
                    DefaultValue::Float(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Float(value))
                    }
                    DefaultValue::Bool(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Boolean(value))
                    }
                    DefaultValue::Char(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::Char(value))
                    }
                    DefaultValue::Str(value) => {
                        FirAnnotationDefaultValue::Constant(FirConstant::String(value))
                    }
                    DefaultValue::Null => FirAnnotationDefaultValue::Constant(FirConstant::Null),
                    DefaultValue::Object(classifier) => {
                        FirAnnotationDefaultValue::Singleton(crate::types::type_name(&classifier))
                    }
                })
            })
            .collect::<Vec<_>>();
        Ok(FirAnnotationConstruction {
            members: members.into_boxed_slice(),
            defaults: defaults.into_boxed_slice(),
        })
    }

    fn checked_constructor_arguments(
        &mut self,
        expression: ExprId,
        arguments: &[ExprId],
        parameters: &[Ty],
        argument_slots: Vec<usize>,
        omitted: Vec<usize>,
        vararg: Option<usize>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let span = self
            .file
            .expr_span(expression)
            .ok_or_else(|| self.failure(None, BodyCheckFailureKind::MissingSourceSpan))?;
        self.checked_constructor_arguments_at(
            span,
            arguments,
            parameters,
            argument_slots,
            omitted,
            vararg,
        )
    }

    fn constructor_arguments_with_context(
        &mut self,
        expression: ExprId,
        parameters: &[Ty],
        context_args: &[ResolvedContextArgument],
        explicit: Box<[FirCallArgument]>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        let cause = self.expression_origin(expression)?;
        self.constructor_arguments_with_context_at(
            self.file.expr_span(expression),
            cause,
            parameters,
            context_args,
            explicit,
        )
    }

    fn constructor_arguments_with_context_at(
        &mut self,
        span: Option<Span>,
        cause: OriginId,
        parameters: &[Ty],
        context_args: &[ResolvedContextArgument],
        explicit: Box<[FirCallArgument]>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        if context_args.is_empty() {
            return Ok(explicit);
        }
        let mut checked = context_args
            .iter()
            .enumerate()
            .map(|(parameter, argument)| {
                let receiver = self.materialize_context_argument_at(span, cause, argument)?;
                Ok(FirCallArgument::Expression {
                    parameter: u32::try_from(parameter).map_err(|_| {
                        self.failure(span, BodyCheckFailureKind::UnsupportedCallShape)
                    })?,
                    value: receiver.value,
                    conversion: self.receiver_conversion_at(
                        span,
                        cause,
                        receiver,
                        parameters.get(parameter).copied(),
                    )?,
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        checked.extend(explicit);
        Ok(checked.into_boxed_slice())
    }

    pub(super) fn checked_constructor_arguments_at(
        &mut self,
        span: Span,
        arguments: &[ExprId],
        parameters: &[Ty],
        argument_slots: Vec<usize>,
        omitted: Vec<usize>,
        vararg: Option<usize>,
    ) -> Result<Box<[FirCallArgument]>, BodyCheckFailure> {
        if arguments.len() != argument_slots.len() {
            return Err(self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape));
        }
        let cause = self.origins.source(self.source, span);
        let mut saw_vararg = false;
        let mut checked_arguments = arguments
            .iter()
            .zip(argument_slots)
            .map(|(argument, parameter)| {
                let parameter_ty = parameters.get(parameter).copied().ok_or_else(|| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let parameter = u32::try_from(parameter).map_err(|_| {
                    self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                })?;
                let value = self.expression(*argument)?;
                if Some(parameter as usize) != vararg {
                    let target = self.resolved_type(span, parameter_ty)?;
                    return Ok(FirCallArgument::Expression {
                        parameter,
                        value,
                        conversion: self
                            .selected_value_conversion(*argument, value, target, cause)?,
                    });
                }
                saw_vararg = true;
                if self
                    .info
                    .resolved_whole_array_vararg_args
                    .contains(argument)
                {
                    let target = self.resolved_type(span, parameter_ty)?;
                    return Ok(FirCallArgument::Expression {
                        parameter,
                        value,
                        conversion: self
                            .selected_value_conversion(*argument, value, target, cause)?,
                    });
                }
                let expected = if self.file.is_spread_arg(*argument) {
                    parameter_ty
                } else {
                    parameter_ty.array_elem().ok_or_else(|| {
                        self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
                    })?
                };
                let target = self.resolved_type(span, expected)?;
                Ok(FirCallArgument::Vararg {
                    parameter,
                    origin: cause,
                    elements: vec![FirVarargElement {
                        value,
                        spread: self.file.is_spread_arg(*argument),
                        conversion: self
                            .selected_value_conversion(*argument, value, target, cause)?,
                    }]
                    .into_boxed_slice(),
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()?;
        // Keep synthesized omissions in parameter order. Source arguments remain in source order,
        // but defaults and an empty vararg are declaration-owned decisions and therefore have one
        // canonical order independent of which call surface produced them (ordinary construction,
        // enum entry, or constructor delegation).
        for parameter in 0..parameters.len() {
            let parameter_id = u32::try_from(parameter).map_err(|_| {
                self.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
            })?;
            if vararg == Some(parameter) && !saw_vararg {
                checked_arguments.push(FirCallArgument::Vararg {
                    parameter: parameter_id,
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::VarargArray),
                    elements: Box::new([]),
                });
            } else if omitted.contains(&parameter) {
                checked_arguments.push(FirCallArgument::Default {
                    parameter: parameter_id,
                    origin: self
                        .origins
                        .synthetic(cause, SyntheticOriginKind::DefaultArgument),
                });
            }
        }
        Ok(checked_arguments.into_boxed_slice())
    }

    fn constructor_outer_receiver(
        &mut self,
        expression: ExprId,
        outer: Option<ExprId>,
    ) -> Result<Option<FirReceiver>, BodyCheckFailure> {
        match outer {
            Some(outer) => Ok(Some(FirReceiver {
                value: self.expression(outer)?,
                conversion: None,
            })),
            None => self.implicit_receiver(expression),
        }
    }

    fn constructor_substitutions(
        &self,
        expression: ExprId,
        constructor: DeclarationId,
    ) -> Result<Box<[FirTypeSubstitution]>, BodyCheckFailure> {
        let Some(owner) = self
            .index
            .declaration_anchor(constructor)
            .and_then(|anchor| anchor.owner)
        else {
            return Ok(Box::new([]));
        };
        let parameters = self.index.classifier_type_arguments(owner).ok_or_else(|| {
            self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::MissingStableCallTarget,
            )
        })?;
        if parameters.is_empty() {
            return Ok(Box::new([]));
        }
        let expression_ty = self.expression_type(expression)?.get();
        if parameters.len() != expression_ty.type_args().len() {
            return Err(self.failure(
                self.file.expr_span(expression),
                BodyCheckFailureKind::UnsupportedCallShape,
            ));
        }
        parameters
            .iter()
            .copied()
            .zip(expression_ty.type_args().iter().copied())
            .map(|(parameter, value)| {
                if self.index.type_parameter_header(parameter).is_none() {
                    return Err(self.failure(
                        self.file.expr_span(expression),
                        BodyCheckFailureKind::MissingStableCallTarget,
                    ));
                }
                Ok(FirTypeSubstitution {
                    parameter: parameter.into(),
                    value: ResolvedTy::new(value).map_err(|error| {
                        self.failure(
                            self.file.expr_span(expression),
                            BodyCheckFailureKind::UnpublishableType(error),
                        )
                    })?,
                    additional_bounds: Box::new([]),
                })
            })
            .collect::<Result<Vec<_>, BodyCheckFailure>>()
            .map(Vec::into_boxed_slice)
    }
}

fn class_at_stable_anchor<'a>(file: &'a File, range: Span) -> Option<(DeclId, &'a ClassDecl)> {
    file.decl_arena
        .iter()
        .enumerate()
        .find_map(|(raw, declaration)| match declaration {
            Decl::Class(class) if class.span == range => {
                Some((DeclId(u32::try_from(raw).ok()?), class))
            }
            Decl::Class(_) | Decl::Fun(_) | Decl::Property(_) => None,
        })
}

fn stable_constructor_callable(
    owner: DeclarationId,
    sibling: u32,
    index: &ResolvedModuleIndex,
) -> Option<ResolvedCallableHeader> {
    (0..index.declaration_count()).find_map(|raw| {
        let declaration = DeclarationId::from_raw(u32::try_from(raw).ok()?);
        let anchor = index.declaration_anchor(declaration)?;
        (anchor.owner == Some(owner)
            && anchor.kind == DeclarationKind::Constructor
            && anchor.sibling == sibling)
            .then(|| index.callable_for_declaration(declaration))?
    })
}

fn secondary_source_arguments(constructor: &SecondaryCtor) -> &[ExprId] {
    match &constructor.delegation {
        CtorDelegation::This(call) | CtorDelegation::Super(call) => &call.args,
        CtorDelegation::None => &[],
    }
}

fn checked_delegation_target(
    checker: &BodyFirChecker<'_>,
    class: DeclarationId,
    span: Span,
    target: &ResolvedCtorDelegationTarget,
) -> Result<FirConstructorTarget, BodyCheckFailure> {
    match target {
        ResolvedCtorDelegationTarget::ThisPrimary { .. } => {
            stable_constructor_callable(class, 0, checker.index)
                .map(|callable| FirConstructorTarget::Module(callable.id))
                .ok_or_else(|| {
                    checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })
        }
        ResolvedCtorDelegationTarget::ThisSecondary { index, .. } => {
            let sibling = u32::try_from(index + 1).map_err(|_| {
                checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
            })?;
            stable_constructor_callable(class, sibling, checker.index)
                .map(|callable| FirConstructorTarget::Module(callable.id))
                .ok_or_else(|| {
                    checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })
        }
        ResolvedCtorDelegationTarget::Super {
            owner,
            params,
            declaration_params,
            implicit_outer: _,
            stable_declaration,
            external_identity,
        } => {
            if let Some(declaration) = stable_declaration {
                return checker
                    .index
                    .callable_for_declaration(*declaration)
                    .map(|callable| FirConstructorTarget::Module(callable.id))
                    .ok_or_else(|| {
                        checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    });
            }
            if external_identity.is_none() {
                return checker
                    .index
                    .unique_constructor_declaration(*owner, declaration_params)
                    .and_then(|declaration| checker.index.callable_for_declaration(declaration))
                    .map(|callable| FirConstructorTarget::Module(callable.id))
                    .ok_or_else(|| {
                        checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                    });
            }
            let parameters = params
                .iter()
                .copied()
                .map(|parameter| checker.resolved_type(span, parameter))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FirConstructorTarget::External {
                declaration: external_identity.ok_or_else(|| {
                    checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget)
                })?,
                classifier: *owner,
                parameters: parameters.into_boxed_slice(),
                annotation: None,
            })
        }
    }
}

fn add_constructor_delegation(
    checker: &mut BodyFirChecker<'_>,
    class: DeclarationId,
    span: Span,
    arguments: &[ExprId],
    resolved: &ResolvedCtorDelegation,
) -> Result<(), BodyCheckFailure> {
    if arguments.len() != resolved.argument_slots.len()
        || arguments.len() != resolved.argument_types.len()
    {
        return Err(checker.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape));
    }
    let target = checked_delegation_target(checker, class, span, &resolved.target)?;
    let origin = checker.origins.source(checker.source, span);
    let previous_capture_access =
        std::mem::replace(&mut checker.constructor_prefix_capture_access, true);
    let checked_sources = (|| {
        let arguments = checker.checked_constructor_arguments_at(
            span,
            arguments,
            resolved.target.params(),
            resolved.argument_slots.clone(),
            resolved.omitted.clone(),
            resolved.vararg,
        )?;
        let arguments = checker.constructor_arguments_with_context_at(
            Some(span),
            origin,
            resolved.target.params(),
            &resolved.context_args,
            arguments,
        )?;
        let outer_receiver = resolved
            .outer_receiver
            .as_ref()
            .map(|selected| checker.materialize_implicit_receiver(origin, Some(span), selected))
            .transpose()?
            .flatten();
        Ok::<_, BodyCheckFailure>((arguments, outer_receiver))
    })();
    checker.constructor_prefix_capture_access = previous_capture_access;
    let (arguments, outer_receiver) = checked_sources?;
    if resolved.outer_receiver.is_some() != outer_receiver.is_some() {
        return Err(checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
    }
    let outer_parameter = match &resolved.target {
        ResolvedCtorDelegationTarget::Super {
            implicit_outer: Some(outer),
            ..
        } => Some(checker.resolved_type(span, Ty::obj_name(*outer))?),
        ResolvedCtorDelegationTarget::ThisPrimary { .. }
        | ResolvedCtorDelegationTarget::ThisSecondary { .. }
        | ResolvedCtorDelegationTarget::Super {
            implicit_outer: None,
            ..
        } => None,
    };
    if outer_parameter.is_some() != outer_receiver.is_some() {
        return Err(checker.failure(Some(span), BodyCheckFailureKind::MissingStableCallTarget));
    }
    let statement = checker.body.add_statement(FirStatement {
        origin,
        kind: FirStatementKind::ConstructorDelegation(FirConstructorCall {
            target,
            context_parameter_count: u32::try_from(resolved.context_args.len()).map_err(|_| {
                checker.failure(Some(span), BodyCheckFailureKind::UnsupportedCallShape)
            })?,
            outer_parameter,
            outer_receiver,
            external_capture_arguments: None,
            parameter_types: checker
                .published_parameter_types(Some(span), resolved.target.params())?,
            arguments,
            substitutions: Box::new([]),
        }),
    });
    checker.body.push_root(statement);
    Ok(())
}

fn checked_constructor_parameters<'a>(
    names: impl IntoIterator<Item = (&'a str, Span, Option<ExprId>)>,
    semantic_types: &[ResolvedTy],
) -> Result<(Vec<CheckedBodyParameter<'a>>, Vec<CheckedBodyDefault>), CheckedBodyDriverFailure> {
    let source = names.into_iter().collect::<Vec<_>>();
    if source.len() != semantic_types.len() {
        return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
    }
    let parameters = source
        .iter()
        .zip(semantic_types.iter().copied())
        .map(|((name, span, _), ty)| CheckedBodyParameter {
            name,
            ty,
            span: *span,
        })
        .collect::<Vec<_>>();
    let defaults = source
        .iter()
        .enumerate()
        .filter_map(|(parameter, (_, _, value))| value.map(|expression| (parameter, expression)))
        .map(|(parameter, expression)| {
            Ok(CheckedBodyDefault {
                parameter: u32::try_from(parameter)
                    .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?,
                expression,
            })
        })
        .collect::<Result<Vec<_>, CheckedBodyDriverFailure>>()?;
    Ok((parameters, defaults))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_and_dispatch_signature_constructor_defaults(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: DefaultArgumentProvider,
    index: &ResolvedModuleIndex,
    active: &ActiveSourceDeclarations,
    origins: &mut OriginStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
) -> Result<(), CheckedBodyDriverFailure> {
    let provider = index
        .declaration_anchor(work.provider)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    if provider.source != source || provider.kind != DeclarationKind::Constructor {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let (_, class, secondary) = active
        .constructor(file, work.provider)
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    let signature = index
        .signature(work.target)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let callable = index
        .callable_for_declaration(work.target)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let context_count = usize::try_from(callable.shape.context_parameter_count)
        .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let context_receivers = signature
        .parameters
        .get(..context_count)
        .ok_or(CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let context_source = class.context_params.iter().map(|parameter| {
        (
            parameter.name.as_str(),
            parameter.ty.span,
            parameter.default,
        )
    });
    let (parameters, defaults, span) = if secondary.is_none() {
        let (parameters, defaults) = checked_constructor_parameters(
            context_source.clone().chain(
                class
                    .props
                    .iter()
                    .map(|parameter| (parameter.name.as_str(), parameter.span, parameter.default)),
            ),
            &signature.parameters,
        )?;
        (parameters, defaults, class.span)
    } else {
        let constructor = secondary.expect("secondary constructor was selected");
        let (parameters, defaults) = checked_constructor_parameters(
            context_source.chain(constructor.params.iter().map(|parameter| {
                (
                    parameter.name.as_str(),
                    parameter.ty.span,
                    parameter.default,
                )
            })),
            &signature.parameters,
        )?;
        (parameters, defaults, constructor.span)
    };
    if defaults.is_empty() {
        return Err(CheckedBodyDriverFailure::MissingBody);
    }
    let owner = BodyOwnerId::from_raw(work.target.raw());
    let mut checker = BodyFirChecker::new(file, info, source, owner, span, index, origins, session);
    checker.constructor_prefix_capture_access = true;
    bind_parameters_and_check_defaults(
        &mut checker,
        &parameters,
        &defaults,
        CheckedBodyReceiverShape {
            context_receivers,
            context_value_count: callable.shape.context_value_count,
            extension_receiver: None,
        },
    )
    .map_err(CheckedBodyDriverFailure::Check)?;
    checker.body.set_default_fragment();
    ordinary_sink.accept(owner, checker.body);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn check_and_dispatch_constructor_body(
    file: &File,
    info: &TypeInfo,
    source: SourceFileId,
    work: BodyWorkItem,
    index: &ResolvedModuleIndex,
    origins: &mut OriginStore,
    ordinary_sink: &mut impl CheckedBodySink,
    session: &mut BodyCheckSession,
    active: Option<&ActiveSourceDeclarations>,
) -> Result<(), CheckedBodyDriverFailure> {
    let anchor = index
        .declaration_anchor(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    if anchor.source != source {
        return Err(CheckedBodyDriverFailure::SourceMismatch);
    }
    let class_declaration = anchor
        .owner
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let (transient_class, class, active_secondary) = match active {
        Some(active) => active
            .constructor(file, work.declaration)
            .ok_or(CheckedBodyDriverFailure::MissingBody)?,
        None => {
            index
                .declaration_anchor(class_declaration)
                .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
            let range = index
                .declaration_range(class_declaration)
                .ok_or(CheckedBodyDriverFailure::MissingBody)?;
            let (transient_class, class) =
                class_at_stable_anchor(file, range).ok_or(CheckedBodyDriverFailure::MissingBody)?;
            (transient_class, class, None)
        }
    };
    if anchor.sibling == 0 {
        let signature = index
            .signature(work.declaration)
            .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
        let callable = index
            .callable_for_declaration(work.declaration)
            .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
        let context_count = callable.shape.context_parameter_count as usize;
        let context_receivers = signature
            .parameters
            .get(..context_count)
            .ok_or(CheckedBodyDriverFailure::ParameterShapeMismatch)?;
        let (parameters, _defaults) = checked_constructor_parameters(
            (0..signature.parameters.len()).map(|ordinal| {
                let context = (ordinal < context_count)
                    .then(|| class.context_params.get(ordinal))
                    .flatten();
                let source = ordinal
                    .checked_sub(context_count)
                    .and_then(|ordinal| class.props.get(ordinal));
                (
                    index
                        .callable_parameter_name(callable.id, ordinal as u32)
                        .expect("a resolved constructor parameter has a stable name"),
                    context.map_or_else(
                        || source.map_or(class.span, |parameter| parameter.span),
                        |parameter| parameter.ty.span,
                    ),
                    source.and_then(|parameter| parameter.default),
                )
            }),
            &signature.parameters,
        )?;
        let mut checker = BodyFirChecker::new(
            file, info, source, work.owner, class.span, index, origins, session,
        );
        bind_parameters_and_check_defaults(
            &mut checker,
            &parameters,
            &[],
            CheckedBodyReceiverShape {
                context_receivers,
                context_value_count: callable.shape.context_value_count,
                extension_receiver: None,
            },
        )
        .map_err(CheckedBodyDriverFailure::Check)?;
        if let Some(resolved) = info.resolved_primary_ctor_delegation(transient_class) {
            add_constructor_delegation(
                &mut checker,
                class_declaration,
                class.span,
                &class.base_args,
                resolved,
            )
            .map_err(CheckedBodyDriverFailure::Check)?;
        } else if class.base_class.is_some() {
            return Err(CheckedBodyDriverFailure::MissingCallable);
        }
        let classifier = index
            .classifier_header(class_declaration)
            .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
        if classifier.interface_delegations.len() != class.interface_delegations.len() {
            return Err(CheckedBodyDriverFailure::ParameterShapeMismatch);
        }
        for (ordinal, (delegation, resolved)) in class
            .interface_delegations
            .iter()
            .zip(classifier.interface_delegations.iter())
            .enumerate()
        {
            match resolved.source {
                crate::fir::ResolvedInterfaceDelegateSource::ConstructorParameter(_)
                | crate::fir::ResolvedInterfaceDelegateSource::SyntheticConstructorParameter(_) => {
                }
                crate::fir::ResolvedInterfaceDelegateSource::ConstructorBodyInitializer => {
                    let value = checker
                        .value_at_selected_boundary(delegation.value, resolved.interface)
                        .map_err(CheckedBodyDriverFailure::Check)?;
                    let origin = checker
                        .expression_origin(delegation.value)
                        .map_err(CheckedBodyDriverFailure::Check)?;
                    let statement = checker.body.add_statement(FirStatement {
                        origin,
                        kind: FirStatementKind::InterfaceDelegationInitializer {
                            classifier: class_declaration,
                            delegation: u32::try_from(ordinal)
                                .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?,
                            value,
                        },
                    });
                    checker.body.push_root(statement);
                }
            }
        }
        ordinary_sink.accept(work.owner, checker.body);
        return Ok(());
    }
    let secondary_index = usize::try_from(anchor.sibling - 1)
        .map_err(|_| CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let constructor = active_secondary
        .or_else(|| class.secondary_ctors.get(secondary_index))
        .ok_or(CheckedBodyDriverFailure::MissingBody)?;
    if active.is_none() && Some(constructor.span) != index.declaration_range(work.declaration) {
        return Err(CheckedBodyDriverFailure::BodyRangeMismatch);
    }
    let signature = index
        .signature(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let callable = index
        .callable_for_declaration(work.declaration)
        .ok_or(CheckedBodyDriverFailure::MissingCallable)?;
    let context_count = callable.shape.context_parameter_count as usize;
    let context_receivers = signature
        .parameters
        .get(..context_count)
        .ok_or(CheckedBodyDriverFailure::ParameterShapeMismatch)?;
    let (parameters, _defaults) = checked_constructor_parameters(
        class
            .context_params
            .iter()
            .map(|parameter| {
                (
                    parameter.name.as_str(),
                    parameter.ty.span,
                    parameter.default,
                )
            })
            .chain(constructor.params.iter().map(|parameter| {
                (
                    parameter.name.as_str(),
                    parameter.ty.span,
                    parameter.default,
                )
            })),
        &signature.parameters,
    )?;
    let mut checker = BodyFirChecker::new(
        file,
        info,
        source,
        work.owner,
        constructor.span,
        index,
        origins,
        session,
    );
    bind_parameters_and_check_defaults(
        &mut checker,
        &parameters,
        &[],
        CheckedBodyReceiverShape {
            context_receivers,
            context_value_count: callable.shape.context_value_count,
            extension_receiver: None,
        },
    )
    .map_err(CheckedBodyDriverFailure::Check)?;
    match info.resolved_ctor_delegation(transient_class, secondary_index) {
        Some(resolved) => add_constructor_delegation(
            &mut checker,
            class_declaration,
            constructor.span,
            secondary_source_arguments(constructor),
            resolved,
        )
        .map_err(CheckedBodyDriverFailure::Check)?,
        None if class.is_enum() && matches!(&constructor.delegation, CtorDelegation::None) => {
            // Enum name/ordinal forwarding is compiler-supplied representation with no source
            // argument or overload decision. The target backend owns that implicit physical call.
        }
        None => return Err(CheckedBodyDriverFailure::MissingCallable),
    }
    if let Some(root) = constructor.body {
        let expression = checker
            .expression(root)
            .map_err(CheckedBodyDriverFailure::Check)?;
        let origin = checker
            .expression_origin(root)
            .map_err(CheckedBodyDriverFailure::Check)?;
        let statement = checker.body.add_statement(FirStatement {
            origin,
            kind: FirStatementKind::Expression(expression),
        });
        checker.body.push_root(statement);
    }
    ordinary_sink.accept(work.owner, checker.body);
    Ok(())
}
